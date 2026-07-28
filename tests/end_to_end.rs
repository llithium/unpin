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

    assert_eq!(report.summary.board_name, "Ideas");
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
async fn pinterest_http_errors_do_not_echo_response_bodies() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/resource/BoardResource/get/"))
        .respond_with(ResponseTemplate::new(403).set_body_string("sensitive upstream body"))
        .mount(&server)
        .await;

    let target = BoardTarget::parse("https://www.pinterest.com/alice/ideas/").unwrap();
    let client =
        PinterestClient::with_api_root(target, Url::parse(&server.uri()).unwrap()).unwrap();
    let error = client.fetch_board().await.unwrap_err();

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
        PinterestClient::with_api_root(target, Url::parse(&server.uri()).unwrap()).unwrap();
    let error = client.fetch_board().await.unwrap_err();

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
        target,
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
    let error = client.fetch_board().await.unwrap_err();

    assert!(matches!(
        error,
        PinterestError::Http {
            status: reqwest::StatusCode::FORBIDDEN,
            ..
        }
    ));
}
