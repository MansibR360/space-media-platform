//! Locating — and if necessary installing — ffmpeg.
//!
//! Music analysis no longer needs ffmpeg: [`crate::music::decode`] decodes in
//! process. But transcoding, chapter-marker detection and the Opus fallback
//! still shell out to it, and requiring every operator to install it by hand is
//! the thing this module exists to avoid.
//!
//! Resolution runs once at startup, in order:
//!
//!   1. `FFMPEG_BIN` / `FFPROBE_BIN`, when set and runnable
//!   2. `ffmpeg` / `ffprobe` on `PATH`
//!   3. a copy this server installed earlier, under the tools directory
//!   4. a fresh download of a static build, cached for next time
//!
//! Step 4 is skipped when `FFMPEG_AUTO_DOWNLOAD=false`. Nothing here is fatal:
//! if every step fails the configured names are left as they were, and the
//! features that need ffmpeg degrade exactly as they did before.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use crate::config::Config;

/// Static builds published by BtbN, the source ffmpeg.org points at for
/// Windows. Pinned to the rolling `latest` tag.
const WINDOWS_BUILD_URL: &str =
    "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip";

/// Sanity bounds on the archive. A truncated download or an error page dressed
/// up as a 200 should fail here rather than half-extract.
const MIN_ARCHIVE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 400 * 1024 * 1024;

/// Where the two binaries ended up, and how they were found.
#[derive(Debug, Clone)]
pub struct Tools {
    pub ffmpeg: String,
    pub ffprobe: String,
    pub source: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Explicitly configured, or already on PATH.
    System,
    /// Installed under the tools directory by an earlier run.
    Cached,
    /// Downloaded just now.
    Downloaded,
    /// Not available. ffmpeg-dependent features will degrade.
    Missing,
}

/// Resolve ffmpeg, downloading it if that is the only way, and write the
/// result back into the config every call site already reads.
pub async fn provision(cfg: &mut Config) {
    let tools = resolve(cfg).await;

    match tools.source {
        Source::System => tracing::info!(ffmpeg = %tools.ffmpeg, "using system ffmpeg"),
        Source::Cached => tracing::info!(ffmpeg = %tools.ffmpeg, "using installed ffmpeg"),
        Source::Downloaded => {
            tracing::info!(ffmpeg = %tools.ffmpeg, "ffmpeg installed successfully")
        }
        Source::Missing => tracing::warn!(
            "ffmpeg is unavailable; transcoding and marker detection will fail gracefully. \
             Music analysis is unaffected — it decodes in process."
        ),
    }

    if tools.source != Source::Missing {
        cfg.ffmpeg_bin = tools.ffmpeg;
        cfg.ffprobe_bin = tools.ffprobe;
    }
}

async fn resolve(cfg: &Config) -> Tools {
    // 1 + 2. Whatever is configured, which defaults to a bare name and so
    // covers the PATH case too.
    if runnable(&cfg.ffmpeg_bin).await && runnable(&cfg.ffprobe_bin).await {
        return Tools {
            ffmpeg: cfg.ffmpeg_bin.clone(),
            ffprobe: cfg.ffprobe_bin.clone(),
            source: Source::System,
        };
    }

    // 3. A copy installed by an earlier run.
    let dir = tools_dir(cfg);
    let (ffmpeg, ffprobe) = installed_paths(&dir);
    if runnable(&ffmpeg).await && runnable(&ffprobe).await {
        return Tools {
            ffmpeg,
            ffprobe,
            source: Source::Cached,
        };
    }

    // 4. Download.
    if !cfg.ffmpeg_auto_download {
        return missing(cfg);
    }
    if !cfg!(target_os = "windows") {
        // Only the Windows archive is a zip; the Linux and macOS builds ship
        // as .tar.xz, which would mean pulling in an xz decoder for a platform
        // where `apt install ffmpeg` is one line. Left to the operator.
        tracing::info!(
            "automatic ffmpeg install is implemented for Windows only; \
             install ffmpeg through your package manager or set FFMPEG_BIN"
        );
        return missing(cfg);
    }

    match install_windows(&dir).await {
        Ok(()) => {
            let (ffmpeg, ffprobe) = installed_paths(&dir);
            if runnable(&ffmpeg).await && runnable(&ffprobe).await {
                Tools {
                    ffmpeg,
                    ffprobe,
                    source: Source::Downloaded,
                }
            } else {
                tracing::warn!("downloaded ffmpeg did not run; leaving it unconfigured");
                missing(cfg)
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not install ffmpeg automatically");
            missing(cfg)
        }
    }
}

fn missing(cfg: &Config) -> Tools {
    Tools {
        ffmpeg: cfg.ffmpeg_bin.clone(),
        ffprobe: cfg.ffprobe_bin.clone(),
        source: Source::Missing,
    }
}

fn tools_dir(cfg: &Config) -> PathBuf {
    cfg.tools_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("data").join("tools"))
        .join("ffmpeg")
}

