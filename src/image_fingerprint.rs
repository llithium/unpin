//! CPU-bound image fingerprinting behind a small analysis-facing interface.
//!
//! This module owns image-format decoding, pixel safety checks, and the
//! fingerprint math. Callers receive comparable fingerprint values without
//! needing to know which `image` crate operations or derived statistics make
//! them safe and useful.

use std::io::Cursor;
use std::sync::Arc;

use image::imageops::{self, FilterType};
use image::{DynamicImage, GenericImageView, ImageReader};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

const STRUCTURAL_SIGNATURE_SIZE: u32 = 64;
const DIFFERENCE_HASH_WIDTH: u32 = 9;
const DIFFERENCE_HASH_HEIGHT: u32 = 8;
/// Keep the decoded raster bounded even when a compressed image is small.
pub(crate) const MAX_DECODED_PIXELS: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ImageFingerprint {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) byte_size: u64,
    pub(crate) sha256: String,
    pub(crate) difference_hash: u64,
    /// Hex rather than a JSON number array: this is 4 KiB of bytes, and parsing
    /// it back as thousands of decimal integers dominated warm-cache runs.
    /// Shared across pins using the same URL so each clone avoids a 4 KiB copy.
    #[serde(with = "hex_bytes")]
    pub(crate) structural_signature: Arc<[u8]>,
    pub(crate) structural_sum: u64,
    pub(crate) structural_sum_squares: u64,
    /// Exact-only runs intentionally omit the visual signature and avoid
    /// decoding the image. Such entries are usable for exact matching, but a
    /// visual run must treat them as misses and refresh them.
    pub(crate) visual_ready: bool,
}

impl ImageFingerprint {
    /// Derives the exact or visual fingerprint required by a scan.
    pub(crate) fn from_bytes(
        bytes: &[u8],
        exact_only: bool,
        max_decoded_pixels: u64,
    ) -> Result<Self, String> {
        if exact_only {
            fingerprint_image_exact(bytes, max_decoded_pixels)
        } else {
            fingerprint_image(bytes, max_decoded_pixels)
        }
    }

    /// Compares two visual fingerprints using the statistics calculated once
    /// during fingerprinting. Exact-only fingerprints intentionally compare as
    /// non-visual values because they have no structural signature.
    pub(crate) fn visual_similarity(&self, other: &Self) -> f64 {
        structural_similarity_with_stats(
            &self.structural_signature,
            self.structural_sum,
            self.structural_sum_squares,
            &other.structural_signature,
            other.structural_sum,
            other.structural_sum_squares,
        )
    }
}

mod hex_bytes {
    use super::*;

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Arc<[u8]>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        hex::decode(&encoded)
            .map(Arc::from)
            .map_err(serde::de::Error::custom)
    }
}

/// Decodes and hashes downloaded bytes. Runs on the blocking pool.
fn fingerprint_image(bytes: &[u8], max_decoded_pixels: u64) -> Result<ImageFingerprint, String> {
    let image = decode_image(bytes, max_decoded_pixels)?;
    let (width, height) = image.dimensions();
    checked_pixel_count(width, height, max_decoded_pixels)?;
    if width == 0 || height == 0 {
        return Err("decoded image has zero width or height".into());
    }

    let sha256 = hex::encode(Sha256::digest(bytes));
    let (difference_hash, structural_signature, structural_sum, structural_sum_squares) =
        fingerprint_hashes(&image);

    Ok(ImageFingerprint {
        width,
        height,
        byte_size: bytes.len() as u64,
        sha256,
        difference_hash,
        structural_signature,
        structural_sum,
        structural_sum_squares,
        visual_ready: true,
    })
}

/// Computes only the byte identity and dimensions needed by an exact-only
/// scan. Reading the image header avoids allocating and decoding the full
/// raster when no perceptual comparison will be performed.
fn fingerprint_image_exact(
    bytes: &[u8],
    max_decoded_pixels: u64,
) -> Result<ImageFingerprint, String> {
    let (width, height) = image_dimensions(bytes)?;
    checked_pixel_count(width, height, max_decoded_pixels)?;
    if width == 0 || height == 0 {
        return Err("decoded image has zero width or height".into());
    }

    Ok(ImageFingerprint {
        width,
        height,
        byte_size: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(bytes)),
        difference_hash: 0,
        structural_signature: Arc::from([]),
        structural_sum: 0,
        structural_sum_squares: 0,
        visual_ready: false,
    })
}

