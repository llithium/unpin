use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::stream::{self, StreamExt};
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::auth::{BrowserCookie, ScopedCookie, scope_explicit_cookies};
use crate::pinterest_api::PinterestApi;
use crate::progress::{Lifecycle, NoProgress, Progress, ProgressStep, SetupTask};

pub use crate::pinterest_api::PinterestError;

const DEFAULT_ROOT: &str = "https://www.pinterest.com/";
/// Profile board listings are independent items, so use the largest page size
/// currently accepted by Pinterest. `paginate` removes this option and falls
/// back to the provider default if the endpoint rejects it.
const BOARD_PAGE_SIZE: usize = 250;
/// Pin feeds default to 25 results per page, and pagination is strictly
/// sequential because each page is addressed by the previous page's bookmark.
/// Asking for larger pages is the only way to shorten that chain.
///
/// The option is undocumented, and Pinterest refuses values it dislikes with
/// HTTP 400 rather than capping them: 250 is served, 300 is not. That is a hard
/// edge to sit on, so `paginate` retries once without the option when a request
/// is refused, giving the round trips back rather than failing the scan.
const FEED_PAGE_SIZE: usize = 250;
/// Sections belong to one board that is already sharing the board-level
/// concurrency budget, so this controls how much work can queue behind the
/// shared request limit.
const SECTION_FETCH_CONCURRENCY: usize = 16;

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
    host == "pinterest.com" || host.ends_with(".pinterest.com")
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

