//! Signal processing for the music analyzer.
//!
//! Everything here operates on one mono f32 buffer that the caller has already
//! decoded, so a track is read off disk exactly once no matter how many
//! descriptors come out of it.
//!
//! The feature set is deliberately the one that survives resampling and lossy
//! encoding — spectral shape, pitch-class content and onset periodicity —
//! because the library holds transcoded files, not masters.

use std::f32::consts::PI;
use std::sync::Arc;

use rustfft::{num_complex::Complex, Fft, FftPlanner};

/// Analysis sample rate. 22.05 kHz keeps everything below the 11 kHz Nyquist,
/// which is above the range that carries pitch and timbre, and quarters the
/// work relative to 44.1 kHz.
pub const ANALYSIS_RATE: u32 = 22_050;
/// STFT window. 2048 @ 22.05 kHz is ~93 ms — long enough to resolve a bass
/// note, short enough not to smear a transient.
pub const FRAME: usize = 2048;
/// 75% overlap. Gives an onset envelope at ~43 Hz, plenty for tempo up to 220.
pub const HOP: usize = 512;
pub const MEL_BANDS: usize = 40;
pub const MFCC_COUNT: usize = 13;

/// Krumhansl-Kessler key profiles: how strongly each pitch class is expected
/// to sound in a major and a minor key. Correlating the track's chroma against
/// all 24 rotations of these is the standard key estimator.
const MAJOR_PROFILE: [f32; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR_PROFILE: [f32; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

pub const KEY_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// Per-frame spectral measurements, one entry per STFT hop.
pub struct Spectral {
    pub centroid: Vec<f32>,
    pub rolloff: Vec<f32>,
    pub flatness: Vec<f32>,
    pub bandwidth: Vec<f32>,
    /// Positive spectral flux — the onset strength envelope.
    pub flux: Vec<f32>,
    /// Summed magnitude per pitch class across the whole track.
    pub chroma: [f32; 12],
    /// Mean MFCC vector, plus the per-frame values used for segmentation.
    pub mfcc_mean: [f32; MFCC_COUNT],
    pub mfcc_frames: Vec<[f32; MFCC_COUNT]>,
    /// Fraction of total energy below 250 Hz / above 4 kHz.
    pub low_ratio: f32,
    pub high_ratio: f32,
    /// Energy in the 200 Hz - 4 kHz band where voices live.
    pub vocal_ratio: f32,
}

struct MelBank {
    /// (start_bin, weights) per filter — triangular, stored sparsely.
    filters: Vec<(usize, Vec<f32>)>,
}

fn hz_to_mel(hz: f32) -> f32 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (10f32.powf(mel / 2595.0) - 1.0)
}

impl MelBank {
    fn new(bins: usize, rate: f32) -> Self {
        let f_min = 20.0f32;
        let f_max = rate / 2.0;
        let mel_min = hz_to_mel(f_min);
        let mel_max = hz_to_mel(f_max);

        // MEL_BANDS triangles need MEL_BANDS + 2 edges.
        let edges: Vec<f32> = (0..MEL_BANDS + 2)
            .map(|i| {
                let mel = mel_min + (mel_max - mel_min) * i as f32 / (MEL_BANDS + 1) as f32;
                mel_to_hz(mel) * bins as f32 * 2.0 / rate
            })
            .collect();

        let mut filters = Vec::with_capacity(MEL_BANDS);
        for m in 0..MEL_BANDS {
            let (lo, mid, hi) = (edges[m], edges[m + 1], edges[m + 2]);
            let start = lo.floor().max(0.0) as usize;
            let end = (hi.ceil() as usize).min(bins.saturating_sub(1));
            let mut weights = Vec::new();
            for bin in start..=end.max(start) {
                let b = bin as f32;
                let w = if b < mid {
                    if mid > lo { (b - lo) / (mid - lo) } else { 0.0 }
                } else if hi > mid {
                    (hi - b) / (hi - mid)
                } else {
                    0.0
                };
                weights.push(w.clamp(0.0, 1.0));
            }
            filters.push((start, weights));
        }
        Self { filters }
    }

    fn apply(&self, mags: &[f32], out: &mut [f32; MEL_BANDS]) {
        for (i, (start, weights)) in self.filters.iter().enumerate() {
            let mut sum = 0.0;
            for (j, w) in weights.iter().enumerate() {
                if let Some(m) = mags.get(start + j) {
                    sum += m * w;
                }
            }
            out[i] = sum;
        }
    }
}

/// Precomputed DCT-II basis for the mel -> MFCC step.
fn dct_basis() -> Vec<[f32; MEL_BANDS]> {
    (0..MFCC_COUNT)
        .map(|k| {
            let mut row = [0.0f32; MEL_BANDS];
            for (n, slot) in row.iter_mut().enumerate() {
                *slot = ((PI * k as f32 * (n as f32 + 0.5)) / MEL_BANDS as f32).cos();
            }
            row
        })
        .collect()
}

fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / n as f32).cos())
        .collect()
}

