use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;
use std::time::Duration;

use futures_util::stream::{self, StreamExt, TryStreamExt};
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageReader};
use reqwest::Client;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::pinterest::{Pin, SkippedPin};
use crate::progress::{NoProgress, ProgressEvent, ProgressSink};
use crate::report::{DuplicateGroup, Recommendation, ReportItem, VisualCandidate, rank_tuple};

const DOWNLOAD_CONCURRENCY: usize = 8;
const MAX_IMAGE_BYTES: u64 = 100 * 1024 * 1024;
const STRUCTURAL_SIGNATURE_SIZE: u32 = 64;
const MIN_STRUCTURAL_SIMILARITY: f64 = 0.97;

#[derive(Debug)]
pub struct AnalysisResult {
    pub analyzed: usize,
    pub exact_groups: Vec<DuplicateGroup>,
    pub visual_candidates: Vec<VisualCandidate>,
    pub skipped: Vec<SkippedPin>,
}

#[derive(Debug, Error)]
pub enum AnalysisError {
    #[error("failed to build the image HTTP client")]
    Client(#[source] reqwest::Error),
}

#[derive(Debug, Clone)]
struct AnalyzedImage {
    pin_id: String,
    pin_url: String,
    image_url: String,
    width: u32,
    height: u32,
    byte_size: u64,
    sha256: String,
    difference_hash: u64,
    structural_signature: Box<[u8]>,
}

#[derive(Debug, Clone)]
struct ImageFingerprint {
    width: u32,
    height: u32,
    byte_size: u64,
    sha256: String,
    difference_hash: u64,
    structural_signature: Box<[u8]>,
}

impl AnalyzedImage {
    fn report_item(&self, recommendation: Recommendation) -> ReportItem {
        ReportItem {
            pin_id: self.pin_id.clone(),
            pin_url: self.pin_url.clone(),
            image_url: self.image_url.clone(),
            width: self.width,
            height: self.height,
            byte_size: self.byte_size,
            recommendation,
        }
    }
}

pub async fn analyze_pins(
    pins: Vec<Pin>,
    exact_only: bool,
    similarity_threshold: u8,
) -> Result<AnalysisResult, AnalysisError> {
    analyze_pins_with_progress(pins, exact_only, similarity_threshold, &NoProgress).await
}

pub async fn analyze_pins_with_progress(
    pins: Vec<Pin>,
    exact_only: bool,
    similarity_threshold: u8,
    progress: &dyn ProgressSink,
) -> Result<AnalysisResult, AnalysisError> {
    let http = Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("unpin/0.1")
        .build()
        .map_err(AnalysisError::Client)?;

    let mut pins_by_media_url: BTreeMap<String, Vec<Pin>> = BTreeMap::new();
    for pin in pins {
        pins_by_media_url
            .entry(pin.media_url.clone())
            .or_default()
            .push(pin);
    }
    let download_total = pins_by_media_url.len();
    progress.emit(ProgressEvent::ImagesStarted {
        total: download_total,
    });

    let downloads = stream::iter(pins_by_media_url.into_iter().map(|(media_url, pins)| {
        let http = http.clone();
        async move {
            match download_and_fingerprint(&http, &media_url).await {
                Ok(fingerprint) => Ok(pins
                    .into_iter()
                    .map(|pin| AnalyzedImage {
                        pin_url: pin.pin_url(),
                        pin_id: pin.id,
                        image_url: media_url.clone(),
                        width: fingerprint.width,
                        height: fingerprint.height,
                        byte_size: fingerprint.byte_size,
                        sha256: fingerprint.sha256.clone(),
                        difference_hash: fingerprint.difference_hash,
                        structural_signature: fingerprint.structural_signature.clone(),
                    })
                    .collect::<Vec<_>>()),
                Err(reason) => Err(pins
                    .into_iter()
                    .map(|pin| SkippedPin {
                        pin_url: Some(pin.pin_url()),
                        pin_id: Some(pin.id),
                        reason: reason.clone(),
                    })
                    .collect::<Vec<_>>()),
            }
        }
    }))
    .buffer_unordered(DOWNLOAD_CONCURRENCY);
    futures_util::pin_mut!(downloads);

    let mut images = Vec::new();
    let mut skipped = Vec::new();
    let mut completed = 0;
    while let Some(result) = downloads.next().await {
        match result {
            Ok(mut downloaded_images) => images.append(&mut downloaded_images),
            Err(mut download_skips) => skipped.append(&mut download_skips),
        }
        completed += 1;
        progress.emit(ProgressEvent::ImageFinished {
            completed,
            total: download_total,
        });
    }
    images.sort_by(|left, right| left.pin_id.cmp(&right.pin_id));

    progress.emit(ProgressEvent::MatchingStarted);
    let exact_groups = build_exact_groups(&images);
    let visual_candidates = if exact_only {
        Vec::new()
    } else {
        build_visual_candidates(&images, similarity_threshold)
    };

    Ok(AnalysisResult {
        analyzed: images.len(),
        exact_groups,
        visual_candidates,
        skipped,
    })
}

async fn download_and_fingerprint(
    http: &Client,
    media_url: &str,
) -> Result<ImageFingerprint, String> {
    let response = http
        .get(media_url)
        .send()
        .await
        .map_err(|error| format!("image download failed: {}", concise_reqwest_error(&error)))?;

    if !response.status().is_success() {
        return Err(format!(
            "image download returned HTTP {}",
            response.status()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_IMAGE_BYTES)
    {
        return Err(format!(
            "image exceeds the {} MiB safety limit",
            MAX_IMAGE_BYTES / 1024 / 1024
        ));
    }

    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(|error| format!("image download failed: {}", concise_reqwest_error(&error)))?
    {
        let new_length = bytes.len() as u64 + chunk.len() as u64;
        if new_length > MAX_IMAGE_BYTES {
            return Err(format!(
                "image exceeds the {} MiB safety limit",
                MAX_IMAGE_BYTES / 1024 / 1024
            ));
        }
        bytes.extend_from_slice(&chunk);
    }

    let image = decode_image(&bytes)?;
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Err("decoded image has zero width or height".into());
    }

    let sha256 = hex::encode(Sha256::digest(&bytes));
    let difference_hash = difference_hash(&image);
    let structural_signature = structural_signature(&image);

    Ok(ImageFingerprint {
        width,
        height,
        byte_size: bytes.len() as u64,
        sha256,
        difference_hash,
        structural_signature,
    })
}

fn decode_image(bytes: &[u8]) -> Result<DynamicImage, String> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("could not identify image format: {error}"))?;
    reader
        .decode()
        .map_err(|error| format!("could not decode image: {error}"))
}

