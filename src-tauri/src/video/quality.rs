//! Deterministic quality choices derived from probed formats.
//!
//! Video and audio-only formats stay in separate lists, DRM and non-video
//! formats never become video choices, and ordering is stable so the same
//! metadata always produces the same dialog and default.

use std::cmp::Ordering;

use super::metadata::{VideoFormat, VideoMetadata};

/// Whether a choice downloads muxed video or audio-only media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceKind {
    /// A video format, optionally merged with the best audio.
    Video,
    /// An audio-only format.
    AudioOnly,
}

/// One selectable quality choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityChoice {
    /// yt-dlp format identifier.
    pub id: String,
    /// Whether this choice carries video.
    pub kind: ChoiceKind,
    /// Human-readable resolution or bitrate label.
    pub label: String,
    /// Estimated output size, or unknown when the inputs are not reported.
    pub size_bytes: Option<u64>,
    /// Whether the container is MP4.
    pub mp4_compatible: bool,
    /// yt-dlp format selector for this choice.
    pub format_selector: String,
}

/// Ordered quality choices and the default video selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityModel {
    /// Video choices ordered from highest to lowest quality.
    pub video: Vec<QualityChoice>,
    /// Audio-only choices ordered from highest to lowest bitrate.
    pub audio: Vec<QualityChoice>,
    /// Index into [`QualityModel::video`] for the default choice.
    pub default_index: Option<usize>,
}

/// Build the deterministic quality model for probed metadata.
pub fn build_quality_model(metadata: &VideoMetadata) -> QualityModel {
    let mut audio_formats: Vec<&VideoFormat> = metadata
        .formats
        .iter()
        .filter(|format| !format.has_drm && is_audio_only(format))
        .collect();
    audio_formats.sort_by(|left, right| audio_order(left, right));

    let best_audio_size = audio_formats.iter().find_map(|format| format.filesize);
    let audio = audio_formats
        .iter()
        .map(|format| audio_choice(format))
        .collect();

    let mut video_formats: Vec<&VideoFormat> = metadata
        .formats
        .iter()
        .filter(|format| !format.has_drm && is_video(format))
        .collect();
    video_formats.sort_by(|left, right| video_order(left, right));

    let video: Vec<QualityChoice> = video_formats
        .iter()
        .map(|format| video_choice(format, best_audio_size))
        .collect();
    let default_index = if video.is_empty() {
        None
    } else {
        Some(
            video
                .iter()
                .position(|choice| choice.mp4_compatible)
                .unwrap_or(0),
        )
    };

    QualityModel {
        video,
        audio,
        default_index,
    }
}

fn is_video(format: &VideoFormat) -> bool {
    has_codec(&format.vcodec)
}

fn is_audio_only(format: &VideoFormat) -> bool {
    !has_codec(&format.vcodec) && has_codec(&format.acodec)
}

fn has_codec(codec: &Option<String>) -> bool {
    matches!(codec.as_deref(), Some(value) if !value.is_empty() && !value.eq_ignore_ascii_case("none"))
}

fn is_mp4(format: &VideoFormat) -> bool {
    format
        .ext
        .as_deref()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"))
}

fn video_order(left: &VideoFormat, right: &VideoFormat) -> Ordering {
    right
        .height
        .unwrap_or(0)
        .cmp(&left.height.unwrap_or(0))
        .then_with(|| right.fps.unwrap_or(0.0).total_cmp(&left.fps.unwrap_or(0.0)))
        .then_with(|| right.bitrate.unwrap_or(0).cmp(&left.bitrate.unwrap_or(0)))
        .then_with(|| right.filesize.unwrap_or(0).cmp(&left.filesize.unwrap_or(0)))
        .then_with(|| left.format_id.cmp(&right.format_id))
}

fn audio_order(left: &VideoFormat, right: &VideoFormat) -> Ordering {
    right
        .bitrate
        .unwrap_or(0)
        .cmp(&left.bitrate.unwrap_or(0))
        .then_with(|| right.filesize.unwrap_or(0).cmp(&left.filesize.unwrap_or(0)))
        .then_with(|| left.format_id.cmp(&right.format_id))
}

fn video_choice(format: &VideoFormat, best_audio_size: Option<u64>) -> QualityChoice {
    let video_only = !has_codec(&format.acodec);
    let size_bytes = match (video_only, format.filesize) {
        (true, Some(video_size)) => best_audio_size.map(|audio| video_size.saturating_add(audio)),
        (_, size) => size,
    };
    QualityChoice {
        id: format.format_id.clone(),
        kind: ChoiceKind::Video,
        label: video_label(format),
        size_bytes,
        mp4_compatible: is_mp4(format),
        format_selector: if video_only {
            format!("{}+bestaudio", format.format_id)
        } else {
            format.format_id.clone()
        },
    }
}

fn audio_choice(format: &VideoFormat) -> QualityChoice {
    QualityChoice {
        id: format.format_id.clone(),
        kind: ChoiceKind::AudioOnly,
        label: match format.bitrate {
            Some(bitrate) => format!("Audio {} kbps", bitrate / 1000),
            None => "Audio".to_string(),
        },
        size_bytes: format.filesize,
        mp4_compatible: is_mp4(format),
        format_selector: format.format_id.clone(),
    }
}

fn video_label(format: &VideoFormat) -> String {
    let mut label = match format.height {
        Some(height) => format!("{height}p"),
        None => "Video".to_string(),
    };
    if let Some(fps) = format.fps {
        label.push_str(&fps.to_string());
    }
    label
}
