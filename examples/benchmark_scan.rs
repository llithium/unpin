//! Offline, fixed-workload scan benchmark. Build before timing; run sequentially.
use std::io::Cursor;
use std::time::{Duration, Instant};

use clap::Parser;
use image::DynamicImage;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use unpin::cli::Cli;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value_t = 5)]
    runs: usize,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let server = MockServer::start().await;
    let boards = (0..4)
        .map(|index| {
            json!({
                "id": format!("board-{index}"), "type": "board", "name": format!("Board {index}"),
                "url": format!("/alice/board-{index}/"), "pin_count": 24,
                "section_count": 0, "privacy": "public"
            })
        })
        .collect::<Vec<_>>();
    Mock::given(path("/resource/BoardsResource/get/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "resource_response": {"data": boards},
            "resource": {"options": {"bookmarks": ["-end-"]}}
        })))
        .mount(&server)
        .await;
    let image_root = server.uri();
    Mock::given(path("/resource/BoardFeedResource/get/"))
        .respond_with(move |request: &wiremock::Request| {
            let data: Value = serde_json::from_str(&request.url.query_pairs()
                .find(|(key, _)| key == "data").unwrap().1).unwrap();
            let board = data["options"]["board_id"].as_str().unwrap();
            let index: u64 = board.strip_prefix("board-").unwrap().parse().unwrap();
            let pins = (0..24).map(|pin| json!({
                "id": format!("{index}-{pin:02}"),
                "images": {"orig": {"url": format!("{image_root}/image/{index}-{pin}.png"), "width": 64, "height": 64}}
            })).collect::<Vec<_>>();
            ResponseTemplate::new(200).set_delay(Duration::from_millis(index * 150))
                .set_body_json(json!({"resource_response": {"data": pins},
                    "resource": {"options": {"bookmarks": ["-end-"]}}}))
        }).mount(&server).await;
    Mock::given(path("/resource/UserPinsResource/get/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "resource_response": {"data": []}, "resource": {"options": {"bookmarks": ["-end-"]}}
        })))
        .mount(&server)
        .await;
    let mut png = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(64, 64)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex("^/image/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(png.into_inner())
                .set_delay(Duration::from_millis(150)),
        )
        .mount(&server)
        .await;
    let cli = Cli::parse_from(["unpin", "alice", "--exact-only"]);
    let mut timings = Vec::new();
    for _ in 0..args.runs {
        let started = Instant::now();
        let report = unpin::run_with_api_root(&cli, Some(server.uri().parse().unwrap()))
            .await
            .unwrap();
        timings.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(report.summary.analyzed, 96);
        assert_eq!(report.exact_groups.len(), 1);
        assert_eq!(report.exact_groups[0].items.len(), 96);
        assert!(report.skipped.is_empty());
        let canonical = serde_json::to_string(&report)
            .unwrap()
            .replace(&server.uri(), "http://fixture");
        println!("report_sha256={:x}", Sha256::digest(canonical.as_bytes()));
    }
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path().starts_with("/image/"))
            .count(),
        args.runs * 96
    );
    println!("scan_ms={timings:?}");
}
