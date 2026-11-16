//! Hugging Face resolver recognition and normalization.
//!
//! Only the four canonical resolver hosts are recognized. A resolver URL that
//! names a repository, tree, or other non-file view is rejected with
//! `HF_NOT_A_FILE`; CDN and unrelated hosts are returned as ordinary URLs so
//! the engine never attaches a Hugging Face token to them.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, RwLock};

use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT_ENCODING, AUTHORIZATION, COOKIE, LOCATION,
};
use reqwest::{Client, StatusCode};
use url::Url;

use crate::platform::secrets::SecretValue;
use crate::store::hf::{HfMetadata, HfMetadataStore};

use super::cookies::CookieJar;
use super::error::EngineError;
use super::http::{check_redirect, validate_url};
use super::retry::classify_reqwest_error;

/// Signature refreshes one task may consume after its initial resolution.
///
/// The initial resolution, including its single 401 retry, is separate from
/// this bound. Each CDN rejection may trigger one refresh, and the persisted
/// refresh count counts those refreshes only.
const MAX_SIGNATURE_REFRESHES: u32 = 3;

/// Maximum resolver redirects followed while resolving a signed URL.
const MAX_RESOLVER_REDIRECTS: usize = 10;

/// The repository family a resolver file URL belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HfRepoType {
    /// A model repository.
    Model,
    /// A dataset repository.
    Dataset,
    /// A Space repository.
    Space,
}

/// A recognized Hugging Face file URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HfUrl {
    /// Repository family.
    pub repo_type: HfRepoType,
    /// Repository identifier without the family prefix.
    pub repo_id: String,
    /// Revision, branch, or tag.
    pub revision: String,
    /// File path within the repository.
    pub file_path: String,
    /// File URL normalized to the `/resolve/` form.
    pub normalized: String,
}

/// The resolver hosts whose non-file paths are rejected for downloads.
fn is_resolver_host(host: &str) -> bool {
    matches!(
        host,
        "huggingface.co" | "www.huggingface.co" | "hf.co" | "www.hf.co"
    )
}

/// Whether a URL points at one of the recognized resolver hosts.
pub(crate) fn is_resolver_url(input: &str) -> bool {
    Url::parse(input)
        .ok()
        .and_then(|url| {
            url.host_str()
                .map(|host| is_resolver_host(&host.to_ascii_lowercase()))
        })
        .unwrap_or(false)
}

fn not_a_file() -> EngineError {
    EngineError::new(
        "HF_NOT_A_FILE",
        "the Hugging Face URL does not point to repository file content",
    )
}

/// A Hugging Face gated request failed without usable authorization.
pub(crate) fn auth_error() -> EngineError {
    EngineError::new(
        "HF_AUTH",
        "the Hugging Face file requires a valid access token",
    )
}

/// The signed CDN URL kept failing after every permitted refresh.
pub(crate) fn signature_error() -> EngineError {
    EngineError::new(
        "HF_SIGNATURE",
        "the Hugging Face download signature expired and could not be refreshed",
    )
}

/// A resolved signed CDN URL plus the refresh bookkeeping for one task.
pub(crate) struct HfDownload {
    resolver_url: String,
    signed_url: RwLock<String>,
    refresh_count: AtomicU32,
    downgraded: AtomicBool,
    reduced: AtomicBool,
    token: Option<SecretValue>,
    base: HfMetadata,
    metadata: Option<Arc<HfMetadataStore>>,
    refresh_lock: tokio::sync::Mutex<()>,
}

