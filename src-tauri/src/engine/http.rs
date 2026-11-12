//! Safe HTTP transport for the download engine.
//!
//! URLs are validated before any request is sent. Redirects are followed
//! manually up to a fixed limit so every hop can be checked, and headers that
//! carry credentials are removed when a hop crosses origins. The shared client
//! never follows redirects on its own, never negotiates compressed responses,
//! and is restricted to HTTP/1.1.

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, COOKIE, LOCATION};
use reqwest::redirect::Policy;
use reqwest::{Client, Response};
use url::Url;

use super::cookies::CookieJar;
use super::error::EngineError;
use super::retry::classify_reqwest_error;

/// Maximum accepted URL length in bytes.
const MAX_URL_BYTES: usize = 8 * 1024;
/// Maximum number of redirects followed before the chain fails.
const MAX_REDIRECTS: usize = 10;

/// An in-memory HTTP proxy configuration.
///
/// The URL must not embed credentials; the optional username and password are
/// applied as basic authentication and are never persisted or logged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyConfig {
    /// Proxy URL without embedded userinfo.
    pub url: String,
    /// Optional basic-authentication username.
    pub username: Option<String>,
    /// Optional basic-authentication password.
    pub password: Option<String>,
}

/// Header names that a caller may never supply as a custom task header.
const FORBIDDEN_HEADERS: &[&str] = &[
    "Host",
    "Authorization",
    "Cookie",
    "Range",
    "Proxy-Authorization",
    "Content-Length",
    "Connection",
    "Transfer-Encoding",
    "Upgrade",
    "TE",
    "Trailer",
    "Set-Cookie",
];

/// Parse and validate an HTTP or HTTPS download URL.
///
/// The returned string is the normalized form produced by the URL parser. A
/// URL with an unsupported scheme, a control character, a missing host, or
/// embedded credentials is rejected with a typed error.
pub fn validate_url(input: &str) -> Result<String, EngineError> {
    if input.is_empty() {
        return Err(invalid_url("the URL is empty"));
    }
    if input.len() > MAX_URL_BYTES {
        return Err(invalid_url("the URL is longer than the supported limit"));
    }
    if input.chars().any(char::is_control) {
        return Err(invalid_url("the URL contains a control character"));
    }

    let url = Url::parse(input).map_err(|_| invalid_url("the URL is not well formed"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid_url("only HTTP and HTTPS URLs are supported"));
    }
    if url.host_str().map(str::is_empty).unwrap_or(true) {
        return Err(invalid_url("the URL does not contain a host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(EngineError::new(
            "CREDENTIALED_URL",
            "the URL must not embed credentials",
        ));
    }

    Ok(url.as_str().to_string())
}

/// Parse and validate a proxy URL that must not embed credentials.
pub fn validate_proxy_url(input: &str) -> Result<String, EngineError> {
    if input.is_empty() || input.len() > MAX_URL_BYTES {
        return Err(invalid_proxy("the proxy URL is empty or too long"));
    }
    if input.chars().any(char::is_control) {
        return Err(invalid_proxy("the proxy URL contains a control character"));
    }

    let url = Url::parse(input).map_err(|_| invalid_proxy("the proxy URL is not well formed"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid_proxy("only HTTP and HTTPS proxies are supported"));
    }
    if url.host_str().map(str::is_empty).unwrap_or(true) {
        return Err(invalid_proxy("the proxy URL does not contain a host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid_proxy(
            "the proxy URL must not embed credentials; set a username and password instead",
        ));
    }

    Ok(url.as_str().to_string())
}

/// Reject forbidden, hop-by-hop, and newline-bearing custom headers.
pub fn validate_extra_headers(headers: &[(String, String)]) -> Result<(), EngineError> {
    for (name, value) in headers {
        if name.is_empty() || name.contains(['\r', '\n']) || value.contains(['\r', '\n']) {
            return Err(invalid_header());
        }
        if FORBIDDEN_HEADERS
            .iter()
            .any(|forbidden| name.trim().eq_ignore_ascii_case(forbidden))
        {
            return Err(invalid_header());
        }
        if HeaderName::from_bytes(name.as_bytes()).is_err() || HeaderValue::from_str(value).is_err()
        {
            return Err(invalid_header());
        }
    }
    Ok(())
}

/// Produce the URL form that may be persisted.
///
/// Query parameters and fragments can carry tokens or signatures, so they are
/// removed before a task URL reaches the durable store. The caller keeps the
/// full URL in memory for the running session.
pub fn storage_url(input: &str) -> String {
    match Url::parse(input) {
        Ok(mut url) => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        }
        Err(_) => {
            let without_fragment = input.split('#').next().unwrap_or(input);
            without_fragment
                .split('?')
                .next()
                .unwrap_or(without_fragment)
                .to_string()
        }
    }
}

/// Verify that `to` is an acceptable redirect target from `from`.
pub fn check_redirect(from: &str, to: &str) -> Result<(), EngineError> {
    let from = Url::parse(&validate_url(from)?)
        .map_err(|_| invalid_url("the source URL is not well formed"))?;
    let to = Url::parse(&validate_url(to)?)
        .map_err(|_| invalid_url("the redirect target is not well formed"))?;

    if from.scheme() == "https" && to.scheme() == "http" {
        return Err(EngineError::new(
            "INSECURE_REDIRECT",
            "an HTTPS request must not be redirected to plain HTTP",
        ));
    }

    Ok(())
}

