use std::process::Command;
use unpin::report::Report;
use unpin::saved_report;

fn report() -> Report {
    let item = |id: &str| {
        serde_json::json!({"pin_id":id, "pin_url":format!("https://www.pinterest.com/pin/{id}/"),
        "image_url":format!("https://example.test/{id}.jpg"), "image_sha256":"abc", "board":"Ideas",
        "width":100, "height":100, "byte_size":1000, "recommendation":"tie"})
    };
    serde_json::from_value(serde_json::json!({
        "summary": {"boards":[], "pins_found":3, "analyzed":3, "skipped":0, "exact_groups":0, "visual_candidates":1},
        "exact_groups":[], "visual_candidates":[{"scope":"same_board", "hash_distance":2, "similarity_percent":98, "items":[item("1"),item("2")], "copies":[[item("3")],[]]}], "skipped":[], "warnings":[],
        "visual_matching_incomplete":true,
    })).unwrap()
}

#[test]
fn saved_scan_round_trip_render_and_no_clobber() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("scan.json");
    let output = directory.path().join("report.html");
    let report = report();
    saved_report::write(&input, report.render_json().unwrap().as_bytes()).unwrap();
    assert_eq!(saved_report::read(&input).unwrap(), report);
    let render = || {
        Command::new(env!("CARGO_BIN_EXE_unpin"))
            .arg("render")
            .arg(&input)
            .arg("--output")
            .arg(&output)
            .arg("--no-open")
            .output()
            .unwrap()
    };
    let result = render();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let html = std::fs::read_to_string(&output).unwrap();
    assert!(html.contains("Visual matching incomplete"));
    assert!(report.render_text().contains("Visual matching incomplete"));
    assert!(!render().status.success());
    assert_eq!(std::fs::read_to_string(&output).unwrap(), html);
    assert!(saved_report::write(&input, b"overwrite").is_err());
}

#[test]
fn unsupported_versions_and_unsafe_saved_links_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("scan.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&report().render_json().unwrap()).unwrap();
    value["schema_version"] = 99.into();
    std::fs::write(&input, value.to_string()).unwrap();
    assert!(matches!(
        saved_report::read(&input),
        Err(saved_report::SavedReportError::Version)
    ));
    value["schema_version"] = 1.into();
    value["skipped"] = serde_json::json!([{"pin_id":"1", "pin_url":"javascript:alert(1)", "reason":"skipped", "board":null}]);
    std::fs::write(&input, value.to_string()).unwrap();
    assert!(matches!(
        saved_report::read(&input),
        Err(saved_report::SavedReportError::UnsafeLink)
    ));
}
