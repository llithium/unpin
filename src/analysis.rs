use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures_util::stream::{self, StreamExt, TryStreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::Builder;
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use url::Url;

#[cfg(test)]
use image::DynamicImage;
#[cfg(test)]
use std::io::Cursor;

use crate::image_fingerprint::{ImageFingerprint, MAX_DECODED_PIXELS};
use crate::pinterest::{Pin, SkippedPin};
use crate::progress::{Lifecycle, NoProgress, Progress, ProgressStep};
use crate::report::{
    DuplicateGroup, MatchScope, Recommendation, ReportItem, VisualCandidate, rank_tuple,
};

const DOWNLOAD_CONCURRENCY: usize = 48;
const IMAGE_DOWNLOAD_ATTEMPTS: usize = 4;
const IMAGE_RETRY_BASE_DELAY: Duration = Duration::from_millis(250);
/// A response larger than this is reported as a skipped pin. The body is checked
/// both from its advertised length and while it streams, because image servers
/// do not always send a trustworthy `Content-Length`.
const MAX_IMAGE_BYTES: u64 = 100 * 1024 * 1024;
/// Downloaded image buffers reserve one semaphore permit per byte and keep that
/// reservation through decoding. This prevents completed downloads from
/// accumulating to `DOWNLOAD_CONCURRENCY * MAX_IMAGE_BYTES` while CPU work lags.
const MAX_IN_FLIGHT_IMAGE_BYTES: u64 = 512 * 1024 * 1024;
/// Chunked image responses have no reliable size to preallocate. Start small
/// while retaining the full buffer-budget reservation that bounds their growth.
const UNKNOWN_LENGTH_INITIAL_BUFFER_BYTES: u64 = 64 * 1024;
const PINTEREST_RENDITION_FALLBACKS: [&str; 4] = ["736x", "564x", "474x", "236x"];
const MIN_STRUCTURAL_SIMILARITY: f64 = 0.97;
/// A report larger than this is not a useful interactive review queue and can
/// otherwise grow quadratically when a permissive similarity threshold is
/// selected. Stop with an actionable error instead of exhausting memory.
const MAX_VISUAL_CANDIDATES: usize = 100_000;
/// Aspect-ratio buckets are expressed in log space so the same relative
/// tolerance works for portrait, landscape, and square images. A ±3 lookup
/// covers every pair allowed by the one-percent final check below.
const ASPECT_RATIO_BUCKET_WIDTH: f64 = 0.005;
const ASPECT_RATIO_BUCKET_RADIUS: i64 = 3;
const CACHE_FORMAT_VERSION: u8 = 3;
const CACHE_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const CACHE_ENTRY_SUBDIRECTORY: &str = "fingerprints-v3";

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

    #[error(
        "visual matching exceeded the {limit}-candidate safety limit; rerun with --exact-only or a lower --similarity-threshold"
    )]
    VisualCandidateLimit { limit: usize },
}

#[derive(Debug, Clone)]
struct AnalyzedImage {
    pin_id: String,
    pin_url: String,
    board: Option<String>,
    image_url: String,
    fingerprint: ImageFingerprint,
}

struct DownloadedImage {
    media_url: String,
    bytes: Vec<u8>,
    buffer_permit: OwnedSemaphorePermit,
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