/// Build the shared client configuration used for probes and downloads.
///
/// The documented transport bounds are a 15-second connect timeout and a
/// 30-second read timeout.
pub fn build_client() -> Result<Client, EngineError> {
    build_client_with_timeouts(Duration::from_secs(15), Duration::from_secs(30))
}

/// Build a client with explicit connect and read timeouts.
///
/// Callers use this to apply tighter bounds for an interactive probe or to
/// exercise timeout classification with a local fixture.
pub fn build_client_with_timeouts(
    connect: Duration,
    read: Duration,
) -> Result<Client, EngineError> {
    build_client_with_options(connect, read, None)
}

/// Build a client that routes every request through an optional proxy.
pub fn build_client_with_proxy(proxy: Option<&ProxyConfig>) -> Result<Client, EngineError> {
    build_client_with_options(Duration::from_secs(15), Duration::from_secs(30), proxy)
}

/// Build a client with explicit timeouts and proxy behavior.
///
/// With no proxy, system proxy discovery stays disabled so tests and the engine
/// behave the same on every host. With a proxy, basic authentication is applied
/// by the transport, which keeps `Proxy-Authorization` away from the origin.
pub fn build_client_with_options(
    connect: Duration,
    read: Duration,
    proxy: Option<&ProxyConfig>,
) -> Result<Client, EngineError> {
    let mut builder = Client::builder()
        .http1_only()
        .connect_timeout(connect)
        .read_timeout(read)
        .redirect(Policy::none());

    match proxy {
        None => builder = builder.no_proxy(),
        Some(proxy) => {
            let url = validate_proxy_url(&proxy.url)?;
            let mut configured = reqwest::Proxy::all(&url)
                .map_err(|_| invalid_proxy("the proxy URL is not supported"))?;
            if let Some(username) = proxy.username.as_deref() {
                configured =
                    configured.basic_auth(username, proxy.password.as_deref().unwrap_or(""));
            }
            builder = builder.proxy(configured);
        }
    }

    builder
        .build()
        .map_err(|_| EngineError::new("CONNECT_FAILED", "the HTTP client could not be initialized"))
}

/// Send a GET request and follow redirects manually.
pub(crate) async fn get_following_redirects(
    client: &Client,
    url: &str,
    headers: HeaderMap,
) -> Result<(Response, Url), EngineError> {
    get_following_redirects_with_cookies(client, url, headers, None).await
}

/// Send a GET request and follow redirects manually with request-scoped cookies.
///
/// Returns the final response and the URL that produced it. Sensitive headers
/// are removed from `headers` as soon as a hop crosses origins, so they cannot
/// be replayed to a different host later in the chain. Cookies, when supplied,
/// are recomputed for each hop from the cookie jar instead of being forwarded
/// across hosts.
pub(crate) async fn get_following_redirects_with_cookies(
    client: &Client,
    url: &str,
    mut headers: HeaderMap,
    cookies: Option<&CookieJar>,
) -> Result<(Response, Url), EngineError> {
    let mut current =
        Url::parse(&validate_url(url)?).map_err(|_| invalid_url("the URL is not well formed"))?;
    let mut hops = 0usize;

    loop {
        headers.remove(COOKIE);
        if let Some(jar) = cookies {
            if let Some(cookie) = jar.header_for(&current) {
                if let Ok(value) = HeaderValue::from_str(&cookie) {
                    headers.insert(COOKIE, value);
                }
            }
        }

        let response = client
            .get(current.clone())
            .headers(headers.clone())
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;

        if !response.status().is_redirection() {
            return Ok((response, current));
        }

        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                EngineError::new(
                    "INVALID_RESPONSE",
                    "a redirect response did not include a usable location",
                )
            })?;

        let resolved = current
            .join(location)
            .map_err(|_| EngineError::new("INVALID_URL", "a redirect target is not a valid URL"))?;
        let target = validate_url(resolved.as_str())?;
        check_redirect(current.as_str(), &target)?;

        if hops >= MAX_REDIRECTS {
            return Err(EngineError::new(
                "TOO_MANY_REDIRECTS",
                "the redirect chain exceeded the allowed number of hops",
            ));
        }
        hops += 1;

        let next = Url::parse(&target)
            .map_err(|_| EngineError::new("INVALID_URL", "a redirect target is not a valid URL"))?;
        if !same_origin(&current, &next) {
            strip_sensitive_headers(&mut headers);
        }
        current = next;
    }
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn strip_sensitive_headers(headers: &mut HeaderMap) {
    for name in ["cookie", "cookie2", "authorization", "proxy-authorization"] {
        headers.remove(name);
    }
}

fn invalid_url(message: &str) -> EngineError {
    EngineError::new("INVALID_URL", message)
}

fn invalid_proxy(message: &str) -> EngineError {
    EngineError::new("INVALID_PROXY_URL", message)
}

fn invalid_header() -> EngineError {
    EngineError::new("INVALID_HEADER", "a custom request header is not allowed")
}