fn concise_reqwest_error(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "request timed out"
    } else if error.is_connect() {
        "connection failed"
    } else if error.is_decode() {
        "invalid response"
    } else {
        "request error"
    }
}

fn difference_hash(image: &DynamicImage) -> u64 {
    let grayscale = image.resize_exact(9, 8, FilterType::Triangle).into_luma8();
    let mut hash = 0_u64;
    let mut bit = 0_u32;

    for y in 0..8 {
        for x in 0..8 {
            if grayscale.get_pixel(x, y)[0] > grayscale.get_pixel(x + 1, y)[0] {
                hash |= 1_u64 << bit;
            }
            bit += 1;
        }
    }

    hash
}

fn structural_signature(image: &DynamicImage) -> Box<[u8]> {
    image
        .resize_exact(
            STRUCTURAL_SIGNATURE_SIZE,
            STRUCTURAL_SIGNATURE_SIZE,
            FilterType::Triangle,
        )
        .into_luma8()
        .into_raw()
        .into_boxed_slice()
}

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

fn build_exact_groups(images: &[AnalyzedImage]) -> Vec<DuplicateGroup> {
    let mut hashes: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, image) in images.iter().enumerate() {
        hashes.entry(&image.sha256).or_default().push(index);
    }

    let mut groups = hashes
        .into_values()
        .filter(|members| members.len() > 1)
        .map(|members| DuplicateGroup {
            items: ranked_items(images, &members),
        })
        .collect::<Vec<_>>();
    groups.sort_by(|left, right| {
        Reverse(left.items.len())
            .cmp(&Reverse(right.items.len()))
            .then_with(|| left.items[0].pin_id.cmp(&right.items[0].pin_id))
    });
    groups
}

