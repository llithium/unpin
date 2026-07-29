use std::io::Cursor;
use std::sync::Mutex;

use clap::Parser;
use image::{DynamicImage, ImageBuffer, ImageFormat, Rgb};
use serde_json::json;
use unpin::auth::BrowserCookie;
use unpin::cli::Cli;
use unpin::pinterest::{BoardTarget, PinterestClient, PinterestError};
use unpin::progress::{ProgressEvent, ProgressSink};
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

    let text = report.render_text();
    assert!(text.contains("[Interiors]"));
    assert!(text.contains("[Mood board]"));
    let html =
        std::fs::read_to_string(unpin::visual::create_temporary_report(&report).unwrap()).unwrap();
    assert!(html.contains("alice — 2 boards"));
    assert!(html.contains("class=\"board\">Interiors<"));
}

#[tokio::test]
async fn profile_targets_refuse_to_guess_boards_without_a_terminal() {
    let cli = Cli::try_parse_from(["unpin", "alice"]).unwrap();

    // The test harness has no terminal, so the picker cannot run.
    let error = unpin::run_with_api_root(&cli, Some(Url::parse("http://127.0.0.1:1/").unwrap()))
        .await
        .unwrap_err();

    let message = error.to_string();
    assert!(message.contains("--boards"), "{message}");
    assert!(message.contains("--all-boards"), "{message}");
}

#[tokio::test]
async fn board_selection_flags_are_rejected_for_a_board_url() {
    let cli = Cli::try_parse_from([
        "unpin",
        "https://www.pinterest.com/alice/ideas/",
        "--all-boards",
    ])
    .unwrap();

    let error = unpin::run_with_api_root(&cli, Some(Url::parse("http://127.0.0.1:1/").unwrap()))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("only apply to a username"));
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