    fn get(&self, media_url: &str, require_visual: bool) -> Option<ImageFingerprint> {
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
        let fingerprint = cached.fingerprint;
        if require_visual && !fingerprint.visual_ready {
            return None;
        }
        Some(fingerprint)
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
            width: self.fingerprint.width,
            height: self.fingerprint.height,
            byte_size: self.fingerprint.byte_size,
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
    let hits = cached_fingerprints(cache.as_ref(), &entries, !exact_only).await;
    let mut misses = Vec::new();
    for ((media_url, pins), hit) in entries.into_iter().zip(hits) {
        match hit {
            Some((cached_media_url, fingerprint)) => {
                images.extend(analyzed_images(&cached_media_url, pins, &fingerprint));
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
            let download =
                download_image(&http, &media_url, &image_buffer_budget, max_image_bytes).await;
            (media_url, pins, download)
        }
    }))
    .buffer_unordered(DOWNLOAD_CONCURRENCY)
    .map(|(media_url, pins, download)| {
        let cache = cache.clone();
        async move {
            let (media_url, fingerprint) = match download {
                Ok(DownloadedImage {
                    media_url,
                    bytes,
                    buffer_permit,
                }) => {
                    let cached_url = media_url.clone();
                    let fingerprint = tokio::task::spawn_blocking(move || {
                        // Keep the compressed-buffer reservation until the
                        // decoded image and its derived hashes are finished.
                        let _buffer_permit = buffer_permit;
                        let fingerprint =
                            ImageFingerprint::from_bytes(&bytes, exact_only, max_decoded_pixels)?;
                        if let Some(cache) = &cache {
                            cache.put(&cached_url, &fingerprint);
                        }
                        Ok(fingerprint)
                    })
                    .await
                    .unwrap_or_else(|_| Err("image analysis did not finish".to_owned()));
                    (media_url, fingerprint)
                }
                Err(reason) => (media_url, Err(reason)),
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
        build_visual_candidates(&images, similarity_threshold)?
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
    require_visual: bool,
) -> Vec<Option<(String, ImageFingerprint)>> {
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
            .map(|media_url| {
                image_download_candidates(media_url)
                    .into_iter()
                    .find_map(|candidate| {
                        cache
                            .get(&candidate, require_visual)
                            .map(|fingerprint| (candidate, fingerprint))
                    })
            })
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
            fingerprint: fingerprint.clone(),
        })
        .collect()
}

async fn download_image(
    http: &Client,
    media_url: &str,
    image_buffer_budget: &Arc<Semaphore>,
    max_image_bytes: u64,
) -> Result<DownloadedImage, String> {
    let mut last_error = None;
    for candidate in image_download_candidates(media_url) {
        match download_bytes_with_details(http, &candidate, image_buffer_budget, max_image_bytes)
            .await
        {
            Ok((bytes, buffer_permit)) => {
                return Ok(DownloadedImage {
                    media_url: candidate,
                    bytes,
                    buffer_permit,
                });
            }
            Err(error) if error.status == Some(reqwest::StatusCode::FORBIDDEN) => {
                last_error = Some(error.reason);
            }
            Err(error) => return Err(error.reason),
        }
    }

    Err(last_error.unwrap_or_else(|| "image download failed".to_owned()))
}

fn image_download_candidates(media_url: &str) -> Vec<String> {
    let mut candidates = vec![media_url.to_owned()];
    let Ok(url) = Url::parse(media_url) else {
        return candidates;
    };
    let Some(host) = url.host_str() else {
        return candidates;
    };
    if host != "pinimg.com" && !host.ends_with(".pinimg.com") {
        return candidates;
    }
    let Some(path_segments) = url.path_segments() else {
        return candidates;
    };
    let path_segments = path_segments.map(str::to_owned).collect::<Vec<_>>();
    let Some(original_index) = path_segments
        .iter()
        .position(|segment| segment == "originals")
    else {
        return candidates;
    };

    for rendition in PINTEREST_RENDITION_FALLBACKS {
        let mut candidate = url.clone();
        let Ok(mut segments) = candidate.path_segments_mut() else {
            continue;
        };
        segments.clear();
        for (index, segment) in path_segments.iter().enumerate() {
            segments.push(if index == original_index {
                rendition
            } else {
                segment
            });
        }
        drop(segments);
        candidates.push(candidate.into());
    }
    candidates
}

#[cfg(test)]
async fn download_bytes(
    http: &Client,
    media_url: &str,
    image_buffer_budget: &Arc<Semaphore>,
    max_image_bytes: u64,
) -> Result<(Vec<u8>, OwnedSemaphorePermit), String> {
    download_bytes_with_details(http, media_url, image_buffer_budget, max_image_bytes)
        .await
        .map_err(|error| error.reason)
}

async fn download_bytes_with_details(
    http: &Client,
    media_url: &str,
    image_buffer_budget: &Arc<Semaphore>,
    max_image_bytes: u64,
) -> Result<(Vec<u8>, OwnedSemaphorePermit), ImageDownloadError> {
    for attempt in 0..IMAGE_DOWNLOAD_ATTEMPTS {
        match download_bytes_once(http, media_url, image_buffer_budget, max_image_bytes).await {
            Ok(download) => return Ok(download),
            Err(error) if error.retryable && attempt + 1 < IMAGE_DOWNLOAD_ATTEMPTS => {
                tokio::time::sleep(IMAGE_RETRY_BASE_DELAY.saturating_mul(1_u32 << attempt.min(16)))
                    .await;
            }
            Err(error) => return Err(error),
        }
    }

    unreachable!("the image download loop always returns on its final attempt")
}

struct ImageDownloadError {
    reason: String,
    retryable: bool,
    status: Option<reqwest::StatusCode>,
}

async fn download_bytes_once(
    http: &Client,
    media_url: &str,
    image_buffer_budget: &Arc<Semaphore>,
    max_image_bytes: u64,
) -> Result<(Vec<u8>, OwnedSemaphorePermit), ImageDownloadError> {
    let response = http.get(media_url).send().await.map_err(|error| {
        let retryable = error.is_connect() || error.is_timeout();
        ImageDownloadError {
            reason: format!("image download failed: {}", concise_reqwest_error(&error)),
            retryable,
            status: None,
        }
    })?;

    if !response.status().is_success() {
        let status = response.status();
        return Err(ImageDownloadError {
            reason: format!("image download returned HTTP {status}"),
            retryable: status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error(),
            status: Some(status),
        });
    }
    let advertised_bytes = response.content_length();
    if advertised_bytes.is_some_and(|length| length > max_image_bytes) {
        return Err(ImageDownloadError {
            reason: image_size_limit_error(max_image_bytes),
            retryable: false,
            status: None,
        });
    }

    // Reserve the advertised size when available. Chunked/unknown-length
    // responses reserve the full per-image ceiling before their first body
    // chunk, so a slow decoder cannot be surrounded by many growing buffers.
    // A stream that contradicts an advertised length is rejected rather than
    // growing past the amount reserved from the shared buffer budget.
    let reserved_bytes = advertised_bytes.unwrap_or(max_image_bytes);
    let reserved_permits = u32::try_from(reserved_bytes).map_err(|_| ImageDownloadError {
        reason: "image safety limit cannot be represented by the buffer budget".to_owned(),
        retryable: false,
        status: None,
    })?;
    let buffer_permit = Arc::clone(image_buffer_budget)
        .acquire_many_owned(reserved_permits)
        .await
        .map_err(|_| ImageDownloadError {
            reason: "image buffer budget is unavailable".to_owned(),
            retryable: false,
            status: None,
        })?;
    let capacity =
        initial_image_buffer_capacity(advertised_bytes, max_image_bytes).map_err(|_| {
            ImageDownloadError {
                reason: "image safety limit cannot fit in memory on this platform".to_owned(),
                retryable: false,
                status: None,
            }
        })?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(|error| ImageDownloadError {
            reason: format!("image download failed: {}", concise_reqwest_error(&error)),
            retryable: error.is_body() || error.is_timeout(),
            status: None,
        })?
    {
        checked_image_length(bytes.len(), chunk.len(), max_image_bytes, advertised_bytes).map_err(
            |reason| ImageDownloadError {
                reason,
                retryable: false,
                status: None,
            },
        )?;
        bytes.extend_from_slice(&chunk);
    }

    Ok((bytes, buffer_permit))
}

fn initial_image_buffer_capacity(
    advertised_bytes: Option<u64>,
    max_image_bytes: u64,
) -> Result<usize, std::num::TryFromIntError> {
    let capacity = advertised_bytes
        .unwrap_or(UNKNOWN_LENGTH_INITIAL_BUFFER_BYTES)
        .min(max_image_bytes);
    usize::try_from(capacity)
}

fn checked_image_length(
    current_length: usize,
    chunk_length: usize,
    max_image_bytes: u64,
    advertised_bytes: Option<u64>,
) -> Result<u64, String> {
    let new_length = u64::try_from(current_length)
        .ok()
        .and_then(|length| {
            u64::try_from(chunk_length)
                .ok()
                .and_then(|chunk_length| length.checked_add(chunk_length))
        })
        .ok_or_else(|| "image response size overflowed its safety check".to_owned())?;
    if new_length > max_image_bytes {
        return Err(image_size_limit_error(max_image_bytes));
    }
    if advertised_bytes.is_some_and(|advertised| new_length > advertised) {
        return Err("image response exceeded its advertised content length".to_owned());
    }
    Ok(new_length)
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

fn build_exact_groups(images: &[AnalyzedImage]) -> Vec<DuplicateGroup> {
    let mut hashes: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, image) in images.iter().enumerate() {
        hashes
            .entry(&image.fingerprint.sha256)
            .or_default()
            .push(index);
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
        left.scope
            .sort_priority()
            .cmp(&right.scope.sort_priority())
            .then_with(|| Reverse(left.items.len()).cmp(&Reverse(right.items.len())))
            .then_with(|| left.items[0].pin_id.cmp(&right.items[0].pin_id))
    });
    groups
}

fn build_visual_candidates(
    images: &[AnalyzedImage],
    threshold: u8,
) -> Result<Vec<VisualCandidate>, AnalysisError> {
    build_visual_candidates_with_limit(images, threshold, MAX_VISUAL_CANDIDATES)
}

fn build_visual_candidates_with_limit(
    images: &[AnalyzedImage],
    threshold: u8,
    max_candidates: usize,
) -> Result<Vec<VisualCandidate>, AnalysisError> {
    // Every pin with the same SHA-256 has the same visual fingerprint. Compare
    // each pair of exact-image groups once, then expand a passing comparison
    // back to the per-pin candidates expected by the report.
    let mut group_by_sha = HashMap::<&str, usize>::new();
    let mut fingerprint_groups = Vec::<Vec<usize>>::new();
    for (index, image) in images.iter().enumerate() {
        let group_index = match group_by_sha.get(image.fingerprint.sha256.as_str()) {
            Some(&group_index) => group_index,
            None => {
                let group_index = fingerprint_groups.len();
                group_by_sha.insert(&image.fingerprint.sha256, group_index);
                fingerprint_groups.push(Vec::new());
                group_index
            }
        };
        fingerprint_groups[group_index].push(index);
    }

    let mut trees = HashMap::<i64, BkTree>::new();
    let mut candidates = Vec::new();

    for (group_index, members) in fingerprint_groups.iter().enumerate() {
        let index = members[0];
        let image = &images[index];
        let bucket = aspect_ratio_bucket(image);
        for offset in -ASPECT_RATIO_BUCKET_RADIUS..=ASPECT_RATIO_BUCKET_RADIUS {
            if let Some(tree) = trees.get(&(bucket + offset)) {
                for other_group_index in tree.query(image.fingerprint.difference_hash, threshold) {
                    let other_members = &fingerprint_groups[other_group_index];
                    let other_index = other_members[0];
                    let other = &images[other_index];
                    if !aspect_ratios_match(image, other) {
                        continue;
                    }
                    let structural_similarity =
                        image.fingerprint.visual_similarity(&other.fingerprint);
                    if structural_similarity < MIN_STRUCTURAL_SIMILARITY {
                        continue;
                    }

                    let distance = (image.fingerprint.difference_hash
                        ^ other.fingerprint.difference_hash)
                        .count_ones() as u8;
                    let similarity_percent = (structural_similarity * 100.0).floor() as u8;
                    for &other_index in other_members {
                        for &index in members {
                            if candidates.len() >= max_candidates {
                                return Err(AnalysisError::VisualCandidateLimit {
                                    limit: max_candidates,
                                });
                            }

                            let pair = [other_index, index];
                            let ranked = ranked_items(images, &pair);
                            candidates.push(VisualCandidate {
                                hash_distance: distance,
                                similarity_percent,
                                scope: MatchScope::of(&ranked),
                                items: [ranked[0].clone(), ranked[1].clone()],
                            });
                        }
                    }
                }
            }
        }
        trees.entry(bucket).or_default().insert(
            image.fingerprint.difference_hash,
            &image.fingerprint.sha256,
            group_index,
        );
    }

    candidates.sort_by(|left, right| {
        left.scope
            .sort_priority()
            .cmp(&right.scope.sort_priority())
            .then_with(|| left.hash_distance.cmp(&right.hash_distance))
            .then_with(|| left.items[0].pin_id.cmp(&right.items[0].pin_id))
            .then_with(|| left.items[1].pin_id.cmp(&right.items[1].pin_id))
    });
    Ok(candidates)
}

fn aspect_ratio_bucket(image: &AnalyzedImage) -> i64 {
    let ratio = f64::from(image.fingerprint.width) / f64::from(image.fingerprint.height);
    (ratio.ln() / ASPECT_RATIO_BUCKET_WIDTH).floor() as i64
}

fn aspect_ratios_match(left: &AnalyzedImage, right: &AnalyzedImage) -> bool {
    let left_ratio = left.fingerprint.width as f64 / left.fingerprint.height as f64;
    let right_ratio = right.fingerprint.width as f64 / right.fingerprint.height as f64;
    (left_ratio - right_ratio).abs() / left_ratio.max(right_ratio) <= 0.01
}

fn ranked_items(images: &[AnalyzedImage], members: &[usize]) -> Vec<ReportItem> {
    let mut sorted = members.to_vec();
    sorted.sort_by(|left, right| {
        rank_tuple(
            images[*right].fingerprint.width,
            images[*right].fingerprint.height,
            images[*right].fingerprint.byte_size,
        )
        .cmp(&rank_tuple(
            images[*left].fingerprint.width,
            images[*left].fingerprint.height,
            images[*left].fingerprint.byte_size,
        ))
        .then_with(|| images[*left].pin_id.cmp(&images[*right].pin_id))
    });

    let best_rank = rank_tuple(
        images[sorted[0]].fingerprint.width,
        images[sorted[0]].fingerprint.height,
        images[sorted[0]].fingerprint.byte_size,
    );
    let best_count = sorted
        .iter()
        .take_while(|index| {
            rank_tuple(
                images[**index].fingerprint.width,
                images[**index].fingerprint.height,
                images[**index].fingerprint.byte_size,
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
    sha_groups: HashMap<String, Vec<usize>>,
    children: BTreeMap<u8, usize>,
}

impl BkTree {
    fn insert(&mut self, hash: u64, sha256: &str, image_index: usize) {
        if self.nodes.is_empty() {
            self.nodes.push(BkNode {
                hash,
                image_indices: vec![image_index],
                sha_groups: HashMap::from([(sha256.to_owned(), vec![image_index])]),
                children: BTreeMap::new(),
            });
            return;
        }

        let mut node_index = 0;
        loop {
            let distance = (hash ^ self.nodes[node_index].hash).count_ones() as u8;
            if distance == 0 {
                let node = &mut self.nodes[node_index];
                node.image_indices.push(image_index);
                node.sha_groups
                    .entry(sha256.to_owned())
                    .or_default()
                    .push(image_index);
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
                sha_groups: HashMap::from([(sha256.to_owned(), vec![image_index])]),
                children: BTreeMap::new(),
            });
            self.nodes[node_index].children.insert(distance, new_index);
            return;
        }
    }

    fn query(&self, hash: u64, threshold: u8) -> Vec<usize> {
        self.query_filtered(hash, threshold, None)
    }

    #[cfg(test)]
    fn query_excluding_sha(&self, hash: u64, threshold: u8, excluded_sha: &str) -> Vec<usize> {
        self.query_filtered(hash, threshold, Some(excluded_sha))
    }

    fn query_filtered(&self, hash: u64, threshold: u8, excluded_sha: Option<&str>) -> Vec<usize> {
        if self.nodes.is_empty() {
            return Vec::new();
        }

        let mut results = Vec::new();
        let mut stack = vec![0];
        while let Some(node_index) = stack.pop() {
            let node = &self.nodes[node_index];
            let distance = (hash ^ node.hash).count_ones() as u8;
            if distance <= threshold {
                match excluded_sha {
                    Some(excluded_sha) => node
                        .sha_groups
                        .iter()
                        .filter(|(sha256, _)| sha256.as_str() != excluded_sha)
                        .flat_map(|(_, indices)| indices.iter().copied())
                        .for_each(|index| results.push(index)),
                    None => results.extend(node.image_indices.iter().copied()),
                }
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
        let structural_signature = vec![0, 64, 128, 255].into_boxed_slice();
        let (structural_sum, structural_sum_squares) =
            test_structural_statistics(&structural_signature);
        ImageFingerprint {
            width: 1200,
            height: 800,
            byte_size: 42_000,
            sha256: "abc123".into(),
            difference_hash: 0xfeed,
            structural_signature,
            structural_sum,
            structural_sum_squares,
            visual_ready: true,
        }
    }

    fn analyzed(id: &str, width: u32, height: u32, bytes: u64, hash: u64) -> AnalyzedImage {
        let structural_signature = vec![0, 10, 50, 100, 180, 255].into_boxed_slice();
        let (structural_sum, structural_sum_squares) =
            test_structural_statistics(&structural_signature);
        AnalyzedImage {
            pin_id: id.into(),
            pin_url: format!("https://www.pinterest.com/pin/{id}/"),
            board: None,
            image_url: format!("https://i.pinimg.com/originals/{id}.jpg"),
            fingerprint: ImageFingerprint {
                width,
                height,
                byte_size: bytes,
                sha256: format!("sha-{id}"),
                difference_hash: hash,
                structural_signature,
                structural_sum,
                structural_sum_squares,
                visual_ready: true,
            },
        }
    }

    fn test_structural_statistics(signature: &[u8]) -> (u64, u64) {
        signature
            .iter()
            .fold((0_u64, 0_u64), |(sum, squares), &value| {
                let value = u64::from(value);
                (sum + value, squares + value * value)
            })
    }

    #[test]
    fn bk_tree_finds_values_within_hamming_distance() {
        let mut tree = BkTree::default();
        tree.insert(0, "zero", 0);
        tree.insert(0b1111, "ones", 1);
        tree.insert(u64::MAX, "max", 2);

        let mut matches = tree.query(0b0011, 2);
        matches.sort_unstable();
        assert_eq!(matches, vec![0, 1]);
    }

    #[test]
    fn bk_tree_can_exclude_one_sha_group_without_dropping_other_same_hash_images() {
        let mut tree = BkTree::default();
        tree.insert(0, "same", 0);
        tree.insert(0, "same", 1);
        tree.insert(0, "different", 2);

        assert_eq!(tree.query_excluding_sha(0, 0, "same"), vec![2]);
    }

    #[test]
    fn visual_matching_expands_unique_fingerprint_pairs_to_pin_pairs() {
        let mut images = vec![
            analyzed("left-a", 1200, 800, 42_000, 0),
            analyzed("right-a", 1200, 800, 42_000, 1),
            analyzed("left-b", 1200, 800, 42_000, 0),
            analyzed("right-b", 1200, 800, 42_000, 1),
        ];
        images[0].fingerprint.sha256 = "left".into();
        images[2].fingerprint.sha256 = "left".into();
        images[1].fingerprint.sha256 = "right".into();
        images[3].fingerprint.sha256 = "right".into();

        let candidates = build_visual_candidates_with_limit(&images, 5, 4).unwrap();
        let pairs = candidates
            .iter()
            .map(|candidate| {
                let mut ids = candidate
                    .items
                    .iter()
                    .map(|item| item.pin_id.as_str())
                    .collect::<Vec<_>>();
                ids.sort_unstable();
                ids
            })
            .collect::<Vec<_>>();

        assert_eq!(candidates.len(), 4);
        assert!(pairs.contains(&vec!["left-a", "right-a"]));
        assert!(pairs.contains(&vec!["left-a", "right-b"]));
        assert!(pairs.contains(&vec!["left-b", "right-a"]));
        assert!(pairs.contains(&vec!["left-b", "right-b"]));
        assert!(matches!(
            build_visual_candidates_with_limit(&images, 5, 3),
            Err(AnalysisError::VisualCandidateLimit { limit: 3 })
        ));
    }

    #[test]
    #[ignore = "manual duplicate-heavy visual matching benchmark"]
    fn benchmark_duplicate_heavy_visual_matching() {
        let mut images = Vec::new();
        for group in 0..2 {
            for member in 0..100 {
                let mut image =
                    analyzed(&format!("group-{group}-{member}"), 1200, 800, 42_000, group);
                image.fingerprint.sha256 = format!("sha-{group}");
                image.fingerprint.structural_signature = (0..4096)
                    .map(|value| (value % 256) as u8)
                    .collect::<Vec<_>>()
                    .into_boxed_slice();
                let (sum, squares) =
                    test_structural_statistics(&image.fingerprint.structural_signature);
                image.fingerprint.structural_sum = sum;
                image.fingerprint.structural_sum_squares = squares;
                images.push(image);
            }
        }

        let started = std::time::Instant::now();
        let candidates = build_visual_candidates(&images, 5).unwrap();
        eprintln!("{} candidates in {:?}", candidates.len(), started.elapsed());
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

        assert_eq!(cache.get(media_url, true), None);
        cache.put(media_url, &expected);
        assert_eq!(cache.get(media_url, true), Some(expected));

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
    fn partial_exact_cache_entries_are_refreshed_for_visual_scans() {
        let directory = tempfile::tempdir().unwrap();
        let cache = FingerprintCache::new(directory.path().to_path_buf());
        let media_url = "https://i.pinimg.com/originals/exact-only.jpg";
        let partial = ImageFingerprint {
            width: 1200,
            height: 800,
            byte_size: 42_000,
            sha256: "abc123".into(),
            difference_hash: 0,
            structural_signature: Vec::new().into_boxed_slice(),
            structural_sum: 0,
            structural_sum_squares: 0,
            visual_ready: false,
        };

        cache.put(media_url, &partial);

        assert!(cache.get(media_url, false).is_some());
        assert!(cache.get(media_url, true).is_none());
    }

    #[test]
    fn fingerprint_cache_discards_corrupt_or_old_format_entries() {
        let directory = tempfile::tempdir().unwrap();
        let cache = FingerprintCache::new(directory.path().to_path_buf());
        let media_url = "https://i.pinimg.com/originals/example.jpg";
        let path = cache.entry_path(media_url);
        fs::create_dir_all(&cache.directory).unwrap();

        fs::write(&path, b"not json").unwrap();
        assert_eq!(cache.get(media_url, true), None);
        assert!(!path.exists());

        let old = CachedFingerprint {
            version: CACHE_FORMAT_VERSION.saturating_sub(1),
            fingerprint: fingerprint(),
        };
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        assert_eq!(cache.get(media_url, true), None);
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
    async fn analysis_uses_a_cached_bounded_rendition_for_a_forbidden_original() {
        let directory = tempfile::tempdir().unwrap();
        let original_url = "http://i.pinimg.com/originals/cached.jpg";
        let fallback_url = "http://i.pinimg.com/736x/cached.jpg";
        let cache = FingerprintCache::new(directory.path().to_path_buf());
        cache.put(fallback_url, &fingerprint());
        let hit =
            cached_fingerprints(Some(&cache), &[(original_url.to_owned(), Vec::new())], true).await;
        assert_eq!(
            hit[0].as_ref().map(|(url, _)| url.as_str()),
            Some(fallback_url)
        );
        let pin = Pin {
            id: "cached-fallback-pin".into(),
            media_url: original_url.into(),
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

    #[test]
    fn image_buffer_accounting_rejects_a_stream_that_exceeds_content_length() {
        let error = checked_image_length(2, 2, 8, Some(3)).unwrap_err();
        assert!(error.contains("advertised content length"));
        assert_eq!(checked_image_length(2, 2, 8, Some(4)).unwrap(), 4);
        assert_eq!(checked_image_length(2, 2, 8, None).unwrap(), 4);
    }

    #[test]
    fn unknown_length_images_start_with_a_small_bounded_buffer() {
        assert_eq!(initial_image_buffer_capacity(Some(42), 1024).unwrap(), 42);
        assert_eq!(
            initial_image_buffer_capacity(None, MAX_IMAGE_BYTES).unwrap(),
            UNKNOWN_LENGTH_INITIAL_BUFFER_BYTES as usize
        );
        assert_eq!(initial_image_buffer_capacity(None, 32).unwrap(), 32);
    }

    #[tokio::test]
    async fn transient_image_server_errors_are_retried() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::new_rgb8(2, 2)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let encoded = encoded.into_inner();
        let attempts = Arc::new(AtomicUsize::new(0));
        let responder_attempts = Arc::clone(&attempts);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/eventually.png"))
            .respond_with(move |_: &wiremock::Request| {
                if responder_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(500)
                } else {
                    ResponseTemplate::new(200).set_body_bytes(encoded.clone())
                }
            })
            .expect(2)
            .mount(&server)
            .await;

        let http = Client::builder().build().unwrap();
        let image_buffer_budget = Arc::new(Semaphore::new(1024));
        let download = download_bytes(
            &http,
            &format!("{}/eventually.png", server.uri()),
            &image_buffer_budget,
            1024,
        )
        .await
        .unwrap();

        assert!(!download.0.is_empty());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn permanent_image_client_errors_are_not_retried() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/missing.png"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        let http = Client::builder().build().unwrap();
        let error = download_bytes(
            &http,
            &format!("{}/missing.png", server.uri()),
            &Arc::new(Semaphore::new(1024)),
            1024,
        )
        .await
        .unwrap_err();

        assert!(error.contains("HTTP 404"), "{error}");
    }

    #[tokio::test]
    async fn forbidden_originals_can_fall_back_to_a_bounded_pinterest_rendition() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/originals/example.jpg"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/736x/example.jpg"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"image".to_vec()))
            .expect(1)
            .mount(&server)
            .await;

        let http = Client::builder()
            .resolve("i.pinimg.com", *server.address())
            .build()
            .unwrap();
        let download = download_image(
            &http,
            "http://i.pinimg.com/originals/example.jpg",
            &Arc::new(Semaphore::new(1024)),
            1024,
        )
        .await
        .unwrap();

        assert_eq!(download.media_url, "http://i.pinimg.com/736x/example.jpg");
        assert_eq!(download.bytes, b"image");
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
        first.fingerprint.sha256 = "same".into();
        second.fingerprint.sha256 = "same".into();

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
        first.fingerprint.sha256 = "same".into();
        second.fingerprint.sha256 = "same".into();
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
        let candidates = build_visual_candidates(&[left, right], 2).unwrap();
        assert_eq!(candidates[0].scope, MatchScope::CrossBoard);
    }

    #[test]
    fn same_board_matches_sort_before_cross_board_matches() {
        let mut same_first = analyzed("3", 800, 600, 1_000, 0);
        let mut same_second = analyzed("4", 800, 600, 1_000, 0);
        same_first.fingerprint.sha256 = "same-board".into();
        same_second.fingerprint.sha256 = "same-board".into();
        same_first.board = Some("Interiors".into());
        same_second.board = Some("Interiors".into());

        let mut cross_first = analyzed("1", 800, 600, 1_000, 0);
        let mut cross_second = analyzed("2", 800, 600, 1_000, 0);
        cross_first.fingerprint.sha256 = "cross-board".into();
        cross_second.fingerprint.sha256 = "cross-board".into();
        cross_first.board = Some("Interiors".into());
        cross_second.board = Some("Mood board".into());

        let groups = build_exact_groups(&[cross_first, cross_second, same_first, same_second]);
        assert_eq!(groups[0].scope, MatchScope::SameBoard);
        assert_eq!(groups[1].scope, MatchScope::CrossBoard);

        // Scope comes before visual similarity, so a same-board candidate is
        // still first when the across-board candidate has the closer hash.
        let mut same_visual_first = analyzed("7", 1000, 1000, 500, 0b01111);
        let mut same_visual_second = analyzed("8", 1000, 1000, 500, 0b11111);
        same_visual_first.board = Some("Interiors".into());
        same_visual_second.board = Some("Interiors".into());

        let mut cross_visual_first = analyzed("5", 1000, 1000, 500, 0);
        let mut cross_visual_second = analyzed("6", 1000, 1000, 500, 0);
        cross_visual_first.board = Some("Interiors".into());
        cross_visual_second.board = Some("Mood board".into());

        let candidates = build_visual_candidates(
            &[
                cross_visual_first,
                cross_visual_second,
                same_visual_first,
                same_visual_second,
            ],
            1,
        )
        .unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].scope, MatchScope::SameBoard);
        assert_eq!(candidates[1].scope, MatchScope::CrossBoard);
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
        let candidates = build_visual_candidates(&images, 2).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].hash_distance, 2);
    }

