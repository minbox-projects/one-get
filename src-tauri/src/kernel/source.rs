//! Verified component catalog cache and build-time trust root access.
//!
//! The cache stores the raw signed bytes and re-runs full verification on every
//! load, so a modified cache file never becomes trusted catalog data. Writes
//! land in a temporary file inside the cache root and are renamed into place.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::redirect::Policy;
use url::{Host, Url};

use super::catalog::{verify_catalog, ComponentCatalog, Target};
use super::error::KernelError;

/// File name of the cached signed catalog inside the cache root.
pub const CATALOG_FILE_NAME: &str = "catalog.json";

/// Maximum accepted catalog response body in bytes.
pub const MAX_CATALOG_BYTES: usize = 1024 * 1024;

/// Maximum accepted catalog URL length in bytes.
const MAX_CATALOG_URL_BYTES: usize = 8192;

/// Environment variable naming a runtime catalog URL override for fixtures.
const MANIFEST_URL_ENV: &str = "ONEGET_COMPONENT_MANIFEST_URL";

const TEMP_FILE_NAME: &str = "catalog.json.tmp";

/// A runtime catalog URL override, trimmed and empty values ignored.
pub fn manifest_url_override() -> Option<String> {
    std::env::var(MANIFEST_URL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Fetch the raw signed catalog bytes over a bounded, policy-checked client.
///
/// The URL must satisfy the artifact policy before any request: HTTPS, or
/// loopback HTTP only when `allow_loopback_http` is set, with no userinfo,
/// fragment, or length above 8192 bytes. Automatic redirects are disabled so a
/// redirect can never leave that policy. The streamed body is capped at
/// [`MAX_CATALOG_BYTES`], and transport failures map to
/// `COMPONENT_MANIFEST_UNAVAILABLE`. The bytes are returned unverified for the
/// caller to authenticate.
pub async fn fetch_catalog(
    url: &str,
    allow_loopback_http: bool,
    timeout: Duration,
) -> Result<Vec<u8>, KernelError> {
    validate_catalog_url(url, allow_loopback_http)?;

    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|_| manifest_unavailable())?;

    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|_| manifest_unavailable())?;
    if !response.status().is_success() {
        return Err(manifest_unavailable());
    }

    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| manifest_unavailable())? {
        if body.len().saturating_add(chunk.len()) > MAX_CATALOG_BYTES {
            return Err(KernelError::new(
                "COMPONENT_MANIFEST_TOO_LARGE",
                "component catalog exceeds the 1048576 byte limit",
            ));
        }
        body.extend_from_slice(&chunk);
    }

    Ok(body)
}

fn validate_catalog_url(value: &str, allow_loopback_http: bool) -> Result<(), KernelError> {
    if value.is_empty() || value.len() > MAX_CATALOG_URL_BYTES {
        return Err(invalid_url(
            "catalog URL length is outside the accepted range",
        ));
    }

    let url = Url::parse(value).map_err(|_| invalid_url("catalog URL is not a valid URL"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid_url("catalog URL must not embed userinfo"));
    }
    if url.fragment().is_some() {
        return Err(invalid_url("catalog URL must not contain a fragment"));
    }
    if url.host_str().is_none() {
        return Err(invalid_url("catalog URL must include a host"));
    }

    match url.scheme() {
        "https" => Ok(()),
        "http" if allow_loopback_http && is_loopback(&url) => Ok(()),
        "http" => Err(invalid_url(
            "catalog URL must use HTTPS unless loopback HTTP is explicitly allowed",
        )),
        _ => Err(invalid_url("catalog URL must use HTTPS")),
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

fn invalid_url(message: impl Into<String>) -> KernelError {
    KernelError::new("COMPONENT_CATALOG_INVALID", message)
}

fn manifest_unavailable() -> KernelError {
    KernelError::new(
        "COMPONENT_MANIFEST_UNAVAILABLE",
        "the component catalog could not be fetched",
    )
}

/// The component trust public key configured when the application is built.
///
/// Returns `None` when the build did not set `ONEGET_COMPONENT_TRUST_PUBLIC_KEY`.
/// The value is a public key only; the private key never enters the repository
/// or the runtime configuration.
pub fn configured_public_key() -> Option<&'static str> {
    match option_env!("ONEGET_COMPONENT_TRUST_PUBLIC_KEY") {
        Some(key) if !key.is_empty() => Some(key),
        _ => None,
    }
}

/// A cache root that stores one verified, signed catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogCache {
    root: PathBuf,
}

impl CatalogCache {
    /// Create a cache rooted at an explicit directory.
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// The absolute or relative path of the cached catalog.
    pub fn path(&self) -> PathBuf {
        self.root.join(CATALOG_FILE_NAME)
    }

    /// Verify a signed catalog and atomically cache its raw bytes.
    ///
    /// Verification runs before the cache directory or any file is touched, so
    /// rejected input leaves no catalog behind.
    pub fn store_verified(
        &self,
        bytes: &[u8],
        public_key_hex: Option<&str>,
        target: Target,
        allow_loopback_http: bool,
    ) -> Result<ComponentCatalog, KernelError> {
        let catalog = verify_catalog(bytes, public_key_hex, target, allow_loopback_http)?;

        fs::create_dir_all(&self.root).map_err(|_| cache_write_failed())?;
        let temporary = self.root.join(TEMP_FILE_NAME);
        if write_atomic(&temporary, &self.path(), bytes).is_err() {
            let _ = fs::remove_file(&temporary);
            return Err(cache_write_failed());
        }

        Ok(catalog)
    }

    /// Read and fully re-verify the cached catalog.
    ///
    /// A missing cache is reported as `COMPONENT_MANIFEST_UNAVAILABLE`; a
    /// present but invalid or tampered file is reported as a verification
    /// failure instead.
    pub fn load(
        &self,
        public_key_hex: Option<&str>,
        target: Target,
        allow_loopback_http: bool,
    ) -> Result<ComponentCatalog, KernelError> {
        let bytes = match fs::read(self.path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(KernelError::new(
                    "COMPONENT_MANIFEST_UNAVAILABLE",
                    "no verified component catalog is cached",
                ));
            }
            Err(_) => {
                return Err(KernelError::new(
                    "COMPONENT_CACHE_READ_FAILED",
                    "the cached component catalog could not be read",
                ));
            }
        };

        verify_catalog(&bytes, public_key_hex, target, allow_loopback_http)
    }
}

fn write_atomic(temporary: &Path, destination: &Path, bytes: &[u8]) -> Result<(), KernelError> {
    let mut file = fs::File::create(temporary).map_err(|_| cache_write_failed())?;
    file.write_all(bytes).map_err(|_| cache_write_failed())?;
    file.sync_all().map_err(|_| cache_write_failed())?;
    drop(file);
    fs::rename(temporary, destination).map_err(|_| cache_write_failed())
}

fn cache_write_failed() -> KernelError {
    KernelError::new(
        "COMPONENT_CACHE_WRITE_FAILED",
        "the verified component catalog could not be cached",
    )
}
