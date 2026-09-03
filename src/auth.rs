use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;
use url::Url;

use crate::cli::CookieBrowser;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BrowserCookie {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct ScopedCookie {
    pub(crate) cookie: BrowserCookie,
    pub(crate) normalized_domain: String,
    pub(crate) host_only: bool,
    pub(crate) path: String,
    pub(crate) secure: bool,
    pub(crate) expires: Option<u64>,
    pub(crate) source_order: usize,
}

impl ScopedCookie {
    fn new(
        cookie: BrowserCookie,
        domain: &str,
        host_only: bool,
        path: &str,
        secure: bool,
        expires: Option<u64>,
        source_order: usize,
    ) -> Self {
        Self {
            cookie,
            normalized_domain: normalize_domain(domain),
            host_only,
            path: normalize_path(path),
            secure,
            expires,
            source_order,
        }
    }
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error(
        "could not read Pinterest cookies from {browser}; make sure the browser is installed and has a signed-in Pinterest profile"
    )]
    Read { browser: CookieBrowser },

    #[error(
        "no usable Pinterest cookies were found in {browser}; sign in to Pinterest there, then try again"
    )]
    NoCookies { browser: CookieBrowser },

    #[error("could not read Pinterest cookies from {path}: {source}")]
    CookieFileRead {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("no usable Pinterest cookies were found in {path}")]
    NoCookiesInFile { path: PathBuf },

    #[error("invalid Netscape cookie file {path} at line {line}: expected 7 tab-separated fields")]
    InvalidCookieFile { path: PathBuf, line: usize },
}

