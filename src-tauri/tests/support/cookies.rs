#![allow(dead_code)] // Shared fixture helpers are consumed by different integration targets.

//! Deterministic Netscape cookie files and Cookie header inspection helpers.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One row of a Netscape cookie file.
#[derive(Debug, Clone)]
pub struct NetscapeCookie {
    pub domain: String,
    pub include_subdomains: bool,
    pub path: String,
    pub secure: bool,
    /// Epoch seconds. Zero means a session cookie.
    pub expires: u64,
    pub name: String,
    pub value: String,
}

/// An expiry far enough in the future that the cookie is not expired.
pub fn future_expiry() -> u64 {
    now_secs().saturating_add(3_600)
}

/// An expiry far enough in the past that the cookie is expired.
pub fn past_expiry() -> u64 {
    now_secs().saturating_sub(3_600)
}

/// Write a Netscape cookie file and return its path.
pub fn write_cookie_file(directory: &Path, cookies: &[NetscapeCookie]) -> PathBuf {
    std::fs::create_dir_all(directory).expect("create cookie directory");
    let path = directory.join("cookies.txt");

    let mut contents = String::from("# Netscape HTTP Cookie File\n");
    for cookie in cookies {
        contents.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            cookie.domain,
            flag(cookie.include_subdomains),
            cookie.path,
            flag(cookie.secure),
            cookie.expires,
            cookie.name,
            cookie.value
        ));
    }

    std::fs::write(&path, contents).expect("write cookie file");
    path
}

/// Parse a recorded `Cookie` request header into name/value pairs.
pub fn cookie_pairs(headers: &HashMap<String, String>) -> Vec<(String, String)> {
    let Some(raw) = headers.get("cookie") else {
        return Vec::new();
    };

    raw.split(';')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            let (name, value) = part.split_once('=')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

/// The set of cookie names in a recorded `Cookie` request header.
pub fn cookie_names(headers: &HashMap<String, String>) -> BTreeSet<String> {
    cookie_pairs(headers)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// The value of one named cookie in a recorded `Cookie` request header.
pub fn cookie_value(headers: &HashMap<String, String>, name: &str) -> Option<String> {
    cookie_pairs(headers)
        .into_iter()
        .find(|(cookie_name, _)| cookie_name == name)
        .map(|(_, value)| value)
}

fn flag(value: bool) -> &'static str {
    if value {
        "TRUE"
    } else {
        "FALSE"
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