fn image_reader(bytes: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>, String> {
    ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("could not identify image format: {error}"))
}

fn image_dimensions(bytes: &[u8]) -> Result<(u32, u32), String> {
    image_reader(bytes)?
        .into_dimensions()
        .map_err(|error| format!("could not read image dimensions: {error}"))
}

fn decode_image(bytes: &[u8], max_decoded_pixels: u64) -> Result<DynamicImage, String> {
    // Read only the format header first. This rejects a decompression bomb
    // before `DynamicImage::from_decoder` allocates the full raster.
    let (width, height) = image_dimensions(bytes)?;
    checked_pixel_count(width, height, max_decoded_pixels)?;

    // `max_alloc` is non-strict for some codecs, so the explicit dimension
    // check above and the post-decode check in `fingerprint_image` remain
    // mandatory. It still protects codecs that honor the decoder allocation
    // limit, including the PNG path used by the regression tests.
    let max_alloc = max_decoded_pixels
        .checked_mul(4)
        .ok_or_else(|| "decoded pixel limit overflowed its allocation check".to_owned())?;
    let mut reader = image_reader(bytes)?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(max_alloc);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|error| format!("could not decode image: {error}"))
}

fn checked_pixel_count(width: u32, height: u32, max_decoded_pixels: u64) -> Result<u64, String> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| "decoded image dimensions overflowed the pixel check".to_owned())?;
    if pixels > max_decoded_pixels {
        return Err(format!(
            "decoded image has {pixels} pixels, exceeding the {max_decoded_pixels}-pixel safety limit"
        ));
    }
    Ok(pixels)
}

/// Derives both hashes from a single full-resolution downscale.
///
/// Grayscale conversion comes first because only luminance is ever used, and
/// resizing one channel instead of three or four is where most of the saving
/// comes from. The difference-hash grid is then taken off the 64×64 signature
/// rather than the original, so the second downscale touches four thousand
/// pixels instead of several million.
fn fingerprint_hashes(image: &DynamicImage) -> (u64, Arc<[u8]>, u64, u64) {
    let luminance = image.to_luma8();
    let signature = imageops::resize(
        &luminance,
        STRUCTURAL_SIGNATURE_SIZE,
        STRUCTURAL_SIGNATURE_SIZE,
        FilterType::Triangle,
    );
    let grid = imageops::resize(
        &signature,
        DIFFERENCE_HASH_WIDTH,
        DIFFERENCE_HASH_HEIGHT,
        FilterType::Triangle,
    );

    let mut hash = 0_u64;
    let mut bit = 0_u32;
    for y in 0..DIFFERENCE_HASH_HEIGHT {
        for x in 0..DIFFERENCE_HASH_WIDTH - 1 {
            if grid.get_pixel(x, y)[0] > grid.get_pixel(x + 1, y)[0] {
                hash |= 1_u64 << bit;
            }
            bit += 1;
        }
    }

    let structural_signature = Arc::from(signature.into_raw());
    let (structural_sum, structural_sum_squares) = structural_statistics(&structural_signature);

    (
        hash,
        structural_signature,
        structural_sum,
        structural_sum_squares,
    )
}

fn structural_statistics(signature: &[u8]) -> (u64, u64) {
    signature
        .iter()
        .fold((0_u64, 0_u64), |(sum, squares), &value| {
            let value = u64::from(value);
            (sum + value, squares + value * value)
        })
}

#[cfg(test)]
fn structural_similarity(left: &[u8], right: &[u8]) -> f64 {
    debug_assert_eq!(left.len(), right.len());
    if left.is_empty() || left.len() != right.len() {
        return 0.0;
    }

    let left_mean = left.iter().map(|&value| f64::from(value)).sum::<f64>() / left.len() as f64;
    let right_mean = right.iter().map(|&value| f64::from(value)).sum::<f64>() / right.len() as f64;
    let mut product = 0.0;
    let mut left_square = 0.0;
    let mut right_square = 0.0;

    for (&left, &right) in left.iter().zip(right) {
        let left = f64::from(left) - left_mean;
        let right = f64::from(right) - right_mean;
        product += left * right;
        left_square += left * left;
        right_square += right * right;
    }

    let denominator = (left_square * right_square).sqrt();
    if denominator <= f64::EPSILON {
        0.0
    } else {
        (product / denominator).clamp(-1.0, 1.0)
    }
}

