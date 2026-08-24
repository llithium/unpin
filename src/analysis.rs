use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures_util::stream::{self, StreamExt, TryStreamExt};
use image::imageops::{self, FilterType};
use image::{DynamicImage, GenericImageView, ImageReader};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::Builder;
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::pinterest::{Pin, SkippedPin};
use crate::progress::{Lifecycle, NoProgress, Progress, ProgressStep};
use crate::report::{
    DuplicateGroup, MatchScope, Recommendation, ReportItem, VisualCandidate, rank_tuple,
};

const DOWNLOAD_CONCURRENCY: usize = 48;
/// A response larger than this is reported as a skipped pin. The body is checked
/// both from its advertised length and while it streams, because image servers
/// do not always send a trustworthy `Content-Length`.
const MAX_IMAGE_BYTES: u64 = 100 * 1024 * 1024;
/// Downloaded image buffers reserve one semaphore permit per byte and keep that
/// reservation through decoding. This prevents completed downloads from
/// accumulating to `DOWNLOAD_CONCURRENCY * MAX_IMAGE_BYTES` while CPU work lags.
const MAX_IN_FLIGHT_IMAGE_BYTES: u64 = 512 * 1024 * 1024;
/// Images above 16 megapixels are skipped before full-resolution decoding. The
/// image decoder also receives a matching allocation ceiling as defense in depth;
/// a breach remains an ordinary skipped-pin reason.
const MAX_DECODED_PIXELS: u64 = 16 * 1024 * 1024;
const STRUCTURAL_SIGNATURE_SIZE: u32 = 64;
const DIFFERENCE_HASH_WIDTH: u32 = 9;
const DIFFERENCE_HASH_HEIGHT: u32 = 8;
const MIN_STRUCTURAL_SIMILARITY: f64 = 0.97;
const CACHE_FORMAT_VERSION: u8 = 2;
const CACHE_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const CACHE_ENTRY_SUBDIRECTORY: &str = "fingerprints-v2";

struct ImageAnalysisLimits {
    max_image_bytes: u64,
    max_decoded_pixels: u64,
    image_buffer_budget: Arc<Semaphore>,
}

/// Decoding and downscaling are CPU-bound and run on the blocking pool, so the
/// number in flight is tied to the machine rather than to the network limit.
fn cpu_concurrency() -> usize {
    std::thread::available_parallelism().map_or(2, |count| count.get().max(2))
}

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
    board: Option<String>,
    image_url: String,
    width: u32,
    height: u32,
    byte_size: u64,
    sha256: String,
    difference_hash: u64,
    structural_signature: Box<[u8]>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
struct ImageFingerprint {
    width: u32,
    height: u32,
    byte_size: u64,
    sha256: String,
    difference_hash: u64,
    /// Hex rather than a JSON number array: this is 4 KiB of bytes, and parsing
    /// it back as thousands of decimal integers dominated warm-cache runs.
    #[serde(with = "hex_bytes")]
    structural_signature: Box<[u8]>,
}

mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Box<[u8]>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        hex::decode(&encoded)
            .map(Vec::into_boxed_slice)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedFingerprint {
    version: u8,
    fingerprint: ImageFingerprint,
}

#[derive(Debug, Clone)]
struct FingerprintCache {
    directory: PathBuf,
}

impl FingerprintCache {
    fn new(root_directory: PathBuf) -> Self {
        // Keep cleanup inside a directory owned by unpin. In particular, an
        // explicit UNPIN_CACHE_DIR may point at a cache root shared by several
        // applications, whose hashed JSON files must never be pruned here.
        let cache = Self {
            directory: root_directory.join(CACHE_ENTRY_SUBDIRECTORY),
        };
        cache.prune_expired_in_background();
        cache
    }

