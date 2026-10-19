//! Streaming multipart upload machinery.
//!
//! A 10 GiB upload must not be buffered in memory. The file is read through
//! a [`ReaderStream`] in [`CHUNK_SIZE`] pieces and handed to reqwest as a
//! streaming multipart part with a known length. Peak memory is a few chunks,
//! regardless of file size.
//!
//! Integrity guards:
//!
//! - The size is captured once and re-checked every time the file is opened
//!   for an attempt. The stream is capped with `take(size)`, so a file that
//!   grows mid-upload cannot overrun the declared `Content-Length`. A file
//!   that shrinks ends the stream early, and the request fails instead of
//!   silently sending a truncated file.
//! - [`StallWatch`] fails an upload whose body makes no progress for the
//!   idle window. A total timeout would kill legitimate slow transfers.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::TryStreamExt;
use reqwest::Body;
use reqwest::multipart::Part;
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;

use super::ApiError;

/// Server-side upload limit: 10 GiB. The API docs say "10 GB", and the
/// account `storage_limit` example (10 737 418 240) shows binary units.
pub const MAX_UPLOAD_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// Folder thumbnail limit (5 MB per the API docs, read as binary units).
pub const MAX_THUMBNAIL_BYTES: u64 = 5 * 1024 * 1024;

/// Read size per chunk. Larger chunks mean fewer syscalls and progress
/// callbacks. Smaller chunks mean smoother progress and less memory.
const CHUNK_SIZE: usize = 256 * 1024;

/// Minimum bytes between progress callbacks, which bounds UI traffic to about
/// 10 000 events for a 10 GiB file.
pub(crate) const PROGRESS_STEP: u64 = 1024 * 1024;

/// Longest file name sent in the multipart header, in bytes.
const MAX_FILE_NAME_BYTES: usize = 255;

/// Snapshot of an in-flight transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferProgress {
    /// Bytes handed to the HTTP stack so far in the current attempt.
    pub sent: u64,
    /// Total bytes in the file.
    pub total: u64,
}

impl TransferProgress {
    /// Completion in `0.0..=1.0`. An empty file counts as complete.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // display-only ratio; f64 is exact to 2^53 bytes
    pub fn fraction(&self) -> f64 {
        if self.total == 0 {
            1.0
        } else {
            (self.sent as f64 / self.total as f64).min(1.0)
        }
    }
}

/// Progress sink. It is called from a worker thread and must not block; the
/// GUI bridge in [`crate::worker`] forwards it over a channel.
///
/// When a retry restarts the upload, `sent` drops back to `0`.
pub type ProgressCallback = Arc<dyn Fn(TransferProgress) + Send + Sync>;

/// Shared byte counter for one upload, read by [`StallWatch`].
#[derive(Debug, Default)]
pub(crate) struct UploadCounter {
    sent: AtomicU64,
}

impl UploadCounter {
    fn reset(&self) {
        self.sent.store(0, Ordering::Relaxed);
    }

    fn add(&self, n: u64) -> u64 {
        self.sent.fetch_add(n, Ordering::Relaxed).saturating_add(n)
    }

    fn get(&self) -> u64 {
        self.sent.load(Ordering::Relaxed)
    }
}

/// Validated description of a local file to upload.
#[derive(Debug, Clone)]
pub(crate) struct UploadSource {
    pub path: std::path::PathBuf,
    pub size: u64,
    pub file_name: String,
    pub mime: String,
}

impl UploadSource {
    /// Inspect `path`: it must be a regular file no larger than `limit`.
    pub(crate) async fn inspect(path: &Path, limit: u64) -> Result<Self, ApiError> {
        let meta = tokio::fs::metadata(path).await?;
        if !meta.is_file() {
            return Err(ApiError::InvalidArgument("not a regular file".to_owned()));
        }
        let size = meta.len();
        if size > limit {
            return Err(ApiError::FileTooLarge { size, limit });
        }
        Ok(Self {
            path: path.to_path_buf(),
            size,
            file_name: upload_file_name(path)?,
            mime: mime_guess::from_path(path)
                .first_or_octet_stream()
                .essence_str()
                .to_owned(),
        })
    }

