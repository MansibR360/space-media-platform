//! Music intelligence.
//!
//! The data splits in two, and the split is the whole design:
//!
//! * **Global** — one row per track, shared by every user. `analysis` holds
//!   what the audio *is* (waveform, key, tempo, timbre, descriptors);
//!   `music_track_stats` and the co-play graph hold what the whole audience
//!   *does* with it. Both compound: every upload adds a row, every listen by
//!   anyone sharpens the engagement half.
//! * **Per user** — the listen log and the taste profile derived from it. One
//!   listener's history never enters another's, except anonymously through the
//!   co-occurrence graph, which stores track pairs and weights but no identity.
//!
//! Modules:
//! - `decode`   — in-process decoding (Symphonia) + EBU R128 loudness
//! - `dsp`      — FFT, chroma, MFCC, onset and key estimation
//! - `analysis` — orchestrates decode + `dsp` into one row; ffmpeg is only a
//!   fallback for containers Symphonia cannot open
//! - `repo`     — persistence for both layers
//! - `service`  — records a listen, folds it into both layers, and blends the
//!   four recommendation signals
//! - `routes` / `handlers` — HTTP surface, including the dataset export
//!
//! Taste is learned from plays rather than declared: each listen adds weight to
//! the genre, artist, key, tempo band and energy band it belongs to, scaled by
//! how much of the track was actually heard. Skips subtract. Weights decay with
//! a 21-day half-life at read time, so the profile tracks what the user is
//! listening to now instead of what they liked a year ago.

pub mod analysis;
pub mod decode;
pub mod dsp;
pub mod handlers;
pub mod models;
pub mod repo;
pub mod routes;
pub mod service;