    /// Pruning stats every entry in the directory, which is pure housekeeping
    /// and has no business delaying the scan behind it.
    fn prune_expired_in_background(&self) {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let cache = self.clone();
                handle.spawn_blocking(move || cache.prune_expired());
            }
            // No runtime to hand it to, so there is nothing to get out of the
            // way of; prune inline instead of silently skipping it.
            Err(_) => self.prune_expired(),
        }
    }

    fn entry_path(&self, media_url: &str) -> PathBuf {
        let key = hex::encode(Sha256::digest(media_url.as_bytes()));
        self.directory.join(format!("{key}.json"))
    }

    fn get(&self, media_url: &str) -> Option<ImageFingerprint> {
        let path = self.entry_path(media_url);
        let metadata = fs::metadata(&path).ok()?;
        let modified = metadata.modified().ok()?;
        if SystemTime::now()
            .duration_since(modified)
            .is_ok_and(|age| age > CACHE_MAX_AGE)
        {
            let _ = fs::remove_file(path);
            return None;
        }

        let cached: CachedFingerprint = match fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<CachedFingerprint>(&bytes).ok())
        {
            Some(cached) if cached.version == CACHE_FORMAT_VERSION => cached,
            _ => {
                let _ = fs::remove_file(path);
                return None;
            }
        };
        Some(cached.fingerprint)
    }

    fn put(&self, media_url: &str, fingerprint: &ImageFingerprint) {
        if fs::create_dir_all(&self.directory).is_err() {
            return;
        }
        let Ok(mut temporary) = Builder::new()
            .prefix(".fingerprint-")
            .tempfile_in(&self.directory)
        else {
            return;
        };
        let cached = CachedFingerprint {
            version: CACHE_FORMAT_VERSION,
            fingerprint: fingerprint.clone(),
        };
        if serde_json::to_writer(temporary.as_file_mut(), &cached).is_err()
            || temporary.as_file_mut().flush().is_err()
        {
            return;
        }
        let _ = temporary.persist(self.entry_path(media_url));
    }

    fn prune_expired(&self) {
        let Ok(entries) = fs::read_dir(&self.directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_fingerprint_cache_entry(&path) {
                continue;
            }
            let expired = entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                .is_some_and(|age| age > CACHE_MAX_AGE);
            if expired {
                let _ = fs::remove_file(path);
            }
        }
    }
}