/// Run the STFT once and derive every frame-level feature from it.
pub fn spectral(samples: &[f32], rate: u32) -> Option<Spectral> {
    if samples.len() < FRAME * 2 {
        return None;
    }

    let bins = FRAME / 2 + 1;
    let mut planner = FftPlanner::<f32>::new();
    let fft: Arc<dyn Fft<f32>> = planner.plan_fft_forward(FRAME);

    let window = hann(FRAME);
    let mel = MelBank::new(bins, rate as f32);
    let dct = dct_basis();

    let frames = (samples.len() - FRAME) / HOP + 1;
    let mut centroid = Vec::with_capacity(frames);
    let mut rolloff = Vec::with_capacity(frames);
    let mut flatness = Vec::with_capacity(frames);
    let mut bandwidth = Vec::with_capacity(frames);
    let mut flux = Vec::with_capacity(frames);
    let mut mfcc_frames = Vec::with_capacity(frames);

    let mut chroma = [0.0f32; 12];
    let mut mfcc_sum = [0.0f32; MFCC_COUNT];
    let (mut low_e, mut high_e, mut vocal_e, mut total_e) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);

    // Bin -> frequency and bin -> pitch class, both precomputed. Bins outside
    // 65 Hz - 2 kHz are dropped from chroma: below that is rumble, above it
    // harmonics start outvoting the fundamental.
    let bin_hz: Vec<f32> = (0..bins)
        .map(|k| k as f32 * rate as f32 / FRAME as f32)
        .collect();
    let pitch_class: Vec<Option<usize>> = bin_hz
        .iter()
        .map(|&hz| {
            if (65.0..2000.0).contains(&hz) {
                let midi = 69.0 + 12.0 * (hz / 440.0).log2();
                Some((midi.round() as i32).rem_euclid(12) as usize)
            } else {
                None
            }
        })
        .collect();

    let mut buffer = vec![Complex::new(0.0f32, 0.0); FRAME];
    let mut mags = vec![0.0f32; bins];
    let mut prev_mags = vec![0.0f32; bins];
    let mut mel_out = [0.0f32; MEL_BANDS];

    for f in 0..frames {
        let offset = f * HOP;
        for i in 0..FRAME {
            buffer[i] = Complex::new(samples[offset + i] * window[i], 0.0);
        }
        fft.process(&mut buffer);

        for (k, slot) in mags.iter_mut().enumerate() {
            *slot = buffer[k].norm();
        }

        let sum: f32 = mags.iter().sum();
        if sum <= 1e-9 {
            centroid.push(0.0);
            rolloff.push(0.0);
            flatness.push(0.0);
            bandwidth.push(0.0);
            flux.push(0.0);
            mfcc_frames.push([0.0; MFCC_COUNT]);
            prev_mags.copy_from_slice(&mags);
            continue;
        }

        // Centroid: energy-weighted mean frequency, i.e. brightness.
        let c: f32 = mags.iter().zip(&bin_hz).map(|(m, hz)| m * hz).sum::<f32>() / sum;
        centroid.push(c);

        // Bandwidth: spread of energy around the centroid.
        let bw = (mags
            .iter()
            .zip(&bin_hz)
            .map(|(m, hz)| m * (hz - c).powi(2))
            .sum::<f32>()
            / sum)
            .sqrt();
        bandwidth.push(bw);

        // Rolloff: the frequency below which 85% of the energy sits.
        let target = sum * 0.85;
        let mut acc = 0.0;
        let mut roll = bin_hz[bins - 1];
        for (k, m) in mags.iter().enumerate() {
            acc += m;
            if acc >= target {
                roll = bin_hz[k];
                break;
            }
        }
        rolloff.push(roll);

        // Flatness: geometric over arithmetic mean. ~1 is noise, ~0 is a tone.
        let log_mean = mags.iter().map(|m| (m + 1e-10).ln()).sum::<f32>() / bins as f32;
        flatness.push((log_mean.exp() / (sum / bins as f32)).clamp(0.0, 1.0));

        // Flux: rectified frame-to-frame change — the onset strength signal.
        let fx: f32 = mags
            .iter()
            .zip(&prev_mags)
            .map(|(m, p)| (m - p).max(0.0))
            .sum();
        flux.push(fx / bins as f32);

        for (k, m) in mags.iter().enumerate() {
            let e = (*m as f64) * (*m as f64);
            total_e += e;
            let hz = bin_hz[k];
            if hz < 250.0 {
                low_e += e;
            }
            if hz > 4000.0 {
                high_e += e;
            }
            if (200.0..4000.0).contains(&hz) {
                vocal_e += e;
            }
            if let Some(pc) = pitch_class[k] {
                chroma[pc] += *m;
            }
        }

        mel.apply(&mags, &mut mel_out);
        let log_mel: Vec<f32> = mel_out.iter().map(|e| (e + 1e-10).ln()).collect();
        let mut coeffs = [0.0f32; MFCC_COUNT];
        for (k, row) in dct.iter().enumerate() {
            coeffs[k] = row.iter().zip(&log_mel).map(|(basis, e)| basis * e).sum::<f32>()
                / MEL_BANDS as f32;
            mfcc_sum[k] += coeffs[k];
        }
        mfcc_frames.push(coeffs);

        prev_mags.copy_from_slice(&mags);
    }

    let n = mfcc_frames.len().max(1) as f32;
    let mut mfcc_mean = [0.0f32; MFCC_COUNT];
    for k in 0..MFCC_COUNT {
        mfcc_mean[k] = mfcc_sum[k] / n;
    }

    let denom = total_e.max(1e-9);
    Some(Spectral {
        centroid,
        rolloff,
        flatness,
        bandwidth,
        flux,
        chroma,
        mfcc_mean,
        mfcc_frames,
        low_ratio: (low_e / denom) as f32,
        high_ratio: (high_e / denom) as f32,
        vocal_ratio: (vocal_e / denom) as f32,
    })
}

