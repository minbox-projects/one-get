//! Typed, validated component catalog built on the strict signed-manifest verifier.
//!
//! The signature and duplicate-key checks stay in [`super::manifest`]; this
//! module parses the verified JSON into typed entries and applies the catalog
//! policy before any caller can download, write, or execute a component.

use std::collections::BTreeMap;

use serde::Deserialize;
use url::{Host, Url};

use super::error::KernelError;
use super::manifest::{verify_manifest, ManifestError};

/// The only supported catalog schema version.
pub const CATALOG_SCHEMA_VERSION: u32 = 1;

/// Maximum accepted artifact URL length in bytes.
const MAX_ARTIFACT_URL_BYTES: usize = 8192;

/// A shipping target with a declared artifact set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Target {
    /// macOS on Apple silicon.
    MacosArm64,
    /// macOS on Intel.
    MacosX64,
    /// Windows on x86-64.
    WindowsX64,
}

impl Target {
    /// The target this build runs on.
    ///
    /// Non-shipping platforms fall back to the first declared target so the
    /// module still compiles for local tooling; catalog selection itself only
    /// uses the returned key.
    pub fn current() -> Self {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            Self::MacosArm64
        }
        #[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
        {
            Self::MacosX64
        }
        #[cfg(target_os = "windows")]
        {
            Self::WindowsX64
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Self::MacosArm64
        }
    }

    /// The catalog key for this target.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MacosArm64 => "macos-arm64",
            Self::MacosX64 => "macos-x64",
            Self::WindowsX64 => "windows-x64",
        }
    }

    fn parse(key: &str) -> Option<Self> {
        match key {
            "macos-arm64" => Some(Self::MacosArm64),
            "macos-x64" => Some(Self::MacosX64),
            "windows-x64" => Some(Self::WindowsX64),
            _ => None,
        }
    }
}

/// The archive format of an artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveType {
    /// A single executable file.
    Binary,
    /// A zip archive with declared member files.
    Zip,
}

/// One declared output file and the component role it fills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRole {
    /// Stable role name such as `yt-dlp`, `ffmpeg`, `ffprobe`, or `deno`.
    pub role: String,
    /// Output file name inside an archive, or the installed file name for a binary.
    pub name: String,
}

/// A downloadable artifact for one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// Artifact URL accepted by the catalog URL policy.
    pub url: String,
    /// Lowercase hexadecimal SHA-256 of the artifact bytes.
    pub sha256: String,
    /// Declared download size in bytes.
    pub size: u64,
    /// Archive format.
    pub archive: ArchiveType,
    /// Declared output files.
    pub files: Vec<FileRole>,
}

/// One component and its per-target artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentEntry {
    /// Component name: `yt-dlp`, `ffmpeg`, or `deno`.
    pub name: String,
    /// Human-readable version label.
    pub display_version: String,
    /// Monotonically increasing update sequence.
    pub sequence: u64,
    /// License label.
    pub license: String,
    /// Artifacts keyed by target.
    pub artifacts: BTreeMap<Target, Artifact>,
}

/// A fully validated component catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentCatalog {
    /// Catalog schema version, always [`CATALOG_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Catalog generation timestamp supplied by the signer.
    pub generated_at: String,
    /// Components keyed by name.
    pub components: BTreeMap<String, ComponentEntry>,
}

/// Verify a signed catalog and validate its entire content.
///
/// A missing or empty trust key is reported as `COMPONENT_TRUST_UNCONFIGURED`.
/// Signature, duplicate-key, size, schema, target, URL, archive, role, and
/// output-name violations return a typed failure before any caller can act on
/// the catalog.
pub fn verify_catalog(
    bytes: &[u8],
    public_key_hex: Option<&str>,
    target: Target,
    allow_loopback_http: bool,
) -> Result<ComponentCatalog, KernelError> {
    let public_key = public_key_hex
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            KernelError::new(
                "COMPONENT_TRUST_UNCONFIGURED",
                "no component trust public key is configured",
            )
        })?;

    let value = verify_manifest(bytes, public_key).map_err(map_manifest_error)?;
    let raw: RawCatalog = serde_json::from_value(value).map_err(|_| {
        KernelError::new(
            "COMPONENT_CATALOG_INVALID",
            "component catalog does not match the required schema",
        )
    })?;

    validate_catalog(raw, target, allow_loopback_http)
}

fn map_manifest_error(error: ManifestError) -> KernelError {
    let code = match error.code.as_str() {
        "manifest_too_large" => "COMPONENT_MANIFEST_TOO_LARGE",
        "duplicate_key" => "COMPONENT_MANIFEST_DUPLICATE_KEY",
        "missing_signature" | "invalid_signature" | "signature_mismatch" => {
            "COMPONENT_SIGNATURE_INVALID"
        }
        "invalid_public_key" => "COMPONENT_TRUST_INVALID",
        "non_object_root" | "invalid_json" => "COMPONENT_MANIFEST_INVALID",
        _ => "COMPONENT_MANIFEST_INVALID",
    };
    KernelError::new(code, error.message)
}