    /// Open the file for one attempt and build a streaming multipart part.
    ///
    /// Resets `counter` and reports `sent = 0`, so a retry restarts progress
    /// cleanly.
    pub(crate) async fn streaming_part(
        &self,
        counter: Arc<UploadCounter>,
        progress: Option<ProgressCallback>,
    ) -> Result<Part, ApiError> {
        let file = tokio::fs::File::open(&self.path).await?;
        let now_size = file.metadata().await?.len();
        if now_size != self.size {
            return Err(ApiError::Io(std::io::Error::other(
                "file size changed since the upload was queued",
            )));
        }

        counter.reset();
        let total = self.size;
        if let Some(cb) = &progress {
            cb(TransferProgress { sent: 0, total });
        }
        let last_reported = Arc::new(AtomicU64::new(0));

        let stream =
            ReaderStream::with_capacity(file.take(total), CHUNK_SIZE).inspect_ok(move |chunk| {
                let sent = counter.add(chunk.len() as u64);
                if let Some(cb) = &progress {
                    let last = last_reported.load(Ordering::Relaxed);
                    if sent >= total || sent.saturating_sub(last) >= PROGRESS_STEP {
                        last_reported.store(sent, Ordering::Relaxed);
                        cb(TransferProgress { sent, total });
                    }
                }
            });

        Part::stream_with_length(Body::wrap_stream(stream), total)
            .file_name(self.file_name.clone())
            .mime_str(&self.mime)
            .map_err(ApiError::from_reqwest)
    }

    /// Read a small file fully into memory (for thumbnails, capped by
    /// [`MAX_THUMBNAIL_BYTES`]).
    pub(crate) async fn buffered_part(&self) -> Result<Part, ApiError> {
        let mut bytes = Vec::with_capacity(usize::try_from(self.size).unwrap_or(0));
        tokio::fs::File::open(&self.path)
            .await?
            .take(self.size)
            .read_to_end(&mut bytes)
            .await?;
        Part::bytes(bytes)
            .file_name(self.file_name.clone())
            .mime_str(&self.mime)
            .map_err(ApiError::from_reqwest)
    }
}

/// Derive the multipart `filename` from a local path.
///
/// Only the final component is sent, never the directory, so local
/// usernames and layout do not leak. Control characters, quotes and
/// backslashes, which could confuse `Content-Disposition` parsing, become
/// `_`. The result is capped at [`MAX_FILE_NAME_BYTES`] on a char boundary.
pub(crate) fn upload_file_name(path: &Path) -> Result<String, ApiError> {
    let raw = path
        .file_name()
        .ok_or_else(|| ApiError::InvalidArgument("path has no file name".to_owned()))?
        .to_string_lossy();
    let mut name: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '"' | '\\') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if name.len() > MAX_FILE_NAME_BYTES {
        let mut cut = MAX_FILE_NAME_BYTES;
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        name.truncate(cut);
    }
    if name.trim().is_empty() {
        return Err(ApiError::InvalidArgument("file name is empty".to_owned()));
    }
    Ok(name)
}

/// Watchdog for one upload attempt.
pub(crate) struct StallWatch {
    pub counter: Arc<UploadCounter>,
    pub total: u64,
    /// Fail if the body makes no progress for this long.
    pub idle: Duration,
    /// After the whole body is sent, how long to wait for the response.
    pub response_timeout: Duration,
}

