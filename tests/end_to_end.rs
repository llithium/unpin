use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use clap::Parser;
use image::{DynamicImage, ImageBuffer, ImageFormat, Rgb};
use serde_json::json;
use unpin::auth::BrowserCookie;
use unpin::cli::Cli;
use unpin::pinterest::{BoardTarget, PinterestClient, PinterestError};
use unpin::progress::{ProgressEvent, ProgressSink};
use unpin::report::MatchScope;
use url::Url;
use wiremock::matchers::{header, header_regex, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Debug, Default)]
struct RecordingProgress {
    events: Mutex<Vec<ProgressEvent>>,
}

impl ProgressSink for RecordingProgress {
    fn emit(&self, event: ProgressEvent) {
        self.events.lock().unwrap().push(event);
    }
}

impl RecordingProgress {
    fn events(&self) -> Vec<ProgressEvent> {
        self.events.lock().unwrap().clone()
    }
}

fn page(results: serde_json::Value, bookmark: &str) -> serde_json::Value {
    json!({
        "resource_response": { "data": results },
        "resource": { "options": { "bookmarks": [bookmark] } }
    })
}

fn request_data(options: serde_json::Value) -> String {
    serde_json::to_string(&json!({ "options": options })).unwrap()
}

fn image_bytes(width: u32, height: u32) -> Vec<u8> {
    let image = DynamicImage::ImageRgb8(ImageBuffer::from_fn(width, height, |x, y| {
        Rgb([
            (x.saturating_mul(255) / width.max(1)) as u8,
            (y.saturating_mul(255) / height.max(1)) as u8,
            64,
        ])
    }));
    let mut cursor = Cursor::new(Vec::new());
    image.write_to(&mut cursor, ImageFormat::Png).unwrap();
    cursor.into_inner()
}

