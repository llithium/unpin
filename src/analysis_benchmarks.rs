//! Manual, offline benchmarks shared with the comparison checkout.
use super::*;
use image::DynamicImage;
use std::io::Cursor;
use std::time::Instant;

fn fixture() -> (Vec<u8>, ImageFingerprint) {
    let mut png = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(64, 64)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let bytes = png.into_inner();
    let fingerprint = ImageFingerprint::from_bytes(&bytes, false, MAX_DECODED_PIXELS).unwrap();
    (bytes, fingerprint)
}

#[test]
#[ignore = "manual fingerprint allocation benchmark"]
fn benchmark_shared_signatures() {
    let (_, fingerprint) = fixture();
    let mut times = Vec::new();
    for _ in 0..7 {
        let pins = (0..20_000)
            .map(|id| Pin {
                id: id.to_string(),
                media_url: "http://fixture/image.png".into(),
                board: None,
                source_id: None,
            })
            .collect();
        let started = Instant::now();
        let images = analyzed_images("http://fixture/image.png", pins, &fingerprint);
        times.push(started.elapsed().as_secs_f64() * 1000.0);
        let allocations = images
            .iter()
            .map(|image| image.fingerprint.structural_signature.as_ptr())
            .collect::<std::collections::HashSet<_>>()
            .len();
        eprintln!(
            "signature_payload_bytes={}",
            allocations * fingerprint.structural_signature.len()
        );
        std::hint::black_box(images);
    }
    eprintln!("clone_ms={times:?}");
}

#[tokio::test]
#[ignore = "manual warm and mixed cache benchmark"]
async fn benchmark_cache_pipeline() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    let (bytes, fingerprint) = fixture();
    for misses in [0, 48] {
        let mut times = Vec::new();
        let mut first_download_times = Vec::new();
        for _ in 0..5 {
            server.reset().await;
            let directory = tempfile::tempdir().unwrap();
            let cache = FingerprintCache {
                directory: directory.path().join(CACHE_ENTRY_SUBDIRECTORY),
            };
            let pins = (0..1536)
                .map(|id| {
                    let url = format!("{}/{id:04}.png", server.uri());
                    if id >= misses {
                        cache.put(&url, &fingerprint);
                    }
                    Pin {
                        id: id.to_string(),
                        media_url: url,
                        board: None,
                        source_id: None,
                    }
                })
                .collect();
            let first = Arc::new(AtomicU64::new(0));
            let observed = Arc::clone(&first);
            let started = Instant::now();
            let response_bytes = bytes.clone();
            Mock::given(wiremock::matchers::method("GET"))
                .respond_with(move |_: &wiremock::Request| {
                    let _ = observed.compare_exchange(
                        0,
                        started.elapsed().as_micros() as u64,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                    ResponseTemplate::new(200)
                        .set_delay(Duration::from_millis(40))
                        .set_body_bytes(response_bytes.clone())
                })
                .mount(&server)
                .await;
            let result = analyze_pins_with_progress_and_cache(
                pins,
                false,
                5,
                &NoProgress,
                Some(directory.path().to_owned()),
            )
            .await
            .unwrap();
            times.push(started.elapsed().as_secs_f64() * 1000.0);
            first_download_times.push(first.load(Ordering::Relaxed) as f64 / 1000.0);
            assert_eq!(result.analyzed, 1536);
            assert_eq!(result.exact_groups.len(), 1);
            assert_eq!(result.exact_groups[0].items.len(), 1536);
            assert!(result.visual_candidates.is_empty());
            assert!(result.skipped.is_empty());
            assert_eq!(server.received_requests().await.unwrap().len(), misses);
        }
        eprintln!("misses={misses} scan_ms={times:?} first_download_ms={first_download_times:?}");
    }
}
