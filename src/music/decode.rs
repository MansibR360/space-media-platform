//! In-process audio decoding.
//!
//! Symphonia decodes the file in this process — no ffmpeg, no subprocess, no
//! pipe — and the same pass feeds three consumers at once:
//!
//!   1. `ebur128` gets the native-rate, native-channel stream, so loudness is
//!      the real EBU R128 measurement rather than an approximation from a
//!      downmix.
//!   2. The DSP stage gets a mono buffer at [`dsp::ANALYSIS_RATE`].
//!   3. Container metadata and embedded tags come off the same reader.
//!
//! This replaces what previously took two ffmpeg invocations and an ffprobe,
//! and it means a machine with no ffmpeg installed can still analyze music.
//! ffmpeg remains the fallback in [`super::analysis`] for anything Symphonia
//! cannot open (notably Opus, which Symphonia does not yet decode).

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use ebur128::{EbuR128, Mode};
use symphonia::core::audio::{SampleBuffer, SignalSpec};
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTagKey};
use symphonia::core::probe::Hint;

use crate::music::dsp;

/// Everything one decode pass yields.
pub struct Decoded {
    /// Mono, resampled to [`dsp::ANALYSIS_RATE`]. What the DSP stage reads.
    pub mono: Vec<f32>,
    pub source_rate: u32,
    pub channels: u16,
    pub duration_seconds: Option<f64>,
    pub codec: Option<String>,
    pub bitrate_kbps: Option<i32>,
    pub tags: BTreeMap<String, String>,
    /// `(integrated LUFS, true peak dBFS, loudness range LU)`.
    pub loudness: Option<(f64, f64, f64)>,
}

/// Hard cap on decoded audio, matching the ffmpeg path: 20 minutes bounds the
/// worst case for an album-length file mistagged as a single track.
const MAX_ANALYSIS_SECONDS: f64 = 1200.0;

/// Decode a file and measure it.
///
/// CPU-bound and fully synchronous — callers run it under `spawn_blocking`.
pub fn decode_and_measure(path: &Path) -> Result<Decoded, String> {
    let file = File::open(path).map_err(|e| format!("open: {e}"))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());

    // The extension is only a hint; Symphonia still sniffs the container.
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions {
                enable_gapless: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("unsupported container: {e}"))?;

    let mut format = probed.format;
    let mut metadata = probed.metadata;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "no audio track".to_string())?;

    let track_id = track.id;
    let params = track.codec_params.clone();

    let codec = symphonia::default::get_codecs()
        .get_codec(params.codec)
        .map(|d| d.short_name.to_string());

    // Duration from the container is exact when present; otherwise it is
    // recovered from the sample count once decoding finishes.
    let declared_duration = match (params.n_frames, params.sample_rate) {
        (Some(frames), Some(rate)) if rate > 0 => Some(frames as f64 / rate as f64),
        _ => None,
    };

    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(|e| format!("no decoder: {e}"))?;

    // ---- state built up across packets ----
    let mut meter: Option<EbuR128> = None;
    let mut resampler: Option<MonoResampler> = None;
    let mut spec: Option<SignalSpec> = None;
    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut mono: Vec<f32> = Vec::new();
    let mut frames_decoded: u64 = 0;
    let mut max_frames: u64 = u64::MAX;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // Clean end of stream. Symphonia signals it as an EOF io error.
            Err(SymphoniaError::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(format!("read: {e}")),
        };

        if packet.track_id() != track_id {
            continue;
        }

        let audio = match decoder.decode(&packet) {
            Ok(buf) => buf,
            // A damaged packet is skippable; only a fatal error aborts. This is
            // why a partially corrupt file still yields usable analysis.
            Err(SymphoniaError::DecodeError(_)) | Err(SymphoniaError::IoError(_)) => continue,
            Err(e) => return Err(format!("decode: {e}")),
        };

        // The first decoded buffer is what reveals the true rate and layout.
        if spec.is_none() {
            let s = *audio.spec();
            let channels = s.channels.count().max(1) as u16;
            max_frames = (MAX_ANALYSIS_SECONDS * s.rate as f64) as u64;

            meter = EbuR128::new(
                channels as u32,
                s.rate,
                Mode::I | Mode::TRUE_PEAK | Mode::LRA,
            )
            .ok();
            resampler = Some(MonoResampler::new(s.rate, dsp::ANALYSIS_RATE, channels));
            // Pre-size the mono buffer when the length is known.
            if let Some(d) = declared_duration {
                mono.reserve((d.min(MAX_ANALYSIS_SECONDS) * dsp::ANALYSIS_RATE as f64) as usize);
            }
            spec = Some(s);
        }

        let s = spec.expect("spec set above");
        let buffer = sample_buf
            .get_or_insert_with(|| SampleBuffer::<f32>::new(audio.capacity() as u64, s));
        buffer.copy_interleaved_ref(audio);
        let interleaved = buffer.samples();

        let channels = s.channels.count().max(1);
        let frames = (interleaved.len() / channels) as u64;

        if let Some(m) = meter.as_mut() {
            // A failed measurement must not abort the decode; loudness is one
            // of several outputs, and the rest are still worth having.
            let _ = m.add_frames_f32(interleaved);
        }
        if let Some(r) = resampler.as_mut() {
            r.push(interleaved, &mut mono);
        }

        frames_decoded += frames;
        if frames_decoded >= max_frames {
            break;
        }
    }

    let Some(s) = spec else {
        return Err("no audio decoded".into());
    };

    let loudness = meter.as_ref().and_then(read_loudness);

    let duration_seconds = declared_duration.or_else(|| {
        (s.rate > 0).then(|| frames_decoded as f64 / s.rate as f64)
    });

    // Tags live in the pre-format metadata for MP3 (ID3v2) and in the format
    // reader for FLAC/Ogg/MP4, so both are merged.
    let mut tags = BTreeMap::new();
    if let Some(rev) = metadata.get().as_ref().and_then(|m| m.current()) {
        collect_tags(rev, &mut tags);
    }
    if let Some(rev) = format.metadata().current() {
        collect_tags(rev, &mut tags);
    }

    Ok(Decoded {
        mono,
        source_rate: s.rate,
        channels: s.channels.count().max(1) as u16,
        duration_seconds,
        codec,
        bitrate_kbps: bitrate_kbps(path, duration_seconds),
        tags,
        loudness,
    })
}

