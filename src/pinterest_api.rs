//! Bounded transport adapter for Pinterest's undocumented resource API.
//!
//! `PinterestClient` owns scan-source semantics; this module owns endpoint
//! construction, authentication headers, retries, pagination, throttling, and
//! response safety limits so those decisions have one owner.

use std::collections::HashSet;
use std::error::Error as StdError;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::stream::TryStreamExt;
use rand::Rng;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};
use thiserror::Error;
use url::Url;

use crate::auth::{BrowserCookie, ScopedCookie};
use crate::progress::{Lifecycle, Progress, ProgressStep};

const MAX_PAGES: usize = 10_000;
/// A successful API response larger than this is rejected as an invalid
/// response. The body is checked from `Content-Length` and while streaming, so
/// a page cannot silently grow without bound; transport failures remain
/// `PinterestError::Request`.
const MAX_API_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
/// A feed that would retain more than this many result values fails with an
/// invalid-response limit error before the extra page is appended. It never
/// returns a silently truncated feed.
pub(crate) const MAX_FEED_RESULTS: usize = 50_000;
/// Board and section streams share this ceiling, so their fan-out cannot
/// multiply into an unbounded burst when several boards have sections.
///
/// Authenticated burst testing completed 2,298 requests without throttling or
/// transport failures through 512 concurrent requests. Feed responses are
/// substantially larger than the metadata response used by that probe, so use
/// one quarter of the observed safe burst instead of making response buffering
/// and provider load scale all the way to the test boundary.
pub(crate) const API_REQUEST_CONCURRENCY: usize = 128;
const MAX_REQUEST_ATTEMPTS: usize = 4;
const RETRY_BASE_DELAY: Duration = Duration::from_millis(250);
pub(crate) const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum PinterestError {
    #[error("invalid Pinterest target: {0}")]
    InvalidTarget(String),

    #[error(
        "authenticated requests with imported cookies require HTTPS for both Pinterest and API URLs"
    )]
    InsecureCookieTransport,

    #[error("cookie-bearing API root must match the Pinterest target origin")]
    CrossOriginCookieTransport,

    #[error("failed to build the Pinterest HTTP client")]
    Client(#[source] reqwest::Error),

    #[error("Pinterest request for {resource} failed")]
    Request {
        resource: &'static str,
        #[source]
        source: Box<dyn StdError + Send + Sync>,
    },

    #[error("Pinterest returned HTTP {status} for {resource}")]
    Http {
        resource: &'static str,
        status: reqwest::StatusCode,
    },

    #[error("Pinterest response for {resource} was invalid: {message}")]
    InvalidResponse {
        resource: &'static str,
        message: String,
    },
}

#[derive(Clone)]
pub(crate) struct PinterestApi {
    http: reqwest::Client,
    api_root: Url,
    cookies: Vec<ScopedCookie>,
    authenticated: bool,
    request_limiter: Arc<tokio::sync::Semaphore>,
}

impl PinterestApi {
    pub(crate) fn new(
        root: Url,
        api_root: Url,
        cookies: Vec<ScopedCookie>,
    ) -> Result<Self, PinterestError> {
        reject_url_userinfo(&root)?;
        reject_url_userinfo(&api_root)?;
        let authenticated = !cookies.is_empty();
        validate_cookie_transport(&root, &api_root, authenticated)?;
        let headers = build_headers(&root)?;
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .user_agent(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
                 AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/138.0.0.0 Safari/537.36",
            )
            .build()
            .map_err(PinterestError::Client)?;

        Ok(Self {
            http,
            api_root,
            cookies,
            authenticated,
            request_limiter: Arc::new(tokio::sync::Semaphore::new(API_REQUEST_CONCURRENCY)),
        })
    }

    pub(crate) fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    #[cfg(test)]
    pub(crate) fn available_permits(&self) -> usize {
        self.request_limiter.available_permits()
    }

