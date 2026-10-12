//! Audio analysis: decode a track, measure loudness, and extract the full
//! descriptor set the recommender and the dataset are built on.
//!
//! The expensive part — decoding — happens exactly once, and by default it
//! happens *in this process*: [`super::decode`] uses Symphonia and produces
//! container metadata, embedded tags, EBU R128 loudness and a mono buffer at
//! [`dsp::ANALYSIS_RATE`] from a single read. Every feature past that point
//! comes out of one STFT pass over that buffer.
//!
//! ffmpeg is a fallback, not a requirement. It is invoked only when Symphonia
//! cannot open the container — chiefly Opus — in which case the original
//! three-call path (`ffprobe`, `ffmpeg -f f32le`, `ffmpeg ebur128`) runs
//! instead.
//!
//! Every stage is independent and failure is never fatal: the analyzer records
//! what it could learn, notes the reason in `last_error`, and never fails a
//! request.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Instant;

use serde_json::{json, Value};
use tokio::process::Command;
use uuid::Uuid;

use crate::music::decode;
use crate::music::dsp;
use crate::music::models::MusicAnalysis;

/// Bucket count for the stored envelopes. 800 draws a scrubber at desktop
/// width without storing a sample-accurate copy of the track.
pub const WAVEFORM_BUCKETS: usize = 800;

/// Bump when the feature set changes; the re-analysis sweep picks up anything
/// older. v1 was container metadata + loudness + a peak envelope.
pub const ANALYSIS_VERSION: i32 = 2;

/// Hard cap on decoded audio. 20 minutes at 22.05 kHz mono f32 is ~106 MB,
/// which bounds the worst case for an album-length file mistagged as a track.
const MAX_ANALYSIS_SECONDS: u32 = 1200;

/// Beats stored in the grid. Enough for a scrubber overlay, not a full score.
const MAX_BEATS: usize = 512;
const MAX_SEGMENTS: usize = 16;

fn empty(media_id: Uuid, file_id: Option<Uuid>) -> MusicAnalysis {
    MusicAnalysis {
        media_id,
        file_id,
        duration_seconds: None,
        sample_rate: None,
        channels: None,
        bitrate_kbps: None,
        codec: None,
        artist: None,
        album: None,
        album_artist: None,
        track_no: None,
        disc_no: None,
        release_year: None,
        tag_genre: None,
        tags: None,
        loudness_lufs: None,
        peak_db: None,
        dynamic_range: None,
        energy: None,
        waveform: None,
        rms_envelope: None,
        waveform_buckets: None,
        spectral_centroid: None,
        spectral_rolloff: None,
        spectral_flatness: None,
        spectral_bandwidth: None,
        spectral_flux: None,
        zero_crossing_rate: None,
        brightness: None,
        chroma: None,
        musical_key: None,
        musical_mode: None,
        key_confidence: None,
        tempo_bpm: None,
        tempo_confidence: None,
        onset_rate: None,
        beat_grid: None,
        danceability: None,
        valence: None,
        acousticness: None,
        instrumentalness: None,
        speechiness: None,
        liveness: None,
        timbre: None,
        embedding: None,
        segments: None,
        analysis_version: ANALYSIS_VERSION,
        analysis_ms: None,
        analyzed_at: None,
        last_error: None,
    }
}

/// Full analysis for one file.
///
/// Tries the in-process decoder first. It needs no external binary, reads the
/// file once, and yields container metadata, tags, loudness and PCM together.
/// ffmpeg is used only when Symphonia cannot open the file — chiefly Opus,
/// which it does not yet decode — and is not required for the common formats.
pub async fn analyze_file(
    media_id: Uuid,
    file_id: Option<Uuid>,
    path: &Path,
    ffprobe_bin: &str,
    ffmpeg_bin: &str,
) -> MusicAnalysis {
    let started = Instant::now();
    let mut out = empty(media_id, file_id);
    let mut problems: Vec<String> = Vec::new();

    // Decoding is CPU-bound and fully synchronous, so it must not run on a
    // runtime worker: a long track would stall every other task on that thread.
    let owned = path.to_path_buf();
    let decoded = tokio::task::spawn_blocking(move || decode::decode_and_measure(&owned))
        .await
        .unwrap_or_else(|e| Err(format!("analysis task panicked: {e}")));

    match decoded {
        Ok(d) => {
            apply_decoded(&mut out, d);
        }
        Err(in_process_error) => {
            // Fall back to ffmpeg, and report both failures if that fails too —
            // otherwise the real reason is invisible.
            problems.push(format!("decode: {in_process_error}"));

            match probe(path, ffprobe_bin).await {
                Ok(p) => apply_probe(&mut out, p),
                Err(e) => problems.push(format!("probe: {e}")),
            }

            match loudness(path, ffmpeg_bin).await {
                Ok((lufs, peak, range)) => {
                    out.loudness_lufs = lufs;
                    out.peak_db = peak;
                    out.dynamic_range = range;
                    out.energy = lufs.map(energy_from_lufs);
                }
                Err(e) => problems.push(format!("loudness: {e}")),
            }

            match decode_mono(path, ffmpeg_bin).await {
                Ok(samples) => apply_signal(&mut out, &samples),
                Err(e) => problems.push(format!("ffmpeg decode: {e}")),
            }
        }
    }

    if !problems.is_empty() {
        out.last_error = Some(problems.join("; "));
    }
    out.analysis_ms = Some(started.elapsed().as_millis().min(i32::MAX as u128) as i32);
    out
}