/// Compares signatures using statistics calculated once during fingerprinting.
/// The per-pair loop therefore computes only the dot product instead of
/// repeatedly calculating both means and both variances.
fn structural_similarity_with_stats(
    left: &[u8],
    left_sum: u64,
    left_sum_squares: u64,
    right: &[u8],
    right_sum: u64,
    right_sum_squares: u64,
) -> f64 {
    debug_assert_eq!(left.len(), right.len());
    if left.is_empty() || left.len() != right.len() {
        return 0.0;
    }

    let count = left.len() as f64;
    let product = left
        .iter()
        .zip(right)
        .map(|(&left, &right)| u64::from(left) * u64::from(right))
        .sum::<u64>() as f64;
    let left_sum = left_sum as f64;
    let right_sum = right_sum as f64;
    let left_sum_squares = left_sum_squares as f64;
    let right_sum_squares = right_sum_squares as f64;
    let product = product - left_sum * right_sum / count;
    let left_square = (left_sum_squares - left_sum * left_sum / count).max(0.0);
    let right_square = (right_sum_squares - right_sum * right_sum / count).max(0.0);
    let denominator = (left_square * right_square).sqrt();

    if !denominator.is_finite() || denominator <= f64::EPSILON {
        0.0
    } else {
        (product / denominator).clamp(-1.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN_STRUCTURAL_SIMILARITY: f64 = 0.97;

    #[test]
    fn exact_fingerprint_reads_dimensions_without_decoding_pixels() {
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::new_rgb8(2, 2)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let encoded = encoded.into_inner();
        let bytes = (0..encoded.len())
            .find_map(|length| {
                let candidate = &encoded[..length];
                image_dimensions(candidate)
                    .ok()
                    .filter(|_| fingerprint_image(candidate, u64::MAX).is_err())
                    .map(|_| candidate.to_vec())
            })
            .expect("PNG should expose dimensions before its payload is complete");

        let exact = ImageFingerprint::from_bytes(&bytes, true, u64::MAX).unwrap();
        assert_eq!((exact.width, exact.height), (2, 2));
        assert!(!exact.visual_ready);
        assert!(exact.structural_signature.is_empty());
        assert!(ImageFingerprint::from_bytes(&bytes, false, u64::MAX).is_err());
    }

    #[test]
    fn difference_hash_is_stable_across_resizing() {
        let image = DynamicImage::ImageRgb8(image::ImageBuffer::from_fn(90, 80, |x, _| {
            image::Rgb([(x * 2) as u8, 0, 0])
        }));
        let resized = image.resize_exact(900, 800, FilterType::Nearest);
        assert_eq!(fingerprint_hashes(&image).0, fingerprint_hashes(&resized).0);
    }

    #[test]
    fn structural_similarity_rejects_different_sparse_drawings() {
        let mut left = vec![255; (STRUCTURAL_SIGNATURE_SIZE.pow(2)) as usize];
        let mut right = left.clone();
        for y in 8..56 {
            left[y * STRUCTURAL_SIGNATURE_SIZE as usize + 16] = 0;
            right[y * STRUCTURAL_SIGNATURE_SIZE as usize + 48] = 0;
        }

        assert!(structural_similarity(&left, &right) < MIN_STRUCTURAL_SIMILARITY);
    }

    #[test]
    fn structural_similarity_accepts_contrast_shifted_copies() {
        let left = (0..=255).collect::<Vec<u8>>();
        let right = left
            .iter()
            .map(|value| (f64::from(*value) * 0.8 + 20.0).round() as u8)
            .collect::<Vec<_>>();

        assert!(structural_similarity(&left, &right) > MIN_STRUCTURAL_SIMILARITY);
    }

    #[test]
    fn precomputed_structural_statistics_match_the_original_similarity() {
        let left = (0..=255).collect::<Vec<u8>>();
        let right = left
            .iter()
            .map(|value| (f64::from(*value) * 0.8 + 20.0).round() as u8)
            .collect::<Vec<_>>();
        let (left_sum, left_sum_squares) = structural_statistics(&left);
        let (right_sum, right_sum_squares) = structural_statistics(&right);

        let expected = structural_similarity(&left, &right);
        let actual = structural_similarity_with_stats(
            &left,
            left_sum,
            left_sum_squares,
            &right,
            right_sum,
            right_sum_squares,
        );
        assert!((expected - actual).abs() < 1e-12);
    }
}