    /// Fetches a complete provider feed while hiding its bookmark protocol,
    /// retry policy, page-size fallback, and retention limits from source code.
    pub(crate) async fn paginate(
        &self,
        resource: &'static str,
        mut options: Value,
        progress: &dyn Progress,
    ) -> Result<Vec<Value>, PinterestError> {
        let mut all_results = Vec::new();
        let mut seen_bookmarks = HashSet::new();

        for page_index in 0..MAX_PAGES {
            progress.step(ProgressStep::PageCollection {
                resource,
                page: page_index + 1,
                items: all_results.len(),
                lifecycle: Lifecycle::Started,
            });
            let response = match self.call(resource, options.clone(), progress).await {
                Ok(response) => response,
                // An oversized `page_size` is refused outright, and the value
                // that is acceptable today is not promised for tomorrow. Drop
                // the option and take Pinterest's default page rather than
                // losing the scan; `remove_page_size` reporting false on the
                // second pass stops this from looping.
                Err(error) => {
                    if !is_bad_request(&error) || !remove_page_size(&mut options) {
                        return Err(error);
                    }
                    self.call(resource, options.clone(), progress).await?
                }
            };
            // Read the bookmark before the results are moved out of the response.
            let bookmark = response_bookmark(&response, resource)?;
            let page_results = response_results(response, resource)?;
            let retained_results = all_results
                .len()
                .checked_add(page_results.len())
                .ok_or_else(|| {
                    invalid_response(resource, "feed result count overflowed its safety check")
                })?;
            if retained_results > MAX_FEED_RESULTS {
                return Err(invalid_response(
                    resource,
                    format!("feed exceeded the {MAX_FEED_RESULTS}-result retention safety limit"),
                ));
            }
            all_results.extend(page_results);
            progress.step(ProgressStep::PageCollection {
                resource,
                page: page_index + 1,
                items: all_results.len(),
                lifecycle: Lifecycle::Completed,
            });

            let Some(bookmark) = bookmark else {
                return Ok(all_results);
            };
            if bookmark == "-end-" || bookmark.starts_with("Y2JOb25lO") {
                return Ok(all_results);
            }
            if !seen_bookmarks.insert(bookmark.clone()) {
                return Err(invalid_response(
                    resource,
                    "Pinterest returned the same pagination bookmark twice",
                ));
            }

            let object = options.as_object_mut().ok_or_else(|| {
                invalid_response(resource, "pagination options were not an object")
            })?;
            object.insert("bookmarks".into(), json!([bookmark]));
        }

        Err(invalid_response(
            resource,
            "pagination exceeded the safety limit",
        ))
    }

    /// Performs one bounded, retried provider request. The permit remains held
    /// until the response body has been consumed, so buffered bodies cannot
    /// bypass the shared API request ceiling.
    pub(crate) async fn call(
        &self,
        resource: &'static str,
        options: Value,
        progress: &dyn Progress,
    ) -> Result<Value, PinterestError> {
        let endpoint = self
            .api_root
            .join(&format!("resource/{resource}Resource/get/"))
            .map_err(|_| invalid_response(resource, "could not construct the endpoint URL"))?;
        let (csrf_token, cookie_header) = build_request_cookie_header(&self.cookies, &endpoint)?;
        let mut csrf_header = HeaderValue::from_str(&csrf_token)
            .map_err(|_| invalid_response("headers", "invalid csrf header value"))?;
        csrf_header.set_sensitive(true);
        let data = serde_json::to_string(&json!({ "options": options }))
            .map_err(|_| invalid_response(resource, "could not serialize request options"))?;

        for attempt in 0..MAX_REQUEST_ATTEMPTS {
            let delay = {
                let _permit = self
                    .request_limiter
                    .acquire()
                    .await
                    .expect("the API request limiter is never closed");
                let response = self
                    .http
                    .get(endpoint.clone())
                    .query(&[("data", data.as_str()), ("source_url", "")])
                    .header("Cookie", cookie_header.clone())
                    .header("X-CSRFToken", csrf_header.clone())
                    .send()
                    .await;

                match response {
                    Ok(response) => {
                        let status = response.status();
                        if status.is_success() {
                            match read_api_body(response, resource, MAX_API_RESPONSE_BYTES).await {
                                Ok(body) => {
                                    return serde_json::from_slice(&body)
                                        .map_err(|source| request_error(resource, source));
                                }
                                Err(error)
                                    if attempt + 1 < MAX_REQUEST_ATTEMPTS
                                        && is_retryable_body_error(&error) =>
                                {
                                    retry_delay(attempt, None)
                                }
                                Err(error) => return Err(error),
                            }
                        } else if attempt + 1 < MAX_REQUEST_ATTEMPTS && is_retryable_status(status)
                        {
                            retry_delay(attempt, response.headers().get("retry-after"))
                        } else {
                            return Err(PinterestError::Http { resource, status });
                        }
                    }
                    Err(source)
                        if attempt + 1 < MAX_REQUEST_ATTEMPTS
                            && (source.is_connect() || source.is_timeout()) =>
                    {
                        retry_delay(attempt, None)
                    }
                    Err(source) => return Err(request_error(resource, source)),
                }
            };

            emit_retry(progress, resource, attempt, delay);
            tokio::time::sleep(delay).await;
        }

        unreachable!("the request loop always returns on its final attempt")
    }
}