/// Perceived energy from integrated loudness: -30 LUFS is quiet, -8 is a
/// loudness-war master. Clamped to 0..1.
fn energy_from_lufs(lufs: f64) -> f64 {
    ((lufs + 30.0) / 22.0).clamp(0.0, 1.0)
}

/// Fold one in-process decode into the row, then run the DSP stage over it.
fn apply_decoded(out: &mut MusicAnalysis, d: decode::Decoded) {
    out.duration_seconds = d.duration_seconds;
    out.sample_rate = Some(d.source_rate as i32);
    out.channels = Some(d.channels as i32);
    out.bitrate_kbps = d.bitrate_kbps;
    out.codec = d.codec;

    apply_tags(out, &d.tags);

    if let Some((lufs, peak, range)) = d.loudness {
        out.loudness_lufs = Some(lufs);
        out.peak_db = Some(peak);
        out.dynamic_range = Some(range);
        out.energy = Some(energy_from_lufs(lufs));
    }

    apply_signal(out, &d.mono);
}

// ------------------------------------------------------------------ probe

struct Probe {
    duration_seconds: Option<f64>,
    sample_rate: Option<i32>,
    channels: Option<i32>,
    bitrate_kbps: Option<i32>,
    codec: Option<String>,
    tags: BTreeMap<String, String>,
}

fn apply_probe(out: &mut MusicAnalysis, p: Probe) {
    out.duration_seconds = p.duration_seconds;
    out.sample_rate = p.sample_rate;
    out.channels = p.channels;
    out.bitrate_kbps = p.bitrate_kbps;
    out.codec = p.codec;

    apply_tags(out, &p.tags);
}

/// Map a tag bag onto the row.
///
/// Shared by both decode paths: Symphonia normalizes to these keys via
/// `StandardTagKey`, and ffprobe's raw container tags are lowercased to match,
/// so one vocabulary covers both.
fn apply_tags(out: &mut MusicAnalysis, tags: &BTreeMap<String, String>) {
    if tags.is_empty() {
        return;
    }
    let tag = |k: &str| tags.get(k).cloned().filter(|v| !v.trim().is_empty());

    out.artist = tag("artist").or_else(|| tag("performer"));
    out.album = tag("album");
    out.album_artist = tag("album_artist").or_else(|| tag("albumartist"));
    out.tag_genre = tag("genre");
    // Track and disc numbers are commonly "3/12" — take the part before the slash.
    out.track_no = tag("track").and_then(|v| leading_number(&v));
    out.disc_no = tag("disc")
        .or_else(|| tag("disctotal"))
        .and_then(|v| leading_number(&v));
    // Dates arrive as "1997", "1997-03-12" or a full timestamp; the year is
    // always the leading four characters.
    out.release_year = tag("date")
        .or_else(|| tag("year"))
        .or_else(|| tag("originalyear"))
        .and_then(|v| v.get(..4).and_then(|y| y.parse().ok()));

    out.tags = serde_json::to_value(tags).ok();
}

fn leading_number(v: &str) -> Option<i32> {
    v.split(['/', '-']).next()?.trim().parse().ok()
}