async fn mount_resource(
    server: &MockServer,
    resource: &str,
    options: serde_json::Value,
    response: serde_json::Value,
) {
    Mock::given(method("GET"))
        .and(path(format!("/resource/{resource}Resource/get/")))
        .and(query_param("data", request_data(options)))
        .and(query_param("source_url", ""))
        .and(header("x-requested-with", "XMLHttpRequest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .mount(server)
        .await;
}

#[tokio::test]
async fn scans_paginated_board_and_sections_end_to_end() {
    let server = MockServer::start().await;
    let image = image_bytes(16, 12);
    Mock::given(method("GET"))
        .and(path("/image.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(image),
        )
        .expect(1)
        .mount(&server)
        .await;

    mount_resource(
        &server,
        "Board",
        json!({
            "slug": "ideas",
            "username": "alice",
            "field_set_key": "detailed"
        }),
        json!({
            "resource_response": { "data": {
                "id": "board-1",
                "name": "Ideas",
                "pin_count": 4,
                "section_count": 1,
                "ignored_schema_field": {"can_change": true}
            }}
        }),
    )
    .await;

    mount_resource(
        &server,
        "BoardFeed",
        json!({
            "board_id": "board-1",
            "field_set_key": "react_grid_pin",
            "prepend": false,
            "bookmarks": null
        }),
        page(
            json!([{
                "id": "101",
                "images": {"orig": {
                    "url": format!("{}/image.png", server.uri()),
                    "width": 16,
                    "height": 12
                }}
            }]),
            "next-page",
        ),
    )
    .await;
    mount_resource(
        &server,
        "BoardFeed",
        json!({
            "board_id": "board-1",
            "field_set_key": "react_grid_pin",
            "prepend": false,
            "bookmarks": ["next-page"]
        }),
        page(
            json!([{
                "id": "102",
                "images": {"orig": {
                    "url": format!("{}/image.png", server.uri()),
                    "width": 16,
                    "height": 12
                }}
            }]),
            "-end-",
        ),
    )
    .await;
    mount_resource(
        &server,
        "BoardSections",
        json!({"board_id": "board-1"}),
        page(json!([{"id": "section-1"}]), "-end-"),
    )
    .await;
    mount_resource(
        &server,
        "BoardSectionPins",
        json!({"section_id": "section-1", "bookmarks": null}),
        page(
            json!([
                {
                    "id": "102",
                    "images": {"orig": {
                        "url": format!("{}/image.png", server.uri())
                    }}
                },
                {
                    "id": "103",
                    "is_video": true,
                    "videos": {"video_list": {}},
                    "images": {"orig": {
                        "url": format!("{}/poster.jpg", server.uri())
                    }}
                }
            ]),
            "-end-",
        ),
    )
    .await;

    let cli = Cli::try_parse_from([
        "unpin",
        "https://www.pinterest.com/alice/ideas/",
        "--format",
        "json",
    ])
    .unwrap();
    let progress = RecordingProgress::default();
    let mut report = unpin::run_with_api_root_and_progress(
        &cli,
        Some(Url::parse(&server.uri()).unwrap()),
        &progress,
    )
    .await
    .unwrap();

    assert_eq!(report.summary.username, None);
    assert_eq!(report.summary.boards.len(), 1);
    assert_eq!(report.summary.boards[0].name, "Ideas");
    assert_eq!(report.title(), "Ideas");
    assert_eq!(report.summary.pins_reported, Some(4));
    assert_eq!(report.summary.pins_found, 3);
    assert_eq!(report.summary.analyzed, 2);
    assert_eq!(report.summary.skipped, 1);
    assert_eq!(report.summary.exact_groups, 1);
    assert_eq!(report.summary.visual_candidates, 0);
    assert_eq!(report.warnings.len(), 1);
    assert!(report.warnings[0].contains("returned only 3 anonymously"));
    assert_eq!(report.exact_groups[0].items.len(), 2);
    assert!(report.skipped[0].reason.contains("video"));
    assert!(progress.events().contains(&ProgressEvent::FetchingBoard));
    assert!(progress.events().contains(&ProgressEvent::PageFetched {
        resource: "BoardFeed",
        page: 2,
        items: 2,
    }));
    assert!(
        progress
            .events()
            .contains(&ProgressEvent::SectionsStarted { total: 1 })
    );
    assert!(
        progress
            .events()
            .contains(&ProgressEvent::ImagesStarted { total: 1 })
    );
    assert!(progress.events().contains(&ProgressEvent::ImageFinished {
        completed: 1,
        total: 1,
    }));
    assert!(progress.events().contains(&ProgressEvent::MatchingStarted));

    let visual_path = unpin::visual::create_temporary_report(&report).unwrap();
    assert!(visual_path.starts_with(std::env::temp_dir()));
    let visual_html = std::fs::read_to_string(&visual_path).unwrap();
    assert!(visual_html.contains("Exact group 1"));
    assert!(visual_html.contains(&format!("{}/image.png", server.uri())));
    report.visual_report = Some(visual_path.to_string_lossy().into_owned());

    let json = report.render_json().unwrap();
    assert!(json.contains("\"exact_groups\""));
    assert!(json.contains("\"image_url\""));
    assert!(json.contains("\"visual_report\""));
    let text = report.render_text();
    assert!(text.contains("https://www.pinterest.com/pin/101/"));
    assert!(text.contains("https://www.pinterest.com/pin/102/"));
    assert!(text.contains("VISUAL REPORT"));

    std::fs::remove_file(visual_path).unwrap();
}

/// Mounts a `Boards` listing for `alice` with the given board entries.
async fn mount_board_listing(server: &MockServer, boards: serde_json::Value) {
    mount_resource(
        server,
        "Boards",
        json!({
            "username": "alice",
            "field_set_key": "profile_grid_item",
            "sort": "last_pinned_to",
            "filter_stories": false,
            "page_size": 25,
            "include_archived": true,
            "bookmarks": null
        }),
        page(boards, "-end-"),
    )
    .await;
}

fn board_entry(id: &str, name: &str, slug: &str, pins: usize) -> serde_json::Value {
    json!({
        "id": id,
        "type": "board",
        "name": name,
        "url": format!("/alice/{slug}/"),
        "pin_count": pins,
        "section_count": 0,
        "privacy": "public"
    })
}

async fn mount_board_feed(server: &MockServer, board_id: &str, pin_id: &str, image: &str) {
    mount_resource(
        server,
        "BoardFeed",
        json!({
            "board_id": board_id,
            "field_set_key": "react_grid_pin",
            "prepend": false,
            "bookmarks": null
        }),
        page(
            json!([{
                "id": pin_id,
                "images": {"orig": {"url": image, "width": 20, "height": 20}}
            }]),
            "-end-",
        ),
    )
    .await;
}

#[tokio::test]
async fn profile_scans_include_unorganized_pins() {
    let server = MockServer::start().await;
    let image = image_bytes(20, 20);
    Mock::given(method("GET"))
        .and(path("/profile-pin.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(image),
        )
        .mount(&server)
        .await;

    mount_board_listing(&server, json!([])).await;
    mount_resource(
        &server,
        "UserPins",
        json!({
            "username": "alice",
            "field_set_key": "grid_item",
            "page_size": 25,
            "bookmarks": null
        }),
        page(
            json!([
                {
                    "id": "board-pin",
                    "board": {"layout": "default", "url": "/alice/ideas/"},
                    "images": {"orig": {
                        "url": format!("{}/profile-pin.png", server.uri()),
                        "width": 20,
                        "height": 20
                    }}
                },
                {
                    "id": "profile-pin",
                    "board": {
                        "layout": "quick_saves",
                        "url": "/alice/_quick_saves/"
                    },
                    "images": {"orig": {
                        "url": format!("{}/profile-pin.png", server.uri()),
                        "width": 20,
                        "height": 20
                    }}
                }
            ]),
            "-end-",
        ),
    )
    .await;

    let cli = Cli::try_parse_from(["unpin", "alice", "--exact-only"]).unwrap();
    let report = unpin::run_with_api_root(&cli, Some(Url::parse(&server.uri()).unwrap()))
        .await
        .unwrap();

    assert_eq!(report.summary.boards.len(), 1);
    assert_eq!(report.summary.boards[0].name, "Unorganized ideas");
    assert_eq!(report.summary.pins_found, 1);
    assert_eq!(report.summary.analyzed, 1);
}

#[tokio::test]
async fn one_failing_board_does_not_discard_the_others() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/shared.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(image_bytes(24, 24)),
        )
        .mount(&server)
        .await;

    mount_board_listing(
        &server,
        json!([
            board_entry("board-1", "Interiors", "interiors", 1),
            board_entry("board-2", "Mood board", "mood-board", 1),
        ]),
    )
    .await;
    // Only the first board's feed resolves; the second returns 403, standing in
    // for a session that expired part-way through a scan.
    mount_board_feed(
        &server,
        "board-1",
        "301",
        &format!("{}/shared.png", server.uri()),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/resource/BoardFeedResource/get/"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    // With no selection option, a profile scans all boards by default.
    let cli = Cli::try_parse_from(["unpin", "alice"]).unwrap();
    let report = unpin::run_with_api_root(&cli, Some(Url::parse(&server.uri()).unwrap()))
        .await
        .unwrap();

    // The reachable board still produced a report.
    assert_eq!(report.summary.boards.len(), 1);
    assert_eq!(report.summary.boards[0].name, "Interiors");
    assert_eq!(report.summary.analyzed, 1);

    // The failure survives as a board-prefixed warning rather than vanishing.
    let warning = report
        .warnings
        .iter()
        .find(|warning| warning.contains("skipped"))
        .expect("the failing board should be reported");
    assert!(warning.starts_with("Mood board: "), "{warning}");
    assert!(warning.contains("403"), "{warning}");
}

#[tokio::test]
async fn total_failure_reports_the_reason_not_an_empty_board() {
    let server = MockServer::start().await;
    mount_board_listing(
        &server,
        json!([
            board_entry("board-1", "Interiors", "interiors", 1),
            board_entry("board-2", "Mood board", "mood-board", 1),
        ]),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/resource/BoardFeedResource/get/"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let cli = Cli::try_parse_from(["unpin", "alice"]).unwrap();
    let error = unpin::run_with_api_root(&cli, Some(Url::parse(&server.uri()).unwrap()))
        .await
        .unwrap_err();
    let message = error.to_string();

    // Reporting this as "no analyzable pins" would be both wrong and
    // unactionable: nothing was fetched at all.
    assert!(message.contains("no board could be scanned"), "{message}");
    assert!(!message.contains("no analyzable"), "{message}");
    // Every scanned source's reason is carried through as an indented list, so
    // the cause is visible instead of being dropped with the report that never
    // got built.
    for board in ["Interiors", "Mood board", "Unorganized ideas"] {
        assert!(
            message.contains(&format!("\n  {board}: skipped,")),
            "{message} missing an indented reason for {board}"
        );
    }
    assert!(message.contains("403"), "{message}");
    assert_eq!(message.lines().count(), 4, "{message}");
}

#[tokio::test]
async fn board_urls_in_the_report_are_encoded_and_http() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/shared.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(image_bytes(24, 24)),
        )
        .mount(&server)
        .await;

    mount_board_listing(
        &server,
        json!([
            // A hostile URL, and a board with none at all.
            {
                "id": "board-1",
                "type": "board",
                "name": "Hostile",
                "url": "javascript:alert(1)",
                "pin_count": 1,
                "section_count": 0,
                "privacy": "public"
            },
            {
                "id": "board-2",
                "type": "board",
                "name": "No URL",
                "pin_count": 1,
                "section_count": 0,
                "privacy": "public"
            }
        ]),
    )
    .await;
    mount_board_feed(
        &server,
        "board-1",
        "401",
        &format!("{}/shared.png", server.uri()),
    )
    .await;
    mount_board_feed(
        &server,
        "board-2",
        "402",
        &format!("{}/shared.png", server.uri()),
    )
    .await;

    let cli = Cli::try_parse_from(["unpin", "alice"]).unwrap();
    let report = unpin::run_with_api_root(&cli, Some(Url::parse(&server.uri()).unwrap()))
        .await
        .unwrap();

    for board in &report.summary.boards {
        assert_eq!(
            board.url, "",
            "{:?} kept a bad URL: {}",
            board.name, board.url
        );
    }

    // Neither an empty href nor a javascript: one reaches the report.
    let html = unpin::visual::render_html(&report);
    assert!(!html.contains("javascript:"));
    assert!(!html.contains("href=\"\""));
}

#[tokio::test]
async fn scans_selected_profile_boards_as_one_pooled_report() {
    let server = MockServer::start().await;
    // Both boards hold the same image, so the duplicate only exists across them.
    Mock::given(method("GET"))
        .and(path("/shared.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(image_bytes(20, 20)),
        )
        .mount(&server)
        .await;

    mount_resource(
        &server,
        "Boards",
        json!({
            "username": "alice",
            "field_set_key": "profile_grid_item",
            "sort": "last_pinned_to",
            "filter_stories": false,
            "page_size": 25,
            "include_archived": true,
            "bookmarks": null
        }),
        page(
            json!([
                {
                    "id": "board-1",
                    "type": "board",
                    "name": "Interiors",
                    "url": "/alice/interiors/",
                    "pin_count": 1,
                    "section_count": 0,
                    "privacy": "public"
                },
                {
                    "id": "board-2",
                    "type": "board",
                    "name": "Mood board",
                    "url": "/alice/mood-board/",
                    "pin_count": 1,
                    "section_count": 0,
                    "privacy": "secret"
                },
                {
                    "id": "6862930149517574268",
                    "type": "userdiditreminder"
                },
                {
                    "id": "board-3",
                    "type": "board",
                    "name": "Recipes",
                    "url": "/alice/recipes/",
                    "pin_count": 9,
                    "section_count": 0,
                    "privacy": "public"
                }
            ]),
            "-end-",
        ),
    )
    .await;

    for (board_id, pin_id) in [("board-1", "201"), ("board-2", "202")] {
        mount_resource(
            &server,
            "BoardFeed",
            json!({
                "board_id": board_id,
                "field_set_key": "react_grid_pin",
                "prepend": false,
                "bookmarks": null
            }),
            page(
                json!([{
                    "id": pin_id,
                    "images": {"orig": {
                        "url": format!("{}/shared.png", server.uri()),
                        "width": 20,
                        "height": 20
                    }}
                }]),
                "-end-",
            ),
        )
        .await;
    }

    // `--boards` picks two of the three boards, by slug and by name.
    let cli = Cli::try_parse_from([
        "unpin",
        "https://www.pinterest.com/alice/",
        "--boards",
        "interiors,Mood board",
    ])
    .unwrap();
    let progress = RecordingProgress::default();
    let report = unpin::run_with_api_root_and_progress(
        &cli,
        Some(Url::parse(&server.uri()).unwrap()),
        &progress,
    )
    .await
    .unwrap();

    assert_eq!(report.summary.username.as_deref(), Some("alice"));
    assert_eq!(report.summary.boards.len(), 2, "Recipes was not selected");
    // The non-board grid entry must never reach BoardFeed; wiremock has no
    // stub for it, so scanning it would have failed this run outright.
    assert!(
        progress
            .events()
            .contains(&ProgressEvent::UserBoardsResolved { total: 3 })
    );
    assert_eq!(report.summary.boards[0].name, "Interiors");
    assert_eq!(report.summary.boards[1].name, "Mood board");
    // Board links point at Pinterest itself, never at the API root in use.
    assert_eq!(
        report.summary.boards[0].url,
        "https://www.pinterest.com/alice/interiors/"
    );
    assert_eq!(report.summary.pins_found, 2);
    assert_eq!(report.summary.pins_reported, Some(2));
    assert_eq!(report.summary.analyzed, 2);
    assert_eq!(report.title(), "alice — 2 boards");

    // The duplicate spans the two boards, which a per-board scan could not find.
    assert_eq!(report.summary.exact_groups, 1);
    let items = &report.exact_groups[0].items;
    assert_eq!(items.len(), 2);
    let mut boards = items
        .iter()
        .map(|item| item.board.clone().unwrap())
        .collect::<Vec<_>>();
    boards.sort();
    assert_eq!(boards, ["Interiors", "Mood board"]);
    assert_eq!(report.exact_groups[0].scope, MatchScope::CrossBoard);

    assert!(
        progress
            .events()
            .contains(&ProgressEvent::FetchingUserBoards {
                username: "alice".into(),
            })
    );
    assert!(
        progress
            .events()
            .contains(&ProgressEvent::UserBoardsResolved { total: 3 })
    );
    assert!(progress.events().contains(&ProgressEvent::BoardStarted {
        name: "Mood board".into(),
        current: 2,
        total: 2,
    }));
    let events = progress.events();
    let second_board_started = events
        .iter()
        .position(|event| matches!(event, ProgressEvent::BoardStarted { current: 2, .. }))
        .unwrap();
    let first_board_page = events
        .iter()
        .position(|event| {
            matches!(
                event,
                ProgressEvent::PageFetched {
                    resource: "BoardFeed",
                    ..
                }
            )
        })
        .unwrap();
    assert!(
        second_board_started < first_board_page,
        "both board requests should be started before either response is merged"
    );

    let text = report.render_text();
    assert!(text.contains("[Interiors]"));
    assert!(text.contains("[Mood board]"));
    assert!(text.contains("ACROSS BOARDS"));
    let html =
        std::fs::read_to_string(unpin::visual::create_temporary_report(&report).unwrap()).unwrap();
    assert!(html.contains("alice — 2 boards"));
    assert!(html.contains("class=\"board\""));
    assert!(html.contains("title=\"Interiors\""));
    assert!(html.contains("badge cross-board"));

    let same_only = Cli::try_parse_from([
        "unpin",
        "alice",
        "--boards",
        "interiors,Mood board",
        "--same-board-only",
    ])
    .unwrap();
    let report = unpin::run_with_api_root(&same_only, Some(Url::parse(&server.uri()).unwrap()))
        .await
        .unwrap();
    assert!(report.exact_groups.is_empty());
    assert_eq!(report.summary.exact_groups, 0);

    let cross_only = Cli::try_parse_from([
        "unpin",
        "alice",
        "--boards",
        "interiors,Mood board",
        "--cross-board-only",
    ])
    .unwrap();
    let report = unpin::run_with_api_root(&cross_only, Some(Url::parse(&server.uri()).unwrap()))
        .await
        .unwrap();
    assert_eq!(report.exact_groups.len(), 1);
    assert_eq!(report.summary.exact_groups, 1);
    assert_eq!(report.exact_groups[0].scope, MatchScope::CrossBoard);
}

#[tokio::test]
async fn interactive_selection_requires_a_terminal() {
    let cli = Cli::try_parse_from(["unpin", "alice", "--interactive"]).unwrap();

    // The test harness has no terminal, so the picker cannot run.
    let error = unpin::run_with_api_root(&cli, Some(Url::parse("http://127.0.0.1:1/").unwrap()))
        .await
        .unwrap_err();

    let message = error.to_string();
    assert!(message.contains("--interactive"), "{message}");
}

#[tokio::test]
async fn board_selection_flags_are_rejected_for_a_board_url() {
    let cli = Cli::try_parse_from([
        "unpin",
        "https://www.pinterest.com/alice/ideas/",
        "--boards",
        "ideas",
    ])
    .unwrap();

    let error = unpin::run_with_api_root(&cli, Some(Url::parse("http://127.0.0.1:1/").unwrap()))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("only apply to a username"));
}

#[tokio::test]
async fn retries_throttled_pinterest_requests_using_retry_after() {
    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let responder_attempts = attempts.clone();
    Mock::given(method("GET"))
        .and(path("/resource/BoardResource/get/"))
        .respond_with(move |_request: &wiremock::Request| {
            if responder_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(429).insert_header("retry-after", "0")
            } else {
                ResponseTemplate::new(200).set_body_json(json!({
                    "resource_response": {"data": {
                        "id": "board-1",
                        "name": "Ideas",
                        "pin_count": 0,
                        "section_count": 0
                    }}
                }))
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    mount_resource(
        &server,
        "BoardFeed",
        json!({
            "board_id": "board-1",
            "field_set_key": "react_grid_pin",
            "prepend": false,
            "bookmarks": null
        }),
        page(json!([]), "-end-"),
    )
    .await;

    let target = BoardTarget::parse("https://www.pinterest.com/alice/ideas/").unwrap();
    let client =
        PinterestClient::with_api_root(target.root.clone(), Url::parse(&server.uri()).unwrap())
            .unwrap();
    let progress = RecordingProgress::default();
    let result = client
        .fetch_board_with_progress(&target, &progress)
        .await
        .unwrap();

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(result.board_name, "Ideas");
    assert!(progress.events().contains(&ProgressEvent::RequestRetry {
        resource: "Board",
        attempt: 2,
        delay: std::time::Duration::ZERO,
    }));
}

#[tokio::test]
async fn pinterest_http_errors_do_not_echo_response_bodies() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/resource/BoardResource/get/"))
        .respond_with(ResponseTemplate::new(403).set_body_string("sensitive upstream body"))
        .mount(&server)
        .await;

    let target = BoardTarget::parse("https://www.pinterest.com/alice/ideas/").unwrap();
    let client =
        PinterestClient::with_api_root(target.root.clone(), Url::parse(&server.uri()).unwrap())
            .unwrap();
    let error = client.fetch_board(&target).await.unwrap_err();

    assert!(matches!(
        error,
        PinterestError::Http {
            status: reqwest::StatusCode::FORBIDDEN,
            ..
        }
    ));
    assert!(!error.to_string().contains("sensitive"));
}

#[tokio::test]
async fn malformed_pinterest_json_is_a_clean_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/resource/BoardResource/get/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not-json"))
        .mount(&server)
        .await;

    let target = BoardTarget::parse("https://www.pinterest.com/alice/ideas/").unwrap();
    let client =
        PinterestClient::with_api_root(target.root.clone(), Url::parse(&server.uri()).unwrap())
            .unwrap();
    let error = client.fetch_board(&target).await.unwrap_err();

    assert!(matches!(
        error,
        PinterestError::Request {
            resource: "Board",
            ..
        }
    ));
    assert!(!error.to_string().contains("not-json"));
}

#[tokio::test]
async fn authenticated_pinterest_requests_send_imported_cookies_and_csrf() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/resource/BoardResource/get/"))
        .and(header_regex("cookie", r".*_pinterest_sess=test-session.*"))
        .and(header("x-csrftoken", "test-csrf"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let target = BoardTarget::parse("https://www.pinterest.com/alice/ideas/").unwrap();
    let client = PinterestClient::with_api_root_and_cookies(
        target.root.clone(),
        Url::parse(&server.uri()).unwrap(),
        vec![
            BrowserCookie {
                name: "_pinterest_sess".into(),
                value: "test-session".into(),
            },
            BrowserCookie {
                name: "csrftoken".into(),
                value: "test-csrf".into(),
            },
        ],
    )
    .unwrap();
    let error = client.fetch_board(&target).await.unwrap_err();

    assert!(matches!(
        error,
        PinterestError::Http {
            status: reqwest::StatusCode::FORBIDDEN,
            ..
        }
    ));
}
