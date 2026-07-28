use std::collections::HashSet;
use std::time::Duration;

use percent_encoding::percent_decode_str;
use rand::Rng;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use url::Url;

use crate::auth::BrowserCookie;
use crate::progress::{NoProgress, ProgressEvent, ProgressSink};

const MAX_PAGES: usize = 10_000;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BoardTarget {
    pub root: Url,
    pub username: String,
    pub board_slug: String,
}

impl BoardTarget {
    pub fn parse(input: &str) -> Result<Self, PinterestError> {
        let url = Url::parse(input)
            .map_err(|_| PinterestError::InvalidBoardUrl("not a valid absolute URL".into()))?;

        if url.scheme() != "https" && url.scheme() != "http" {
            return Err(PinterestError::InvalidBoardUrl(
                "the URL must use http or https".into(),
            ));
        }

        let host = url
            .host_str()
            .ok_or_else(|| PinterestError::InvalidBoardUrl("the URL has no host".into()))?
            .to_ascii_lowercase();
        if !is_pinterest_host(&host) {
            return Err(PinterestError::InvalidBoardUrl(
                "the host is not a Pinterest domain".into(),
            ));
        }

        let segments = url
            .path_segments()
            .ok_or_else(|| PinterestError::InvalidBoardUrl("the URL has no path".into()))?
            .filter(|part| !part.is_empty())
            .map(decode_segment)
            .collect::<Result<Vec<_>, _>>()?;

        if segments.len() != 2
            || matches!(
                segments[0].to_ascii_lowercase().as_str(),
                "pin" | "search" | "ideas" | "today"
            )
        {
            return Err(PinterestError::InvalidBoardUrl(
                "expected a board URL in the form https://www.pinterest.com/USER/BOARD/".into(),
            ));
        }

        let mut root = url;
        root.set_path("/");
        root.set_query(None);
        root.set_fragment(None);

        Ok(Self {
            root,
            username: segments[0].clone(),
            board_slug: segments[1].clone(),
        })
    }
}

fn decode_segment(segment: &str) -> Result<String, PinterestError> {
    percent_decode_str(segment)
        .decode_utf8()
        .map(|decoded| decoded.into_owned())
        .map_err(|_| PinterestError::InvalidBoardUrl("the path is not valid UTF-8".into()))
}

