//! Netscape cookie-file parsing and request-scoped matching.
//!
//! Cookie values stay in memory only. A jar is reloaded at each task start or
//! resume and builds a `Cookie` header from the cookies whose domain, path,
//! expiry, and secure flag match the request URL. Cross-host redirects get the
//! header for the target host instead of the original host's cookies.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use url::Url;

use super::error::EngineError;

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
        if request_path == self.path {
            return true;
        }
        request_path.starts_with(&self.path)
            && (self.path.ends_with('/')
                || request_path.as_bytes().get(self.path.len()) == Some(&b'/'))
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

    /// Build the `Cookie` header for `url`, or `None` when no cookie matches.
    pub fn header_for(&self, url: &Url) -> Option<String> {
        let host = url.host_str()?.to_ascii_lowercase();
        let scheme = url.scheme();
        let path = url.path();
        let now = now_secs();

        let mut pairs = Vec::new();
        for cookie in &self.cookies {
            if !cookie.matches_host(&host) || !cookie.matches_path(path) {
                continue;
            }
            if cookie.secure && scheme != "https" {
                continue;
            }
            if cookie.is_expired(now) {
                continue;
            }
            if has_control(&cookie.name) || has_control(&cookie.value) {
                continue;
            }
            pairs.push(format!("{}={}", cookie.name, cookie.value));
        }

        if pairs.is_empty() {
            None
        } else {
            Some(pairs.join("; "))
        }
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

        let domain = fields[0]
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
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
