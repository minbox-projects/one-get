//! Netscape cookie-file parsing and request-scoped matching.
//!
//! Cookie values stay in memory only. A jar is reloaded at each task start or
//! resume and builds a `Cookie` header from the cookies whose domain, path,
//! expiry, and secure flag match the request URL. Cross-host redirects get the
//! header for the target host instead of the original host's cookies.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use url::Url;

use super::error::EngineError;

/// One cookie captured from a browser and carried only in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserCookie {
    /// Cookie name.
    pub name: String,
    /// Cookie value; never persisted, logged, or rendered.
    pub value: String,
    /// Cookie domain without a leading dot.
    pub domain: String,
    /// Cookie path.
    pub path: String,
    /// Whether the cookie is restricted to secure schemes.
    pub secure: bool,
    /// Expiry in seconds since the Unix epoch; zero means a session cookie.
    pub expires: u64,
}

/// The single cookie source selected for one task.
///
/// A task-level file always wins over captured browser cookies, and the two
/// sources are never merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CookieSource {
    /// A Netscape cookie file reloaded from disk at each start or resume.
    File(PathBuf),
    /// Captured browser cookies retained only in memory.
    Browser(Vec<BrowserCookie>),
}

impl BrowserCookie {
    /// Whether this cookie applies to `url` at `now`.
    pub(crate) fn matches(&self, url: &Url, now: u64) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        if self.secure && url.scheme() != "https" {
            return false;
        }
        if self.expires != 0 && self.expires <= now {
            return false;
        }
        if has_control(&self.name) || has_control(&self.value) {
            return false;
        }
        let domain = normalize_domain(&self.domain);
        if domain.is_empty() {
            return false;
        }
        let host = host.to_ascii_lowercase();
        let host_matches = host == domain || host.ends_with(&format!(".{domain}"));
        host_matches && path_matches(&normalize_path(&self.path), url.path())
    }
}

/// One parsed Netscape cookie row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cookie {
    domain: String,
    include_subdomains: bool,
    path: String,
    secure: bool,
    expires: u64,
    name: String,
    value: String,
}

impl Cookie {
    fn matches_host(&self, host: &str) -> bool {
        if host.eq_ignore_ascii_case(&self.domain) {
            return true;
        }
        self.include_subdomains
            && host
                .to_ascii_lowercase()
                .ends_with(&format!(".{}", self.domain))
    }

    fn matches_path(&self, request_path: &str) -> bool {
        path_matches(&self.path, request_path)
    }

    fn is_expired(&self, now: u64) -> bool {
        self.expires != 0 && self.expires <= now
    }
}

/// Cookies loaded from one Netscape cookie file.
#[derive(Debug, Clone, Default)]
pub struct CookieJar {
    cookies: Vec<Cookie>,
}

impl CookieJar {
    /// Read and parse a Netscape cookie file.
    pub fn load(path: &Path) -> Result<Self, EngineError> {
        let text = std::fs::read_to_string(path).map_err(|_| {
            EngineError::new("COOKIE_FILE_ERROR", "the cookie file could not be read")
        })?;
        Ok(Self {
            cookies: parse(&text),
        })
    }

    /// Build a jar from memory-only captured browser cookies.
    pub fn from_browser(cookies: Vec<BrowserCookie>) -> Self {
        Self {
            cookies: cookies
                .into_iter()
                .map(|cookie| Cookie {
                    domain: normalize_domain(&cookie.domain),
                    include_subdomains: true,
                    path: normalize_path(&cookie.path),
                    secure: cookie.secure,
                    expires: cookie.expires,
                    name: cookie.name,
                    value: cookie.value,
                })
                .collect(),
        }
    }

    /// Build the `Cookie` header for `url`, or `None` when no cookie matches.
    pub fn header_for(&self, url: &Url) -> Option<String> {
        let matching = self.matching(url);
        if matching.is_empty() {
            None
        } else {
            Some(
                matching
                    .into_iter()
                    .map(|cookie| format!("{}={}", cookie.name, cookie.value))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        }
    }

    /// Count the cookies from this jar that apply to `url`.
    pub fn matching_count(&self, url: &Url) -> u32 {
        self.matching(url).len() as u32
    }

    fn matching(&self, url: &Url) -> Vec<&Cookie> {
        let Some(host) = url.host_str() else {
            return Vec::new();
        };
        let host = host.to_ascii_lowercase();
        let scheme = url.scheme();
        let path = url.path();
        let now = now_secs();

        self.cookies
            .iter()
            .filter(|cookie| {
                cookie.matches_host(&host)
                    && cookie.matches_path(path)
                    && !(cookie.secure && scheme != "https")
                    && !cookie.is_expired(now)
                    && !has_control(&cookie.name)
                    && !has_control(&cookie.value)
            })
            .collect()
    }
}

fn parse(text: &str) -> Vec<Cookie> {
    let mut cookies = Vec::new();
    for line in text.lines() {
        let line = match line.strip_prefix("#HttpOnly_") {
            Some(rest) => rest,
            None if line.starts_with('#') => continue,
            None => line,
        };
        if line.trim().is_empty() {
            continue;
        }

        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 7 {
            continue;
        }

        let domain = normalize_domain(fields[0]);
        if domain.is_empty() {
            continue;
        }
        let Ok(expires) = fields[4].trim().parse::<u64>() else {
            continue;
        };
        let name = fields[5].to_string();
        if name.is_empty() {
            continue;
        }
        let value = fields[6..].join("\t");

        cookies.push(Cookie {
            domain,
            include_subdomains: fields[1].eq_ignore_ascii_case("TRUE"),
            path: normalize_path(fields[2]),
            secure: fields[3].eq_ignore_ascii_case("TRUE"),
            expires,
            name,
            value,
        });
    }
    cookies
}

fn normalize_domain(raw: &str) -> String {
    raw.trim().trim_start_matches('.').to_ascii_lowercase()
}

fn path_matches(cookie_path: &str, request_path: &str) -> bool {
    if request_path == cookie_path {
        return true;
    }
    request_path.starts_with(cookie_path)
        && (cookie_path.ends_with('/')
            || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/'))
}

fn normalize_path(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        String::from("/")
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

fn has_control(value: &str) -> bool {
    value.chars().any(char::is_control)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