fn installed_paths(dir: &Path) -> (String, String) {
    let ext = if cfg!(target_os = "windows") { ".exe" } else { "" };
    (
        dir.join("bin").join(format!("ffmpeg{ext}")).to_string_lossy().into_owned(),
        dir.join("bin").join(format!("ffprobe{ext}")).to_string_lossy().into_owned(),
    )
}

/// Does this binary exist and actually execute?
///
/// `-version` rather than a mere file check: a zero-byte file or a partially
/// extracted archive would otherwise pass.
async fn runnable(bin: &str) -> bool {
    if bin.trim().is_empty() {
        return false;
    }
    matches!(
        Command::new(bin)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await,
        Ok(status) if status.success()
    )
}

/// Download and unpack the Windows static build into `dir/bin`.
async fn install_windows(dir: &Path) -> Result<(), String> {
    tracing::info!(
        url = WINDOWS_BUILD_URL,
        "ffmpeg not found; downloading a static build (about 80 MB, once)"
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| format!("http client: {e}"))?;

    let response = client
        .get(WINDOWS_BUILD_URL)
        .send()
        .await
        .map_err(|e| format!("download: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("download returned {}", response.status()));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("download body: {e}"))?;

    let len = bytes.len() as u64;
    if !(MIN_ARCHIVE_BYTES..=MAX_ARCHIVE_BYTES).contains(&len) {
        return Err(format!("archive size {len} is implausible; refusing to unpack"));
    }

    let target = dir.to_path_buf();
    // Unzipping is blocking and CPU-bound.
    tokio::task::spawn_blocking(move || extract(&bytes, &target))
        .await
        .map_err(|e| format!("extract task: {e}"))?
}

/// Pull just `ffmpeg.exe` and `ffprobe.exe` out of the archive.
///
/// The build nests everything under a versioned directory and ships ffplay and
/// docs besides; only the two binaries are wanted, and flattening them into
/// `bin/` keeps the resolved path stable across versions.
fn extract(bytes: &[u8], dir: &Path) -> Result<(), String> {
    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| format!("bad zip: {e}"))?;

    let bin_dir = dir.join("bin");
    std::fs::create_dir_all(&bin_dir).map_err(|e| format!("mkdir: {e}"))?;

    let mut found = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("zip entry: {e}"))?;

        // `enclosed_name` rejects absolute paths and `..` traversal, which is
        // what stops a hostile archive writing outside the target directory.
        let Some(path) = entry.enclosed_name() else {
            continue;
        };
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !matches!(name, "ffmpeg.exe" | "ffprobe.exe") {
            continue;
        }

        let out_path = bin_dir.join(name);
        let mut out = std::fs::File::create(&out_path)
            .map_err(|e| format!("create {}: {e}", out_path.display()))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| format!("write {name}: {e}"))?;
        found += 1;
    }

    if found < 2 {
        return Err(format!("archive contained {found} of the 2 expected binaries"));
    }
    Ok(())
}
