//! Downloads of **public** files.
//!
//! The documented REST API has no download endpoint. This module uses the
//! route the website's share page uses instead, which works only for public
//! files:
//!
//! 1. `POST https://filester.me/v2/api/public/download` with
//!    `{"file_slug": "<slug>"}` returns a short-lived token, the stored file
//!    name and the server that holds the bytes. Private files get HTTP 403
//!    (code `E2007`).
//! 2. `GET <server>/v2/<file>?token=<token>&download=true` returns the bytes.
//!
//! Security properties (see README "Security & Auditing"):
//!
//! - [`PublicDownloader`] has its own HTTP client and never holds the API
//!   key, so no request it makes can carry the `Authorization` header. The
//!   key stays confined to the API origin.
//! - The server named in a token response is untrusted input. It must be
//!   `https`, carry no credentials, path, query or fragment, and its host
//!   must be one of [`config::DOWNLOAD_HOSTS`] or a subdomain of one.
//!   Redirects are held to the same rule. Anything else is refused before
//!   it is contacted.
//! - The body is streamed straight to disk and capped at the size the API
//!   reported for the file (and at [`MAX_DOWNLOAD_BYTES`]). A short or long
//!   body is an error, and the partial file is removed.
//! - The destination is created with `O_EXCL` and mode `0600`, so an
//!   existing file or symlink is never followed or overwritten.
//! - The token is part of the request URL. reqwest errors are stripped of
//!   their URL before they are stored, and nothing here logs URLs.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use reqwest::{StatusCode, redirect};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tracing::debug;
use url::Url;

use super::client::{decode_json, error_from_response, is_loopback_ip, read_capped};
use super::error::sanitize_server_text;
use super::models::is_valid_slug;
use super::upload::{MAX_UPLOAD_BYTES, PROGRESS_STEP};
use super::{ApiError, ProgressCallback, Result, TransferProgress};
use crate::config;

/// Largest file this client downloads: the same 10 GiB as the upload limit.
pub const MAX_DOWNLOAD_BYTES: u64 = MAX_UPLOAD_BYTES;

/// Cap on the token response body.
const MAX_TOKEN_RESPONSE_BYTES: usize = 64 * 1024;
/// Redirect hop limit for the file request.
const MAX_REDIRECTS: usize = 5;
/// Error code the token route returns for private files.
const PRIVATE_FILE_CODE: &str = "E2007";

/// Settings for [`PublicDownloader`].
#[derive(Debug, Clone)]
pub struct DownloadConfig {
    token_url: Url,
    allowed_hosts: Vec<String>,
    fallback_servers: Vec<String>,
    /// TCP + TLS connect timeout.
    pub connect_timeout: Duration,
    /// Total timeout for the token request.
    pub token_timeout: Duration,
    /// The download fails if no body bytes arrive for this long.
    pub idle_timeout: Duration,
    /// Size cap, in bytes.
    pub max_bytes: u64,
}

impl DownloadConfig {
    /// Downloads using the token route at `token_url`, accepting file servers
    /// on `allowed_hosts` (exact host or parent domain).
    ///
    /// # Errors
    ///
    /// [`ApiError::Config`] if `token_url` is not `https` (literal loopback
    /// `http` excepted), carries credentials, or has no host.
    pub fn new(token_url: &str, allowed_hosts: &[&str]) -> Result<Self> {
        let url = Url::parse(token_url)
            .map_err(|e| ApiError::Config(format!("invalid download token URL: {e}")))?;
        let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && is_loopback_ip(&url));
        if !scheme_ok
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(ApiError::Config(
                "download token URL must be https with a host and no credentials".to_owned(),
            ));
        }
        Ok(Self {
            token_url: url,
            allowed_hosts: allowed_hosts
                .iter()
                .map(|h| h.to_ascii_lowercase())
                .collect(),
            fallback_servers: Vec::new(),
            connect_timeout: Duration::from_secs(15),
            token_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(120),
            max_bytes: MAX_DOWNLOAD_BYTES,
        })
    }

    /// The compiled-in production route, hosts and fallback servers.
    ///
    /// # Errors
    ///
    /// Only if the compiled-in constants are invalid, which a unit test
    /// rules out.
    pub fn production() -> Result<Self> {
        let mut cfg = Self::new(config::PUBLIC_DOWNLOAD_TOKEN_URL, config::DOWNLOAD_HOSTS)?;
        cfg.fallback_servers = config::DOWNLOAD_FALLBACK_SERVERS
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        Ok(cfg)
    }

    /// Validate a server origin named by the token route.
    fn server_url(&self, raw: &str) -> Result<Url> {
        let untrusted = || ApiError::UntrustedDownloadServer(sanitize_server_text(raw));
        let url = Url::parse(raw).map_err(|_| untrusted())?;
        let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && is_loopback_ip(&url));
        let clean = url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/";
        if scheme_ok && clean && self.host_allowed(&url) {
            Ok(url)
        } else {
            Err(untrusted())
        }
    }

    /// Whether the URL's host is an allowed host or a subdomain of one.
    fn host_allowed(&self, url: &Url) -> bool {
        let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
            return false;
        };
        self.allowed_hosts.iter().any(|allowed| {
            host == *allowed
                || host
                    .strip_suffix(allowed.as_str())
                    .is_some_and(|prefix| prefix.ends_with('.'))
        })
    }
}

