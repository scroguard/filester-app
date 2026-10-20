//! Filester REST API engine.
//!
//! Everything in this module runs on the background tokio runtime (see
//! [`crate::worker`]) and must never import GTK. The public surface is:
//!
//! - [`FilesterClient`]: one method per documented endpoint. It is cheap to
//!   clone and safe to share between tasks.
//! - [`ClientConfig`] / [`RetryPolicy`]: timeouts, retry and backoff limits.
//! - [`ApiKey`]: a validated, redacting wrapper around the bearer token.
//! - [`FolderTree`]: the in-memory folder hierarchy rebuilt from parent links.
//! - [`PublicDownloader`]: downloads of public files through the website's
//!   token route (not part of the documented API; see [`download`]).
//! - [`ApiError`]: every failure mode as a typed variant.
//!
//! API reference: <https://filester.me/api-docs>.

mod auth;
mod client;
pub mod download;
mod error;
pub mod models;
mod rate_limit;
pub mod tree;
mod upload;

pub use auth::ApiKey;
pub use client::{ClientConfig, FilesterClient, MAX_BULK_ITEMS};
pub use download::{DownloadConfig, MAX_DOWNLOAD_BYTES, PublicDownloader};
pub use error::ApiError;
pub use rate_limit::{RateLimitInfo, RetryPolicy};
pub use tree::{FolderTree, TreeAnomaly};
pub use upload::{MAX_THUMBNAIL_BYTES, MAX_UPLOAD_BYTES, ProgressCallback, TransferProgress};

/// Result alias used throughout the API engine.
pub type Result<T, E = ApiError> = std::result::Result<T, E>;