#[derive(Clone)]
pub struct PinterestClient {
    api: PinterestApi,
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
        let scoped = scope_explicit_cookies(&root, cookies);
        Self::with_api_root_and_scoped_cookies(root, api_root, scoped)
    }

    pub(crate) fn with_api_root_and_scoped_cookies(
        root: Url,
        api_root: Url,
        cookies: Vec<ScopedCookie>,
    ) -> Result<Self, PinterestError> {
        Ok(Self {
            api: PinterestApi::new(root, api_root, cookies)?,
        })
    }

    pub fn http_client(&self) -> reqwest::Client {
        self.api.http_client()
    }

    pub(crate) fn is_authenticated(&self) -> bool {
        self.api.is_authenticated()
    }

    pub async fn fetch_board(&self, target: &BoardTarget) -> Result<BoardPins, PinterestError> {
        self.fetch_board_with_progress(target, &NoProgress).await
    }

    pub async fn fetch_board_with_progress(
        &self,
        target: &BoardTarget,
        progress: &dyn Progress,
    ) -> Result<BoardPins, PinterestError> {
        let board = self.resolve_board_source(target, progress).await?;
        self.collect_board_source(&board, progress).await
    }

    /// Collects one board source, owning deduplication across its main feed and
    /// sections so callers never manage provider-fetch state.
    pub(crate) async fn collect_board_source(
        &self,
        board: &BoardRef,
        progress: &dyn Progress,
    ) -> Result<BoardPins, PinterestError> {
        self.fetch_board_pins(board, &mut HashSet::new(), progress)
            .await
    }

    /// Collects pins saved directly to a profile as one source.
    pub(crate) async fn collect_unorganized_source(
        &self,
        target: &UserTarget,
        progress: &dyn Progress,
    ) -> Result<BoardPins, PinterestError> {
        self.fetch_user_pins(target, &mut HashSet::new(), progress)
            .await
    }

    /// Resolves a direct-board Scan source without exposing provider response
    /// mechanics to Scan intake.
    pub(crate) async fn resolve_board_source(
        &self,
        target: &BoardTarget,
        progress: &dyn Progress,
    ) -> Result<BoardRef, PinterestError> {
        progress.step(ProgressStep::Setup {
            task: SetupTask::BoardMetadata { name: None },
            lifecycle: Lifecycle::Started,
        });
        let board_response = self
            .api
            .call(
                "Board",
                json!({
                    "slug": target.board_slug,
                    "username": target.username,
                    "field_set_key": "detailed"
                }),
                progress,
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
        progress.step(ProgressStep::Setup {
            task: SetupTask::BoardMetadata {
                name: Some(name.clone()),
            },
            lifecycle: Lifecycle::Completed,
        });

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

    /// Lists the stable board sources offered by a profile.
    pub(crate) async fn list_profile_sources(
        &self,
        target: &UserTarget,
        progress: &dyn Progress,
    ) -> Result<Vec<BoardRef>, PinterestError> {
        progress.step(ProgressStep::Setup {
            task: SetupTask::UserBoards {
                username: target.username.clone(),
                total: None,
            },
            lifecycle: Lifecycle::Started,
        });
        let raw_boards = self
            .api
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

        progress.step(ProgressStep::Setup {
            task: SetupTask::UserBoards {
                username: target.username.clone(),
                total: Some(boards.len()),
            },
            lifecycle: Lifecycle::Completed,
        });
        Ok(boards)
    }

    /// Fetches pins saved directly to a profile rather than into a board.
    ///
    /// Pinterest presents these as "Unorganized ideas" in the Saved Ideas
    /// view. They are not board records, so they never appear in `Boards`.
    async fn fetch_user_pins(
        &self,
        target: &UserTarget,
        seen_pin_ids: &mut HashSet<String>,
        progress: &dyn Progress,
    ) -> Result<BoardPins, PinterestError> {
        let raw_pins = self
            .api
            .paginate(
                "UserPins",
                json!({
                    "username": target.username,
                    "field_set_key": "grid_item",
                    "page_size": FEED_PAGE_SIZE,
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
    /// `seen_pin_ids` spans the board's main feed and sections so a repeated
    /// pin is counted once.
    async fn fetch_board_pins(
        &self,
        board: &BoardRef,
        seen_pin_ids: &mut HashSet<String>,
        progress: &dyn Progress,
    ) -> Result<BoardPins, PinterestError> {
        let board_feed = self.api.paginate(
            "BoardFeed",
            json!({
                "board_id": board.id,
                "field_set_key": "react_grid_pin",
                "prepend": false,
                "page_size": FEED_PAGE_SIZE,
                "bookmarks": null
            }),
            progress,
        );

        // Section discovery does not depend on any page of the main feed. Keep
        // the whole section pipeline in its own future so section pin pages can
        // start as soon as discovery completes, even while BoardFeed is still
        // walking its bookmark chain.
        let (raw_pins, warnings) = if board.section_count > 0 {
            let section_pins = async {
                let sections = self
                    .api
                    .paginate("BoardSections", json!({ "board_id": board.id }), progress)
                    .await?;
                self.fetch_section_pins(sections, progress).await
            };
            let (raw_pins, section_pins) = tokio::try_join!(board_feed, section_pins)?;
            let (section_pins, warnings) = section_pins;
            let mut raw_pins = raw_pins;
            raw_pins.extend(section_pins);
            (raw_pins, warnings)
        } else {
            (board_feed.await?, Vec::new())
        };

        let mut parsed =
            self.parse_pins(raw_pins, &board.name, board.pins_reported, seen_pin_ids)?;
        parsed.warnings.splice(0..0, warnings);
        Ok(parsed)
    }

    /// Fetches section feeds concurrently and restores their provider order so
    /// the final scan remains deterministic even though network completion is
    /// intentionally unordered.
    async fn fetch_section_pins(
        &self,
        sections: Vec<Value>,
        progress: &dyn Progress,
    ) -> Result<(Vec<Value>, Vec<String>), PinterestError> {
        let mut section_ids = Vec::new();
        let mut warnings = Vec::new();
        for section in sections {
            match value_string(section.get("id")) {
                Some(id) => section_ids.push(id),
                None => warnings.push("Pinterest returned a board section without an ID".into()),
            }
        }
        // Announced after filtering so the total matches the number of
        // sections that will actually report progress.
        progress.step(ProgressStep::SectionCollection {
            current: 0,
            completed: 0,
            total: section_ids.len(),
            lifecycle: Lifecycle::Started,
        });

        // Each section is an independent paginated feed, so they overlap
        // rather than running end to end. Results are reordered back into
        // section order below to keep a scan's pin order deterministic.
        let section_total = section_ids.len();
        let section_completed = Arc::new(AtomicUsize::new(0));
        let section_fetches =
            stream::iter(section_ids.into_iter().enumerate().map(|(index, id)| {
                let section_completed = Arc::clone(&section_completed);
                async move {
                    progress.step(ProgressStep::SectionCollection {
                        current: index + 1,
                        completed: section_completed.load(Ordering::Relaxed),
                        total: section_total,
                        lifecycle: Lifecycle::Started,
                    });
                    let fetched = self
                        .api
                        .paginate(
                            "BoardSectionPins",
                            json!({
                                "section_id": id,
                                "page_size": FEED_PAGE_SIZE,
                                "bookmarks": null
                            }),
                            progress,
                        )
                        .await;
                    let completed = section_completed.fetch_add(1, Ordering::Relaxed) + 1;
                    progress.step(ProgressStep::SectionCollection {
                        current: index + 1,
                        completed,
                        total: section_total,
                        lifecycle: Lifecycle::Completed,
                    });
                    (index, fetched)
                }
            }))
            .buffer_unordered(SECTION_FETCH_CONCURRENCY);
        futures_util::pin_mut!(section_fetches);
        let mut fetched_sections = section_fetches.collect::<Vec<_>>().await;
        fetched_sections.sort_by_key(|(index, _)| *index);

        let mut raw_pins = Vec::new();
        for (_, fetched) in fetched_sections {
            raw_pins.extend(fetched?);
        }
        Ok((raw_pins, warnings))
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

        if let Some(warning) =
            incomplete_scan_warning(self.api.is_authenticated(), pins_reported, pins_found)
        {
            warnings.push(warning);
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
}

pub(crate) fn incomplete_scan_warning(
    authenticated: bool,
    pins_reported: Option<usize>,
    pins_found: usize,
) -> Option<String> {
    let reported = pins_reported.filter(|reported| pins_found < *reported)?;
    if authenticated {
        Some(format!(
            "Pinterest reports {reported} pins, but returned {pins_found} through its authenticated web API. Some unavailable or restricted pins may still be hidden."
        ))
    } else {
        Some(format!(
            "Pinterest reports {reported} pins, but returned only {pins_found} anonymously. Rerun with --cookies-from-browser chrome while signed in to Pinterest."
        ))
    }
}

fn response_data<'a>(
    response: &'a Value,
    resource: &'static str,
) -> Result<&'a Value, PinterestError> {
    response
        .pointer("/resource_response/data")
        .ok_or_else(|| invalid_response(resource, "resource_response.data is missing"))
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
    use crate::pinterest_api::test_support::{
        build_request_cookie_header, generate_csrf_token, is_retryable_status, read_api_body,
        response_bookmark, retry_delay, select_applicable_cookies, unix_time_now,
    };
    use crate::pinterest_api::{API_REQUEST_CONCURRENCY, MAX_FEED_RESULTS, MAX_RETRY_DELAY};
    use reqwest::header::HeaderValue;
    use std::time::Duration;

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
            "https://pinterest.net/alice/board/",
            "https://pinterest.com.attacker.example/alice/board/",
            "https://anything.pinterest.com.attacker.example/alice/board/",
            "https://notpinterest.com/alice/board/",
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
            }}}))
            .unwrap(),
            Some("abc".into())
        );
        assert_eq!(
            response_bookmark(&json!({"resource": {"options": {
                "bookmarks": null
            }}}))
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

        let client = PinterestClient::with_api_root(
            Url::parse("https://www.pinterest.com/").unwrap(),
            Url::parse(&server.uri()).unwrap(),
        )
        .unwrap();
        let error = client
            .api
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

        let client = PinterestClient::with_api_root(
            Url::parse("https://www.pinterest.com/").unwrap(),
            Url::parse(&format!("http://{address}/")).unwrap(),
        )
        .unwrap();
        let response = client
            .http_client()
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

        let client = PinterestClient::with_api_root(
            Url::parse("https://www.pinterest.com/").unwrap(),
            Url::parse(&server.uri()).unwrap(),
        )
        .unwrap();
        let error = client
            .api
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

    fn imported_cookie_fixture() -> Vec<BrowserCookie> {
        vec![
            BrowserCookie {
                name: "_pinterest_sess".into(),
                value: "fixture-session".into(),
            },
            BrowserCookie {
                name: "csrftoken".into(),
                value: "fixture-csrf".into(),
            },
        ]
    }

    #[test]
    fn imported_cookies_allow_same_origin_https_api_roots() {
        let client = PinterestClient::with_api_root_and_cookies(
            Url::parse("https://www.pinterest.com/").unwrap(),
            Url::parse("https://www.pinterest.com/resource/BoardResource/get/").unwrap(),
            imported_cookie_fixture(),
        )
        .unwrap();

        assert!(client.is_authenticated());
    }

    #[test]
    fn imported_cookies_reject_http_target_roots() {
        let result = PinterestClient::with_cookies(
            Url::parse("http://www.pinterest.com/").unwrap(),
            imported_cookie_fixture(),
        );

        assert!(matches!(
            result,
            Err(PinterestError::InsecureCookieTransport)
        ));
    }

    #[test]
    fn imported_cookies_reject_http_api_roots() {
        let result = PinterestClient::with_api_root_and_cookies(
            Url::parse("https://www.pinterest.com/").unwrap(),
            Url::parse("http://api.pinterest.test/").unwrap(),
            imported_cookie_fixture(),
        );

        assert!(matches!(
            result,
            Err(PinterestError::InsecureCookieTransport)
        ));
    }

    #[test]
    fn imported_cookies_reject_cross_origin_https_api_hosts() {
        let result = PinterestClient::with_api_root_and_cookies(
            Url::parse("https://www.pinterest.com/").unwrap(),
            Url::parse("https://api.pinterest.test/").unwrap(),
            imported_cookie_fixture(),
        );

        assert!(matches!(
            result,
            Err(PinterestError::CrossOriginCookieTransport)
        ));
    }

    #[test]
    fn imported_cookies_reject_cross_origin_https_api_ports() {
        let result = PinterestClient::with_api_root_and_cookies(
            Url::parse("https://www.pinterest.com/").unwrap(),
            Url::parse("https://www.pinterest.com:444/").unwrap(),
            imported_cookie_fixture(),
        );

        assert!(matches!(
            result,
            Err(PinterestError::CrossOriginCookieTransport)
        ));
    }

    #[test]
    fn anonymous_http_targets_remain_constructible() {
        let client = PinterestClient::with_api_root(
            Url::parse("http://www.pinterest.com/").unwrap(),
            Url::parse("http://api.pinterest.test/").unwrap(),
        )
        .unwrap();

        assert!(!client.is_authenticated());
    }

    #[tokio::test]
    async fn board_source_collection_owns_source_local_deduplication() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/resource/BoardFeedResource/get/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "resource_response": { "data": [
                    { "id": "shared", "images": { "orig": { "url": "https://example.com/a.png" } } },
                    { "id": "shared", "images": { "orig": { "url": "https://example.com/a.png" } } }
                ] },
                "resource": { "options": { "bookmarks": ["-end-"] } }
            })))
            .mount(&server)
            .await;

        let client = PinterestClient::with_api_root(
            Url::parse("https://www.pinterest.com/").unwrap(),
            Url::parse(&server.uri()).unwrap(),
        )
        .unwrap();
        let board = BoardRef {
            id: "board-1".into(),
            name: "Ideas".into(),
            slug: "ideas".into(),
            url: "https://www.pinterest.com/alice/ideas/".into(),
            pins_reported: Some(2),
            section_count: 0,
            is_secret: false,
        };

        let collected = client
            .collect_board_source(&board, &NoProgress)
            .await
            .unwrap();

        assert_eq!(collected.pins_found, 1);
        assert_eq!(collected.pins.len(), 1);
        assert_eq!(collected.pins[0].id, "shared");
    }

    #[tokio::test]
    async fn section_pipeline_starts_while_the_main_feed_is_still_loading() {
        use std::time::{Duration, Instant};

        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let empty_page = json!({
            "resource_response": { "data": [] },
            "resource": { "options": { "bookmarks": ["-end-"] } }
        });
        Mock::given(method("GET"))
            .and(path("/resource/BoardFeedResource/get/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(empty_page.clone())
                    .set_delay(Duration::from_millis(500)),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/resource/BoardSectionsResource/get/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "resource_response": { "data": [{ "id": "section-1" }] },
                "resource": { "options": { "bookmarks": ["-end-"] } }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/resource/BoardSectionPinsResource/get/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(empty_page)
                    .set_delay(Duration::from_millis(500)),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = PinterestClient::with_api_root(
            Url::parse("https://www.pinterest.com/").unwrap(),
            Url::parse(&server.uri()).unwrap(),
        )
        .unwrap();
        let board = BoardRef {
            id: "board-1".into(),
            name: "Ideas".into(),
            slug: "ideas".into(),
            url: "https://www.pinterest.com/alice/ideas/".into(),
            pins_reported: Some(0),
            section_count: 1,
            is_secret: false,
        };
        let scan = tokio::spawn(async move {
            let progress = NoProgress;
            client
                .fetch_board_pins(&board, &mut HashSet::new(), &progress)
                .await
        });

        let deadline = Instant::now() + Duration::from_millis(250);
        loop {
            let section_request_started = server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|request| request.url.path() == "/resource/BoardSectionPinsResource/get/");
            if section_request_started {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "section pins did not start while the main feed was delayed"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        assert!(scan.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn section_fetches_are_bounded_by_the_section_concurrency_limit() {
        use std::time::{Duration, Instant};

        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _test_guard = crate::test_support::high_concurrency_test_guard().await;
        let server = MockServer::start().await;
        let total = SECTION_FETCH_CONCURRENCY * 2 + 1;
        let sections = (0..total)
            .map(|index| json!({ "id": format!("section-{index}") }))
            .collect::<Vec<_>>();
        let empty_page = json!({
            "resource_response": { "data": [] },
            "resource": { "options": { "bookmarks": ["-end-"] } }
        });
        Mock::given(method("GET"))
            .and(path("/resource/BoardFeedResource/get/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(empty_page.clone()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/resource/BoardSectionsResource/get/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "resource_response": { "data": sections },
                "resource": { "options": { "bookmarks": ["-end-"] } }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/resource/BoardSectionPinsResource/get/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(empty_page)
                    .set_delay(Duration::from_millis(500)),
            )
            .expect(total as u64)
            .mount(&server)
            .await;

        let client = PinterestClient::with_api_root(
            Url::parse("https://www.pinterest.com/").unwrap(),
            Url::parse(&server.uri()).unwrap(),
        )
        .unwrap();
        let board = BoardRef {
            id: "board-1".into(),
            name: "Ideas".into(),
            slug: "ideas".into(),
            url: "https://www.pinterest.com/alice/ideas/".into(),
            pins_reported: Some(0),
            section_count: total as u64,
            is_secret: false,
        };
        let scan = tokio::spawn(async move {
            let progress = NoProgress;
            client
                .fetch_board_pins(&board, &mut HashSet::new(), &progress)
                .await
        });

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let received = server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|request| request.url.path() == "/resource/BoardSectionPinsResource/get/")
                .count();
            if received >= SECTION_FETCH_CONCURRENCY {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the first section-fetch wave did not start"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let received = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path() == "/resource/BoardSectionPinsResource/get/")
            .count();
        assert_eq!(
            received, SECTION_FETCH_CONCURRENCY,
            "a section fetch beyond the configured limit started before a response completed"
        );

        assert!(scan.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn api_requests_share_the_global_concurrency_limit() {
        use std::time::{Duration, Instant};

        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _test_guard = crate::test_support::high_concurrency_test_guard().await;
        let server = MockServer::start().await;
        let total = API_REQUEST_CONCURRENCY * 2 + 1;
        Mock::given(method("GET"))
            .and(path("/resource/SlowResource/get/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "ok": true }))
                    .set_delay(Duration::from_millis(500)),
            )
            .expect(total as u64)
            .mount(&server)
            .await;

        let client = PinterestClient::with_api_root(
            Url::parse("https://www.pinterest.com/").unwrap(),
            Url::parse(&server.uri()).unwrap(),
        )
        .unwrap();
        let tasks = (0..total)
            .map(|index| {
                let client = client.clone();
                tokio::spawn(async move {
                    let progress = NoProgress;
                    client
                        .api
                        .call("Slow", json!({ "index": index }), &progress)
                        .await
                })
            })
            .collect::<Vec<_>>();

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let received = server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|request| request.url.path() == "/resource/SlowResource/get/")
                .count();
            if received >= API_REQUEST_CONCURRENCY {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the first API request wave did not start"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let received = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path() == "/resource/SlowResource/get/")
            .count();
        assert_eq!(
            received, API_REQUEST_CONCURRENCY,
            "an API request beyond the shared limit started before a response completed"
        );

        for task in tasks {
            assert!(task.await.unwrap().is_ok());
        }
    }

    #[tokio::test]
    async fn api_requests_share_the_global_concurrency_limit_through_body_consumption() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        use tokio::sync::Notify;
        use tokio::time::timeout;

        let _test_guard = crate::test_support::high_concurrency_test_guard().await;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let headers_ready = Arc::new(Notify::new());
        let started = Arc::new(AtomicUsize::new(0));
        let (body_release, _) = tokio::sync::watch::channel(false);
        let server = tokio::spawn({
            let body_release = body_release.clone();
            let headers_ready = Arc::clone(&headers_ready);
            let started = Arc::clone(&started);
            async move {
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let headers_ready = Arc::clone(&headers_ready);
                    let started = Arc::clone(&started);
                    let mut body_released = body_release.subscribe();
                    tokio::spawn(async move {
                        let mut request = Vec::new();
                        let mut buffer = [0_u8; 1024];
                        loop {
                            let bytes = match stream.read(&mut buffer).await {
                                Ok(bytes) => bytes,
                                Err(_) => return,
                            };
                            if bytes == 0 {
                                return;
                            }
                            request.extend_from_slice(&buffer[..bytes]);
                            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                break;
                            }
                        }

                        const BODY: &[u8] = br#"{"ok":true}"#;
                        let response_headers = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            BODY.len()
                        );
                        if stream.write_all(response_headers.as_bytes()).await.is_err() {
                            return;
                        }

                        let request_count = started.fetch_add(1, Ordering::SeqCst) + 1;
                        if request_count == API_REQUEST_CONCURRENCY {
                            headers_ready.notify_one();
                        }

                        loop {
                            if *body_released.borrow() {
                                break;
                            }
                            if body_released.changed().await.is_err() {
                                return;
                            }
                        }
                        let _ = stream.write_all(BODY).await;
                    });
                }
            }
        });

        let client = PinterestClient::with_api_root(
            Url::parse("https://www.pinterest.com/").unwrap(),
            Url::parse(&format!("http://{address}/")).unwrap(),
        )
        .unwrap();
        let tasks = (0..=API_REQUEST_CONCURRENCY)
            .map(|index| {
                let client = client.clone();
                tokio::spawn(async move {
                    let progress = NoProgress;
                    client
                        .api
                        .call("Slow", json!({ "index": index }), &progress)
                        .await
                })
            })
            .collect::<Vec<_>>();

        let headers_ready_before_release =
            timeout(Duration::from_secs(3), headers_ready.notified())
                .await
                .is_ok();
        let started_before_release = started.load(Ordering::SeqCst);
        let permits_before_release = client.api.available_permits();

        let _ = body_release.send(true);
        for task in tasks {
            assert!(task.await.unwrap().is_ok());
        }
        server.abort();
        let _ = server.await;

        assert!(
            headers_ready_before_release,
            "the first API response-header wave did not start"
        );
        assert_eq!(
            started_before_release, API_REQUEST_CONCURRENCY,
            "an API request beyond the shared limit started before a response body was released"
        );
        assert_eq!(
            permits_before_release, 0,
            "a permit was released before a response body was consumed"
        );
    }

    /// Fixed-workload throughput benchmark for tuning the shared request
    /// limit. Run with:
    ///
    /// `cargo test --release benchmark_api_request_throughput -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "manual network-concurrency benchmark"]
    async fn benchmark_api_request_throughput() {
        use std::time::{Duration, Instant};

        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        const REQUESTS: usize = 256;
        const RESPONSE_DELAY: Duration = Duration::from_millis(50);

        let _test_guard = crate::test_support::high_concurrency_test_guard().await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/resource/BenchmarkResource/get/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "ok": true }))
                    .set_delay(RESPONSE_DELAY),
            )
            .expect(REQUESTS as u64)
            .mount(&server)
            .await;

        let client = PinterestClient::with_api_root(
            Url::parse("https://www.pinterest.com/").unwrap(),
            Url::parse(&server.uri()).unwrap(),
        )
        .unwrap();
        let started = Instant::now();
        let results = stream::iter(0..REQUESTS)
            .map(|index| {
                let client = client.clone();
                async move {
                    let progress = NoProgress;
                    client
                        .api
                        .call("Benchmark", json!({ "index": index }), &progress)
                        .await
                }
            })
            .buffer_unordered(REQUESTS)
            .collect::<Vec<_>>()
            .await;
        let elapsed = started.elapsed();

        assert!(results.into_iter().all(|result| result.is_ok()));
        eprintln!(
            "{REQUESTS} requests at concurrency {API_REQUEST_CONCURRENCY}: {:.1} req/s ({elapsed:?})",
            REQUESTS as f64 / elapsed.as_secs_f64()
        );
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
}