impl HfDownload {
    /// Resolve the initial signed URL and remember the task's metadata.
    ///
    /// A resumed task starts from its persisted refresh budget, downgrade flag,
    /// and one-connection fallback; a fresh task starts with none of them.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn initial(
        resolver_url: &str,
        client: &Client,
        token: Option<SecretValue>,
        cookies: Option<&CookieJar>,
        extra_headers: &[(String, String)],
        base: HfMetadata,
        resumed_reduced: bool,
        metadata: Option<Arc<HfMetadataStore>>,
    ) -> Result<Self, EngineError> {
        let signed_url =
            resolve_with_auth_refresh(client, resolver_url, token.as_ref(), cookies, extra_headers)
                .await?;
        Ok(Self {
            resolver_url: resolver_url.to_string(),
            signed_url: RwLock::new(signed_url),
            refresh_count: AtomicU32::new(base.refresh_count),
            downgraded: AtomicBool::new(base.downgraded),
            reduced: AtomicBool::new(resumed_reduced),
            token,
            base,
            metadata,
            refresh_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// The signed CDN URL every worker should currently request.
    pub(crate) fn current_url(&self) -> String {
        self.signed_url
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Whether the task already dropped to a single connection.
    pub(crate) fn is_reduced(&self) -> bool {
        self.reduced.load(Ordering::SeqCst)
    }

    /// Record that the task must continue with one connection.
    pub(crate) fn mark_reduced(&self) {
        if !self.reduced.swap(true, Ordering::SeqCst) {
            self.persist();
        }
    }

    /// Record that the transfer fell back because Range is unsupported.
    pub(crate) fn mark_downgraded(&self) {
        if !self.downgraded.swap(true, Ordering::SeqCst) {
            self.persist();
        }
    }

    /// Re-resolve after a CDN rejection and atomically publish the fresh URL.
    ///
    /// A task may consume up to [`MAX_SIGNATURE_REFRESHES`] refreshes after its
    /// initial resolution; each successful one increments the persisted refresh
    /// count. A worker that observed an already-replaced URL does no extra
    /// network work, so concurrent rejections consume one refresh between them.
    pub(crate) async fn refresh(
        &self,
        observed_url: &str,
        client: &Client,
        cookies: Option<&CookieJar>,
        extra_headers: &[(String, String)],
    ) -> Result<(), EngineError> {
        let _guard = self.refresh_lock.lock().await;
        if self.current_url() != observed_url {
            return Ok(());
        }
        if self.refresh_count.load(Ordering::SeqCst) >= MAX_SIGNATURE_REFRESHES {
            return Err(signature_error());
        }
        let signed_url = match resolve_signed_url(
            client,
            &self.resolver_url,
            self.token.as_ref(),
            cookies,
            extra_headers,
        )
        .await
        {
            Ok(url) => url,
            Err(error) if error.code() == "HF_AUTH" => return Err(auth_error()),
            Err(error) => return Err(error),
        };
        self.refresh_count.fetch_add(1, Ordering::SeqCst);
        *self
            .signed_url
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = signed_url;
        self.persist();
        Ok(())
    }

    fn persist(&self) {
        let Some(store) = self.metadata.as_ref() else {
            return;
        };
        let downgraded = self.downgraded.load(Ordering::SeqCst);
        let reduced = self.reduced.load(Ordering::SeqCst);
        let metadata = HfMetadata {
            task_id: self.base.task_id.clone(),
            repo_id: self.base.repo_id.clone(),
            file_path: self.base.file_path.clone(),
            revision: self.base.revision.clone(),
            connection_cap: self.base.connection_cap,
            effective_connections: if downgraded || reduced {
                1
            } else {
                self.base.effective_connections
            },
            refresh_count: self.refresh_count.load(Ordering::SeqCst),
            downgraded,
        };
        let _ = store.record(&metadata);
    }
}

/// Resolve once and retry a single time when the resolver reports unauthorized.
///
/// The retry belongs to the initial resolution and never consumes a signature
/// refresh.
async fn resolve_with_auth_refresh(
    client: &Client,
    resolver_url: &str,
    token: Option<&SecretValue>,
    cookies: Option<&CookieJar>,
    extra_headers: &[(String, String)],
) -> Result<String, EngineError> {
    match resolve_signed_url(client, resolver_url, token, cookies, extra_headers).await {
        Ok(url) => Ok(url),
        Err(error) if error.code() == "HF_AUTH" => {
            match resolve_signed_url(client, resolver_url, token, cookies, extra_headers).await {
                Ok(url) => Ok(url),
                Err(error) if error.code() == "HF_AUTH" => Err(auth_error()),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

/// Ask the resolver for the current signed CDN URL.
///
/// The resolver may redirect among its own hosts; the first redirect that
/// leaves the resolver hosts is the signed CDN URL and is returned without
/// contacting the CDN. The token is therefore only ever sent to resolver hosts.
async fn resolve_signed_url(
    client: &Client,
    resolver_url: &str,
    token: Option<&SecretValue>,
    cookies: Option<&CookieJar>,
    extra_headers: &[(String, String)],
) -> Result<String, EngineError> {
    let mut current = Url::parse(&validate_url(resolver_url)?)
        .map_err(|_| EngineError::new("INVALID_URL", "the resolver URL is not well formed"))?;
    let mut hops = 0usize;

    loop {
        let mut headers = resolver_headers(extra_headers, token)?;
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
            .headers(headers)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        let status = response.status();

        if status.is_redirection() {
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    EngineError::new(
                        "INVALID_RESPONSE",
                        "a resolver redirect did not include a usable location",
                    )
                })?;
            let resolved = current.join(location).map_err(|_| {
                EngineError::new(
                    "INVALID_URL",
                    "a resolver redirect target is not a valid URL",
                )
            })?;
            let target = validate_url(resolved.as_str())?;
            check_redirect(current.as_str(), &target)?;
            if !is_resolver_url(&target) {
                return Ok(target);
            }
            if hops >= MAX_RESOLVER_REDIRECTS {
                return Err(EngineError::new(
                    "TOO_MANY_REDIRECTS",
                    "the resolver redirect chain exceeded the allowed number of hops",
                ));
            }
            hops += 1;
            current = Url::parse(&target).map_err(|_| {
                EngineError::new(
                    "INVALID_URL",
                    "a resolver redirect target is not a valid URL",
                )
            })?;
            continue;
        }

        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err(auth_error());
        }
        if status == StatusCode::OK {
            return Ok(current.to_string());
        }
        return Err(resolver_status_error(status));
    }
}

fn resolver_headers(
    extra_headers: &[(String, String)],
    token: Option<&SecretValue>,
) -> Result<HeaderMap, EngineError> {
    let mut headers = HeaderMap::new();
    for (name, value) in extra_headers {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            EngineError::new("INVALID_HEADER", "an extra request header is not valid")
        })?;
        let value = HeaderValue::from_str(value).map_err(|_| {
            EngineError::new(
                "INVALID_HEADER",
                "an extra request header value is not valid",
            )
        })?;
        headers.insert(name, value);
    }
    headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    if let Some(token) = token {
        let value =
            HeaderValue::from_str(&format!("Bearer {}", token.expose_secret())).map_err(|_| {
                EngineError::new(
                    "INVALID_HEADER",
                    "the Hugging Face token is not a valid header",
                )
            })?;
        headers.insert(AUTHORIZATION, value);
    }
    Ok(headers)
}

fn resolver_status_error(status: StatusCode) -> EngineError {
    match status.as_u16() {
        404 => EngineError::new("NOT_FOUND", "the remote file was not found"),
        408 => EngineError::retryable("TIMEOUT", "the server reported a request timeout"),
        429 => EngineError::retryable("RATE_LIMITED", "the server asked the client to slow down"),
        _ if status.is_server_error() => {
            EngineError::retryable("SERVER_ERROR", "the server failed to serve the file")
        }
        _ => EngineError::new(
            "INVALID_RESPONSE",
            "the resolver returned an unusable response",
        ),
    }
}

/// Parse a resolver file URL.
///
/// Returns `Ok(None)` for CDN or unrelated hosts so their existing HTTP
/// behavior is unchanged. A resolver host URL that does not name a file under
/// `/resolve/` or `/blob/` is `HF_NOT_A_FILE`.
pub fn parse_hf_url(input: &str) -> Result<Option<HfUrl>, EngineError> {
    let Ok(url) = Url::parse(input) else {
        return Ok(None);
    };
    if !matches!(url.scheme(), "http" | "https") {
        return Ok(None);
    }
    let Some(host) = url.host_str() else {
        return Ok(None);
    };
    if !is_resolver_host(&host.to_ascii_lowercase()) {
        return Ok(None);
    }

    let segments: Vec<&str> = url
        .path()
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();

    let (repo_type, offset) = match segments.first().copied() {
        Some("datasets") => (HfRepoType::Dataset, 1),
        Some("spaces") => (HfRepoType::Space, 1),
        Some(_) => (HfRepoType::Model, 0),
        None => return Err(not_a_file()),
    };

    let marker = segments
        .iter()
        .enumerate()
        .skip(offset)
        .find(|(_, segment)| matches!(**segment, "resolve" | "blob"))
        .map(|(index, _)| index)
        .ok_or_else(not_a_file)?;

    if marker <= offset || marker + 2 >= segments.len() {
        return Err(not_a_file());
    }

    let repo_id = segments[offset..marker].join("/");
    if repo_id.is_empty() {
        return Err(not_a_file());
    }
    let revision = segments[marker + 1].to_string();
    let file_path = segments[marker + 2..].join("/");
    if revision.is_empty() || file_path.is_empty() {
        return Err(not_a_file());
    }

    let mut normalized_segments: Vec<&str> = segments.clone();
    normalized_segments[marker] = "resolve";
    let normalized_path = format!("/{}", normalized_segments.join("/"));
    let mut normalized_url = url.clone();
    normalized_url.set_path(&normalized_path);

    Ok(Some(HfUrl {
        repo_type,
        repo_id,
        revision,
        file_path,
        normalized: normalized_url.to_string(),
    }))
}

/// Normalize a download URL, rejecting non-file resolver paths.
///
/// Non-Hugging Face URLs are returned unchanged.
pub(crate) fn normalize_download_url(input: &str) -> Result<String, EngineError> {
    match parse_hf_url(input)? {
        Some(parsed) => Ok(parsed.normalized),
        None => Ok(input.to_string()),
    }
}