/// Loads Pinterest cookies while retaining their original request scope.
pub(crate) fn load_pinterest_scoped_cookies_file(
    path: &Path,
) -> Result<Vec<ScopedCookie>, AuthError> {
    let content = std::fs::read_to_string(path).map_err(|source| AuthError::CookieFileRead {
        path: path.to_owned(),
        source,
    })?;
    let now = unix_time_now();
    let mut cookies = Vec::new();

    for (index, raw_line) in content.lines().enumerate() {
        let line = raw_line.strip_prefix("#HttpOnly_").unwrap_or(raw_line);
        if line.trim().is_empty() || (line.starts_with('#') && line == raw_line) {
            continue;
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 7 {
            return Err(AuthError::InvalidCookieFile {
                path: path.to_owned(),
                line: index + 1,
            });
        }
        let domain = fields[0];
        let allow_subdomains = fields[1].eq_ignore_ascii_case("TRUE");
        let path = fields[2];
        let secure = fields[3].eq_ignore_ascii_case("TRUE");
        let expires = fields[4].parse::<u64>().unwrap_or(0);
        let name = fields[5];
        let value = fields[6];
        if !domain_matches_pinterest(domain)
            || name.is_empty()
            || value.is_empty()
            || (expires != 0 && expires <= now)
        {
            continue;
        }
        cookies.push(ScopedCookie::new(
            BrowserCookie {
                name: name.to_owned(),
                value: value.to_owned(),
            },
            domain,
            !allow_subdomains,
            path,
            secure,
            (expires != 0).then_some(expires),
            index,
        ));
    }

    if cookies.is_empty() {
        return Err(AuthError::NoCookiesInFile {
            path: path.to_owned(),
        });
    }
    Ok(cookies)
}

pub(crate) async fn load_pinterest_scoped_cookies(
    browser: CookieBrowser,
) -> Result<Vec<ScopedCookie>, AuthError> {
    tokio::task::spawn_blocking(move || load_pinterest_scoped_cookies_blocking(browser))
        .await
        .map_err(|_| AuthError::Read { browser })?
}

fn load_pinterest_scoped_cookies_blocking(
    browser: CookieBrowser,
) -> Result<Vec<ScopedCookie>, AuthError> {
    let domains = Some(vec!["pinterest.com".to_owned()]);
    let cookies = match browser {
        CookieBrowser::Chrome => load_chrome_cookies(domains),
        CookieBrowser::Chromium => rookie::chromium(domains),
        CookieBrowser::Brave => rookie::brave(domains),
        CookieBrowser::Edge => rookie::edge(domains),
        CookieBrowser::Firefox => rookie::firefox(domains),
        CookieBrowser::Arc => rookie::arc(domains),
        CookieBrowser::Vivaldi => rookie::vivaldi(domains),
    }
    .map_err(|_| AuthError::Read { browser })?;

    let now = unix_time_now();
    let mut scoped = Vec::new();

    for (source_order, cookie) in cookies.into_iter().enumerate() {
        if cookie.name.is_empty()
            || cookie.value.is_empty()
            || cookie.expires.is_some_and(|expires| expires <= now)
            || !domain_matches_pinterest(&cookie.domain)
        {
            continue;
        }

        // `rookie` does not surface the browser's host-only bit directly, so
        // use the conventional leading-dot representation when importing it.
        scoped.push(ScopedCookie::new(
            BrowserCookie {
                name: cookie.name,
                value: cookie.value,
            },
            &cookie.domain,
            !cookie.domain.starts_with('.'),
            &cookie.path,
            cookie.secure,
            cookie.expires.filter(|expires| *expires != 0),
            source_order,
        ));
    }

    if scoped.is_empty() {
        return Err(AuthError::NoCookies { browser });
    }
    Ok(scoped)
}

pub(crate) fn scope_explicit_cookies(root: &Url, cookies: Vec<BrowserCookie>) -> Vec<ScopedCookie> {
    let domain = root.host_str().unwrap_or_default().to_owned();
    cookies
        .into_iter()
        .enumerate()
        .map(|(source_order, cookie)| {
            ScopedCookie::new(cookie, &domain, true, "/", true, None, source_order)
        })
        .collect()
}

fn load_chrome_cookies(domains: Option<Vec<String>>) -> rookie::Result<Vec<rookie::enums::Cookie>> {
    #[cfg(unix)]
    if let Some(cookies) = load_chrome_profile_cookies(domains.clone()) {
        return Ok(cookies);
    }

    rookie::chrome(domains)
}

#[cfg(unix)]
fn select_chrome_profile_cookies<I>(candidates: I, now: u64) -> Option<Vec<rookie::enums::Cookie>>
where
    I: IntoIterator<Item = Vec<rookie::enums::Cookie>>,
{
    let mut first_non_empty = None;

    for cookies in candidates {
        if cookies.is_empty() {
            continue;
        }
        if cookies
            .iter()
            .any(|cookie| is_live_pinterest_session_cookie(cookie, now))
        {
            return Some(cookies);
        }
        if first_non_empty.is_none() {
            first_non_empty = Some(cookies);
        }
    }

    first_non_empty
}

#[cfg(unix)]
fn is_live_pinterest_session_cookie(cookie: &rookie::enums::Cookie, now: u64) -> bool {
    cookie.name == "_pinterest_sess"
        && !cookie.value.is_empty()
        && domain_matches_pinterest(&cookie.domain)
        && cookie.expires.is_none_or(|expires| expires > now)
}

#[cfg(unix)]
fn load_chrome_profile_cookies(domains: Option<Vec<String>>) -> Option<Vec<rookie::enums::Cookie>> {
    load_chrome_profiles(chrome_profile_bases(), unix_time_now(), |database| {
        rookie::any_browser(database.to_str()?, domains.clone(), None).ok()
    })
}

#[cfg(unix)]
fn load_chrome_profiles(
    bases: impl IntoIterator<Item = PathBuf>,
    now: u64,
    mut load_database: impl FnMut(&Path) -> Option<Vec<rookie::enums::Cookie>>,
) -> Option<Vec<rookie::enums::Cookie>> {
    // Keep reads lazy: a signed-in preferred profile ends the search before
    // opening cookie databases in other profiles or Chrome channels.
    let candidates = bases
        .into_iter()
        .flat_map(|base| ordered_profile_directories(&base))
        .flat_map(|profile| [profile.join("Network/Cookies"), profile.join("Cookies")])
        .filter(|database| database.is_file())
        .filter_map(|database| load_database(&database));
    select_chrome_profile_cookies(candidates, now)
}

#[cfg(target_os = "macos")]
fn chrome_profile_bases() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    ["Chrome", "Chrome-beta", "Chrome-dev", "Chrome-nightly"]
        .into_iter()
        .map(|channel| {
            PathBuf::from(&home)
                .join("Library/Application Support/Google")
                .join(channel)
        })
        .collect()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn chrome_profile_bases() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    [
        "google-chrome",
        "google-chrome-beta",
        "google-chrome-unstable",
    ]
    .into_iter()
    .map(|channel| PathBuf::from(&home).join(".config").join(channel))
    .collect()
}