async fn read_api_body(
    response: reqwest::Response,
    resource: &'static str,
    max_bytes: u64,
) -> Result<Vec<u8>, PinterestError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes)
    {
        return Err(invalid_response(
            resource,
            format!("response body exceeded the {max_bytes}-byte safety limit"),
        ));
    }

    let capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or_default();
    let mut body = Vec::with_capacity(capacity);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(|source| request_error(resource, source))?
    {
        let new_length = u64::try_from(body.len())
            .ok()
            .and_then(|length| {
                u64::try_from(chunk.len())
                    .ok()
                    .and_then(|chunk_length| length.checked_add(chunk_length))
            })
            .ok_or_else(|| {
                invalid_response(resource, "response body size overflowed its safety check")
            })?;
        if new_length > max_bytes {
            return Err(invalid_response(
                resource,
                format!("response body exceeded the {max_bytes}-byte safety limit"),
            ));
        }
        body.extend_from_slice(&chunk);
    }

    Ok(body)
}

fn retry_delay(attempt: usize, retry_after: Option<&HeaderValue>) -> Duration {
    retry_after
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| RETRY_BASE_DELAY.saturating_mul(1_u32 << attempt.min(16)))
        .min(MAX_RETRY_DELAY)
}

fn is_bad_request(error: &PinterestError) -> bool {
    matches!(
        error,
        PinterestError::Http { status, .. } if *status == reqwest::StatusCode::BAD_REQUEST
    )
}

fn is_retryable_body_error(error: &PinterestError) -> bool {
    let PinterestError::Request { source, .. } = error else {
        return false;
    };

    let mut current: &(dyn StdError + 'static) = source.as_ref();
    loop {
        if current
            .downcast_ref::<reqwest::Error>()
            .is_some_and(|source| source.is_body() || source.is_timeout())
        {
            return true;
        }
        let Some(next) = current.source() else {
            return false;
        };
        current = next;
    }
}

/// Removes the `page_size` option, reporting whether there was one to remove.
fn remove_page_size(options: &mut Value) -> bool {
    options
        .as_object_mut()
        .is_some_and(|options| options.remove("page_size").is_some())
}

pub(crate) fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn emit_retry(progress: &dyn Progress, resource: &'static str, attempt: usize, delay: Duration) {
    progress.step(ProgressStep::PageRetry {
        resource,
        attempt: attempt + 2,
        delay,
    });
}

fn build_request_cookie_header(
    cookies: &[ScopedCookie],
    request_url: &Url,
) -> Result<(String, HeaderValue), PinterestError> {
    build_cookie_header(
        select_applicable_cookies(cookies, request_url)
            .into_iter()
            .map(|cookie| cookie.cookie.clone())
            .collect(),
    )
}

fn select_applicable_cookies<'a>(
    cookies: &'a [ScopedCookie],
    request_url: &Url,
) -> Vec<&'a ScopedCookie> {
    let now = unix_time_now();
    let Some(request_host) = request_host(request_url) else {
        return Vec::new();
    };
    let mut selected: Vec<&ScopedCookie> = Vec::new();

    for cookie in cookies
        .iter()
        .filter(|cookie| cookie_applies_to_url(cookie, request_url, now))
    {
        if let Some(existing) = selected
            .iter_mut()
            .find(|existing| existing.cookie.name == cookie.cookie.name)
        {
            if prefers_cookie(cookie, existing, &request_host) {
                *existing = cookie;
            }
            continue;
        }
        selected.push(cookie);
    }

    selected.sort_by(|left, right| left.cookie.name.cmp(&right.cookie.name));
    selected
}