fn is_fingerprint_cache_entry(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".json"))
        .is_some_and(|stem| {
            stem.len() == 64
                && stem
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn absolute_environment_path(name: &str) -> Option<PathBuf> {
    absolute_path(std::env::var_os(name))
}

fn absolute_path(value: Option<OsString>) -> Option<PathBuf> {
    value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

pub(crate) fn default_fingerprint_cache_dir() -> Option<PathBuf> {
    if let Some(configured) = std::env::var_os("UNPIN_CACHE_DIR").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(configured));
    }

    #[cfg(target_os = "macos")]
    {
        absolute_environment_path("HOME")
            .map(|home| home.join("Library").join("Caches").join("unpin"))
    }

    #[cfg(target_os = "windows")]
    {
        absolute_environment_path("LOCALAPPDATA").map(|local| local.join("unpin").join("cache"))
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        absolute_environment_path("XDG_CACHE_HOME")
            .or_else(|| absolute_environment_path("HOME").map(|home| home.join(".cache")))
            .map(|cache| cache.join("unpin"))
    }
}

impl AnalyzedImage {
    fn report_item(&self, recommendation: Recommendation) -> ReportItem {
        ReportItem {
            pin_id: self.pin_id.clone(),
            pin_url: self.pin_url.clone(),
            board: self.board.clone(),
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
    progress: &dyn Progress,
) -> Result<AnalysisResult, AnalysisError> {
    analyze_pins_with_progress_and_cache(pins, exact_only, similarity_threshold, progress, None)
        .await
}

pub(crate) async fn analyze_pins_with_progress_and_cache(
    pins: Vec<Pin>,
    exact_only: bool,
    similarity_threshold: u8,
    progress: &dyn Progress,
    cache_directory: Option<PathBuf>,
) -> Result<AnalysisResult, AnalysisError> {
    let image_buffer_budget = Arc::new(Semaphore::new(
        usize::try_from(MAX_IN_FLIGHT_IMAGE_BYTES)
            .expect("the image buffer budget must fit in usize"),
    ));
    analyze_pins_with_limits(
        pins,
        exact_only,
        similarity_threshold,
        progress,
        cache_directory,
        ImageAnalysisLimits {
            max_image_bytes: MAX_IMAGE_BYTES,
            max_decoded_pixels: MAX_DECODED_PIXELS,
            image_buffer_budget,
        },
    )
    .await
}

async fn analyze_pins_with_limits(
    pins: Vec<Pin>,
    exact_only: bool,
    similarity_threshold: u8,
    progress: &dyn Progress,
    cache_directory: Option<PathBuf>,
    limits: ImageAnalysisLimits,
) -> Result<AnalysisResult, AnalysisError> {
    let ImageAnalysisLimits {
        max_image_bytes,
        max_decoded_pixels,
        image_buffer_budget,
    } = limits;
    let http = Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("unpin/0.1")
        .build()
        .map_err(AnalysisError::Client)?;
    let cache = cache_directory.map(FingerprintCache::new);

    let mut pins_by_media_url: BTreeMap<String, Vec<Pin>> = BTreeMap::new();
    for pin in pins {
        pins_by_media_url
            .entry(pin.media_url.clone())
            .or_default()
            .push(pin);
    }
    let entries = pins_by_media_url.into_iter().collect::<Vec<_>>();
    let download_total = entries.len();
    progress.step(ProgressStep::ImageAnalysis {
        completed: 0,
        total: download_total,
        lifecycle: Lifecycle::Started,
    });

    let mut images = Vec::new();
    let mut skipped = Vec::new();
    let mut completed = 0;

    // Cache hits are resolved up front in one blocking batch. Doing them inside
    // the download stream put thousands of small synchronous reads on the async
    // runtime and made a fully cached run wait behind the network limit.
    let hits = cached_fingerprints(cache.as_ref(), &entries).await;
    let mut misses = Vec::new();
    for ((media_url, pins), hit) in entries.into_iter().zip(hits) {
        match hit {
            Some(fingerprint) => {
                images.extend(analyzed_images(&media_url, pins, &fingerprint));
                completed += 1;
                progress.step(ProgressStep::ImageAnalysis {
                    completed,
                    total: download_total,
                    lifecycle: if completed >= download_total {
                        Lifecycle::Completed
                    } else {
                        Lifecycle::Advanced
                    },
                });
            }
            None => misses.push((media_url, pins)),
        }
    }

    // Two stages so that the network limit and the CPU limit are independent:
    // decoding an image no longer occupies a slot that could be pulling bytes.
    let fingerprints = stream::iter(misses.into_iter().map(|(media_url, pins)| {
        let http = http.clone();
        let image_buffer_budget = Arc::clone(&image_buffer_budget);
        async move {
            let bytes =
                download_bytes(&http, &media_url, &image_buffer_budget, max_image_bytes).await;
            (media_url, pins, bytes)
        }
    }))
    .buffer_unordered(DOWNLOAD_CONCURRENCY)
    .map(|(media_url, pins, bytes)| {
        let cache = cache.clone();
        async move {
            let fingerprint = match bytes {
                Ok((bytes, buffer_permit)) => {
                    let cached_url = media_url.clone();
                    tokio::task::spawn_blocking(move || {
                        // Keep the compressed-buffer reservation until the
                        // decoded image and its derived hashes are finished.
                        let _buffer_permit = buffer_permit;
                        let fingerprint = fingerprint_image(&bytes, max_decoded_pixels)?;
                        if let Some(cache) = &cache {
                            cache.put(&cached_url, &fingerprint);
                        }
                        Ok(fingerprint)
                    })
                    .await
                    .unwrap_or_else(|_| Err("image analysis did not finish".to_owned()))
                }
                Err(reason) => Err(reason),
            };
            (media_url, pins, fingerprint)
        }
    })
    .buffer_unordered(cpu_concurrency());
    futures_util::pin_mut!(fingerprints);

    while let Some((media_url, pins, fingerprint)) = fingerprints.next().await {
        match fingerprint {
            Ok(fingerprint) => images.extend(analyzed_images(&media_url, pins, &fingerprint)),
            Err(reason) => skipped.extend(pins.into_iter().map(|pin| SkippedPin {
                pin_url: Some(pin.pin_url()),
                pin_id: Some(pin.id),
                reason: reason.clone(),
                board: pin.board,
            })),
        }
        completed += 1;
        progress.step(ProgressStep::ImageAnalysis {
            completed,
            total: download_total,
            lifecycle: if completed >= download_total {
                Lifecycle::Completed
            } else {
                Lifecycle::Advanced
            },
        });
    }
    images.sort_by(|left, right| left.pin_id.cmp(&right.pin_id));

    progress.step(ProgressStep::Matching {
        lifecycle: Lifecycle::Started,
    });
    let exact_groups = build_exact_groups(&images);
    let visual_candidates = if exact_only {
        Vec::new()
    } else {
        build_visual_candidates(&images, similarity_threshold)
    };
    progress.step(ProgressStep::Matching {
        lifecycle: Lifecycle::Completed,
    });

    Ok(AnalysisResult {
        analyzed: images.len(),
        exact_groups,
        visual_candidates,
        skipped,
    })
}

/// Reads every entry's cached fingerprint in one blocking batch, returning one
/// slot per entry. A cache that cannot be read at all is treated as all misses.
async fn cached_fingerprints(
    cache: Option<&FingerprintCache>,
    entries: &[(String, Vec<Pin>)],
) -> Vec<Option<ImageFingerprint>> {
    let Some(cache) = cache.cloned() else {
        return vec![None; entries.len()];
    };
    let urls = entries
        .iter()
        .map(|(media_url, _)| media_url.clone())
        .collect::<Vec<_>>();
    let total = urls.len();
    tokio::task::spawn_blocking(move || {
        urls.iter()
            .map(|media_url| cache.get(media_url))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_else(|_| vec![None; total])
}

fn analyzed_images(
    media_url: &str,
    pins: Vec<Pin>,
    fingerprint: &ImageFingerprint,
) -> Vec<AnalyzedImage> {
    pins.into_iter()
        .map(|pin| AnalyzedImage {
            pin_url: pin.pin_url(),
            pin_id: pin.id,
            board: pin.board,
            image_url: media_url.to_owned(),
            width: fingerprint.width,
            height: fingerprint.height,
            byte_size: fingerprint.byte_size,
            sha256: fingerprint.sha256.clone(),
            difference_hash: fingerprint.difference_hash,
            structural_signature: fingerprint.structural_signature.clone(),
        })
        .collect()
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
    let (difference_hash, structural_signature) = fingerprint_hashes(&image);

    Ok(ImageFingerprint {
        width,
        height,
        byte_size: bytes.len() as u64,
        sha256,
        difference_hash,
        structural_signature,
    })
}

async fn download_bytes(
    http: &Client,
    media_url: &str,
    image_buffer_budget: &Arc<Semaphore>,
    max_image_bytes: u64,
) -> Result<(Vec<u8>, OwnedSemaphorePermit), String> {
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
        .is_some_and(|length| length > max_image_bytes)
    {
        return Err(image_size_limit_error(max_image_bytes));
    }

    // Reserve the advertised size when available. Chunked/unknown-length
    // responses reserve the full per-image ceiling before their first body
    // chunk, so a slow decoder cannot be surrounded by many growing buffers.
    let reserved_bytes = response.content_length().unwrap_or(max_image_bytes);
    let reserved_permits = u32::try_from(reserved_bytes)
        .map_err(|_| "image safety limit cannot be represented by the buffer budget".to_owned())?;
    let buffer_permit = Arc::clone(image_buffer_budget)
        .acquire_many_owned(reserved_permits)
        .await
        .map_err(|_| "image buffer budget is unavailable".to_owned())?;
    let capacity = usize::try_from(reserved_bytes)
        .map_err(|_| "image safety limit cannot fit in memory on this platform".to_owned())?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(|error| format!("image download failed: {}", concise_reqwest_error(&error)))?
    {
        let new_length = u64::try_from(bytes.len())
            .ok()
            .and_then(|length| {
                u64::try_from(chunk.len())
                    .ok()
                    .and_then(|chunk_length| length.checked_add(chunk_length))
            })
            .ok_or_else(|| "image response size overflowed its safety check".to_owned())?;
        if new_length > max_image_bytes {
            return Err(image_size_limit_error(max_image_bytes));
        }
        bytes.extend_from_slice(&chunk);
    }

    Ok((bytes, buffer_permit))
}

fn image_reader(bytes: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>, String> {
    ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("could not identify image format: {error}"))
}

fn decode_image(bytes: &[u8], max_decoded_pixels: u64) -> Result<DynamicImage, String> {
    // Read only the format header first. This rejects a decompression bomb
    // before `DynamicImage::from_decoder` allocates the full raster.
    let (width, height) = image_reader(bytes)?
        .into_dimensions()
        .map_err(|error| format!("could not read image dimensions: {error}"))?;
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

fn image_size_limit_error(max_image_bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if max_image_bytes.is_multiple_of(MIB) {
        format!(
            "image exceeds the {} MiB safety limit",
            max_image_bytes / MIB
        )
    } else {
        format!("image exceeds the {max_image_bytes}-byte safety limit")
    }
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

/// Derives both hashes from a single full-resolution downscale.
///
/// Grayscale conversion comes first because only luminance is ever used, and
/// resizing one channel instead of three or four is where most of the saving
/// comes from. The difference-hash grid is then taken off the 64×64 signature
/// rather than the original, so the second downscale touches four thousand
/// pixels instead of several million.
fn fingerprint_hashes(image: &DynamicImage) -> (u64, Box<[u8]>) {
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

    (hash, signature.into_raw().into_boxed_slice())
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
        .map(|members| {
            let items = ranked_items(images, &members);
            DuplicateGroup {
                scope: MatchScope::of(&items),
                items,
            }
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
                scope: MatchScope::of(&ranked),
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

    fn fingerprint() -> ImageFingerprint {
        ImageFingerprint {
            width: 1200,
            height: 800,
            byte_size: 42_000,
            sha256: "abc123".into(),
            difference_hash: 0xfeed,
            structural_signature: vec![0, 64, 128, 255].into_boxed_slice(),
        }
    }

    fn analyzed(id: &str, width: u32, height: u32, bytes: u64, hash: u64) -> AnalyzedImage {
        AnalyzedImage {
            pin_id: id.into(),
            pin_url: format!("https://www.pinterest.com/pin/{id}/"),
            board: None,
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
    fn cached_signatures_are_stored_as_hex_not_a_number_array() {
        // A real signature is 4 KiB. Written as a JSON number array it costs
        // roughly four thousand integer parses per cached image, which is the
        // whole cost of a warm run; keep the compact encoding pinned.
        let mut fingerprint = fingerprint();
        fingerprint.structural_signature = vec![0, 15, 16, 255].into_boxed_slice();
        let encoded = serde_json::to_string(&CachedFingerprint {
            version: CACHE_FORMAT_VERSION,
            fingerprint: fingerprint.clone(),
        })
        .unwrap();

        assert!(
            encoded.contains(r#""structural_signature":"000f10ff""#),
            "{encoded}"
        );
        let decoded: CachedFingerprint = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.fingerprint, fingerprint);
    }

    #[test]
    fn fingerprint_cache_round_trips_without_exposing_the_url() {
        let directory = tempfile::tempdir().unwrap();
        let cache = FingerprintCache::new(directory.path().to_path_buf());
        let media_url = "https://i.pinimg.com/originals/private-looking-name.jpg?token=value";
        let expected = fingerprint();

        assert_eq!(cache.get(media_url), None);
        cache.put(media_url, &expected);
        assert_eq!(cache.get(media_url), Some(expected));

        let entries = fs::read_dir(&cache.directory)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(entries.len(), 1);
        let filename = entries[0].file_name().to_string_lossy().into_owned();
        assert!(filename.ends_with(".json"));
        assert!(!filename.contains("private-looking-name"));
        assert!(!filename.contains("token"));
    }

    #[test]
    fn fingerprint_cache_discards_corrupt_or_old_format_entries() {
        let directory = tempfile::tempdir().unwrap();
        let cache = FingerprintCache::new(directory.path().to_path_buf());
        let media_url = "https://i.pinimg.com/originals/example.jpg";
        let path = cache.entry_path(media_url);
        fs::create_dir_all(&cache.directory).unwrap();

        fs::write(&path, b"not json").unwrap();
        assert_eq!(cache.get(media_url), None);
        assert!(!path.exists());

        let old = CachedFingerprint {
            version: CACHE_FORMAT_VERSION.saturating_sub(1),
            fingerprint: fingerprint(),
        };
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        assert_eq!(cache.get(media_url), None);
        assert!(!path.exists());
    }

    #[test]
    fn fingerprint_cache_prunes_expired_entries_but_leaves_other_files() {
        let directory = tempfile::tempdir().unwrap();
        let cache_directory = directory.path().join(CACHE_ENTRY_SUBDIRECTORY);
        fs::create_dir_all(&cache_directory).unwrap();
        let unpruned = FingerprintCache {
            directory: cache_directory,
        };
        let expired = unpruned.entry_path("https://i.pinimg.com/expired.jpg");
        let current = unpruned.entry_path("https://i.pinimg.com/current.jpg");
        let unrelated = unpruned.directory.join("notes.json");
        let shared_root_entry = directory.path().join(format!("{}.json", "a".repeat(64)));
        fs::write(&expired, b"expired").unwrap();
        fs::write(&current, b"current").unwrap();
        fs::write(&unrelated, b"leave this alone").unwrap();
        fs::write(&shared_root_entry, b"belongs to another application").unwrap();

        let old_time = SystemTime::now() - CACHE_MAX_AGE - Duration::from_secs(1);
        fs::File::options()
            .write(true)
            .open(&expired)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(old_time))
            .unwrap();

        let _cache = FingerprintCache::new(directory.path().to_path_buf());
        assert!(!expired.exists());
        assert!(current.exists());
        assert!(unrelated.exists());
        assert!(shared_root_entry.exists());
    }

    #[test]
    fn platform_cache_paths_must_be_nonempty_and_absolute() {
        assert_eq!(absolute_path(None), None);
        assert_eq!(absolute_path(Some(OsString::new())), None);
        assert_eq!(absolute_path(Some("relative/cache".into())), None);
        #[cfg(not(target_os = "windows"))]
        assert_eq!(
            absolute_path(Some("/tmp/unpin-cache".into())),
            Some(PathBuf::from("/tmp/unpin-cache"))
        );
    }

    #[tokio::test]
    async fn analysis_uses_a_cached_fingerprint_without_downloading() {
        let directory = tempfile::tempdir().unwrap();
        let media_url = "http://127.0.0.1:9/should-not-be-requested.png";
        FingerprintCache::new(directory.path().to_path_buf()).put(media_url, &fingerprint());
        let pin = Pin {
            id: "cached-pin".into(),
            media_url: media_url.into(),
            metadata_width: None,
            metadata_height: None,
            board: None,
        };

        let result = analyze_pins_with_progress_and_cache(
            vec![pin],
            false,
            5,
            &NoProgress,
            Some(directory.path().to_path_buf()),
        )
        .await
        .unwrap();

        assert_eq!(result.analyzed, 1);
        assert!(result.skipped.is_empty());
    }

    #[tokio::test]
    async fn image_streams_over_the_byte_limit_are_skipped() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/oversized"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0_u8; 5]))
            .expect(1)
            .mount(&server)
            .await;

        let pin = Pin {
            id: "oversized-pin".into(),
            media_url: format!("{}/oversized", server.uri()),
            metadata_width: None,
            metadata_height: None,
            board: None,
        };
        let result = analyze_pins_with_limits(
            vec![pin],
            true,
            5,
            &NoProgress,
            None,
            ImageAnalysisLimits {
                max_image_bytes: 4,
                max_decoded_pixels: MAX_DECODED_PIXELS,
                image_buffer_budget: Arc::new(Semaphore::new(16)),
            },
        )
        .await
        .unwrap();

        assert_eq!(result.analyzed, 0);
        assert_eq!(result.skipped.len(), 1);
        assert!(result.skipped[0].reason.contains("safety limit"));
    }

    #[tokio::test]
    async fn images_over_the_decoded_pixel_limit_are_skipped_before_fingerprinting() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::new_rgb8(3, 2)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let encoded = encoded.into_inner();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/too-many-pixels.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(encoded.clone()))
            .expect(1)
            .mount(&server)
            .await;

        let pin = Pin {
            id: "too-many-pixels".into(),
            media_url: format!("{}/too-many-pixels.png", server.uri()),
            metadata_width: None,
            metadata_height: None,
            board: None,
        };
        let result = analyze_pins_with_limits(
            vec![pin],
            true,
            5,
            &NoProgress,
            None,
            ImageAnalysisLimits {
                max_image_bytes: u64::try_from(encoded.len()).unwrap(),
                max_decoded_pixels: 5,
                image_buffer_budget: Arc::new(Semaphore::new(encoded.len() + 1)),
            },
        )
        .await
        .unwrap();

        assert_eq!(result.analyzed, 0);
        assert_eq!(result.skipped.len(), 1);
        assert!(
            result.skipped[0].reason.contains("6 pixels")
                && result.skipped[0].reason.contains("5-pixel safety limit"),
            "{}",
            result.skipped[0].reason
        );
    }

    #[tokio::test]
    async fn image_buffer_budget_does_not_allow_completed_downloads_to_accumulate() {
        use tokio::time::timeout;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/small"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![1_u8; 4]))
            .expect(2)
            .mount(&server)
            .await;

        let http = Client::builder().build().unwrap();
        let image_buffer_budget = Arc::new(Semaphore::new(4));
        let first = download_bytes(
            &http,
            &format!("{}/small", server.uri()),
            &image_buffer_budget,
            4,
        )
        .await
        .unwrap();
        assert_eq!(first.0.len(), 4);
        assert_eq!(image_buffer_budget.available_permits(), 0);

        let mut second = tokio::spawn({
            let http = http.clone();
            let image_buffer_budget = Arc::clone(&image_buffer_budget);
            let url = format!("{}/small", server.uri());
            async move { download_bytes(&http, &url, &image_buffer_budget, 4).await }
        });
        assert!(
            timeout(Duration::from_millis(100), &mut second)
                .await
                .is_err()
        );

        drop(first);
        let second = timeout(Duration::from_secs(1), second)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(second.0.len(), 4);
        drop(second);
    }

    #[tokio::test]
    async fn image_downloads_are_bounded_by_the_network_concurrency_limit() {
        use std::time::Instant;

        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _test_guard = crate::test_support::high_concurrency_test_guard().await;
        let server = MockServer::start().await;
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::new_rgb8(2, 2)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let total = DOWNLOAD_CONCURRENCY * 2 + 1;
        Mock::given(method("GET"))
            .and(path_regex(r"/image/\d+\.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(bytes.into_inner())
                    // Keep the first wave in flight long enough to observe it
                    // before the next request can be admitted.
                    .set_delay(Duration::from_millis(500)),
            )
            .expect(total as u64)
            .mount(&server)
            .await;

        let pins = (0..total)
            .map(|index| Pin {
                id: format!("pin-{index}"),
                media_url: format!("{}/image/{index}.png", server.uri()),
                metadata_width: None,
                metadata_height: None,
                board: None,
            })
            .collect();
        let scan = tokio::spawn(async move {
            let progress = NoProgress;
            analyze_pins_with_progress_and_cache(pins, true, 5, &progress, None).await
        });

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let received = server.received_requests().await.unwrap().len();
            if received >= DOWNLOAD_CONCURRENCY {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the first download wave did not start"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let received = server.received_requests().await.unwrap().len();
        assert_eq!(
            received, DOWNLOAD_CONCURRENCY,
            "a request beyond the configured limit started before a response completed"
        );

        let result = scan.await.unwrap().unwrap();
        assert_eq!(result.analyzed, total);
        assert!(result.skipped.is_empty());
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
    fn groups_and_candidates_record_their_board_scope() {
        let mut first = analyzed("1", 800, 600, 1_000, 0);
        let mut second = analyzed("2", 1600, 1200, 2_000, 0);
        first.sha256 = "same".into();
        second.sha256 = "same".into();
        first.board = Some("Interiors".into());
        second.board = Some("Interiors".into());

        let same = build_exact_groups(&[first.clone(), second.clone()]);
        assert_eq!(same[0].scope, MatchScope::SameBoard);

        second.board = Some("Mood board".into());
        let cross = build_exact_groups(&[first, second]);
        assert_eq!(cross[0].scope, MatchScope::CrossBoard);

        // Visual candidates are classified the same way.
        let mut left = analyzed("1", 1000, 1000, 500, 0);
        let mut right = analyzed("2", 500, 500, 400, 0b11);
        left.board = Some("Interiors".into());
        right.board = Some("Mood board".into());
        let candidates = build_visual_candidates(&[left, right], 2);
        assert_eq!(candidates[0].scope, MatchScope::CrossBoard);
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
}
