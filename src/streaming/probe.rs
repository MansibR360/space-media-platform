use std::path::Path;
use std::process::Stdio;

use serde::Deserialize;
use tokio::process::Command;

use crate::error::{ApiError, ApiResult};

#[derive(Debug, Clone, Default)]
pub struct ProbeResult {
    pub duration_seconds: f64,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub bitrate: Option<i32>,
    pub audio_tracks: Vec<AudioTrack>,
    pub subtitle_tracks: Vec<SubtitleTrack>,
    pub hdr: bool,
    pub replaygain_db: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct AudioTrack {
    pub index: i32,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub channels: Option<i32>,
    pub is_default: bool,
}

#[derive(Debug, Clone)]
pub struct SubtitleTrack {
    pub index: i32,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FfprobeOutput {
    #[serde(default)]
    streams: Vec<FfStream>,
    #[serde(default)]
    format: FfFormat,
}

#[derive(Debug, Deserialize)]
struct FfStream {
    index: i32,
    codec_type: Option<String>,
    codec_name: Option<String>,
    channels: Option<i32>,
    #[serde(default)]
    disposition: FfDisposition,
    tags: Option<serde_json::Value>,
    width: Option<i32>,
    height: Option<i32>,
    color_transfer: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FfDisposition {
    default: Option<i32>,
}

#[derive(Debug, Default, Deserialize)]
struct FfFormat {
    duration: Option<String>,
    bit_rate: Option<String>,
    tags: Option<serde_json::Value>,
}

fn tag(stream: &FfStream, key: &str) -> Option<String> {
    stream
        .tags
        .as_ref()?
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

#[allow(dead_code)]
pub async fn available(ffprobe_bin: &str) -> bool {
    Command::new(ffprobe_bin)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok()
}

pub async fn probe(ffprobe_bin: &str, path: &Path) -> ApiResult<ProbeResult> {
    let output = Command::new(ffprobe_bin)
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .await
        .map_err(|e| ApiError::Unavailable(format!("ffprobe not available: {e}")))?;

    if !output.status.success() {
        return Err(ApiError::Unavailable(format!(
            "ffprobe failed with status {}",
            output.status
        )));
    }

    let parsed: FfprobeOutput = serde_json::from_slice(&output.stdout)
        .map_err(|e| ApiError::internal_msg(format!("ffprobe json parse failed: {e}")))?;

    let mut result = ProbeResult::default();
    result.duration_seconds = parsed
        .format
        .duration
        .as_deref()
        .and_then(|d| d.parse().ok())
        .unwrap_or(0.0);
    result.bitrate = parsed
        .format
        .bit_rate
        .as_deref()
        .and_then(|b| b.parse().ok());
    if let Some(tags) = &parsed.format.tags {
        if let Some(gain) = tags.get("REPLAYGAIN_TRACK_GAIN").and_then(|g| g.as_str()) {
            result.replaygain_db = gain.trim().trim_end_matches("dB").trim().parse().ok();
        }
    }

    for stream in parsed.streams {
        match stream.codec_type.as_deref() {
            Some("video") => {
                if result.video_codec.is_none() {
                    result.video_codec = stream.codec_name.clone();
                    result.width = stream.width;
                    result.height = stream.height;
                    result.hdr = matches!(
                        stream.color_transfer.as_deref(),
                        Some("smpte2084") | Some("arib-std-b67")
                    );
                }
            }
            Some("audio") => {
                if result.audio_codec.is_none() {
                    result.audio_codec = stream.codec_name.clone();
                }
                result.audio_tracks.push(AudioTrack {
                    index: stream.index,
                    codec: stream.codec_name.clone(),
                    language: tag(&stream, "language"),
                    title: tag(&stream, "title"),
                    channels: stream.channels,
                    is_default: stream.disposition.default.unwrap_or(0) == 1,
                });
            }
            Some("subtitle") => {
                result.subtitle_tracks.push(SubtitleTrack {
                    index: stream.index,
                    codec: stream.codec_name.clone(),
                    language: tag(&stream, "language"),
                    title: tag(&stream, "title"),
                });
            }
            _ => {}
        }
    }

    Ok(result)
}
