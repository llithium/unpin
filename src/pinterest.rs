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
const DEFAULT_ROOT: &str = "https://www.pinterest.com/";
const BOARD_PAGE_SIZE: usize = 25;

/// Path segments that never name a Pinterest user.
const RESERVED_SEGMENTS: [&str; 4] = ["pin", "search", "ideas", "today"];

/// Profile tabs that follow a username and still identify a user, not a board.
const PROFILE_TABS: [&str; 3] = ["_saved", "_created", "_shop"];

/// What the user asked `unpin` to inspect.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Target {
    /// A single board, named directly.
    Board(BoardTarget),
    /// A whole profile, whose boards still need to be chosen.
    User(UserTarget),
}

impl Target {
    /// Accepts a board URL, `username/board`, a profile URL, or a username.
    pub fn parse(input: &str) -> Result<Self, PinterestError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(PinterestError::InvalidTarget("the target is empty".into()));
        }

        if input.contains(':') {
            return Self::parse_url(input);
        }

        if input.contains('/') {
            let segments = input
                .split('/')
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>();
            if segments.len() == 2 && is_username(segments[0]) && !segments[1].trim().is_empty() {
                return Ok(Self::Board(BoardTarget {
                    root: Url::parse(DEFAULT_ROOT).expect("the default root is a valid URL"),
                    username: segments[0].to_owned(),
                    board_slug: segments[1].to_owned(),
                }));
            }
            return Err(PinterestError::InvalidTarget(INVALID_TARGET_HELP.into()));
        }

        let username = input.strip_prefix('@').unwrap_or(input);
        if !is_username(username) {
            return Err(PinterestError::InvalidTarget(format!(
                "{input:?} is neither a Pinterest URL nor a valid username"
            )));
        }

        Ok(Self::User(UserTarget {
            root: Url::parse(DEFAULT_ROOT).expect("the default root is a valid URL"),
            username: username.to_owned(),
        }))
    }

    fn parse_url(input: &str) -> Result<Self, PinterestError> {
        let url = Url::parse(input)
            .map_err(|_| PinterestError::InvalidTarget("not a valid absolute URL".into()))?;

        if url.scheme() != "https" && url.scheme() != "http" {
            return Err(PinterestError::InvalidTarget(
                "the URL must use http or https".into(),
            ));
        }

        let host = url
            .host_str()
            .ok_or_else(|| PinterestError::InvalidTarget("the URL has no host".into()))?
            .to_ascii_lowercase();
        if !is_pinterest_host(&host) {
            return Err(PinterestError::InvalidTarget(
                "the host is not a Pinterest domain".into(),
            ));
        }

        let segments = url
            .path_segments()
            .ok_or_else(|| PinterestError::InvalidTarget("the URL has no path".into()))?
            .filter(|part| !part.is_empty())
            .map(decode_segment)
            .collect::<Result<Vec<_>, _>>()?;

        let reserved = segments
            .first()
            .is_some_and(|first| RESERVED_SEGMENTS.contains(&first.to_ascii_lowercase().as_str()));
        if segments.is_empty() || reserved {
            return Err(PinterestError::InvalidTarget(INVALID_TARGET_HELP.into()));
        }

        let mut root = url;
        root.set_path("/");
        root.set_query(None);
        root.set_fragment(None);

        // `/USER/`, `/USER/_saved/`, and `/USER/_created/` all name a profile.
        let is_profile = segments.len() == 1
            || (segments.len() == 2
                && PROFILE_TABS.contains(&segments[1].to_ascii_lowercase().as_str()));
        if is_profile {
            return Ok(Self::User(UserTarget {
                root,
                username: segments[0].clone(),
            }));
        }

        if segments.len() != 2 {
            return Err(PinterestError::InvalidTarget(INVALID_TARGET_HELP.into()));
        }

        Ok(Self::Board(BoardTarget {
            root,
            username: segments[0].clone(),
            board_slug: segments[1].clone(),
        }))
    }

    pub fn root(&self) -> &Url {
        match self {
            Self::Board(target) => &target.root,
            Self::User(target) => &target.root,
        }
    }
}

const INVALID_TARGET_HELP: &str = "expected a board URL \
     (https://www.pinterest.com/USER/BOARD/), a profile URL \
     (https://www.pinterest.com/USER/), USER/BOARD, or a username";

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BoardTarget {
    pub root: Url,
    pub username: String,
    pub board_slug: String,
}