fn build_visual_candidates(images: &[AnalyzedImage], threshold: u8) -> Vec<VisualCandidate> {
    let mut tree = BkTree::default();
    let mut candidates = Vec::new();

    for (index, image) in images.iter().enumerate() {
        for other_index in tree.query(image.difference_hash, threshold) {
            let other = &images[other_index];
            if image.sha256 == other.sha256 || !aspect_ratios_match(image, other) {
                continue;
            }
            let structural_similarity =
                structural_similarity(&image.structural_signature, &other.structural_signature);
            if structural_similarity < MIN_STRUCTURAL_SIMILARITY {
                continue;
            }

            let distance = (image.difference_hash ^ other.difference_hash).count_ones() as u8;
            let members = [other_index, index];
            let ranked = ranked_items(images, &members);
            candidates.push(VisualCandidate {
                hash_distance: distance,
                similarity_percent: (structural_similarity * 100.0).floor() as u8,
                items: [ranked[0].clone(), ranked[1].clone()],
            });
        }
        tree.insert(image.difference_hash, index);
    }

    candidates.sort_by(|left, right| {
        left.hash_distance
            .cmp(&right.hash_distance)
            .then_with(|| left.items[0].pin_id.cmp(&right.items[0].pin_id))
            .then_with(|| left.items[1].pin_id.cmp(&right.items[1].pin_id))
    });
    candidates
}

fn aspect_ratios_match(left: &AnalyzedImage, right: &AnalyzedImage) -> bool {
    let left_ratio = left.width as f64 / left.height as f64;
    let right_ratio = right.width as f64 / right.height as f64;
    (left_ratio - right_ratio).abs() / left_ratio.max(right_ratio) <= 0.01
}

fn ranked_items(images: &[AnalyzedImage], members: &[usize]) -> Vec<ReportItem> {
    let mut sorted = members.to_vec();
    sorted.sort_by(|left, right| {
        rank_tuple(
            images[*right].width,
            images[*right].height,
            images[*right].byte_size,
        )
        .cmp(&rank_tuple(
            images[*left].width,
            images[*left].height,
            images[*left].byte_size,
        ))
        .then_with(|| images[*left].pin_id.cmp(&images[*right].pin_id))
    });

    let best_rank = rank_tuple(
        images[sorted[0]].width,
        images[sorted[0]].height,
        images[sorted[0]].byte_size,
    );
    let best_count = sorted
        .iter()
        .take_while(|index| {
            rank_tuple(
                images[**index].width,
                images[**index].height,
                images[**index].byte_size,
            ) == best_rank
        })
        .count();

    sorted
        .into_iter()
        .enumerate()
        .map(|(position, index)| {
            let recommendation = if position < best_count && best_count > 1 {
                Recommendation::Tie
            } else if position == 0 {
                Recommendation::Keep
            } else {
                Recommendation::DeleteCandidate
            };
            images[index].report_item(recommendation)
        })
        .collect()
}

#[derive(Debug, Default)]
struct BkTree {
    nodes: Vec<BkNode>,
}

#[derive(Debug)]
struct BkNode {
    hash: u64,
    image_indices: Vec<usize>,
    children: BTreeMap<u8, usize>,
}

impl BkTree {
    fn insert(&mut self, hash: u64, image_index: usize) {
        if self.nodes.is_empty() {
            self.nodes.push(BkNode {
                hash,
                image_indices: vec![image_index],
                children: BTreeMap::new(),
            });
            return;
        }

        let mut node_index = 0;
        loop {
            let distance = (hash ^ self.nodes[node_index].hash).count_ones() as u8;
            if distance == 0 {
                self.nodes[node_index].image_indices.push(image_index);
                return;
            }
            if let Some(child) = self.nodes[node_index].children.get(&distance).copied() {
                node_index = child;
                continue;
            }

            let new_index = self.nodes.len();
            self.nodes.push(BkNode {
                hash,
                image_indices: vec![image_index],
                children: BTreeMap::new(),
            });
            self.nodes[node_index].children.insert(distance, new_index);
            return;
        }
    }

