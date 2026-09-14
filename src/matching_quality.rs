//! Hand-labeled synthetic examples for the complete visual matching pipeline.
//! These measure repeatable behavior, not accuracy on a real Pinterest sample.
use super::*;
use image::{DynamicImage, ImageFormat, Rgb, RgbImage, imageops::FilterType};

fn scene(seed: u32) -> DynamicImage {
    let mut image = RgbImage::new(240, 180);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let gradient = (30 + x / 3 + y / 4) as u8;
        let circle = (i64::from(x) - i64::from(65 + seed * 22)).pow(2) + (i64::from(y) - 70).pow(2)
            < 35 * 35;
        *pixel = if circle {
            Rgb([220, 70, 40])
        } else if x > 125 && x < 205 && y > 95 + seed * 4 {
            Rgb([30, 110, 190])
        } else {
            Rgb([gradient, gradient.saturating_add(25), 180])
        };
    }
    DynamicImage::ImageRgb8(image)
}

fn encode(image: &DynamicImage, jpeg: bool) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    if jpeg {
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 65)
            .encode_image(image)
            .unwrap();
    } else {
        image.write_to(&mut bytes, ImageFormat::Png).unwrap();
    }
    bytes.into_inner()
}

#[test]
fn labeled_matching_quality_baseline() {
    let mut rows = Vec::new();
    for seed in 0..3 {
        let original = scene(seed);
        let resized = original.resize_exact(480, 360, FilterType::Lanczos3);
        let cropped = original.crop_imm(24, 18, 192, 144);
        let mut overlay = original.to_rgb8();
        // A deliberately conspicuous caption band changes what the pin conveys.
        for y in 135..170 {
            for x in 15..225 {
                overlay.put_pixel(x, y, Rgb([250, 250, 250]));
            }
        }
        for x in (25..210).step_by(18) {
            for dx in 0..9 {
                for y in 143..163 {
                    overlay.put_pixel(x + dx, y, Rgb([20, 20, 20]));
                }
            }
        }
        let variants = [
            ("resized", resized, false, true),
            ("recompressed", original.clone(), true, true),
            ("crop", cropped, false, false),
            (
                "caption_overlay",
                DynamicImage::ImageRgb8(overlay),
                false,
                false,
            ),
            (
                "similar_layout_different_content",
                scene(seed + 3),
                false,
                false,
            ),
        ];
        let original_bytes = encode(&original, false);
        for (kind, variant, jpeg, expected) in variants {
            let variant_bytes = encode(&variant, jpeg);
            let fingerprints = [&original_bytes, &variant_bytes].map(|bytes| {
                ImageFingerprint::from_bytes(bytes, false, MAX_DECODED_PIXELS).unwrap()
            });
            let distance =
                (fingerprints[0].difference_hash ^ fingerprints[1].difference_hash).count_ones();
            let similarity = fingerprints[0].visual_similarity(&fingerprints[1]);
            let images: Vec<_> = fingerprints
                .into_iter()
                .enumerate()
                .map(|(index, fingerprint)| AnalyzedImage {
                    pin_id: index.to_string(),
                    pin_url: format!("https://www.pinterest.com/pin/{index}/"),
                    board: None,
                    source_id: None,
                    image_url: format!("https://example.test/{index}.png"),
                    fingerprint,
                })
                .collect();
            let actual = !build_visual_candidates(&images, 5).is_empty();
            rows.push(serde_json::json!({"scene":seed, "variant":kind, "expected_match":expected,
                "actual_match":actual, "hash_distance":distance, "structural_similarity":similarity}));
        }
    }
    let count = |expected, actual| {
        rows.iter()
            .filter(|row| row["expected_match"] == expected && row["actual_match"] == actual)
            .count()
    };
    let results = serde_json::json!({
        "dataset":"synthetic-scenes-v1", "threshold":5, "structural_threshold":MIN_STRUCTURAL_SIMILARITY,
        "label_policy":"Resizing and recompression preserve content; substantial crops, captions, and changed objects require separate review and are labeled non-interchangeable.",
        "true_positives":count(true,true), "false_positives":count(false,true),
        "true_negatives":count(false,false), "false_negatives":count(true,false), "pairs":rows,
    });
    println!("{}", serde_json::to_string_pretty(&results).unwrap());
    if let Some(path) = std::env::var_os("UNPIN_QUALITY_RESULTS") {
        std::fs::write(path, serde_json::to_vec_pretty(&results).unwrap()).unwrap();
    }
    assert_eq!(
        count(true, false),
        0,
        "content-preserving variants must remain detectable"
    );
    assert_eq!(
        count(false, true),
        0,
        "labeled non-interchangeable images must not become cleanup candidates"
    );
}