impl BoardTarget {
    pub fn parse(input: &str) -> Result<Self, PinterestError> {
        match Target::parse(input)? {
            Target::Board(target) => Ok(target),
            Target::User(_) => Err(PinterestError::InvalidTarget(INVALID_TARGET_HELP.into())),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct UserTarget {
    pub root: Url,
    pub username: String,
}

/// Builds a board's web URL from its owner and slug.
///
/// Both come from percent-decoded path segments, so they are pushed back through
/// `path_segments_mut`, which re-encodes them. Formatting them into a string
/// instead would emit raw spaces and `?`/`#` into the URL.
fn board_url(root: &Url, username: &str, slug: &str) -> String {
    let mut url = root.clone();
    match url.path_segments_mut() {
        Ok(mut segments) => {
            segments.clear().push(username).push(slug).push("");
        }
        // Only reachable for a cannot-be-a-base root, which `Target::parse`
        // rejects before this point.
        Err(()) => return String::new(),
    }
    url.into()
}

/// Resolves a Pinterest-supplied path against the site root, keeping only
/// ordinary web URLs.
///
/// The result is written into an `href` in the generated report, and joining is
/// not a scheme filter: `javascript:` and `data:` values pass straight through,
/// and an empty path silently resolves to the site root. This mirrors the check
/// [`parse_pin`] already applies to image URLs.
fn absolute_web_url(root: &Url, path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    root.join(path)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .map(String::from)
        .unwrap_or_default()
}

/// Pinterest usernames are ASCII alphanumerics and underscores.
fn is_username(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate.len() <= 30
        && candidate
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn decode_segment(segment: &str) -> Result<String, PinterestError> {
    percent_decode_str(segment)
        .decode_utf8()
        .map(|decoded| decoded.into_owned())
        .map_err(|_| PinterestError::InvalidTarget("the path is not valid UTF-8".into()))
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
    /// Name of the board this pin was found in, for multi-board scans.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub board: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub board: Option<String>,
}

/// A board that is known well enough to fetch its pins, from either a direct
/// board URL or a profile's board listing.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BoardRef {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub url: String,
    pub pins_reported: Option<usize>,
    pub section_count: u64,
    pub is_secret: bool,
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
    #[error("invalid Pinterest target: {0}")]
    InvalidTarget(String),

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
    api_root: Url,
    cookie_header: HeaderValue,
    authenticated: bool,
}

impl PinterestClient {
    pub fn new(root: Url) -> Result<Self, PinterestError> {
        Self::with_cookies(root, Vec::new())
    }

    pub fn with_cookies(root: Url, cookies: Vec<BrowserCookie>) -> Result<Self, PinterestError> {
        let api_root = root.clone();
        Self::with_api_root_and_cookies(root, api_root, cookies)
    }

    pub fn with_api_root(root: Url, api_root: Url) -> Result<Self, PinterestError> {
        Self::with_api_root_and_cookies(root, api_root, Vec::new())
    }

    pub fn with_api_root_and_cookies(
        root: Url,
        api_root: Url,
        cookies: Vec<BrowserCookie>,
    ) -> Result<Self, PinterestError> {
        let (csrf_token, cookie_header, authenticated) = build_cookie_header(cookies)?;
        let headers = build_headers(&root, &csrf_token)?;
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
            cookie_header,
            authenticated,
        })
    }

    pub fn http_client(&self) -> reqwest::Client {
        self.http.clone()
    }

    pub async fn fetch_board(&self, target: &BoardTarget) -> Result<BoardPins, PinterestError> {
        self.fetch_board_with_progress(target, &NoProgress).await
    }

    pub async fn fetch_board_with_progress(
        &self,
        target: &BoardTarget,
        progress: &dyn ProgressSink,
    ) -> Result<BoardPins, PinterestError> {
        let board = self.resolve_board(target, progress).await?;
        self.fetch_board_pins(&board, &mut HashSet::new(), progress)
            .await
    }

    /// Looks up a board named by URL, which is the only way to learn its ID.
    pub async fn resolve_board(
        &self,
        target: &BoardTarget,
        progress: &dyn ProgressSink,
    ) -> Result<BoardRef, PinterestError> {
        progress.emit(ProgressEvent::FetchingBoard);
        let board_response = self
            .call(
                "Board",
                json!({
                    "slug": target.board_slug,
                    "username": target.username,
                    "field_set_key": "detailed"
                }),
            )
            .await?;
        let board = response_data(&board_response, "Board")?;
        let id = value_string(board.get("id"))
            .ok_or_else(|| invalid_response("Board", "resource_response.data.id is missing"))?;
        let name = board
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&target.board_slug)
            .to_owned();
        progress.emit(ProgressEvent::BoardResolved { name: name.clone() });

        Ok(BoardRef {
            id,
            name,
            slug: target.board_slug.clone(),
            url: board_url(&target.root, &target.username, &target.board_slug),
            pins_reported: ["pin_count", "pins_count"]
                .iter()
                .find_map(|field| value_usize(board.get(*field))),
            section_count: board
                .get("section_count")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            is_secret: board.get("privacy").and_then(Value::as_str) == Some("secret"),
        })
    }

