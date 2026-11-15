//! Hugging Face resolver recognition and normalization.
//!
//! Only the four canonical resolver hosts are recognized. A resolver URL that
//! names a repository, tree, or other non-file view is rejected with
//! `HF_NOT_A_FILE`; CDN and unrelated hosts are returned as ordinary URLs so
//! the engine never attaches a Hugging Face token to them.

use url::Url;

use super::error::EngineError;

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