/// Average bitrate from file size over duration.
///
/// Symphonia exposes no container bitrate field, and for a VBR file this is
/// the same average ffprobe reports anyway.
pub fn bitrate_kbps(path: &Path, duration_seconds: Option<f64>) -> Option<i32> {
    let seconds = duration_seconds.filter(|d| *d > 0.5)?;
    let bytes = std::fs::metadata(path).ok()?.len();
    Some(((bytes as f64 * 8.0) / seconds / 1000.0) as i32)
}

fn read_loudness(meter: &EbuR128) -> Option<(f64, f64, f64)> {
    let integrated = meter.loudness_global().ok()?;
    // A silent or too-short track reports -inf; there is nothing to record.
    if !integrated.is_finite() {
        return None;
    }
    let range = meter.loudness_range().unwrap_or(0.0);

    // True peak is linear per channel; report the loudest as dBFS to match
    // what ffmpeg's ebur128 filter prints.
    let mut peak_linear = 0.0f64;
    for ch in 0..meter.channels() {
        if let Ok(p) = meter.true_peak(ch) {
            peak_linear = peak_linear.max(p);
        }
    }
    let peak_db = if peak_linear > 0.0 {
        20.0 * peak_linear.log10()
    } else {
        -99.0
    };

    Some((integrated, peak_db, range))
}

fn collect_tags(
    rev: &symphonia::core::meta::MetadataRevision,
    out: &mut BTreeMap<String, String>,
) {
    for tag in rev.tags() {
        let value = tag.value.to_string();
        if value.trim().is_empty() {
            continue;
        }
        // Prefer the standardized key so FLAC's "ALBUMARTIST" and MP4's
        // "aART" both land under one name; fall back to the raw key.
        let key = match tag.std_key {
            Some(StandardTagKey::Artist) => "artist",
            Some(StandardTagKey::Album) => "album",
            Some(StandardTagKey::AlbumArtist) => "album_artist",
            Some(StandardTagKey::TrackTitle) => "title",
            Some(StandardTagKey::Genre) => "genre",
            Some(StandardTagKey::Date) => "date",
            Some(StandardTagKey::OriginalDate) => "originalyear",
            Some(StandardTagKey::TrackNumber) => "track",
            Some(StandardTagKey::DiscNumber) => "disc",
            _ => &tag.key.to_lowercase(),
        };
        out.entry(key.to_string()).or_insert(value);
    }
}

/// Downmix to mono and resample to the analysis rate in one pass.
///
/// Decimation without filtering would alias high frequencies down into the
/// band the spectral features measure, which would corrupt brightness and the
/// centroid. Averaging every input frame that falls inside an output sample is
/// a box filter — crude, but a real low-pass, and it costs nothing on top of
/// the downmix that has to happen anyway.
struct MonoResampler {
    ratio: f64,
    channels: usize,
    /// Fractional position within the current output sample.
    position: f64,
    accumulator: f32,
    count: u32,
}