    /// Lists the boards on a profile so the caller can choose among them.
    pub async fn fetch_user_boards(
        &self,
        target: &UserTarget,
        progress: &dyn ProgressSink,
    ) -> Result<Vec<BoardRef>, PinterestError> {
        progress.emit(ProgressEvent::FetchingUserBoards {
            username: target.username.clone(),
        });
        let raw_boards = self
            .paginate(
                "Boards",
                json!({
                    "username": target.username,
                    "field_set_key": "profile_grid_item",
                    "sort": "last_pinned_to",
                    "filter_stories": false,
                    "page_size": BOARD_PAGE_SIZE,
                    "include_archived": true,
                    "bookmarks": null
                }),
                progress,
            )
            .await?;

        let mut boards = Vec::new();
        for raw_board in raw_boards {
            // The profile grid mixes in non-board entries that have an ID but
            // no feed behind it; fetching one returns HTTP 404.
            if raw_board.get("type").and_then(Value::as_str) != Some("board") {
                continue;
            }
            let Some(id) = value_string(raw_board.get("id")) else {
                continue;
            };
            // Pinterest returns the path, not a slug; collaborative boards are
            // listed under their owner's username rather than this profile's.
            let url = raw_board
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let slug = url
                .trim_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_owned();
            // Fall back through slug to ID so a board is never unlabeled in
            // the picker; Pinterest occasionally returns neither for a board.
            let name = raw_board
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    if slug.is_empty() {
                        format!("Board {id}")
                    } else {
                        slug.clone()
                    }
                });

            boards.push(BoardRef {
                id,
                name,
                slug,
                url: absolute_web_url(&target.root, url),
                pins_reported: value_usize(raw_board.get("pin_count")),
                section_count: raw_board
                    .get("section_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                is_secret: raw_board.get("privacy").and_then(Value::as_str) == Some("secret"),
            });
        }