async fn probe(path: &Path, ffprobe_bin: &str) -> Result<Probe, String> {
    let output = Command::new(ffprobe_bin)
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            "-select_streams",
            "a:0",
        ])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("ffprobe unavailable: {e}"))?;

    if !output.status.success() {
        return Err("ffprobe exited non-zero".into());
    }

    let json: Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("bad ffprobe json: {e}"))?;
    let stream = json["streams"].get(0).cloned().unwrap_or(Value::Null);
    let format = &json["format"];

    let num = |v: &Value| v.as_str().and_then(|s| s.parse::<f64>().ok());

    // Tags live on the container for MP3/FLAC and on the stream for some MKV
    // and MP4 muxes; merge both, lowercased, so lookups are predictable.
    let mut tags = BTreeMap::new();
    for source in [&format["tags"], &stream["tags"]] {
        if let Some(map) = source.as_object() {
            for (k, v) in map {
                if let Some(s) = v.as_str() {
                    tags.insert(k.to_lowercase(), s.to_string());
                }
            }
        }
    }

    Ok(Probe {
        duration_seconds: num(&format["duration"]).or_else(|| num(&stream["duration"])),
        sample_rate: num(&stream["sample_rate"]).map(|v| v as i32),
        channels: stream["channels"].as_i64().map(|v| v as i32),
        bitrate_kbps: num(&format["bit_rate"]).map(|v| (v / 1000.0) as i32),
        codec: stream["codec_name"].as_str().map(str::to_string),
        tags,
    })
}

// --------------------------------------------------------------- loudness

/// Integrated loudness, true peak and loudness range via the ebur128 filter.
/// ffmpeg writes the summary to stderr, so that is what gets parsed.
async fn loudness(
    path: &Path,
    ffmpeg_bin: &str,
) -> Result<(Option<f64>, Option<f64>, Option<f64>), String> {
    let output = Command::new(ffmpeg_bin)
        .arg("-nostdin")
        .arg("-i")
        .arg(path)
        .args(["-vn", "-filter_complex", "ebur128=peak=true", "-f", "null", "-"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("ffmpeg unavailable: {e}"))?;

    let text = String::from_utf8_lossy(&output.stderr);

    // Only the trailing Summary block is authoritative — everything before it
    // is per-frame progress with the same labels.
    let summary = match text.rfind("Summary:") {
        Some(idx) => &text[idx..],
        None => return Err("no ebur128 summary".into()),
    };

    let grab = |label: &str| -> Option<f64> {
        summary.find(label).and_then(|idx| {
            summary[idx + label.len()..]
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<f64>().ok())
        })
    };

    Ok((grab("I:"), grab("Peak:"), grab("LRA:")))
}

// ----------------------------------------------------------------- decode