/// Body of the token request.
#[derive(Serialize)]
struct TokenRequest<'a> {
    file_slug: &'a str,
}

/// Token response. Only `file` and `token` are required.
#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    server: Option<String>,
}

/// Downloads public files. Cheap to clone; clones share one connection pool.
#[derive(Clone)]
pub struct PublicDownloader {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    config: DownloadConfig,
}

impl std::fmt::Debug for PublicDownloader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublicDownloader")
            .field("token_url", &self.inner.config.token_url.as_str())
            .finish_non_exhaustive()
    }
}

impl PublicDownloader {
    /// Build a downloader.
    ///
    /// # Errors
    ///
    /// [`ApiError::Network`] if the TLS backend fails to initialise.
    pub fn new(config: DownloadConfig) -> Result<Self> {
        let policy_config = config.clone();
        let policy = redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error(super::error::CrossOriginRedirect);
            }
            let url = attempt.url();
            let scheme_ok =
                url.scheme() == "https" || (url.scheme() == "http" && is_loopback_ip(url));
            if scheme_ok && url.username().is_empty() && policy_config.host_allowed(url) {
                attempt.follow()
            } else {
                attempt.error(super::error::CrossOriginRedirect)
            }
        });
        let http = reqwest::Client::builder()
            .user_agent(config::USER_AGENT)
            .tls_backend_rustls()
            .tls_version_min(reqwest::tls::Version::TLS_1_2)
            .https_only(config.token_url.scheme() == "https")
            .connect_timeout(config.connect_timeout)
            .read_timeout(config.idle_timeout)
            .redirect(policy)
            .build()
            .map_err(ApiError::from_reqwest)?;
        Ok(Self {
            inner: Arc::new(Inner { http, config }),
        })
    }

    /// Download the public file `slug` to `dest`, which must not exist.
    ///
    /// `expected_size` is the size the API reported. The body must match it
    /// exactly. `progress` is called at most once per MiB. On error, `dest`
    /// is removed. If the future is dropped mid-transfer, the partial file
    /// stays and must be cleaned up by the caller (the cache prunes stale
    /// partial files).
    ///
    /// Returns the number of bytes written.
    ///
    /// # Errors
    ///
    /// - [`ApiError::InvalidArgument`] for a malformed slug.
    /// - [`ApiError::NotPublic`] if the file is private.
    /// - [`ApiError::UntrustedDownloadServer`] if the token names a server
    ///   outside the allowlist.
    /// - [`ApiError::DownloadTooLarge`] / [`ApiError::SizeMismatch`] for size
    ///   violations.
    /// - [`ApiError::Io`] if `dest` exists or cannot be written.
    /// - Status, network and decode errors as described on [`ApiError`].
    pub async fn download(
        &self,
        slug: &str,
        expected_size: u64,
        dest: &Path,
        progress: Option<ProgressCallback>,
    ) -> Result<u64> {
        if !is_valid_slug(slug) {
            return Err(ApiError::InvalidArgument("malformed file slug".to_owned()));
        }
        let limit = self.inner.config.max_bytes;
        if expected_size > limit {
            return Err(ApiError::DownloadTooLarge {
                size: expected_size,
                limit,
            });
        }
        let url = self.file_url(slug).await?;
        let resp = self
            .inner
            .http
            .get(url)
            .send()
            .await
            .map_err(ApiError::from_reqwest)?;
        if resp.status() != StatusCode::OK {
            return Err(error_from_response(resp).await);
        }
        if let Some(len) = resp.content_length()
            && len != expected_size
        {
            return Err(if len > limit {
                ApiError::DownloadTooLarge { size: len, limit }
            } else {
                ApiError::SizeMismatch {
                    expected: expected_size,
                    received: len,
                }
            });
        }

        let mut file = create_private(dest).await?;
        let written = stream_to(resp, &mut file, expected_size, progress).await;
        let result = match written {
            Ok(n) => file.sync_all().await.map(|()| n).map_err(ApiError::from),
            Err(e) => Err(e),
        };
        drop(file);
        if result.is_err() {
            // Best effort: the error being returned matters more.
            let _ = tokio::fs::remove_file(dest).await;
        }
        result
    }

    /// Request a token and build the file URL from it.
    async fn file_url(&self, slug: &str) -> Result<Url> {
        let cfg = &self.inner.config;
        let resp = self
            .inner
            .http
            .post(cfg.token_url.clone())
            .timeout(cfg.token_timeout)
            .json(&TokenRequest { file_slug: slug })
            .send()
            .await
            .map_err(ApiError::from_reqwest)?;
        let status = resp.status();
        if status == StatusCode::FORBIDDEN {
            return Err(ApiError::NotPublic);
        }
        if !status.is_success() {
            return Err(error_from_response(resp).await);
        }
        let body = read_capped(resp, MAX_TOKEN_RESPONSE_BYTES).await?;
        let token: TokenResponse = decode_json(&body)?;
        if token.success == Some(false) {
            if token.error.as_deref() == Some(PRIVATE_FILE_CODE) {
                return Err(ApiError::NotPublic);
            }
            return Err(ApiError::Rejected {
                message: token
                    .message
                    .map_or_else(|| "no message".to_owned(), |m| sanitize_server_text(&m)),
                code: token.error.map(|c| sanitize_server_text(&c)),
            });
        }
        let (Some(file), Some(secret)) = (token.file, token.token) else {
            return Err(ApiError::Decode(
                "download token response lacks \"file\" or \"token\"".to_owned(),
            ));
        };
        if file.is_empty() || file.contains('/') {
            return Err(ApiError::Decode(
                "download token names an invalid file".to_owned(),
            ));
        }

        let server = match token.server {
            Some(server) => server,
            None => cfg
                .fallback_servers
                .get(rand::random_range(0..cfg.fallback_servers.len().max(1)))
                .cloned()
                .ok_or_else(|| ApiError::Decode("download token names no server".to_owned()))?,
        };
        let mut url = cfg.server_url(&server)?;
        url.path_segments_mut()
            .map_err(|()| ApiError::UntrustedDownloadServer(sanitize_server_text(&server)))?
            .clear()
            .extend(["v2", file.as_str()]);
        url.query_pairs_mut()
            .append_pair("token", &secret)
            .append_pair("download", "true");
        debug!("download token issued");
        Ok(url)
    }
}

