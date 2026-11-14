//! Bounded component artifact download with size and SHA-256 verification.
//!
//! The same URL policy as the catalog fetch applies before any request: HTTPS,
//! or loopback HTTP only when explicitly allowed for fixtures, with no userinfo
//! or fragment. Automatic redirects are disabled so a redirect cannot leave that
//! policy. The streamed body is rejected as soon as it exceeds the declared
//! size, so an oversized response is never fully buffered.

use std::time::Duration;

use reqwest::redirect::Policy;
use sha2::{Digest, Sha256};
use url::{Host, Url};

use super::error::KernelError;

/// Maximum accepted artifact URL length in bytes.
const MAX_ARTIFACT_URL_BYTES: usize = 8192;

/// Download `url`, then require the exact declared size and SHA-256.
///
/// A size or hash mismatch returns `COMPONENT_HASH`; a URL outside the policy
/// returns `COMPONENT_ARTIFACT_INVALID` before any request; and a transport or
/// HTTP failure returns `COMPONENT_DOWNLOAD_UNAVAILABLE`.
pub async fn download_artifact(
    url: &str,
    expected_sha256: &str,
    expected_size: u64,
    allow_loopback_http: bool,
    timeout: Duration,
) -> Result<Vec<u8>, KernelError> {
    validate_url(url, allow_loopback_http)?;

    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|_| unavailable())?;

    let mut response = client.get(url).send().await.map_err(|_| unavailable())?;
    if !response.status().is_success() {
        return Err(unavailable());
    }

    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        let total = body.len() as u64 + chunk.len() as u64;
        if total > expected_size {
            return Err(hash_mismatch());
        }
        body.extend_from_slice(&chunk);
    }

    if body.len() as u64 != expected_size {
        return Err(hash_mismatch());
    }
    let digest = hex::encode(Sha256::digest(&body));
    if !digest.eq_ignore_ascii_case(expected_sha256) {
        return Err(hash_mismatch());
    }
    Ok(body)
}

fn validate_url(value: &str, allow_loopback_http: bool) -> Result<(), KernelError> {
    if value.is_empty() || value.len() > MAX_ARTIFACT_URL_BYTES {
        return Err(invalid("artifact URL length is outside the accepted range"));
    }

    let url = Url::parse(value).map_err(|_| invalid("artifact URL is not a valid URL"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("artifact URL must not embed userinfo"));
    }
    if url.fragment().is_some() {
        return Err(invalid("artifact URL must not contain a fragment"));
    }
    if url.host_str().is_none() {
        return Err(invalid("artifact URL must include a host"));
    }

    match url.scheme() {
        "https" => Ok(()),
        "http" if allow_loopback_http && is_loopback(&url) => Ok(()),
        "http" => Err(invalid(
            "artifact URL must use HTTPS unless loopback HTTP is explicitly allowed",
        )),
        _ => Err(invalid("artifact URL must use HTTPS")),
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

fn invalid(message: &str) -> KernelError {
    KernelError::new("COMPONENT_ARTIFACT_INVALID", message)
}

fn hash_mismatch() -> KernelError {
    KernelError::new(
        "COMPONENT_HASH",
        "the downloaded component artifact does not match its declared hash or size",
    )
}

fn unavailable() -> KernelError {
    KernelError::new(
        "COMPONENT_DOWNLOAD_UNAVAILABLE",
        "the component artifact could not be downloaded",
    )
}
