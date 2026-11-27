//! The review server against a mock Pinterest: saving decisions to the
//! review file, checking pins, and refusing requests from other sites.

use serde_json::{Value, json};
use unpin::pinterest::{PinPresence, PinterestClient};
use unpin::report::Report;
use unpin::review_store::ReviewStore;
use unpin::server::ReviewServer;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn report() -> Report {
    let item = |id: &str| {
        json!({"pin_id": id, "pin_url": format!("https://www.pinterest.com/pin/{id}/"),
        "image_url": format!("https://example.test/{id}.jpg"), "image_sha256": "abc", "board": "Ideas </script>",
        "width": 100, "height": 100, "byte_size": 1000, "recommendation": "keep"})
    };
    serde_json::from_value(json!({
        "summary": {"boards": [], "pins_found": 2, "analyzed": 2, "skipped": 0, "exact_groups": 1, "visual_candidates": 0},
        "exact_groups": [{"scope": "same_board", "items": [item("11"), item("22")]}],
        "visual_candidates": [], "skipped": [], "warnings": [],
    }))
    .unwrap()
}

/// Pinterest's pin resource: 404 "Pin not found." for pin 22, the pin for
/// any other ID.
async fn mock_pinterest() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/resource/PinResource/get/"))
        .respond_with(|request: &Request| {
            let data = request
                .url
                .query_pairs()
                .find(|(name, _)| name == "data")
                .map(|(_, value)| value.into_owned())
                .unwrap();
            let id = serde_json::from_str::<Value>(&data).unwrap()["options"]["id"]
                .as_str()
                .unwrap()
                .to_owned();
            if id == "22" {
                ResponseTemplate::new(404).set_body_json(json!({"resource_response": {
                    "error": {"http_status": 404, "code": 50, "message": "Pin not found."}, "data": null}}))
            } else {
                ResponseTemplate::new(200).set_body_json(
                    json!({"resource_response": {"data": {"id": id, "board": {"name": "Ideas"}}}}),
                )
            }
        })
        .mount(&server)
        .await;
    server
}

fn client(pinterest: &MockServer) -> PinterestClient {
    PinterestClient::with_api_root(
        Url::parse("https://www.pinterest.com/").unwrap(),
        Url::parse(&format!("{}/", pinterest.uri())).unwrap(),
    )
    .unwrap()
}

async fn start(store: ReviewStore, client: Option<PinterestClient>) -> String {
    let server = ReviewServer::bind(report(), client, store, Some(0))
        .await
        .unwrap();
    let url = server.url().to_owned();
    tokio::spawn(server.run());
    url
}

fn decision(remove: &[&str]) -> Value {
    let key = unpin_key();
    json!({"key": key, "action": "remove", "reviewed": true, "remove_pin_ids": remove, "items": [
        {"pin_id": "11", "pin_url": "https://www.pinterest.com/pin/11/", "image_url": "https://example.test/11.jpg", "image_sha256": "abc", "media_kind": "image"},
        {"pin_id": "22", "pin_url": "https://www.pinterest.com/pin/22/", "image_url": "https://example.test/22.jpg", "image_sha256": "abc", "media_kind": "image"}
    ]})
}

fn unpin_key() -> String {
    format!("exact:same-board:{}", "a".repeat(64))
}

#[tokio::test]
async fn pin_presence_reads_pinterests_answers() {
    let pinterest = mock_pinterest().await;
    let client = client(&pinterest);
    assert_eq!(
        client.pin_presence("11").await.unwrap(),
        PinPresence::Present {
            board: Some("Ideas".into())
        }
    );
    assert_eq!(
        client.pin_presence("22").await.unwrap(),
        PinPresence::Missing
    );
}

#[tokio::test]
async fn decisions_and_pin_checks_are_saved_to_the_review_file() {
    let directory = tempfile::tempdir().unwrap();
    let store_path = directory.path().join("review.json");
    let pinterest = mock_pinterest().await;
    let url = start(
        ReviewStore::new(store_path.clone()),
        Some(client(&pinterest)),
    )
    .await;
    let http = reqwest::Client::new();

    let page = http.get(&url).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert_eq!(page.headers()["referrer-policy"], "no-referrer");
    let html = page.text().await.unwrap();
    assert!(html.contains(r#"<script type="application/json" id="unpin-server">"#));
    // Saved data and report text can never close the config script early.
    let config = html
        .split(r#"id="unpin-server">"#)
        .nth(1)
        .unwrap()
        .split("</script>")
        .next()
        .unwrap();
    let config: Value = serde_json::from_str(config).unwrap();
    assert_eq!(config["checks"], true);
    assert_eq!(config["signed_in"], false);

    let saved = http
        .put(format!("{url}api/decisions"))
        .json(&json!({"set": [decision(&["22"])], "remove": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 204);
    let rejected = http
        .put(format!("{url}api/decisions"))
        .json(&json!({"set": [decision(&["99"])]}))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), 400);

    let present: Value = http
        .get(format!("{url}api/pins/11"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        present,
        json!({"pin_id": "11", "state": "present", "board": "Ideas"})
    );
    let removed: Value = http
        .get(format!("{url}api/pins/22"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(removed["state"], "removed");

    let manual = http
        .post(format!("{url}api/pins/11/removed"))
        .json(&json!({"removed": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(manual.status(), 204);

    let file: Value = serde_json::from_slice(&std::fs::read(&store_path).unwrap()).unwrap();
    assert_eq!(file["schema_version"], 1);
    assert_eq!(file["decisions"], json!([decision(&["22"])]));
    let removed_pins = file["removed_pins"].as_object().unwrap();
    assert!(removed_pins.contains_key("11") && removed_pins.contains_key("22"));

    // The reloaded page carries the saved state.
    let html = http.get(&url).send().await.unwrap().text().await.unwrap();
    assert!(html.contains(&unpin_key()));
}

#[tokio::test]
async fn other_hosts_sites_and_paths_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    let url = start(ReviewStore::new(directory.path().join("review.json")), None).await;
    let origin = url
        .trim_end_matches('/')
        .rsplit_once('/')
        .unwrap()
        .0
        .to_owned();
    let http = reqwest::Client::new();
    let status = |response: reqwest::Response| response.status().as_u16();

    let rebound = http
        .get(&url)
        .header("host", "attacker.test")
        .send()
        .await
        .unwrap();
    assert_eq!(status(rebound), 403);
    let form_post = http
        .put(format!("{url}api/decisions"))
        .header("content-type", "text/plain")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(status(form_post), 403);
    let cross_site = http
        .put(format!("{url}api/decisions"))
        .header("origin", "https://attacker.test")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(status(cross_site), 403);
    let wrong_token = http
        .get(format!("{origin}/0000/api/pins/1"))
        .send()
        .await
        .unwrap();
    assert_eq!(status(wrong_token), 404);
    let bad_id = http
        .get(format!("{url}api/pins/1%2F2"))
        .send()
        .await
        .unwrap();
    assert_eq!(status(bad_id), 400);

    // Without a Pinterest connection, checks say so instead of guessing.
    let offline: Value = http
        .get(format!("{url}api/pins/11"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(offline["state"], "unknown");
    assert!(!directory.path().join("review.json").exists());
}