/// Decode to mono f32 at the analysis rate. `-vn` matters: music files carry
/// cover art as a video stream, and without it ffmpeg tries to map the image.
async fn decode_mono(path: &Path, ffmpeg_bin: &str) -> Result<Vec<f32>, String> {
    let output = Command::new(ffmpeg_bin)
        .arg("-nostdin")
        .args(["-t", &MAX_ANALYSIS_SECONDS.to_string()])
        .arg("-i")
        .arg(path)
        .args([
            "-vn",
            "-ac",
            "1",
            "-ar",
            &dsp::ANALYSIS_RATE.to_string(),
            "-f",
            "f32le",
            "-acodec",
            "pcm_f32le",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("ffmpeg unavailable: {e}"))?;

    if !output.status.success() || output.stdout.is_empty() {
        return Err("no pcm produced".into());
    }

    let bytes = output.stdout;
    let samples: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        // Guard the FFT against NaN/inf from a corrupt decode.
        .map(|s| if s.is_finite() { s.clamp(-1.0, 1.0) } else { 0.0 })
        .collect();

    if samples.is_empty() {
        return Err("empty pcm".into());
    }
    Ok(samples)
}

// --------------------------------------------------------------- features

fn apply_signal(out: &mut MusicAnalysis, samples: &[f32]) {
    let rate = dsp::ANALYSIS_RATE;

    // Envelopes first — these work even on a clip too short to analyze.
    let peaks = dsp::envelope(samples, WAVEFORM_BUCKETS, false);
    let rms = dsp::envelope(samples, WAVEFORM_BUCKETS, true);
    if !peaks.is_empty() {
        out.waveform_buckets = Some(peaks.len() as i32);
        out.waveform = serde_json::to_value(&peaks).ok();
        out.rms_envelope = serde_json::to_value(&rms).ok();
    }

    out.zero_crossing_rate = Some(dsp::zero_crossing_rate(samples) as f64);

    // The decode gives a better duration than the container when a file has a
    // broken header, but only trust it if the probe found nothing.
    if out.duration_seconds.is_none() {
        out.duration_seconds = Some(samples.len() as f64 / rate as f64);
    }

    let Some(spec) = dsp::spectral(samples, rate) else {
        return;
    };

    let centroid = dsp::mean(&spec.centroid) as f64;
    let rolloff = dsp::mean(&spec.rolloff) as f64;
    let flatness = dsp::mean(&spec.flatness) as f64;
    let bandwidth = dsp::mean(&spec.bandwidth) as f64;
    let flux = dsp::mean(&spec.flux) as f64;

    out.spectral_centroid = Some(centroid);
    out.spectral_rolloff = Some(rolloff);
    out.spectral_flatness = Some(flatness);
    out.spectral_bandwidth = Some(bandwidth);
    out.spectral_flux = Some(flux);
    // Brightness is the centroid mapped onto 0..1 against the analysis Nyquist,
    // so it stays comparable across tracks regardless of source rate.
    out.brightness = Some((centroid / (rate as f64 / 2.0)).clamp(0.0, 1.0));

    out.chroma = serde_json::to_value(spec.chroma).ok();
    if let Some((key, is_major, confidence)) = dsp::detect_key(&spec.chroma) {
        out.musical_key = Some(key);
        out.musical_mode = Some(if is_major { 1 } else { 0 });
        out.key_confidence = Some(confidence);
    }

    // Onset rate: how many flux peaks per second clear one standard deviation.
    let flux_mean = dsp::mean(&spec.flux);
    let flux_sd = dsp::std_dev(&spec.flux);
    let onsets = spec
        .flux
        .iter()
        .filter(|f| **f > flux_mean + flux_sd)
        .count();
    let frame_rate = rate as f64 / dsp::HOP as f64;
    let seconds = spec.flux.len() as f64 / frame_rate;
    if seconds > 0.0 {
        out.onset_rate = Some(onsets as f64 / seconds);
    }

    if let Some((bpm, confidence)) = dsp::tempo(&spec.flux, rate) {
        out.tempo_bpm = Some(bpm);
        out.tempo_confidence = Some(confidence);
        let beats = dsp::beat_grid(&spec.flux, rate, bpm, MAX_BEATS);
        if !beats.is_empty() {
            out.beat_grid = serde_json::to_value(&beats).ok();
        }
    }

    let boundaries = dsp::segments(&spec.mfcc_frames, rate, MAX_SEGMENTS);
    if !boundaries.is_empty() {
        out.segments = serde_json::to_value(&boundaries).ok();
    }

    out.timbre = serde_json::to_value(spec.mfcc_mean).ok();

    derive_descriptors(out, &spec);
    out.embedding = build_embedding(out, &spec);
}

/// The high-level descriptors.
///
/// These are heuristics over the measured features, not learned models: each
/// one combines the signals that separate the trait in practice, then clamps
/// to 0..1. They are stable and explainable, which is what the recommender and
/// the dataset need — not that they match any particular vendor's numbers.
fn derive_descriptors(out: &mut MusicAnalysis, spec: &dsp::Spectral) {
    let energy = out.energy.unwrap_or(0.5);
    let zcr = out.zero_crossing_rate.unwrap_or(0.0);
    let flatness = out.spectral_flatness.unwrap_or(0.5);
    let brightness = out.brightness.unwrap_or(0.5);
    let tempo_conf = out.tempo_confidence.unwrap_or(0.0);
    let dynamic_range = out.dynamic_range.unwrap_or(7.0);

    // Danceability: a steady pulse, in the range people move to, with weight
    // in the low end. A confident 124 BPM track with real bass scores high.
    let tempo_fit = out
        .tempo_bpm
        .map(|b| 1.0 - ((b - 118.0).abs() / 70.0).clamp(0.0, 1.0))
        .unwrap_or(0.3);
    out.danceability =
        Some((0.45 * tempo_fit + 0.35 * tempo_conf + 0.20 * spec.low_ratio as f64).clamp(0.0, 1.0));

    // Valence: major mode, bright, and moving. Minor-key ballads land low.
    let mode_lift = match out.musical_mode {
        Some(1) => 0.30 * out.key_confidence.unwrap_or(0.5),
        Some(0) => -0.20 * out.key_confidence.unwrap_or(0.5),
        _ => 0.0,
    };
    let tempo_lift = out
        .tempo_bpm
        .map(|b| ((b - 60.0) / 120.0).clamp(0.0, 1.0))
        .unwrap_or(0.4);
    out.valence =
        Some((0.35 + mode_lift + 0.25 * brightness + 0.20 * tempo_lift - 0.15 * flatness).clamp(0.0, 1.0));

    // Acousticness: wide dynamics, tonal rather than noisy, and little energy
    // up top. A compressed electronic master is the opposite on all four —
    // synthesised highs are exactly what `high_ratio` catches.
    let dr_norm = (dynamic_range / 15.0).clamp(0.0, 1.0);
    out.acousticness = Some(
        (0.35 * dr_norm
            + 0.30 * (1.0 - flatness)
            + 0.20 * (1.0 - brightness)
            + 0.15 * (1.0 - (spec.high_ratio as f64 * 4.0).clamp(0.0, 1.0)))
        .clamp(0.0, 1.0),
    );

    // Speechiness: speech is noisy in the vocal band and crosses zero often,
    // with an irregular envelope. Sung vocals over music score much lower.
    let vocal = spec.vocal_ratio as f64;
    let flux_var = (dsp::std_dev(&spec.flux) / dsp::mean(&spec.flux).max(1e-6)).min(3.0) as f64;
    out.speechiness = Some(
        (0.40 * (zcr * 8.0).clamp(0.0, 1.0) + 0.35 * vocal + 0.25 * (flux_var / 3.0)
            - 0.30 * tempo_conf)
            .clamp(0.0, 1.0),
    );

    // Instrumentalness: the inverse claim. Little energy in the vocal band and
    // a low speechiness reading together suggest nothing is being sung.
    out.instrumentalness = Some(
        (0.55 * (1.0 - vocal) + 0.45 * (1.0 - out.speechiness.unwrap_or(0.3))).clamp(0.0, 1.0),
    );

    // Liveness: room sound is broadband and never silent between phrases, so a
    // high noise floor plus high flatness is the tell.
    let floor = spec
        .flux
        .iter()
        .fold(f32::MAX, |acc, f| acc.min(*f))
        .max(0.0) as f64;
    let floor_norm = (floor / dsp::mean(&spec.flux).max(1e-6) as f64).clamp(0.0, 1.0);
    out.liveness = Some((0.55 * floor_norm + 0.45 * flatness).clamp(0.0, 1.0));

    let _ = energy;
}

/// The similarity vector: chroma, timbre and the descriptors, L2-normalized so
/// cosine similarity is a dot product.
///
/// Chroma is rotated to be relative to the detected key, so two recordings of
/// the same song in different keys land near each other instead of 5 semitones
/// apart.
fn build_embedding(out: &MusicAnalysis, spec: &dsp::Spectral) -> Option<Value> {
    let mut v: Vec<f32> = Vec::with_capacity(32);

    let chroma_total: f32 = spec.chroma.iter().sum();
    let rotation = out.musical_key.unwrap_or(0).clamp(0, 11) as usize;
    for i in 0..12 {
        let value = spec.chroma[(i + rotation) % 12];
        v.push(if chroma_total > 1e-6 { value / chroma_total } else { 0.0 });
    }

    // MFCC 0 is overall level, which the loudness fields already carry and
    // which would otherwise dominate the distance; skip it.
    for k in 1..dsp::MFCC_COUNT {
        v.push((spec.mfcc_mean[k] / 8.0).clamp(-1.0, 1.0));
    }

    let f = |x: Option<f64>, default: f64| x.unwrap_or(default) as f32;
    v.push(f(out.energy, 0.5));
    v.push(f(out.danceability, 0.5));
    v.push(f(out.valence, 0.5));
    v.push(f(out.acousticness, 0.5));
    v.push(f(out.instrumentalness, 0.5));
    v.push(f(out.brightness, 0.5));
    v.push(out.tempo_bpm.map(|b| (b / 220.0) as f32).unwrap_or(0.5));

    dsp::l2_normalize(&mut v);
    serde_json::to_value(&v).ok()
}

/// Cosine similarity between two stored embeddings.
pub fn cosine(a: &Value, b: &Value) -> Option<f64> {
    let a: Vec<f32> = serde_json::from_value(a.clone()).ok()?;
    let b: Vec<f32> = serde_json::from_value(b.clone()).ok()?;
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    // Both vectors are stored L2-normalized, so the dot product is the cosine.
    Some(a.iter().zip(&b).map(|(x, y)| (x * y) as f64).sum())
}

/// Summary line for logs and the admin console.
pub fn describe(a: &MusicAnalysis) -> String {
    json!({
        "tempo": a.tempo_bpm,
        "key": a.key_name(),
        "energy": a.energy,
        "danceability": a.danceability,
        "valence": a.valence,
        "ms": a.analysis_ms,
    })
    .to_string()
}