fn build_cookie_header(
    mut cookies: Vec<BrowserCookie>,
) -> Result<(String, HeaderValue), PinterestError> {
    cookies.retain(|cookie| {
        is_cookie_name(&cookie.name)
            && !cookie
                .value
                .bytes()
                .any(|byte| byte == b';' || byte.is_ascii_control())
    });
    let csrf_token = cookies
        .iter()
        .find(|cookie| cookie.name == "csrftoken")
        .map(|cookie| cookie.value.clone())
        .unwrap_or_else(generate_csrf_token);
    if !cookies.iter().any(|cookie| cookie.name == "csrftoken") {
        cookies.push(BrowserCookie {
            name: "csrftoken".into(),
            value: csrf_token.clone(),
        });
    }

    cookies.sort_by(|left, right| left.name.cmp(&right.name));
    let header = cookies
        .iter()
        .map(|cookie| format!("{}={}", cookie.name, cookie.value))
        .collect::<Vec<_>>()
        .join("; ");
    let mut header = HeaderValue::from_str(&header)
        .map_err(|_| invalid_response("headers", "browser cookies were not header-safe"))?;
    header.set_sensitive(true);
    Ok((csrf_token, header))
}

fn cookie_applies_to_url(cookie: &ScopedCookie, request_url: &Url, now: u64) -> bool {
    let Some(request_host) = request_host(request_url) else {
        return false;
    };
    if cookie.secure && request_url.scheme() != "https" {
        return false;
    }
    if cookie.expires.is_some_and(|expires| expires <= now) {
        return false;
    }
    if !domain_matches_cookie(cookie, &request_host) {
        return false;
    }
    path_matches_cookie(&cookie.path, request_path(request_url))
}

fn prefers_cookie(candidate: &ScopedCookie, current: &ScopedCookie, request_host: &str) -> bool {
    let candidate_host_only = exact_host_only_match(candidate, request_host);
    let current_host_only = exact_host_only_match(current, request_host);
    if candidate_host_only != current_host_only {
        return candidate_host_only;
    }
    if candidate.path.len() != current.path.len() {
        return candidate.path.len() > current.path.len();
    }
    if candidate.normalized_domain.len() != current.normalized_domain.len() {
        return candidate.normalized_domain.len() > current.normalized_domain.len();
    }
    candidate.source_order < current.source_order
}

fn exact_host_only_match(cookie: &ScopedCookie, request_host: &str) -> bool {
    cookie.host_only && cookie.normalized_domain == request_host
}