pub fn mean(v: &[f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.iter().sum::<f32>() / v.len() as f32
}

pub fn std_dev(v: &[f32]) -> f32 {
    if v.len() < 2 {
        return 0.0;
    }
    let m = mean(v);
    (v.iter().map(|x| (x - m).powi(2)).sum::<f32>() / v.len() as f32).sqrt()
}

/// Fraction of sample-to-sample sign changes. High for noise and consonants,
/// low for sustained tones — the cheapest voiced/unvoiced discriminator there is.
pub fn zero_crossing_rate(samples: &[f32]) -> f32 {
    if samples.len() < 2 {
        return 0.0;
    }
    let crossings = samples
        .windows(2)
        .filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0))
        .count();
    crossings as f32 / (samples.len() - 1) as f32
}

/// Tempo from the onset envelope's autocorrelation.
///
/// Returns `(bpm, confidence)`. Confidence is the winning lag's correlation
/// relative to the strongest lag overall, so a track with no steady pulse
/// reports a low number rather than a confident wrong answer.
pub fn tempo(flux: &[f32], rate: u32) -> Option<(f64, f64)> {
    if flux.len() < 64 {
        return None;
    }
    let frame_rate = rate as f32 / HOP as f32;

    // Remove the mean so the autocorrelation measures periodicity, not level.
    let m = mean(flux);
    let centered: Vec<f32> = flux.iter().map(|f| f - m).collect();

    let min_lag = ((60.0 / 220.0) * frame_rate).round().max(2.0) as usize;
    let max_lag = (((60.0 / 40.0) * frame_rate).round() as usize).min(centered.len() / 2);
    if max_lag <= min_lag {
        return None;
    }

    let energy: f32 = centered.iter().map(|x| x * x).sum();
    if energy <= 1e-9 {
        return None;
    }

    let mut best = (min_lag, f32::MIN);
    let mut peak_any = f32::MIN;
    for lag in min_lag..=max_lag {
        let corr: f32 = centered[..centered.len() - lag]
            .iter()
            .zip(&centered[lag..])
            .map(|(a, b)| a * b)
            .sum::<f32>()
            / energy;
        // Favour the fundamental over its harmonics: without this a 120 BPM
        // track reports 60 about as often as not.
        let weighted = corr * (1.0 + 0.35 * (lag as f32 / max_lag as f32));
        if weighted > best.1 {
            best = (lag, weighted);
        }
        peak_any = peak_any.max(corr.abs());
    }

    if best.1 <= 0.0 {
        return None;
    }
    let bpm = 60.0 * frame_rate as f64 / best.0 as f64;
    let confidence = (best.1 / peak_any.max(1e-6)).clamp(0.0, 1.0) as f64;

    // Fold octave errors into the range people actually label tracks with.
    let bpm = if bpm < 60.0 {
        bpm * 2.0
    } else if bpm > 190.0 {
        bpm / 2.0
    } else {
        bpm
    };
    (40.0..=220.0).contains(&bpm).then_some((bpm, confidence))
}