#[cfg(unix)]
fn ordered_profile_directories(base: &Path) -> Vec<PathBuf> {
    let mut profiles = std::fs::read_dir(base)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name == "Default" || name.starts_with("Profile ")
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    profiles.sort();

    let last_used = std::fs::read_to_string(base.join("Local State"))
        .ok()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
        .and_then(|state| {
            state
                .pointer("/profile/last_used")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    if let Some(last_used) = last_used
        && let Some(index) = profiles.iter().position(|profile| {
            profile.file_name().and_then(|name| name.to_str()) == Some(last_used.as_str())
        })
    {
        profiles.swap(0, index);
    }
    profiles
}

fn domain_matches_pinterest(domain: &str) -> bool {
    let domain = normalize_domain(domain);
    domain == "pinterest.com" || domain.ends_with(".pinterest.com")
}

fn normalize_domain(domain: &str) -> String {
    domain
        .trim()
        .trim_start_matches('.')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

fn normalize_path(path: &str) -> String {
    if path.is_empty() {
        "/".into()
    } else {
        path.to_owned()
    }
}

fn unix_time_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_pinterest_cookie_domains() {
        assert!(domain_matches_pinterest(".pinterest.com"));
        assert!(domain_matches_pinterest("www.pinterest.com"));
        assert!(!domain_matches_pinterest("notpinterest.com"));
        assert!(!domain_matches_pinterest("example.com"));
    }

    #[test]
    fn loads_netscape_cookie_file_and_filters_other_domains() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            file.path(),
            "# Netscape HTTP Cookie File\n\
             .pinterest.com\tTRUE\t/\tFALSE\t0\t_pinterest_sess\tsecret\n\
             example.com\tFALSE\t/\tFALSE\t0\tignored\tvalue\n\
             #HttpOnly_www.pinterest.com\tFALSE\t/\tTRUE\t0\tcsrftoken\ttoken\n",
        )
        .unwrap();

        let cookies = load_pinterest_scoped_cookies_file(file.path()).unwrap();
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0].cookie.name, "_pinterest_sess");
        assert_eq!(cookies[0].normalized_domain, "pinterest.com");
        assert!(!cookies[0].host_only);
        assert_eq!(cookies[0].path, "/");
        assert!(!cookies[0].secure);
        assert_eq!(cookies[0].expires, None);
        assert_eq!(cookies[0].source_order, 1);
        assert_eq!(cookies[1].cookie.name, "csrftoken");
        assert_eq!(cookies[1].normalized_domain, "www.pinterest.com");
        assert!(cookies[1].host_only);
        assert_eq!(cookies[1].path, "/");
        assert!(cookies[1].secure);
        assert_eq!(cookies[1].expires, None);
        assert_eq!(cookies[1].source_order, 3);
    }

    #[test]
    fn explicit_cookies_are_scoped_to_the_exact_root_host() {
        let cookies = scope_explicit_cookies(
            &Url::parse("https://www.pinterest.com/alice/ideas/").unwrap(),
            vec![BrowserCookie {
                name: "csrftoken".into(),
                value: "secret".into(),
            }],
        );

        assert_eq!(
            cookies,
            [ScopedCookie {
                cookie: BrowserCookie {
                    name: "csrftoken".into(),
                    value: "secret".into(),
                },
                normalized_domain: "www.pinterest.com".into(),
                host_only: true,
                path: "/".into(),
                secure: true,
                expires: None,
                source_order: 0,
            }]
        );
    }

    #[cfg(unix)]
    fn raw_cookie(
        name: &str,
        domain: &str,
        value: &str,
        expires: Option<u64>,
    ) -> rookie::enums::Cookie {
        rookie::enums::Cookie {
            domain: domain.into(),
            path: "/".into(),
            secure: true,
            expires,
            name: name.into(),
            value: value.into(),
            http_only: false,
            same_site: 0,
        }
    }

    #[cfg(unix)]
    fn profile_marker(profile_id: &str) -> rookie::enums::Cookie {
        raw_cookie(profile_id, "example.com", "marker", None)
    }

    #[cfg(unix)]
    fn profile_with_session(
        profile_id: &str,
        session_value: &str,
        expires: Option<u64>,
    ) -> Vec<rookie::enums::Cookie> {
        vec![
            profile_marker(profile_id),
            raw_cookie("_pinterest_sess", ".pinterest.com", session_value, expires),
        ]
    }

    #[cfg(unix)]
    fn profile_without_live_session(profile_id: &str) -> Vec<rookie::enums::Cookie> {
        vec![profile_marker(profile_id)]
    }

    #[cfg(unix)]
    fn selected_profile_id(cookies: &[rookie::enums::Cookie]) -> Option<&str> {
        cookies
            .iter()
            .find(|cookie| cookie.domain == "example.com")
            .map(|cookie| cookie.name.as_str())
    }

    #[cfg(unix)]
    #[test]
    fn chrome_profile_selector_skips_expired_session_for_later_live_profile() {
        let now = 1_700_000_000;
        let selected = select_chrome_profile_cookies(
            vec![
                profile_with_session("profile_one", "expired", Some(now - 1)),
                profile_with_session("profile_two", "live", Some(now + 60)),
            ],
            now,
        )
        .unwrap();

        assert_eq!(selected_profile_id(&selected), Some("profile_two"));
    }

    #[cfg(unix)]
    #[test]
    fn chrome_profile_loading_stops_at_first_live_profile() {
        let now = 1_700_000_000;
        let base = tempfile::tempdir().unwrap();
        for profile in ["Default", "Profile 1", "Profile 2"] {
            std::fs::create_dir(base.path().join(profile)).unwrap();
            std::fs::write(base.path().join(profile).join("Cookies"), "").unwrap();
        }
        std::fs::write(
            base.path().join("Local State"),
            r#"{"profile":{"last_used":"Profile 1"}}"#,
        )
        .unwrap();
        let mut loaded = Vec::new();
        let selected = load_chrome_profiles([base.path().to_owned()], now, |database| {
            loaded.push(database.to_owned());
            Some(profile_with_session("preferred", "live", Some(now + 60)))
        })
        .unwrap();

        assert_eq!(selected_profile_id(&selected), Some("preferred"));
        assert_eq!(loaded, [base.path().join("Profile 1/Cookies")]);
    }

    #[cfg(unix)]
    #[test]
    fn chrome_profile_selector_rejects_empty_session_value() {
        let now = 1_700_000_000;
        let selected = select_chrome_profile_cookies(
            vec![
                profile_with_session("profile_one", "", Some(now + 60)),
                profile_with_session("profile_two", "live", Some(now + 120)),
            ],
            now,
        )
        .unwrap();

        assert_eq!(selected_profile_id(&selected), Some("profile_two"));
    }

    #[cfg(unix)]
    #[test]
    fn chrome_profile_selector_rejects_session_expiring_at_now() {
        let now = 1_700_000_000;
        let selected = select_chrome_profile_cookies(
            vec![
                profile_with_session("profile_one", "boundary", Some(now)),
                profile_with_session("profile_two", "live", Some(now + 1)),
            ],
            now,
        )
        .unwrap();

        assert_eq!(selected_profile_id(&selected), Some("profile_two"));
    }

    #[cfg(unix)]
    #[test]
    fn chrome_profile_selector_rejects_zero_expiry_session() {
        let now = 1_700_000_000;
        let selected = select_chrome_profile_cookies(
            vec![
                profile_with_session("profile_one", "zero", Some(0)),
                profile_with_session("profile_two", "live", Some(now + 1)),
            ],
            now,
        )
        .unwrap();

        assert_eq!(selected_profile_id(&selected), Some("profile_two"));
    }

    #[cfg(unix)]
    #[test]
    fn chrome_profile_selector_accepts_session_without_expiry() {
        let now = 1_700_000_000;
        let selected = select_chrome_profile_cookies(
            vec![
                profile_with_session("profile_one", "session", None),
                profile_with_session("profile_two", "live", Some(now + 1)),
            ],
            now,
        )
        .unwrap();

        assert_eq!(selected_profile_id(&selected), Some("profile_one"));
    }

    #[cfg(unix)]
    #[test]
    fn chrome_profile_selector_falls_back_to_first_non_empty_profile_without_live_session() {
        let now = 1_700_000_000;
        let selected = select_chrome_profile_cookies(
            vec![
                profile_without_live_session("profile_one"),
                profile_with_session("profile_two", "", Some(now + 60)),
            ],
            now,
        )
        .unwrap();

        assert_eq!(selected_profile_id(&selected), Some("profile_one"));
    }

    #[cfg(unix)]
    #[test]
    fn chrome_profile_selector_returns_none_when_all_candidates_are_empty() {
        assert!(
            select_chrome_profile_cookies(vec![Vec::new(), Vec::new()], 1_700_000_000).is_none()
        );
    }
}