fn domain_matches_cookie(cookie: &ScopedCookie, request_host: &str) -> bool {
    if cookie.host_only {
        return cookie.normalized_domain == request_host;
    }

    request_host == cookie.normalized_domain
        || request_host
            .strip_suffix(&cookie.normalized_domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

fn path_matches_cookie(cookie_path: &str, request_path: &str) -> bool {
    request_path == cookie_path
        || (cookie_path.ends_with('/') && request_path.starts_with(cookie_path))
        || request_path
            .strip_prefix(cookie_path)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn request_host(request_url: &Url) -> Option<String> {
    request_url.host_str().map(str::to_ascii_lowercase)
}

fn request_path(request_url: &Url) -> &str {
    let path = request_url.path();
    if path.is_empty() { "/" } else { path }
}

fn unix_time_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn is_cookie_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn validate_cookie_transport(
    root: &Url,
    api_root: &Url,
    authenticated: bool,
) -> Result<(), PinterestError> {
    if !authenticated {
        return Ok(());
    }

    if root.scheme() != "https" || api_root.scheme() != "https" {
        return Err(PinterestError::InsecureCookieTransport);
    }

    if root.origin() != api_root.origin() {
        return Err(PinterestError::CrossOriginCookieTransport);
    }

    Ok(())
}

fn reject_url_userinfo(url: &Url) -> Result<(), PinterestError> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(PinterestError::InvalidTarget(
            "target URLs must not contain embedded credentials".into(),
        ));
    }
    Ok(())
}

fn build_headers(root: &Url) -> Result<HeaderMap, PinterestError> {
    let mut headers = HeaderMap::new();
    let host = root
        .host_str()
        .ok_or_else(|| invalid_response("headers", "Pinterest root URL had no host"))?;
    let values = [
        ("Accept", "application/json, text/javascript, */*, q=0.01"),
        ("X-Requested-With", "XMLHttpRequest"),
        ("X-APP-VERSION", "a89153f"),
        ("X-Pinterest-AppState", "active"),
        ("X-Pinterest-PWS-Handler", "www/[username].js"),
        ("Alt-Used", host),
        ("Sec-Fetch-Dest", "empty"),
        ("Sec-Fetch-Mode", "cors"),
        ("Sec-Fetch-Site", "same-origin"),
    ];

    for (name, value) in values {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| invalid_response("headers", "invalid header name"))?;
        let value = HeaderValue::from_str(value)
            .map_err(|_| invalid_response("headers", "invalid header value"))?;
        headers.insert(name, value);
    }

    Ok(headers)
}

fn generate_csrf_token() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::rng();
    (0..32)
        .map(|_| {
            let index = rng.random_range(0..ALPHABET.len());
            ALPHABET[index] as char
        })
        .collect()
}

fn response_results(
    mut response: Value,
    resource: &'static str,
) -> Result<Vec<Value>, PinterestError> {
    let data = response
        .pointer_mut("/resource_response/data")
        .map(Value::take)
        .ok_or_else(|| invalid_response(resource, "resource_response.data is missing"))?;
    match data {
        Value::Array(results) => Ok(results),
        Value::Object(mut data) => match data.get_mut("results").map(Value::take) {
            Some(Value::Array(results)) => Ok(results),
            _ => Err(invalid_response(
                resource,
                "resource_response.data was not a result list",
            )),
        },
        _ => Err(invalid_response(
            resource,
            "resource_response.data was not a result list",
        )),
    }
}

fn response_bookmark(
    response: &Value,
    resource: &'static str,
) -> Result<Option<String>, PinterestError> {
    let bookmarks = response
        .pointer("/resource/options/bookmarks")
        .ok_or_else(|| invalid_response(resource, "pagination bookmark metadata is missing"))?;
    match bookmarks {
        Value::String(bookmark) => Ok(Some(bookmark.clone())),
        Value::Array(bookmarks) => match bookmarks.first() {
            None => Ok(None),
            Some(bookmark) => bookmark
                .as_str()
                .map(|bookmark| Some(bookmark.to_owned()))
                .ok_or_else(|| {
                    invalid_response(
                        resource,
                        "pagination bookmark metadata contained a non-string bookmark",
                    )
                }),
        },
        Value::Null => Ok(None),
        _ => Err(invalid_response(
            resource,
            "pagination bookmark metadata was not a bookmark, list, or null",
        )),
    }
}

fn request_error(
    resource: &'static str,
    source: impl StdError + Send + Sync + 'static,
) -> PinterestError {
    PinterestError::Request {
        resource,
        source: Box::new(source),
    }
}