        progress.emit(ProgressEvent::UserBoardsResolved {
            total: boards.len(),
        });
        Ok(boards)
    }

    /// Fetches pins saved directly to a profile rather than into a board.
    ///
    /// Pinterest presents these as "Unorganized ideas" in the Saved Ideas
    /// view. They are not board records, so they never appear in `Boards`.
    pub async fn fetch_user_pins(
        &self,
        target: &UserTarget,
        seen_pin_ids: &mut HashSet<String>,
        progress: &dyn ProgressSink,
    ) -> Result<BoardPins, PinterestError> {
        let raw_pins = self
            .paginate(
                "UserPins",
                json!({
                    "username": target.username,
                    "field_set_key": "grid_item",
                    "page_size": BOARD_PAGE_SIZE,
                    "bookmarks": null
                }),
                progress,
            )
            .await?;

        // UserPins is the account-wide saved-pin feed. Pinterest identifies
        // pins shown under "Unorganized ideas" by assigning them to its hidden
        // Quick Saves board, so discard ordinary board pins here.
        let raw_pins = raw_pins
            .into_iter()
            .filter(is_unorganized_pin)
            .collect::<Vec<_>>();
        let pins_reported = Some(raw_pins.len());
        self.parse_pins(raw_pins, "Unorganized ideas", pins_reported, seen_pin_ids)
    }

    /// Fetches every pin in a board, including its sections.
    ///
    /// `seen_pin_ids` is shared across boards so a pin repeated in a board's
    /// main feed and one of its sections is counted once.
    pub async fn fetch_board_pins(
        &self,
        board: &BoardRef,
        seen_pin_ids: &mut HashSet<String>,
        progress: &dyn ProgressSink,
    ) -> Result<BoardPins, PinterestError> {
        let mut raw_pins = self
            .paginate(
                "BoardFeed",
                json!({
                    "board_id": board.id,
                    "field_set_key": "react_grid_pin",
                    "prepend": false,
                    "bookmarks": null
                }),
                progress,
            )
            .await?;
        let mut warnings = Vec::new();

        if board.section_count > 0 {
            let sections = self
                .paginate("BoardSections", json!({ "board_id": board.id }), progress)
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

        let mut parsed =
            self.parse_pins(raw_pins, &board.name, board.pins_reported, seen_pin_ids)?;
        parsed.warnings.splice(0..0, warnings);
        Ok(parsed)
    }

    fn parse_pins(
        &self,
        raw_pins: Vec<Value>,
        source_name: &str,
        pins_reported: Option<usize>,
        seen_pin_ids: &mut HashSet<String>,
    ) -> Result<BoardPins, PinterestError> {
        let mut pins = Vec::new();
        let mut skipped = Vec::new();
        let mut pins_found = 0;
        let mut warnings = Vec::new();

        for raw_pin in raw_pins {
            let id = value_string(raw_pin.get("id"));
            if let Some(id) = &id
                && !seen_pin_ids.insert(id.clone())
            {
                continue;
            }
            pins_found += 1;

            match parse_pin(&raw_pin, source_name) {
                Ok(pin) => pins.push(pin),
                Err(reason) => skipped.push(SkippedPin {
                    pin_url: id
                        .as_ref()
                        .map(|id| format!("https://www.pinterest.com/pin/{id}/")),
                    pin_id: id,
                    reason,
                    board: Some(source_name.to_owned()),
                }),
            }
        }

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
            board_name: source_name.to_owned(),
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

fn build_headers(root: &Url, csrf_token: &str) -> Result<HeaderMap, PinterestError> {
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

fn is_unorganized_pin(raw: &Value) -> bool {
    raw.pointer("/board/layout").and_then(Value::as_str) == Some("quick_saves")
        || raw
            .pointer("/board/url")
            .and_then(Value::as_str)
            .is_some_and(|url| url.trim_end_matches('/').ends_with("/_quick_saves"))
}

fn parse_pin(raw: &Value, board_name: &str) -> Result<Pin, String> {
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
        board: Some(board_name.to_owned()),
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

    fn board(input: &str) -> BoardTarget {
        match Target::parse(input).unwrap() {
            Target::Board(target) => target,
            Target::User(_) => panic!("{input} should parse as a board"),
        }
    }

    fn user(input: &str) -> UserTarget {
        match Target::parse(input).unwrap() {
            Target::User(target) => target,
            Target::Board(_) => panic!("{input} should parse as a user"),
        }
    }

    #[test]
    fn parses_board_urls_and_localized_domains() {
        let target = board("https://uk.pinterest.com/alice/home%20ideas/?invite=true");
        assert_eq!(target.username, "alice");
        assert_eq!(target.board_slug, "home ideas");
        assert_eq!(target.root.as_str(), "https://uk.pinterest.com/");
    }

    #[test]
    fn parses_username_board_shorthand() {
        let target = board("alice/home-ideas");
        assert_eq!(target.username, "alice");
        assert_eq!(target.board_slug, "home-ideas");
        assert_eq!(target.root.as_str(), DEFAULT_ROOT);
    }

    #[test]
    fn parses_profile_urls_and_bare_usernames() {
        assert_eq!(user("https://www.pinterest.com/alice/").username, "alice");
        assert_eq!(
            user("https://uk.pinterest.com/alice/_saved/").username,
            "alice"
        );
        assert_eq!(
            user("https://www.pinterest.com/alice/_created/").username,
            "alice"
        );

        for input in ["alice", "@alice", "  alice  "] {
            let target = user(input);
            assert_eq!(target.username, "alice", "{input}");
            assert_eq!(
                target.root.as_str(),
                "https://www.pinterest.com/",
                "{input}"
            );
        }

        // A localized profile URL keeps its host for later API calls.
        assert_eq!(
            user("https://uk.pinterest.com/alice/").root.as_str(),
            "https://uk.pinterest.com/"
        );
    }

    #[test]
    fn rejects_unusable_targets() {
        for input in [
            "https://example.com/alice/board/",
            "https://www.pinterest.com/pin/123/",
            "https://www.pinterest.com/search/pins/?q=test",
            "https://www.pinterest.com/alice/board/extra/",
            "ftp://www.pinterest.com/alice/board/",
            "",
            "  ",
            "not a username",
            "alice-with-dashes",
            "alice.com",
        ] {
            assert!(Target::parse(input).is_err(), "{input:?}");
        }
    }

    #[test]
    fn board_target_parse_rejects_profile_targets() {
        assert!(BoardTarget::parse("https://www.pinterest.com/alice/").is_err());
        assert!(BoardTarget::parse("alice").is_err());
        assert!(BoardTarget::parse("https://www.pinterest.com/alice/ideas/").is_ok());
    }

    #[test]
    fn board_urls_re_encode_decoded_segments() {
        let root = Url::parse("https://uk.pinterest.com/").unwrap();

        assert_eq!(
            board_url(&root, "alice", "interiors"),
            "https://uk.pinterest.com/alice/interiors/"
        );
        // These come from percent-decoded path segments, so they must go back in
        // encoded rather than being formatted into the string raw.
        assert_eq!(
            board_url(&root, "alice", "home ideas"),
            "https://uk.pinterest.com/alice/home%20ideas/"
        );
        for (slug, encoded) in [("a?b", "a%3Fb"), ("a#b", "a%23b"), ("a/b", "a%2Fb")] {
            assert_eq!(
                board_url(&root, "alice", slug),
                format!("https://uk.pinterest.com/alice/{encoded}/"),
                "{slug}"
            );
        }
    }

    #[test]
    fn listed_board_urls_keep_only_web_schemes() {
        let root = Url::parse("https://www.pinterest.com/").unwrap();

        assert_eq!(
            absolute_web_url(&root, "/alice/interiors/"),
            "https://www.pinterest.com/alice/interiors/"
        );

        // Joining is not a scheme filter, and this value lands in an href in the
        // generated report.
        for hostile in ["javascript:alert(1)", "data:text/html,<b>x", "vbscript:x"] {
            assert_eq!(absolute_web_url(&root, hostile), "", "{hostile}");
        }

        // An empty path resolves to the site root, which would silently link a
        // board to the Pinterest homepage.
        assert_eq!(absolute_web_url(&root, ""), "");
    }

    #[test]
    fn parses_static_image_and_rejects_other_media() {
        let pin = parse_pin(
            &json!({
                "id": "123",
                "images": { "orig": {
                    "url": "https://i.pinimg.com/originals/a.jpg",
                    "width": 1200,
                    "height": 800
                }}
            }),
            "Ideas",
        )
        .unwrap();
        assert_eq!(pin.id, "123");
        assert_eq!(pin.metadata_width, Some(1200));
        assert_eq!(pin.board.as_deref(), Some("Ideas"));

        assert!(
            parse_pin(
                &json!({
                    "id": "124",
                    "videos": {"video_list": {}},
                    "images": {"orig": {"url": "https://example.com/poster.jpg"}}
                }),
                "Ideas",
            )
            .unwrap_err()
            .contains("video")
        );
    }

    #[test]
    fn analyzes_story_pins_when_they_have_a_static_cover() {
        let pin = parse_pin(
            &json!({
                "id": "125",
                "story_pin_data": {"pages": [{"blocks": []}]},
                "images": {"orig": {
                    "url": "https://i.pinimg.com/originals/story-cover.jpg",
                    "width": 900,
                    "height": 1600
                }}
            }),
            "Ideas",
        )
        .unwrap();

        assert_eq!(pin.id, "125");
        assert_eq!(
            pin.media_url,
            "https://i.pinimg.com/originals/story-cover.jpg"
        );

        let error = parse_pin(
            &json!({
                "id": "126",
                "story_pin_data": {"pages": [{"blocks": []}]}
            }),
            "Ideas",
        )
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