/// Create `path` exclusively, readable and writable by the owner only.
async fn create_private(path: &Path) -> Result<tokio::fs::File> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    Ok(options.open(path).await?)
}

/// Copy the body to `file`, refusing to write more than `expected` bytes.
async fn stream_to(
    mut resp: reqwest::Response,
    file: &mut tokio::fs::File,
    expected: u64,
    progress: Option<ProgressCallback>,
) -> Result<u64> {
    let mut received: u64 = 0;
    let mut reported: u64 = 0;
    if let Some(cb) = &progress {
        cb(TransferProgress {
            sent: 0,
            total: expected,
        });
    }
    while let Some(chunk) = resp.chunk().await.map_err(ApiError::from_reqwest)? {
        received = received.saturating_add(chunk.len() as u64);
        if received > expected {
            return Err(ApiError::SizeMismatch { expected, received });
        }
        file.write_all(&chunk).await?;
        if let Some(cb) = &progress
            && (received == expected || received - reported >= PROGRESS_STEP)
        {
            reported = received;
            cb(TransferProgress {
                sent: received,
                total: expected,
            });
        }
    }
    file.flush().await?;
    if received != expected {
        return Err(ApiError::SizeMismatch { expected, received });
    }
    Ok(received)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_config_is_valid() {
        let cfg = DownloadConfig::production().unwrap();
        assert_eq!(cfg.token_url.host_str(), Some("filester.me"));
        for server in config::DOWNLOAD_FALLBACK_SERVERS {
            assert!(cfg.server_url(server).is_ok(), "{server}");
        }
    }

    #[test]
    fn token_url_validation() {
        assert!(DownloadConfig::new("https://filester.me/v2/api/public/download", &[]).is_ok());
        assert!(DownloadConfig::new("http://127.0.0.1:9/x", &[]).is_ok());
        assert!(DownloadConfig::new("http://filester.me/x", &[]).is_err());
        assert!(DownloadConfig::new("https://u:p@filester.me/x", &[]).is_err());
    }

    #[test]
    fn server_allowlist() {
        let cfg = DownloadConfig::production().unwrap();
        for ok in [
            "https://fsc2.cdn.cr",
            "https://cdn.cr/",
            "https://cn1.filester.me",
            "https://FSC2.CDN.CR",
        ] {
            assert!(cfg.server_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://fsc2.cdn.cr",
            "https://evilcdn.cr",
            "https://cdn.cr.evil.example",
            "https://evil.example",
            "https://user@fsc2.cdn.cr",
            "https://fsc2.cdn.cr/sub",
            "https://fsc2.cdn.cr/?q=1",
            "https://127.0.0.1",
            "not a url",
        ] {
            assert!(
                matches!(
                    cfg.server_url(bad),
                    Err(ApiError::UntrustedDownloadServer(_))
                ),
                "{bad}"
            );
        }
    }
}