fn invalid_response(resource: &'static str, message: impl Into<String>) -> PinterestError {
    PinterestError::InvalidResponse {
        resource,
        message: message.into(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::NoProgress;

    #[test]
    fn generated_csrf_token_is_header_safe() {
        let token = generate_csrf_token();
        assert_eq!(token.len(), 32);
        assert!(token.bytes().all(|byte| byte.is_ascii_alphanumeric()));
        assert!(HeaderValue::from_str(&token).is_ok());
    }

    #[allow(clippy::too_many_arguments)]
    fn scoped_cookie(
        name: &str,
        value: &str,
        domain: &str,
        host_only: bool,
        path: &str,
        secure: bool,
        expires: Option<u64>,
        source_order: usize,
    ) -> ScopedCookie {
        ScopedCookie {
            cookie: BrowserCookie {
                name: name.into(),
                value: value.into(),
            },
            normalized_domain: domain.into(),
            host_only,
            path: path.into(),
            secure,
            expires,
            source_order,
        }
    }

    fn request(url: &str) -> Url {
        Url::parse(url).unwrap()
    }

    #[test]
    fn imported_cookies_supply_csrf_and_are_marked_sensitive() {
        let (csrf, header) = build_request_cookie_header(
            &[
                scoped_cookie(
                    "_pinterest_sess",
                    "session-value",
                    "www.pinterest.com",
                    true,
                    "/",
                    true,
                    None,
                    0,
                ),
                scoped_cookie(
                    "csrftoken",
                    "browser-csrf",
                    "www.pinterest.com",
                    true,
                    "/",
                    true,
                    None,
                    1,
                ),
            ],
            &request("https://www.pinterest.com/resource/BoardResource/get/"),
        )
        .unwrap();

        assert_eq!(csrf, "browser-csrf");
        assert!(header.is_sensitive());
        let value = header.to_str().unwrap();
        assert!(value.contains("_pinterest_sess=session-value"));
        assert!(value.contains("csrftoken=browser-csrf"));
    }

    #[test]
    fn unsafe_browser_cookie_values_are_not_sent() {
        let (_, header) = build_request_cookie_header(
            &[scoped_cookie(
                "bad",
                "value\r\ninjected: true",
                "www.pinterest.com",
                true,
                "/",
                true,
                None,
                0,
            )],
            &request("https://www.pinterest.com/resource/BoardResource/get/"),
        )
        .unwrap();

        assert!(!header.to_str().unwrap().contains("injected"));
        assert!(header.to_str().unwrap().contains("csrftoken="));
        assert!(!header.to_str().unwrap().contains("bad="));
    }

    #[test]
    fn host_only_cookies_do_not_cross_pinterest_subdomains() {
        let cookies = [scoped_cookie(
            "_pinterest_sess",
            "host-only",
            "www.pinterest.com",
            true,
            "/",
            true,
            None,
            0,
        )];
        let selected = select_applicable_cookies(
            &cookies,
            &request("https://api.pinterest.com/resource/BoardResource/get/"),
        );

        assert!(selected.is_empty());
    }

    #[test]
    fn domain_cookies_cover_allowed_pinterest_subdomains() {
        let cookies = [scoped_cookie(
            "_pinterest_sess",
            "domain",
            "pinterest.com",
            false,
            "/",
            true,
            None,
            0,
        )];
        let selected = select_applicable_cookies(
            &cookies,
            &request("https://uk.pinterest.com/resource/BoardResource/get/"),
        );

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].normalized_domain, "pinterest.com");
        assert!(!selected[0].host_only);
    }

    #[test]
    fn lookalike_domains_do_not_match_cookie_scope() {
        let cookies = [scoped_cookie(
            "_pinterest_sess",
            "domain",
            "pinterest.com",
            false,
            "/",
            true,
            None,
            0,
        )];
        let selected = select_applicable_cookies(
            &cookies,
            &request("https://notpinterest.com/resource/BoardResource/get/"),
        );

        assert!(selected.is_empty());
    }

    #[test]
    fn path_restricted_cookies_are_excluded_when_request_path_does_not_match() {
        let cookies = [scoped_cookie(
            "_pinterest_sess",
            "path",
            "www.pinterest.com",
            true,
            "/pin/",
            true,
            None,
            0,
        )];
        let selected = select_applicable_cookies(
            &cookies,
            &request("https://www.pinterest.com/resource/BoardResource/get/"),
        );

        assert!(selected.is_empty());
    }

    #[test]
    fn cookie_path_matching_respects_segment_boundaries() {
        let cookies = [scoped_cookie(
            "_pinterest_sess",
            "path",
            "www.pinterest.com",
            true,
            "/resource/api",
            true,
            None,
            0,
        )];

        let matching = select_applicable_cookies(
            &cookies,
            &request("https://www.pinterest.com/resource/api/get/"),
        );
        let near_miss = select_applicable_cookies(
            &cookies,
            &request("https://www.pinterest.com/resource/apis/get/"),
        );

        assert_eq!(matching.len(), 1);
        assert!(near_miss.is_empty());
    }

    #[test]
    fn secure_cookies_require_https_requests() {
        let cookies = [scoped_cookie(
            "_pinterest_sess",
            "secure",
            "www.pinterest.com",
            true,
            "/",
            true,
            None,
            0,
        )];

        let http_selected = select_applicable_cookies(
            &cookies,
            &request("http://www.pinterest.com/resource/BoardResource/get/"),
        );
        let https_selected = select_applicable_cookies(
            &cookies,
            &request("https://www.pinterest.com/resource/BoardResource/get/"),
        );

        assert!(http_selected.is_empty());
        assert_eq!(https_selected.len(), 1);
    }

    #[test]
    fn expired_cookies_are_excluded() {
        let cookies = [scoped_cookie(
            "_pinterest_sess",
            "expired",
            "www.pinterest.com",
            true,
            "/",
            true,
            Some(unix_time_now().saturating_sub(1)),
            0,
        )];
        let selected = select_applicable_cookies(
            &cookies,
            &request("https://www.pinterest.com/resource/BoardResource/get/"),
        );

        assert!(selected.is_empty());
    }

    #[test]
    fn duplicate_cookie_names_follow_host_path_domain_specificity() {
        let cookies = [
            scoped_cookie(
                "sid",
                "domain-root",
                "pinterest.com",
                false,
                "/",
                true,
                None,
                0,
            ),
            scoped_cookie(
                "sid",
                "domain-www",
                "www.pinterest.com",
                false,
                "/",
                true,
                None,
                1,
            ),
            scoped_cookie(
                "sid",
                "host-path",
                "www.pinterest.com",
                true,
                "/resource/",
                true,
                None,
                2,
            ),
        ];
        let selected = select_applicable_cookies(
            &cookies,
            &request("https://www.pinterest.com/resource/BoardResource/get/"),
        );

        assert_eq!(selected.len(), 1);
        assert!(selected[0].host_only);
        assert_eq!(selected[0].path, "/resource/");
        assert_eq!(selected[0].source_order, 2);
    }

    #[test]
    fn duplicate_cookie_names_fall_back_to_stable_source_order() {
        let cookies = [
            scoped_cookie(
                "sid",
                "first",
                "www.pinterest.com",
                true,
                "/",
                true,
                None,
                0,
            ),
            scoped_cookie(
                "sid",
                "second",
                "www.pinterest.com",
                true,
                "/",
                true,
                None,
                1,
            ),
        ];
        let selected = select_applicable_cookies(
            &cookies,
            &request("https://www.pinterest.com/resource/BoardResource/get/"),
        );

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].source_order, 0);
    }

    #[test]
    fn csrf_comes_from_the_same_applicable_cookie_set() {
        let (csrf, header) = build_request_cookie_header(
            &[
                scoped_cookie(
                    "csrftoken",
                    "path-miss",
                    "www.pinterest.com",
                    true,
                    "/pin/",
                    true,
                    None,
                    0,
                ),
                scoped_cookie(
                    "csrftoken",
                    "applicable",
                    "www.pinterest.com",
                    true,
                    "/resource/",
                    true,
                    None,
                    1,
                ),
            ],
            &request("https://www.pinterest.com/resource/BoardResource/get/"),
        )
        .unwrap();

        assert_eq!(csrf, "applicable");
        assert!(header.to_str().unwrap().contains("csrftoken=applicable"));
    }
    #[test]
    fn retries_only_throttling_and_transient_server_errors() {
        assert!(is_retryable_status(reqwest::StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        ));
        assert!(is_retryable_status(
            reqwest::StatusCode::SERVICE_UNAVAILABLE
        ));
        assert!(!is_retryable_status(reqwest::StatusCode::BAD_REQUEST));
        assert!(!is_retryable_status(reqwest::StatusCode::FORBIDDEN));
    }

    #[test]
    fn retry_after_seconds_override_bounded_exponential_backoff() {
        let retry_after = HeaderValue::from_static("7");
        assert_eq!(retry_delay(0, Some(&retry_after)), Duration::from_secs(7));
        assert_eq!(retry_delay(0, None), Duration::from_millis(250));
        assert_eq!(retry_delay(1, None), Duration::from_millis(500));

        let excessive = HeaderValue::from_static("3600");
        assert_eq!(
            retry_delay(0, Some(&excessive)),
            MAX_RETRY_DELAY,
            "a hostile Retry-After header must not stall the CLI indefinitely"
        );
    }
    #[test]
    fn recognizes_terminal_bookmarks() {
        assert_eq!(
            response_bookmark(
                &json!({"resource": {"options": {
                    "bookmarks": ["abc"]
                }}}),
                "Feed"
            )
            .unwrap(),
            Some("abc".into())
        );
        assert_eq!(
            response_bookmark(
                &json!({"resource": {"options": {
                    "bookmarks": null
                }}}),
                "Feed"
            )
            .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn missing_pagination_metadata_is_not_treated_as_end_of_feed() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/resource/FeedResource/get/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "resource_response": { "data": [] },
                "resource": { "options": {} }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = PinterestApi::new(
            Url::parse("https://www.pinterest.com/").unwrap(),
            Url::parse(&server.uri()).unwrap(),
            Vec::new(),
        )
        .unwrap();
        let error = client
            .paginate("Feed", json!({}), &NoProgress)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            PinterestError::InvalidResponse {
                resource: "Feed",
                message
            } if message.contains("bookmark metadata is missing")
        ));
    }

    #[tokio::test]
    async fn oversized_api_response_bodies_are_rejected_before_json_deserialization() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let bytes = stream.read(&mut buffer).await.unwrap();
                if bytes == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..bytes]);
            }

            let response = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: application/json\r\n",
                "Transfer-Encoding: chunked\r\n",
                "Connection: close\r\n\r\n",
                "5\r\nhello\r\n",
                "5\r\nworld\r\n",
                "0\r\n\r\n"
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });

        let response = reqwest::Client::new()
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        let error = read_api_body(response, "Feed", 4).await.unwrap_err();

        assert!(matches!(
            error,
            PinterestError::InvalidResponse {
                resource: "Feed",
                message
            } if message.contains("safety limit")
        ));
        let _ = server.await;
    }

    #[tokio::test]
    async fn retries_transient_api_response_body_failures() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        use tokio::time::timeout;

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut accepted = 0;
            for _ in 0..4 {
                let Ok(Ok((mut stream, _))) =
                    timeout(Duration::from_secs(5), listener.accept()).await
                else {
                    break;
                };
                accepted += 1;
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let bytes = stream.read(&mut buffer).await.unwrap();
                    if bytes == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..bytes]);
                }

                // The declared length is intentionally larger than the body;
                // the peer closing the connection makes this a body transport
                // failure rather than malformed JSON.
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\ntruncated",
                    )
                    .await;
            }
            accepted
        });

        let client = PinterestApi::new(
            Url::parse("https://www.pinterest.com/").unwrap(),
            Url::parse(&format!("http://{address}/")).unwrap(),
            Vec::new(),
        )
        .unwrap();
        let error = client
            .call("Feed", json!({}), &NoProgress)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            PinterestError::Request {
                resource: "Feed",
                ..
            }
        ));
        assert!(!error.to_string().contains("truncated"));
        assert_eq!(server.await.unwrap(), 4);
    }

    #[tokio::test]
    async fn feeds_exceeding_the_result_limit_return_an_invalid_response() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let results = (0..=MAX_FEED_RESULTS)
            .map(|id| json!({ "id": id }))
            .collect::<Vec<_>>();
        Mock::given(method("GET"))
            .and(path("/resource/FeedResource/get/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "resource_response": { "data": results },
                "resource": { "options": { "bookmarks": ["-end-"] } }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = PinterestApi::new(
            Url::parse("https://www.pinterest.com/").unwrap(),
            Url::parse(&server.uri()).unwrap(),
            Vec::new(),
        )
        .unwrap();
        let error = client
            .paginate("Feed", json!({}), &NoProgress)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            PinterestError::InvalidResponse {
                resource: "Feed",
                message
            } if message.contains("retention safety limit")
        ));
    }
}
