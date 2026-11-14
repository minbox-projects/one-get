//! Video probing and download support built on the guarded process layer.
//!
//! Target-specific process and environment rules stay inside
//! [`crate::platform::process`]; this module only consumes the guard and applies
//! the shared timeout, output-bound, and minimal-environment policy.

pub mod error;
pub mod metadata;
pub mod probe;
pub mod process;
pub mod quality;

pub use metadata::{VideoFormat, VideoMetadata};
pub use probe::{probe_video, ProbeOptions};
pub use quality::{build_quality_model, ChoiceKind, QualityChoice, QualityModel};
