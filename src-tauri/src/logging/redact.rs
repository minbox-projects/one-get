//! Redaction of secrets before they reach a log sink.
//!
//! [`redact_text`] handles secrets embedded in free-form text: URL query
//! credentials, authorization and cookie headers, URL userinfo passwords,
//! proxy password fields, and HuggingFace API tokens. [`redact_json`] walks a
//! JSON value and redacts every string, so a structured record stays valid JSON
//! instead of being sanitized after serialization.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

/// Marker written in place of every redacted value.
pub const REDACTED: &str = "[REDACTED]";

struct Rules {
    headers: Regex,
    query: Regex,
    userinfo: Regex,
    password_fields: Regex,
    hf_token: Regex,
}

fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| Rules {
        headers: Regex::new(
            r"(?i)(\b(?:proxy-authorization|set-cookie|authorization|cookie)\s*:\s*)([^\r\n]+)",
        )
        .expect("valid header redaction pattern"),
        query: Regex::new(
            r#"(?i)([?&](?:access_token|id_token|refresh_token|token|signature|sig|x-amz-signature|x-amz-credential|x-amz-security-token)=)([^&\s"'#<>]+)"#,
        )
        .expect("valid query redaction pattern"),
        userinfo: Regex::new(r"(?i)(://[^/\s:@]*:)([^@\s/]+)(@)")
            .expect("valid userinfo redaction pattern"),
        password_fields: Regex::new(
            r#"(?i)(\b(?:proxy[_-]?)?(?:password|passwd)\b["']?\s*[=:]\s*)("[^"]*"|'[^']*'|[^,;&\s}\)\]"']+)"#,
        )
        .expect("valid password redaction pattern"),
        hf_token: Regex::new(r"\bhf_[A-Za-z0-9_]{4,}")
            .expect("valid HuggingFace token redaction pattern"),
    })
}

/// Redact known secret shapes from a free-form string.
///
/// The returned string contains [`REDACTED`] in place of every recognized
/// secret value and otherwise preserves the surrounding text.
pub fn redact_text(input: &str) -> String {
    let rules = rules();
    let mut output = rules
        .headers
        .replace_all(input, "$1[REDACTED]")
        .into_owned();
    output = rules
        .query
        .replace_all(&output, "$1[REDACTED]")
        .into_owned();
    output = rules
        .userinfo
        .replace_all(&output, "$1[REDACTED]$3")
        .into_owned();
    output = rules
        .password_fields
        .replace_all(&output, "$1[REDACTED]")
        .into_owned();
    rules.hf_token.replace_all(&output, REDACTED).into_owned()
}

/// Redact every string inside a JSON value in place.
///
/// Object keys whose names denote credentials are replaced wholesale; all
/// other strings are passed through [`redact_text`]. The value remains valid
/// JSON because redaction happens before serialization.
pub fn redact_json(value: &mut Value) {
    match value {
        Value::String(text) => {
            let redacted = redact_text(text);
            if redacted != *text {
                *text = redacted;
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_json(item);
            }
        }
        Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                if is_sensitive_key(key) && entry.is_string() {
                    *entry = Value::String(REDACTED.to_string());
                } else {
                    redact_json(entry);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|ch| *ch != '_' && *ch != '-' && *ch != '.')
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        normalized.as_str(),
        "password"
            | "passwd"
            | "proxypassword"
            | "proxypasswd"
            | "secret"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "sessiontoken"
            | "apitoken"
            | "apikey"
            | "hftoken"
            | "huggingfacetoken"
            | "authorization"
            | "proxyauthorization"
            | "proxyauth"
            | "cookie"
            | "setcookie"
            | "sig"
            | "signature"
            | "xamzsignature"
            | "xamzcredential"
            | "xamzsecuritytoken"
    )
}