/// Beat times in seconds, from the tempo plus the phase that best lines a
/// pulse train up with the onset envelope.
pub fn beat_grid(flux: &[f32], rate: u32, bpm: f64, max_beats: usize) -> Vec<f32> {
    if bpm <= 0.0 || flux.is_empty() {
        return Vec::new();
    }
    let frame_rate = rate as f64 / HOP as f64;
    let period = 60.0 / bpm * frame_rate;
    if period < 1.0 {
        return Vec::new();
    }

    let mut best_phase = 0usize;
    let mut best_score = f32::MIN;
    for phase in 0..period as usize {
        let mut score = 0.0;
        let mut i = phase as f64;
        while (i as usize) < flux.len() {
            score += flux[i as usize];
            i += period;
        }
        if score > best_score {
            best_score = score;
            best_phase = phase;
        }
    }

    let mut beats = Vec::new();
    let mut i = best_phase as f64;
    while (i as usize) < flux.len() && beats.len() < max_beats {
        beats.push((i / frame_rate) as f32);
        i += period;
    }
    beats
}

/// Key estimate as `(pitch class, is_major, confidence)`.
///
/// Correlates the pitch-class histogram against all 24 rotated
/// Krumhansl-Kessler profiles. Confidence is the margin over the runner-up,
/// which is what separates a genuinely tonal track from a drum loop.
pub fn detect_key(chroma: &[f32; 12]) -> Option<(i16, bool, f64)> {
    let total: f32 = chroma.iter().sum();
    if total <= 1e-6 {
        return None;
    }
    let norm: Vec<f32> = chroma.iter().map(|c| c / total).collect();

    let correlate = |profile: &[f32; 12], rotation: usize| -> f32 {
        let rotated: Vec<f32> = (0..12).map(|i| profile[(i + 12 - rotation) % 12]).collect();
        let (mx, my) = (mean(&norm), mean(&rotated));
        let num: f32 = norm
            .iter()
            .zip(&rotated)
            .map(|(a, b)| (a - mx) * (b - my))
            .sum();
        let den = (norm.iter().map(|a| (a - mx).powi(2)).sum::<f32>()
            * rotated.iter().map(|b| (b - my).powi(2)).sum::<f32>())
        .sqrt();
        if den <= 1e-9 { 0.0 } else { num / den }
    };

    let mut scores: Vec<(i16, bool, f32)> = Vec::with_capacity(24);
    for rot in 0..12 {
        scores.push((rot as i16, true, correlate(&MAJOR_PROFILE, rot)));
        scores.push((rot as i16, false, correlate(&MINOR_PROFILE, rot)));
    }
    scores.sort_by(|a, b| b.2.total_cmp(&a.2));

    let best = scores[0];
    let runner_up = scores[1].2;
    // Margin, scaled so a clear winner lands near 1 and a coin flip near 0.
    let confidence = ((best.2 - runner_up) * 4.0).clamp(0.0, 1.0) as f64;
    Some((best.0, best.1, confidence))
}

