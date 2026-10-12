use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use async_trait::async_trait;
use futures_util::Stream;
use tokio::fs::File;
use tokio::io::AsyncSeekExt;

use crate::error::ApiError;

pub type ByteStream = Pin<Box<dyn Stream<Item = io::Result<bytes::Bytes>> + Send>>;

#[derive(Debug, Clone)]
pub struct FileStat {
    pub size: u64,
    #[allow(dead_code)]
    pub modified_at: Option<std::time::SystemTime>,
}

#[async_trait]
pub trait Storage: Send + Sync {
    #[allow(dead_code)]
    fn name(&self) -> &'static str;

    #[allow(dead_code)]
    fn resolve(&self, root: &Path, relative: &str) -> PathBuf {
        let candidate = root.join(relative);
        if !candidate.starts_with(root) {
            tracing::warn!(%relative, "blocked path traversal attempt");
            return root.to_path_buf();
        }
        candidate
    }

    async fn exists(&self, path: &Path) -> bool {
        tokio::fs::metadata(path).await.is_ok()
    }

    async fn stat(&self, path: &Path) -> io::Result<FileStat> {
        let meta = tokio::fs::metadata(path).await?;
        Ok(FileStat {
            size: meta.len(),
            modified_at: meta.modified().ok(),
        })
    }

    async fn stream_range(
        &self,
        path: &Path,
        start: u64,
        end_inclusive: Option<u64>,
    ) -> io::Result<ByteStream>;
}

pub struct LocalStorage;

impl LocalStorage {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LocalStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Storage for LocalStorage {
    #[allow(dead_code)]
    fn name(&self) -> &'static str {
        "local"
    }

    async fn stream_range(
        &self,
        path: &Path,
        start: u64,
        end_inclusive: Option<u64>,
    ) -> io::Result<ByteStream> {
        use tokio::io::AsyncReadExt;
        use tokio_util::io::ReaderStream;

        let mut file = File::open(path).await?;
        let meta = file.metadata().await?;
        if !meta.is_file() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "not a file"));
        }

        let start = start.min(meta.len());
        file.seek(std::io::SeekFrom::Start(start)).await?;

        match end_inclusive {
            Some(end) => {
                let length = (end.saturating_sub(start)).saturating_add(1);
                Ok(Box::pin(ReaderStream::new(file.take(length))))
            }
            None => Ok(Box::pin(ReaderStream::new(file))),
        }
    }
}

pub fn ensure_parent_dir(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

pub fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "ts" => "video/mp2t",
        "m3u8" => "application/vnd.apple.mpegurl",
        "vtt" => "text/vtt; charset=utf-8",
        "srt" => "application/x-subrip",
        "ass" | "ssa" => "text/x-ssa; charset=utf-8",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

#[allow(dead_code)]
pub fn not_found_if<T>(value: Option<T>) -> Result<T, ApiError> {
    value.ok_or_else(|| ApiError::not_found("file"))
}