fn validate_catalog(
    raw: RawCatalog,
    target: Target,
    allow_loopback_http: bool,
) -> Result<ComponentCatalog, KernelError> {
    if raw.schema_version != CATALOG_SCHEMA_VERSION {
        return Err(invalid("catalog schema version is not supported"));
    }
    if raw.generated_at.trim().is_empty() {
        return Err(invalid("catalog generated time is empty"));
    }

    let mut components = BTreeMap::new();
    for raw_component in raw.components {
        let name = raw_component.name;
        if expected_roles(&name).is_none() {
            return Err(invalid("catalog declares an unexpected component"));
        }
        if components.contains_key(&name) {
            return Err(invalid("catalog declares a duplicate component"));
        }
        if raw_component.sequence == 0 {
            return Err(invalid("component sequence must be positive"));
        }
        if raw_component.version.trim().is_empty() {
            return Err(invalid("component display version is empty"));
        }
        if raw_component.license.trim().is_empty() {
            return Err(invalid("component license is empty"));
        }

        let mut artifacts = BTreeMap::new();
        for (key, raw_artifact) in raw_component.artifacts {
            let artifact_target = Target::parse(&key)
                .ok_or_else(|| invalid("catalog declares an unsupported target key"))?;
            let artifact = validate_artifact(raw_artifact, &name, allow_loopback_http)?;
            artifacts.insert(artifact_target, artifact);
        }

        if !artifacts.contains_key(&target) {
            return Err(KernelError::new(
                "COMPONENT_UNSUPPORTED",
                format!(
                    "component `{name}` has no artifact for the current target {}",
                    target.as_str()
                ),
            ));
        }

        components.insert(
            name.clone(),
            ComponentEntry {
                name,
                display_version: raw_component.version,
                sequence: raw_component.sequence,
                license: raw_component.license,
                artifacts,
            },
        );
    }

    if components.len() != 3 {
        return Err(invalid(
            "catalog must declare exactly yt-dlp, ffmpeg, and deno",
        ));
    }

    Ok(ComponentCatalog {
        schema_version: raw.schema_version,
        generated_at: raw.generated_at,
        components,
    })
}

fn validate_artifact(
    raw: RawArtifact,
    component: &str,
    allow_loopback_http: bool,
) -> Result<Artifact, KernelError> {
    let archive = match raw.archive.as_str() {
        "binary" => ArchiveType::Binary,
        "zip" => ArchiveType::Zip,
        _ => return Err(invalid("artifact archive type is not supported")),
    };
    if raw.size == 0 {
        return Err(invalid("artifact size must be positive"));
    }
    if !is_lowercase_hex_sha256(&raw.sha256) {
        return Err(invalid(
            "artifact sha256 must be 64 lowercase hexadecimal characters",
        ));
    }
    validate_artifact_url(&raw.url, allow_loopback_http)?;

    let roles = expected_roles(component).ok_or_else(|| invalid("unexpected component"))?;
    let mut declared_roles: Vec<&str> = raw.files.iter().map(|file| file.role.as_str()).collect();
    declared_roles.sort_unstable();
    if declared_roles.as_slice() != roles {
        return Err(invalid(
            "artifact file roles do not match the component requirements",
        ));
    }

    let mut names = Vec::with_capacity(raw.files.len());
    for file in &raw.files {
        validate_output_name(&file.name)?;
        names.push(file.name.to_lowercase());
    }
    names.sort_unstable();
    if names.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(invalid("artifact output names must be unique"));
    }

    Ok(Artifact {
        url: raw.url,
        sha256: raw.sha256,
        size: raw.size,
        archive,
        files: raw
            .files
            .into_iter()
            .map(|file| FileRole {
                role: file.role,
                name: file.name,
            })
            .collect(),
    })
}

fn expected_roles(component: &str) -> Option<&'static [&'static str]> {
    match component {
        "yt-dlp" => Some(&["yt-dlp"]),
        "ffmpeg" => Some(&["ffmpeg", "ffprobe"]),
        "deno" => Some(&["deno"]),
        _ => None,
    }
}

fn validate_artifact_url(value: &str, allow_loopback_http: bool) -> Result<(), KernelError> {
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

fn validate_output_name(name: &str) -> Result<(), KernelError> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(invalid("artifact output name is empty or a dot segment"));
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err(invalid(
            "artifact output name must be a single file name without separators",
        ));
    }
    if name.trim() != name || name.ends_with('.') {
        return Err(invalid(
            "artifact output name has unsafe boundary characters",
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(invalid("artifact output name contains control characters"));
    }
    if name
        .chars()
        .any(|character| matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
    {
        return Err(invalid(
            "artifact output name contains a reserved platform character",
        ));
    }
    Ok(())
}

fn is_lowercase_hex_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn invalid(message: impl Into<String>) -> KernelError {
    KernelError::new("COMPONENT_CATALOG_INVALID", message)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCatalog {
    schema_version: u32,
    generated_at: String,
    components: Vec<RawComponent>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawComponent {
    name: String,
    version: String,
    sequence: u64,
    license: String,
    artifacts: BTreeMap<String, RawArtifact>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawArtifact {
    url: String,
    sha256: String,
    size: u64,
    archive: String,
    files: Vec<RawFile>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFile {
    role: String,
    name: String,
}