/// Structural boundaries in seconds, from novelty in the MFCC trajectory.
///
/// Averages MFCCs into one-second blocks, then marks where timbre changes most
/// sharply — verse to chorus, the drop, the outro.
pub fn segments(mfcc_frames: &[[f32; MFCC_COUNT]], rate: u32, max_segments: usize) -> Vec<f32> {
    let frame_rate = rate as f32 / HOP as f32;
    let block = frame_rate.round().max(1.0) as usize;
    if mfcc_frames.len() < block * 4 {
        return Vec::new();
    }

    let blocks: Vec<[f32; MFCC_COUNT]> = mfcc_frames
        .chunks(block)
        .map(|chunk| {
            let mut avg = [0.0f32; MFCC_COUNT];
            for f in chunk {
                for k in 0..MFCC_COUNT {
                    avg[k] += f[k];
                }
            }
            for slot in avg.iter_mut() {
                *slot /= chunk.len() as f32;
            }
            avg
        })
        .collect();

    // Novelty: euclidean distance between neighbouring blocks.
    let novelty: Vec<f32> = blocks
        .windows(2)
        .map(|w| {
            (0..MFCC_COUNT)
                .map(|k| (w[1][k] - w[0][k]).powi(2))
                .sum::<f32>()
                .sqrt()
        })
        .collect();

    let threshold = mean(&novelty) + std_dev(&novelty);
    let mut peaks: Vec<(usize, f32)> = Vec::new();
    for i in 1..novelty.len().saturating_sub(1) {
        if novelty[i] > threshold && novelty[i] >= novelty[i - 1] && novelty[i] >= novelty[i + 1] {
            peaks.push((i, novelty[i]));
        }
    }

    peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
    peaks.truncate(max_segments);
    let mut times: Vec<f32> = peaks.into_iter().map(|(i, _)| (i + 1) as f32).collect();
    times.sort_by(f32::total_cmp);
    times
}

/// Reduce a signal to `buckets` values — peak per bucket, or RMS per bucket.
pub fn envelope(samples: &[f32], buckets: usize, rms: bool) -> Vec<f32> {
    if samples.is_empty() || buckets == 0 {
        return Vec::new();
    }
    let buckets = buckets.min(samples.len());
    let per = samples.len() as f64 / buckets as f64;

    (0..buckets)
        .map(|b| {
            let start = (b as f64 * per) as usize;
            let end = (((b + 1) as f64 * per) as usize).clamp(start + 1, samples.len());
            let slice = &samples[start..end];
            if rms {
                (slice.iter().map(|s| s * s).sum::<f32>() / slice.len() as f32).sqrt()
            } else {
                slice.iter().fold(0.0f32, |acc, s| acc.max(s.abs()))
            }
        })
        .map(|v| v.clamp(0.0, 1.0))
        .collect()
}

/// L2-normalize in place so cosine similarity is a plain dot product.
pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-9 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}