    #[test]
    fn visual_candidates_keep_close_aspect_ratios_across_bucket_boundaries() {
        let images = [
            analyzed("1", 1000, 1000, 500, 0),
            analyzed("2", 1009, 1000, 400, 0b11),
        ];

        assert!(aspect_ratios_match(&images[0], &images[1]));
        assert!(
            (aspect_ratio_bucket(&images[0]) - aspect_ratio_bucket(&images[1])).abs()
                <= ASPECT_RATIO_BUCKET_RADIUS
        );
        assert_eq!(build_visual_candidates(&images, 2).unwrap().len(), 1);
    }

    #[test]
    fn visual_candidates_reject_structural_false_positives() {
        let mut left = analyzed("1", 1000, 1000, 500, 0);
        let mut right = analyzed("2", 500, 500, 400, 0b1);
        left.fingerprint.structural_signature = vec![255; 64 * 64].into_boxed_slice();
        right.fingerprint.structural_signature = vec![255; 64 * 64].into_boxed_slice();
        for y in 8..56 {
            left.fingerprint.structural_signature[y * 64 + 16] = 0;
            right.fingerprint.structural_signature[y * 64 + 48] = 0;
        }
        (
            left.fingerprint.structural_sum,
            left.fingerprint.structural_sum_squares,
        ) = test_structural_statistics(&left.fingerprint.structural_signature);
        (
            right.fingerprint.structural_sum,
            right.fingerprint.structural_sum_squares,
        ) = test_structural_statistics(&right.fingerprint.structural_signature);

        assert!(
            build_visual_candidates(&[left, right], 5)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn visual_matching_stops_before_candidate_memory_grows_unboundedly() {
        let images = [
            analyzed("1", 1000, 1000, 500, 0),
            analyzed("2", 1000, 1000, 400, 0),
        ];

        let error = build_visual_candidates_with_limit(&images, 0, 0).unwrap_err();
        assert_eq!(
            error.to_string(),
            "visual matching exceeded the 0-candidate safety limit; rerun with --exact-only or a lower --similarity-threshold"
        );
    }
}