impl MonoResampler {
    fn new(source_rate: u32, target_rate: u32, channels: u16) -> Self {
        Self {
            ratio: if source_rate == 0 {
                1.0
            } else {
                target_rate as f64 / source_rate as f64
            },
            channels: channels.max(1) as usize,
            position: 0.0,
            accumulator: 0.0,
            count: 0,
        }
    }

    fn push(&mut self, interleaved: &[f32], out: &mut Vec<f32>) {
        for frame in interleaved.chunks_exact(self.channels) {
            // Downmix: plain average across channels.
            let mut sum = 0.0f32;
            for s in frame {
                sum += *s;
            }
            self.accumulator += sum / self.channels as f32;
            self.count += 1;

            self.position += self.ratio;
            if self.position >= 1.0 {
                out.push(self.accumulator / self.count as f32);
                self.position -= 1.0;
                self.accumulator = 0.0;
                self.count = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal 16-bit PCM WAV writer, so the tests exercise the real container
    /// parser rather than a synthetic buffer handed straight to the DSP.
    fn write_wav(path: &Path, samples: &[i16], rate: u32, channels: u16) {
        let bytes_per_sample = 2u32;
        let data_len = (samples.len() as u32) * bytes_per_sample;
        let byte_rate = rate * channels as u32 * bytes_per_sample;
        let block_align = channels * bytes_per_sample as u16;

        let mut out: Vec<u8> = Vec::with_capacity(44 + data_len as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(path, out).expect("write wav");
    }

    /// Stereo sine at `freq`, amplitude chosen to sit well below clipping.
    fn sine_wav(path: &Path, freq: f64, seconds: f64, rate: u32) {
        let frames = (seconds * rate as f64) as usize;
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let t = i as f64 / rate as f64;
            let v = (t * freq * std::f64::consts::TAU).sin() * 0.5;
            let q = (v * i16::MAX as f64) as i16;
            samples.push(q); // L
            samples.push(q); // R
        }
        write_wav(path, &samples, rate, 2);
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("space-decode-test-{name}.wav"));
        p
    }

    #[test]
    fn decodes_container_without_ffmpeg() {
        let path = temp_path("basic");
        sine_wav(&path, 440.0, 3.0, 44_100);

        let d = decode_and_measure(&path).expect("decode");

        assert_eq!(d.source_rate, 44_100);
        assert_eq!(d.channels, 2);

        let duration = d.duration_seconds.expect("duration");
        assert!((duration - 3.0).abs() < 0.05, "duration was {duration}");

        // Resampled to the analysis rate, not the source rate.
        let expected = 3.0 * dsp::ANALYSIS_RATE as f64;
        let ratio = d.mono.len() as f64 / expected;
        assert!(ratio > 0.98 && ratio < 1.02, "mono len ratio {ratio}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn measures_loudness() {
        let path = temp_path("loudness");
        sine_wav(&path, 440.0, 4.0, 44_100);

        let d = decode_and_measure(&path).expect("decode");
        let (lufs, peak_db, _range) = d.loudness.expect("loudness measured");

        // A half-amplitude sine is about -6 dBFS peak and lands well inside
        // the range real music occupies.
        assert!((-40.0..0.0).contains(&lufs), "lufs was {lufs}");
        assert!((peak_db - -6.0).abs() < 1.5, "peak was {peak_db} dBFS");

        let _ = std::fs::remove_file(&path);
    }

    /// The end-to-end check that matters: a 440 Hz tone is an A, and the whole
    /// decode -> resample -> STFT -> chroma -> key chain has to agree.
    #[test]
    fn detects_pitch_of_a440() {
        let path = temp_path("pitch");
        sine_wav(&path, 440.0, 4.0, 44_100);

        let d = decode_and_measure(&path).expect("decode");
        let spec = dsp::spectral(&d.mono, dsp::ANALYSIS_RATE).expect("spectral");

        // Chroma index 9 is A.
        let (loudest, _) = spec
            .chroma
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .expect("chroma");
        assert_eq!(loudest, 9, "expected A, got pitch class {loudest}");

        let _ = std::fs::remove_file(&path);
    }

    /// Resampling must preserve duration regardless of the source rate.
    #[test]
    fn resamples_from_any_rate() {
        for rate in [22_050u32, 44_100, 48_000] {
            let path = temp_path(&format!("rate{rate}"));
            sine_wav(&path, 220.0, 2.0, rate);

            let d = decode_and_measure(&path).expect("decode");
            let seconds = d.mono.len() as f64 / dsp::ANALYSIS_RATE as f64;
            assert!(
                (seconds - 2.0).abs() < 0.05,
                "rate {rate} produced {seconds}s of mono"
            );

            let _ = std::fs::remove_file(&path);
        }
    }
}