impl StallWatch {
    /// Resolve with an error when the attempt stalls. Race it against the
    /// request with `tokio::select!`. It never resolves on a healthy
    /// transfer.
    pub(crate) async fn expired(&self) -> ApiError {
        let tick = (self.idle / 4).clamp(Duration::from_millis(10), Duration::from_secs(1));
        let mut last_sent = self.counter.get();
        let mut last_change = Instant::now();
        loop {
            tokio::time::sleep(tick).await;
            let sent = self.counter.get();
            if sent >= self.total {
                // Body fully handed off; now bound only the server's reply.
                tokio::time::sleep(self.response_timeout).await;
                return ApiError::Timeout;
            }
            if sent == last_sent {
                if last_change.elapsed() >= self.idle {
                    return ApiError::Stalled(self.idle);
                }
            } else {
                last_sent = sent;
                last_change = Instant::now();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_sanitised_and_directory_dropped() {
        let n = upload_file_name(Path::new("/home/alice/secret-dir/re\"port\\\n.pdf")).unwrap();
        assert_eq!(n, "re_port__.pdf");
    }

    #[test]
    fn file_name_truncated_on_char_boundary() {
        let long = format!("/tmp/{}.bin", "é".repeat(200));
        let n = upload_file_name(Path::new(&long)).unwrap();
        assert!(n.len() <= MAX_FILE_NAME_BYTES);
        assert!(n.is_char_boundary(n.len()));
    }

    #[test]
    fn file_name_rejects_empty() {
        assert!(upload_file_name(Path::new("/")).is_err());
        assert!(upload_file_name(Path::new("/tmp/ ")).is_err());
    }

    #[test]
    fn fraction_handles_edges() {
        assert!((TransferProgress { sent: 0, total: 0 }.fraction() - 1.0).abs() < f64::EPSILON);
        assert!((TransferProgress { sent: 5, total: 10 }.fraction() - 0.5).abs() < f64::EPSILON);
        assert!(
            (TransferProgress {
                sent: 20,
                total: 10
            }
            .fraction()
                - 1.0)
                .abs()
                < f64::EPSILON
        );
    }

    #[tokio::test]
    async fn inspect_rejects_directories_and_oversize() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            UploadSource::inspect(dir.path(), MAX_UPLOAD_BYTES).await,
            Err(ApiError::InvalidArgument(_))
        ));
        let path = dir.path().join("big.bin");
        std::fs::write(&path, [0u8; 16]).unwrap();
        assert!(matches!(
            UploadSource::inspect(&path, 15).await,
            Err(ApiError::FileTooLarge {
                size: 16,
                limit: 15
            })
        ));
        let ok = UploadSource::inspect(&path, 16).await.unwrap();
        assert_eq!(ok.file_name, "big.bin");
        assert_eq!(ok.mime, "application/octet-stream");
    }

    #[tokio::test]
    async fn changed_file_is_detected_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, b"hello").unwrap();
        let src = UploadSource::inspect(&path, 100).await.unwrap();
        std::fs::write(&path, b"hello world").unwrap();
        let err = src.streaming_part(Arc::default(), None).await.unwrap_err();
        assert!(matches!(err, ApiError::Io(_)));
    }

    #[tokio::test]
    async fn stall_watch_fires_without_progress() {
        let watch = StallWatch {
            counter: Arc::default(),
            total: 100,
            idle: Duration::from_millis(50),
            response_timeout: Duration::from_secs(60),
        };
        let err = tokio::time::timeout(Duration::from_secs(5), watch.expired())
            .await
            .unwrap();
        assert!(matches!(err, ApiError::Stalled(_)));
    }

    #[tokio::test]
    async fn stall_watch_switches_to_response_timeout_when_body_sent() {
        let counter = Arc::new(UploadCounter::default());
        counter.add(100);
        let watch = StallWatch {
            counter,
            total: 100,
            idle: Duration::from_millis(20),
            response_timeout: Duration::from_millis(50),
        };
        let err = tokio::time::timeout(Duration::from_secs(5), watch.expired())
            .await
            .unwrap();
        assert!(matches!(err, ApiError::Timeout));
    }
}