    fn query(&self, hash: u64, threshold: u8) -> Vec<usize> {
        if self.nodes.is_empty() {
            return Vec::new();
        }

        let mut results = Vec::new();
        let mut stack = vec![0];
        while let Some(node_index) = stack.pop() {
            let node = &self.nodes[node_index];
            let distance = (hash ^ node.hash).count_ones() as u8;
            if distance <= threshold {
                results.extend(node.image_indices.iter().copied());
            }
            let lower = distance.saturating_sub(threshold);
            let upper = distance.saturating_add(threshold);
            stack.extend(node.children.range(lower..=upper).map(|(_, child)| *child));
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyzed(id: &str, width: u32, height: u32, bytes: u64, hash: u64) -> AnalyzedImage {
        AnalyzedImage {
            pin_id: id.into(),
            pin_url: format!("https://www.pinterest.com/pin/{id}/"),
            image_url: format!("https://i.pinimg.com/originals/{id}.jpg"),
            width,
            height,
            byte_size: bytes,
            sha256: format!("sha-{id}"),
            difference_hash: hash,
            structural_signature: vec![0, 10, 50, 100, 180, 255].into_boxed_slice(),
        }
    }

    #[test]
    fn bk_tree_finds_values_within_hamming_distance() {
        let mut tree = BkTree::default();
        tree.insert(0, 0);
        tree.insert(0b1111, 1);
        tree.insert(u64::MAX, 2);

        let mut matches = tree.query(0b0011, 2);
        matches.sort_unstable();
        assert_eq!(matches, vec![0, 1]);
    }

    #[test]
    fn exact_groups_rank_best_resolution_first() {
        let mut first = analyzed("1", 800, 600, 1_000, 0);
        let mut second = analyzed("2", 1600, 1200, 2_000, 0);
        first.sha256 = "same".into();
        second.sha256 = "same".into();

        let groups = build_exact_groups(&[first, second]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].items[0].pin_id, "2");
        assert_eq!(groups[0].items[0].recommendation, Recommendation::Keep);
        assert_eq!(
            groups[0].items[1].recommendation,
            Recommendation::DeleteCandidate
        );
    }

    #[test]
    fn equal_best_images_are_ties() {
        let images = [
            analyzed("1", 100, 100, 500, 0),
            analyzed("2", 100, 100, 500, 0),
        ];
        let items = ranked_items(&images, &[0, 1]);
        assert!(
            items
                .iter()
                .all(|item| item.recommendation == Recommendation::Tie)
        );
    }

    #[test]
    fn visual_candidates_require_matching_aspect_ratio() {
        let images = [
            analyzed("1", 1000, 1000, 500, 0),
            analyzed("2", 500, 500, 400, 0b11),
            analyzed("3", 500, 250, 300, 0b1),
        ];
        let candidates = build_visual_candidates(&images, 2);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].hash_distance, 2);
    }

    #[test]
    fn visual_candidates_reject_structural_false_positives() {
        let mut left = analyzed("1", 1000, 1000, 500, 0);
        let mut right = analyzed("2", 500, 500, 400, 0b1);
        left.structural_signature = vec![255; 64 * 64].into_boxed_slice();
        right.structural_signature = vec![255; 64 * 64].into_boxed_slice();
        for y in 8..56 {
            left.structural_signature[y * 64 + 16] = 0;
            right.structural_signature[y * 64 + 48] = 0;
        }

        assert!(build_visual_candidates(&[left, right], 5).is_empty());
    }

    #[test]
    fn difference_hash_is_stable_across_resizing() {
        let image = DynamicImage::ImageRgb8(image::ImageBuffer::from_fn(90, 80, |x, _| {
            image::Rgb([(x * 2) as u8, 0, 0])
        }));
        let resized = image.resize_exact(900, 800, FilterType::Nearest);
        assert_eq!(difference_hash(&image), difference_hash(&resized));
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
}