fn is_pinterest_host(host: &str) -> bool {
    host.starts_with("pinterest.") || host.contains(".pinterest.")
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct Pin {
    pub id: String,
    pub media_url: String,
    pub metadata_width: Option<u32>,
    pub metadata_height: Option<u32>,
}

impl Pin {
    pub fn pin_url(&self) -> String {
        format!("https://www.pinterest.com/pin/{}/", self.id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct SkippedPin {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin_url: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct BoardPins {
    pub board_name: String,
    pub pins_reported: Option<usize>,
    pub pins_found: usize,
    pub pins: Vec<Pin>,
    pub skipped: Vec<SkippedPin>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Error)]
pub enum PinterestError {
    #[error("invalid Pinterest board URL: {0}")]
    InvalidBoardUrl(String),

    #[error("failed to build the Pinterest HTTP client")]
    Client(#[source] reqwest::Error),

    #[error("Pinterest request for {resource} failed")]
    Request {
        resource: &'static str,
        #[source]
        source: reqwest::Error,
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
pub struct PinterestClient {
    http: reqwest::Client,
    target: BoardTarget,
    api_root: Url,
    cookie_header: HeaderValue,
    authenticated: bool,
}

impl PinterestClient {
    pub fn new(target: BoardTarget) -> Result<Self, PinterestError> {
        Self::with_cookies(target, Vec::new())
    }

    pub fn with_cookies(
        target: BoardTarget,
        cookies: Vec<BrowserCookie>,
    ) -> Result<Self, PinterestError> {
        let api_root = target.root.clone();
        Self::with_api_root_and_cookies(target, api_root, cookies)
    }

    pub fn with_api_root(target: BoardTarget, api_root: Url) -> Result<Self, PinterestError> {
        Self::with_api_root_and_cookies(target, api_root, Vec::new())
    }

    pub fn with_api_root_and_cookies(
        target: BoardTarget,
        api_root: Url,
        cookies: Vec<BrowserCookie>,
    ) -> Result<Self, PinterestError> {
        let (csrf_token, cookie_header, authenticated) = build_cookie_header(cookies)?;
        let headers = build_headers(&target, &csrf_token)?;
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
            target,
            api_root,
            cookie_header,
            authenticated,
        })
    }

    pub fn http_client(&self) -> reqwest::Client {
        self.http.clone()
    }

    pub async fn fetch_board(&self) -> Result<BoardPins, PinterestError> {
        self.fetch_board_with_progress(&NoProgress).await
    }

    pub async fn fetch_board_with_progress(
        &self,
        progress: &dyn ProgressSink,
    ) -> Result<BoardPins, PinterestError> {
        progress.emit(ProgressEvent::FetchingBoard);
        let board_response = self
            .call(
                "Board",
                json!({
                    "slug": self.target.board_slug,
                    "username": self.target.username,
                    "field_set_key": "detailed"
                }),
            )
            .await?;
        let board = response_data(&board_response, "Board")?;
        let board_id = value_string(board.get("id"))
            .ok_or_else(|| invalid_response("Board", "resource_response.data.id is missing"))?;
        let board_name = board
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&self.target.board_slug)
            .to_owned();
        let pins_reported = ["pin_count", "pins_count"]
            .iter()
            .find_map(|field| value_usize(board.get(*field)));
        progress.emit(ProgressEvent::BoardResolved {
            name: board_name.clone(),
        });
        let section_count = board
            .get("section_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);

        let mut raw_pins = self
            .paginate(
                "BoardFeed",
                json!({
                    "board_id": board_id,
                    "field_set_key": "react_grid_pin",
                    "prepend": false,
                    "bookmarks": null
                }),
                progress,
            )
            .await?;
        let mut warnings = Vec::new();

        if section_count > 0 {
            let sections = self
                .paginate("BoardSections", json!({ "board_id": board_id }), progress)
                .await?;
            progress.emit(ProgressEvent::SectionsStarted {
                total: sections.len(),
            });
            let section_total = sections.len();
            for (section_index, section) in sections.into_iter().enumerate() {
                progress.emit(ProgressEvent::SectionStarted {
                    current: section_index + 1,
                    total: section_total,
                });
                let Some(section_id) = value_string(section.get("id")) else {
                    warnings.push("Pinterest returned a board section without an ID".into());
                    continue;
                };
                raw_pins.extend(
                    self.paginate(
                        "BoardSectionPins",
                        json!({
                            "section_id": section_id,
                            "bookmarks": null
                        }),
                        progress,
                    )
                    .await?,
                );
            }
        }

        let mut unique_ids = HashSet::new();
        let mut pins = Vec::new();
        let mut skipped = Vec::new();

        for raw_pin in raw_pins {
            let id = value_string(raw_pin.get("id"));
            if let Some(id) = &id
                && !unique_ids.insert(id.clone())
            {
                continue;
            }

            match parse_pin(&raw_pin) {
                Ok(pin) => pins.push(pin),
                Err(reason) => skipped.push(SkippedPin {
                    pin_url: id
                        .as_ref()
                        .map(|id| format!("https://www.pinterest.com/pin/{id}/")),
                    pin_id: id,
                    reason,
                }),
            }
        }

        let pins_found =
            unique_ids.len() + skipped.iter().filter(|pin| pin.pin_id.is_none()).count();
        if let Some(reported) = pins_reported
            && pins_found < reported
        {
            if self.authenticated {
                warnings.push(format!(
                    "Pinterest reports {reported} pins, but returned {pins_found} through its authenticated web API. Some unavailable or restricted pins may still be hidden."
                ));
            } else {
                warnings.push(format!(
                    "Pinterest reports {reported} pins, but returned only {pins_found} anonymously. Rerun with --cookies-from-browser chrome while signed in to Pinterest."
                ));
            }
        }

        Ok(BoardPins {
            board_name,
            pins_reported,
            pins_found,
            pins,
            skipped,
            warnings,
        })
    }

    async fn paginate(
        &self,
        resource: &'static str,
        mut options: Value,
        progress: &dyn ProgressSink,
    ) -> Result<Vec<Value>, PinterestError> {
        let mut all_results = Vec::new();
        let mut seen_bookmarks = HashSet::new();

        for page_index in 0..MAX_PAGES {
            let response = self.call(resource, options.clone()).await?;
            all_results.extend(response_results(&response, resource)?);
            progress.emit(ProgressEvent::PageFetched {
                resource,
                page: page_index + 1,
                items: all_results.len(),
            });

            let Some(bookmark) = response_bookmark(&response) else {
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

    async fn call(&self, resource: &'static str, options: Value) -> Result<Value, PinterestError> {
        let endpoint = self
            .api_root
            .join(&format!("resource/{resource}Resource/get/"))
            .map_err(|_| invalid_response(resource, "could not construct the endpoint URL"))?;
        let data = serde_json::to_string(&json!({ "options": options }))
            .map_err(|_| invalid_response(resource, "could not serialize request options"))?;

        let response = self
            .http
            .get(endpoint)
            .query(&[("data", data.as_str()), ("source_url", "")])
            .header("Cookie", self.cookie_header.clone())
            .send()
            .await
            .map_err(|source| PinterestError::Request { resource, source })?;

        let status = response.status();
        if !status.is_success() {
            return Err(PinterestError::Http { resource, status });
        }

        response
            .json()
            .await
            .map_err(|source| PinterestError::Request { resource, source })
    }
}

fn build_cookie_header(
    mut cookies: Vec<BrowserCookie>,
) -> Result<(String, HeaderValue, bool), PinterestError> {
    cookies.retain(|cookie| {
        is_cookie_name(&cookie.name)
            && !cookie
                .value
                .bytes()
                .any(|byte| byte == b';' || byte.is_ascii_control())
    });
    let authenticated = !cookies.is_empty();
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
    Ok((csrf_token, header, authenticated))
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

fn build_headers(target: &BoardTarget, csrf_token: &str) -> Result<HeaderMap, PinterestError> {
    let mut headers = HeaderMap::new();
    let host = target
        .root
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
        ("X-CSRFToken", csrf_token),
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

fn response_data<'a>(
    response: &'a Value,
    resource: &'static str,
) -> Result<&'a Value, PinterestError> {
    response
        .pointer("/resource_response/data")
        .ok_or_else(|| invalid_response(resource, "resource_response.data is missing"))
}

fn response_results(
    response: &Value,
    resource: &'static str,
) -> Result<Vec<Value>, PinterestError> {
    let data = response_data(response, resource)?;
    if let Some(results) = data.as_array() {
        return Ok(results.clone());
    }
    if let Some(results) = data.get("results").and_then(Value::as_array) {
        return Ok(results.clone());
    }
    Err(invalid_response(
        resource,
        "resource_response.data was not a result list",
    ))
}

fn response_bookmark(response: &Value) -> Option<String> {
    let bookmarks = response.pointer("/resource/options/bookmarks")?;
    match bookmarks {
        Value::String(bookmark) => Some(bookmark.clone()),
        Value::Array(bookmarks) => bookmarks.first()?.as_str().map(str::to_owned),
        _ => None,
    }
}

fn parse_pin(raw: &Value) -> Result<Pin, String> {
    let id = value_string(raw.get("id")).ok_or_else(|| "pin ID is missing".to_owned())?;

    if raw.get("carousel_data").is_some_and(is_present) {
        return Err("multi-image carousel pin".into());
    }
    if raw.get("videos").is_some_and(is_present)
        || raw.get("is_video").and_then(Value::as_bool) == Some(true)
    {
        return Err("video pin".into());
    }

    let is_story = raw.get("story_pin_data").is_some_and(is_present);
    let image = raw.pointer("/images/orig").ok_or_else(|| {
        if is_story {
            "story pin has no usable static cover".to_owned()
        } else {
            "original image metadata is missing".to_owned()
        }
    })?;
    let media_url = image
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
        .ok_or_else(|| "original image URL is missing or invalid".to_owned())?
        .to_owned();

    Ok(Pin {
        id,
        media_url,
        metadata_width: value_u32(image.get("width")),
        metadata_height: value_u32(image.get("height")),
    })
}

fn is_present(value: &Value) -> bool {
    !value.is_null()
        && value.as_object().is_none_or(|object| !object.is_empty())
        && value.as_array().is_none_or(|array| !array.is_empty())
}

fn value_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn value_u32(value: Option<&Value>) -> Option<u32> {
    value?.as_u64()?.try_into().ok()
}

fn value_usize(value: Option<&Value>) -> Option<usize> {
    match value? {
        Value::Number(value) => value.as_u64()?.try_into().ok(),
        Value::String(value) => value.parse().ok(),
        _ => None,
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

    #[test]
    fn parses_board_urls_and_localized_domains() {
        let target =
            BoardTarget::parse("https://uk.pinterest.com/alice/home%20ideas/?invite=true").unwrap();
        assert_eq!(target.username, "alice");
        assert_eq!(target.board_slug, "home ideas");
        assert_eq!(target.root.as_str(), "https://uk.pinterest.com/");
    }

    #[test]
    fn rejects_non_board_urls() {
        for url in [
            "https://example.com/alice/board/",
            "https://www.pinterest.com/alice/",
            "https://www.pinterest.com/pin/123/",
            "https://www.pinterest.com/search/pins/?q=test",
        ] {
            assert!(BoardTarget::parse(url).is_err(), "{url}");
        }
    }

    #[test]
    fn parses_static_image_and_rejects_other_media() {
        let pin = parse_pin(&json!({
            "id": "123",
            "images": { "orig": {
                "url": "https://i.pinimg.com/originals/a.jpg",
                "width": 1200,
                "height": 800
            }}
        }))
        .unwrap();
        assert_eq!(pin.id, "123");
        assert_eq!(pin.metadata_width, Some(1200));

        assert!(
            parse_pin(&json!({
                "id": "124",
                "videos": {"video_list": {}},
                "images": {"orig": {"url": "https://example.com/poster.jpg"}}
            }))
            .unwrap_err()
            .contains("video")
        );
    }

    #[test]
    fn analyzes_story_pins_when_they_have_a_static_cover() {
        let pin = parse_pin(&json!({
            "id": "125",
            "story_pin_data": {"pages": [{"blocks": []}]},
            "images": {"orig": {
                "url": "https://i.pinimg.com/originals/story-cover.jpg",
                "width": 900,
                "height": 1600
            }}
        }))
        .unwrap();

        assert_eq!(pin.id, "125");
        assert_eq!(
            pin.media_url,
            "https://i.pinimg.com/originals/story-cover.jpg"
        );

        let error = parse_pin(&json!({
            "id": "126",
            "story_pin_data": {"pages": [{"blocks": []}]}
        }))
        .unwrap_err();
        assert_eq!(error, "story pin has no usable static cover");
    }

    #[test]
    fn recognizes_terminal_bookmarks() {
        assert_eq!(
            response_bookmark(&json!({"resource": {"options": {
                "bookmarks": ["abc"]
            }}})),
            Some("abc".into())
        );
        assert_eq!(
            response_bookmark(&json!({"resource": {"options": {
                "bookmarks": null
            }}})),
            None
        );
    }

    #[test]
    fn generated_csrf_token_is_header_safe() {
        let token = generate_csrf_token();
        assert_eq!(token.len(), 32);
        assert!(token.bytes().all(|byte| byte.is_ascii_alphanumeric()));
        assert!(HeaderValue::from_str(&token).is_ok());
    }

    #[test]
    fn imported_cookies_supply_csrf_and_are_marked_sensitive() {
        let (csrf, header, authenticated) = build_cookie_header(vec![
            BrowserCookie {
                name: "_pinterest_sess".into(),
                value: "session-value".into(),
            },
            BrowserCookie {
                name: "csrftoken".into(),
                value: "browser-csrf".into(),
            },
        ])
        .unwrap();

        assert_eq!(csrf, "browser-csrf");
        assert!(authenticated);
        assert!(header.is_sensitive());
        let value = header.to_str().unwrap();
        assert!(value.contains("_pinterest_sess=session-value"));
        assert!(value.contains("csrftoken=browser-csrf"));
    }

    #[test]
    fn unsafe_browser_cookie_values_are_not_sent() {
        let (_, header, authenticated) = build_cookie_header(vec![BrowserCookie {
            name: "bad".into(),
            value: "value\r\ninjected: true".into(),
        }])
        .unwrap();

        assert!(!header.to_str().unwrap().contains("injected"));
        assert!(header.to_str().unwrap().contains("csrftoken="));
        assert!(!authenticated);
    }
}
