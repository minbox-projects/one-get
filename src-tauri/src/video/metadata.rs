//! Parsed media metadata returned by the yt-dlp probe.
//!
//! The shape mirrors the fields the quality model and interface need, and it
//! intentionally carries only scalar values so a probe result can be persisted
//! or compared without retaining the full yt-dlp JSON document.

/// One selectable yt-dlp format.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoFormat {
    /// Stable yt-dlp format identifier used to build a selector.
    pub format_id: String,
    /// Vertical resolution in pixels, when reported.
    pub height: Option<u32>,
    /// Frames per second, when reported.
    pub fps: Option<f64>,
    /// Reported bitrate in bits per second, when available.
    pub bitrate: Option<u64>,
    /// Reported download size in bytes, when available.
    pub filesize: Option<u64>,
    /// Video codec name, or `none` for audio-only formats.
    pub vcodec: Option<String>,
    /// Audio codec name, or `none` for video-only formats.
    pub acodec: Option<String>,
    /// Container extension such as `mp4` or `webm`.
    pub ext: Option<String>,
    /// Whether yt-dlp marked this format as DRM protected.
    pub has_drm: bool,
}

/// Metadata parsed from a single yt-dlp probe result.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoMetadata {
    /// Media title.
    pub title: String,
    /// Duration in seconds, when reported.
    pub duration_secs: Option<f64>,
    /// Uploader or channel name, when reported.
    pub uploader: Option<String>,
    /// Whether yt-dlp reported the item as currently live.
    pub is_live: bool,
    /// Every format yt-dlp returned for the item.
    pub formats: Vec<VideoFormat>,
}
