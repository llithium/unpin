use std::collections::HashMap;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::cli::CookieBrowser;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BrowserCookie {
    pub name: String,
    pub value: String,
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

/// Loads Pinterest cookies from the Netscape/Mozilla cookies.txt format.
pub fn load_pinterest_cookies_file(path: &Path) -> Result<Vec<BrowserCookie>, AuthError> {
    let content = std::fs::read_to_string(path).map_err(|source| AuthError::CookieFileRead {
        path: path.to_owned(),
        source,
    })?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut by_name: HashMap<String, (usize, BrowserCookie)> = HashMap::new();

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
        let specificity = domain.trim_start_matches('.').len();
        let candidate = BrowserCookie {
            name: name.to_owned(),
            value: value.to_owned(),
        };
        match by_name.get(name) {
            Some((current_specificity, _)) if *current_specificity > specificity => {}
            _ => {
                by_name.insert(name.to_owned(), (specificity, candidate));
            }
        }
    }

    let mut cookies = by_name
        .into_values()
        .map(|(_, cookie)| cookie)
        .collect::<Vec<_>>();
    cookies.sort_by(|left, right| left.name.cmp(&right.name));
    if cookies.is_empty() {
        return Err(AuthError::NoCookiesInFile {
            path: path.to_owned(),
        });
    }
    Ok(cookies)
}

pub async fn load_pinterest_cookies(
    browser: CookieBrowser,
) -> Result<Vec<BrowserCookie>, AuthError> {
    tokio::task::spawn_blocking(move || load_pinterest_cookies_blocking(browser))
        .await
        .map_err(|_| AuthError::Read { browser })?
}

fn load_pinterest_cookies_blocking(
    browser: CookieBrowser,
) -> Result<Vec<BrowserCookie>, AuthError> {
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

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut by_name: HashMap<String, (usize, BrowserCookie)> = HashMap::new();

    for cookie in cookies {
        if cookie.name.is_empty()
            || cookie.value.is_empty()
            || cookie.expires.is_some_and(|expires| expires <= now)
            || !domain_matches_pinterest(&cookie.domain)
        {
            continue;
        }

        // When both `.pinterest.com` and `www.pinterest.com` define the same
        // cookie, use the more specific domain just as a browser would.
        let specificity = cookie.domain.trim_start_matches('.').len();
        let candidate = BrowserCookie {
            name: cookie.name.clone(),
            value: cookie.value,
        };
        match by_name.get(&cookie.name) {
            Some((current_specificity, _)) if *current_specificity > specificity => {}
            _ => {
                by_name.insert(cookie.name, (specificity, candidate));
            }
        }
    }

    let mut cookies = by_name
        .into_values()
        .map(|(_, cookie)| cookie)
        .collect::<Vec<_>>();
    cookies.sort_by(|left, right| left.name.cmp(&right.name));
    if cookies.is_empty() {
        return Err(AuthError::NoCookies { browser });
    }
    Ok(cookies)
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
        // `rookie` uses `0` for session cookies; keep treating that like no expiry.
        && cookie
            .expires
            .map(|expires| expires == 0 || expires > now)
            .unwrap_or(true)
}

#[cfg(unix)]
fn load_chrome_profile_cookies(domains: Option<Vec<String>>) -> Option<Vec<rookie::enums::Cookie>> {
    let now = unix_time_now();
    let mut candidates = Vec::new();

    for base in chrome_profile_bases() {
        for profile in ordered_profile_directories(&base) {
            for relative_db in ["Network/Cookies", "Cookies"] {
                let database = profile.join(relative_db);
                if !database.is_file() {
                    continue;
                }
                let Some(database) = database.to_str() else {
                    continue;
                };
                let Ok(cookies) = rookie::any_browser(database, domains.clone(), None) else {
                    continue;
                };
                candidates.push(cookies);
            }
        }
    }

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
    let domain = domain.trim_start_matches('.').to_ascii_lowercase();
    domain == "pinterest.com" || domain.ends_with(".pinterest.com")
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

        assert_eq!(
            load_pinterest_cookies_file(file.path()).unwrap(),
            [
                BrowserCookie {
                    name: "_pinterest_sess".into(),
                    value: "secret".into()
                },
                BrowserCookie {
                    name: "csrftoken".into(),
                    value: "token".into()
                }
            ]
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
    fn chrome_profile_selector_keeps_first_live_profile() {
        let now = 1_700_000_000;
        let selected = select_chrome_profile_cookies(
            vec![
                profile_with_session("profile_one", "live", Some(now + 60)),
                profile_with_session("profile_two", "also_live", Some(now + 120)),
            ],
            now,
        )
        .unwrap();

        assert_eq!(selected_profile_id(&selected), Some("profile_one"));
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
