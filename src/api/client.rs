//! The Filester HTTP client.
//!
//! [`FilesterClient`] wraps a single `reqwest::Client` (connection pool, TLS
//! session cache) behind an `Arc`, so clones are cheap and share the pool
//! and the rate-limit state.
//!
//! Request lifecycle, implemented in [`FilesterClient::execute`]:
//!
//! 1. If the known budget is exhausted, wait for the reset, or fail fast if
//!    the wait exceeds [`RetryPolicy::max_wait`].
//! 2. Build the request (fresh each attempt; uploads reopen the file).
//! 3. Send it, racing a [`StallWatch`] for uploads.
//! 4. Record `X-RateLimit-*` headers.
//! 5. On 429 (any method) or 502/503/504 and timeouts (idempotent methods
//!    only), back off and retry. Connect failures are retried for every
//!    method, because the request never left the machine.
//! 6. Map non-success statuses to [`ApiError`] and decode JSON with a size
//!    cap.
//!
//! Security properties (see README "Security & Auditing"):
//!
//! - The base URL must be `https`. The only exception is literal loopback
//!   addresses, used by the integration tests' mock server. `https_only` is
//!   set on the reqwest client whenever the base is `https`.
//! - TLS: rustls with the platform verifier, minimum TLS 1.2, verification
//!   not configurable.
//! - Redirects are followed only within the API origin (at most 5 hops).
//!   Anything else is refused.
//! - The bearer header is attached per request from [`ApiKey`] and marked
//!   sensitive. The client's `Debug` output never includes it.

use std::fmt;
use std::future::{Future, ready};
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use reqwest::header::{AUTHORIZATION, HeaderValue, RETRY_AFTER};
use reqwest::multipart::Form;
use reqwest::{RequestBuilder, Response, StatusCode, redirect};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracing::{debug, warn};
use url::{Host, Url};

use super::error::{CrossOriginRedirect, sanitize_server_text};
use super::models::{
    Account, BulkMoveReport, CreateFolderBody, CreatedFolder, DeleteBody, DeleteReport, Envelope,
    FileEntry, FileQuery, FilesPage, Folder, FolderFilter, FolderId, FolderVisibility, Health,
    MoveFileBody, MoveFilesBody, MoveFolderBody, MoveFoldersBody, MovedFile, MovedFolder,
    Pagination, ROOT, UploadResult, UploadStatus,
};
use super::rate_limit::{RateLimitInfo, RateLimiter, RetryPolicy, parse_retry_after};
use super::tree::FolderTree;
use super::upload::{
    MAX_THUMBNAIL_BYTES, MAX_UPLOAD_BYTES, ProgressCallback, StallWatch, UploadCounter,
    UploadSource,
};
use super::{ApiError, ApiKey, Result};
use crate::config;

/// Maximum folder name length (API limit).
const MAX_FOLDER_NAME_CHARS: usize = 100;
/// Maximum items in one bulk move (API limit).
const MAX_BULK_ITEMS: usize = 200;
/// Cap on error-response bodies we bother to read.
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
/// Redirect hop limit (same-origin only).
const MAX_REDIRECTS: usize = 5;

/// Tunable client settings. [`ClientConfig::new`] gives production defaults.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    base_url: Url,
    /// Retry and backoff limits.
    pub retry: RetryPolicy,
    /// TCP + TLS connect timeout.
    pub connect_timeout: Duration,
    /// Per-read idle timeout on responses.
    pub read_timeout: Duration,
    /// Total timeout for ordinary (non-upload) requests.
    pub request_timeout: Duration,
    /// Upload fails if no body bytes are accepted for this long.
    pub upload_idle_timeout: Duration,
    /// After an upload body is fully sent, how long to wait for the reply.
    /// Large files may need server-side processing time.
    pub upload_response_timeout: Duration,
    /// Cap on JSON response bodies, against memory exhaustion by a hostile
    /// or broken server.
    pub max_response_bytes: usize,
}

impl ClientConfig {
    /// Production defaults against `base_url`.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Config`] if `base_url` does not parse, is not
    /// `https` (literal loopback `http` excepted), carries credentials, a
    /// query or a fragment, or has a non-root path.
    pub fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            base_url: validate_base_url(base_url)?,
            retry: RetryPolicy::default(),
            connect_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_secs(60),
            upload_idle_timeout: Duration::from_secs(120),
            upload_response_timeout: Duration::from_secs(600),
            max_response_bytes: 32 * 1024 * 1024,
        })
    }

    /// Defaults against [`config::API_BASE_URL`].
    ///
    /// # Errors
    ///
    /// Only if the compiled-in constant is invalid, which a unit test rules
    /// out.
    pub fn production() -> Result<Self> {
        Self::new(config::API_BASE_URL)
    }

    /// The validated API origin.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }
}

fn validate_base_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|e| ApiError::Config(format!("invalid base URL: {e}")))?;
    let loopback = match url.host() {
        Some(Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
        Some(Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        _ => false,
    };
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        other => {
            return Err(ApiError::Config(format!(
                "base URL must use https (got {other}://); plain http is only accepted for \
                 literal loopback addresses"
            )));
        }
    }
    if url.host().is_none() {
        return Err(ApiError::Config("base URL has no host".to_owned()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ApiError::Config(
            "base URL must not contain credentials".to_owned(),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ApiError::Config(
            "base URL must not have a query or fragment".to_owned(),
        ));
    }
    if url.path() != "/" {
        return Err(ApiError::Config("base URL must not have a path".to_owned()));
    }
    Ok(url)
}

/// The `pagination` sibling of `data` in `GET /api/v1/files`.
#[derive(serde::Deserialize)]
struct Paged {
    #[serde(default)]
    pagination: Pagination,
}

/// Whether a request may be repeated without side effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Idempotency {
    /// GET: retry on 429, 502/503/504, timeouts and connect errors.
    Safe,
    /// POST: retry only on 429 (rejected before processing) and connect
    /// errors (never sent).
    Unsafe,
}

/// Asynchronous Filester API client.
#[derive(Clone)]
pub struct FilesterClient {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    config: ClientConfig,
    key: Option<ApiKey>,
    limiter: RateLimiter,
}

impl fmt::Debug for FilesterClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FilesterClient")
            .field("base_url", &self.inner.config.base_url.as_str())
            .field("authenticated", &self.inner.key.is_some())
            .finish_non_exhaustive()
    }
}

impl FilesterClient {
    /// Build a client. Pass `None` for guest mode, which allows only
    /// [`health`](Self::health) and [`upload`](Self::upload).
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Network`] if the TLS backend fails to initialise
    /// (for example, the platform trust store cannot be loaded).
    pub fn new(config: ClientConfig, key: Option<ApiKey>) -> Result<Self> {
        let origin = config.base_url.origin();
        let policy = redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error(CrossOriginRedirect)
            } else if attempt.url().origin() == origin {
                attempt.follow()
            } else {
                attempt.error(CrossOriginRedirect)
            }
        });
        let http = reqwest::Client::builder()
            .user_agent(config::USER_AGENT)
            .tls_backend_rustls()
            .tls_version_min(reqwest::tls::Version::TLS_1_2)
            .https_only(config.base_url.scheme() == "https")
            .connect_timeout(config.connect_timeout)
            .read_timeout(config.read_timeout)
            .redirect(policy)
            .build()
            .map_err(ApiError::from_reqwest)?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                config,
                key,
                limiter: RateLimiter::default(),
            }),
        })
    }

    /// Whether the client holds an API key.
    #[must_use]
    pub fn is_authenticated(&self) -> bool {
        self.inner.key.is_some()
    }

    /// Latest rate-limit budget seen in response headers.
    #[must_use]
    pub fn rate_limit(&self) -> Option<RateLimitInfo> {
        self.inner.limiter.snapshot()
    }

    // --- Health & account -----------------------------------------------------

    /// `GET /health`. Needs no authentication.
    ///
    /// # Errors
    ///
    /// Network, status and decode errors as described on [`ApiError`].
    pub async fn health(&self) -> Result<Health> {
        let url = self.endpoint(&["health"])?;
        let resp = self
            .execute(Idempotency::Safe, None, || ready(Ok(self.get(url.clone()))))
            .await?;
        self.decode_plain(resp).await
    }

    /// `GET /api/v1/account`.
    ///
    /// # Errors
    ///
    /// [`ApiError::NotSignedIn`] without a key, otherwise as on [`ApiError`].
    pub async fn account(&self) -> Result<Account> {
        self.get_data(&["api", "v1", "account"]).await
    }

    // --- Folders ----------------------------------------------------------------

    /// `GET /api/v1/folders`: every folder, flat.
    ///
    /// # Errors
    ///
    /// See [`ApiError`].
    pub async fn list_folders(&self) -> Result<Vec<Folder>> {
        self.get_data(&["api", "v1", "folders"]).await
    }

    /// Fetch every folder and rebuild the hierarchy from parent links.
    ///
    /// Anomalies (orphans, cycles, duplicate IDs) are repaired and logged.
    /// See [`FolderTree::anomalies`].
    ///
    /// # Errors
    ///
    /// See [`ApiError`].
    pub async fn folder_tree(&self) -> Result<FolderTree> {
        let tree = FolderTree::build(self.list_folders().await?);
        for anomaly in tree.anomalies() {
            warn!(
                ?anomaly,
                "repaired inconsistent folder hierarchy from server"
            );
        }
        Ok(tree)
    }

    /// `GET /api/v1/folder/{id}/files`.
    ///
    /// # Errors
    ///
    /// See [`ApiError`].
    pub async fn folder_files(&self, folder: &FolderId) -> Result<Vec<FileEntry>> {
        self.get_data(&["api", "v1", "folder", folder.as_str(), "files"])
            .await
    }

    /// `POST /api/v1/folder`. `parent = None` creates a top-level folder.
    ///
    /// # Errors
    ///
    /// [`ApiError::InvalidArgument`] if the trimmed name is empty, longer
    /// than 100 characters, or contains control characters. Otherwise see
    /// [`ApiError`].
    pub async fn create_folder(
        &self,
        name: &str,
        parent: Option<&FolderId>,
        visibility: FolderVisibility,
        password: Option<&SecretString>,
    ) -> Result<CreatedFolder> {
        let name = validate_folder_name(name)?;
        let body = CreateFolderBody {
            name,
            parent: parent.map(FolderId::as_str),
            public: u8::from(visibility == FolderVisibility::Public),
            password: password.map(ExposeSecret::expose_secret),
        };
        self.post_data(&["api", "v1", "folder"], &body).await
    }

    /// `POST /api/v1/folder/move`. `parent = None` moves to the top level.
    ///
    /// # Errors
    ///
    /// The server rejects moves into the folder's own subtree, name clashes
    /// and depth overflow; see [`ApiError`].
    pub async fn move_folder(
        &self,
        folder: &FolderId,
        parent: Option<&FolderId>,
    ) -> Result<MovedFolder> {
        let body = MoveFolderBody {
            folder: folder.as_str(),
            parent: dest(parent),
        };
        self.post_data(&["api", "v1", "folder", "move"], &body)
            .await
    }

    /// `POST /api/v1/folders/move`. Partial failure is reported in the
    /// result, not as an error.
    ///
    /// # Errors
    ///
    /// [`ApiError::InvalidArgument`] for an empty list or more than 200
    /// items. Otherwise see [`ApiError`].
    pub async fn move_folders(
        &self,
        folders: &[FolderId],
        parent: Option<&FolderId>,
    ) -> Result<BulkMoveReport> {
        let ids = bulk_ids(folders.iter().map(FolderId::as_str))?;
        let body = MoveFoldersBody {
            folders: &ids,
            parent: dest(parent),
        };
        self.post_bulk(&["api", "v1", "folders", "move"], &body)
            .await
    }

    /// `POST /folder/delete`: deletes folders **and every file inside them**.
    ///
    /// # Errors
    ///
    /// [`ApiError::InvalidArgument`] for an empty list. Otherwise see
    /// [`ApiError`].
    pub async fn delete_folders(&self, folders: &[FolderId]) -> Result<DeleteReport> {
        let ids = nonempty_ids(folders.iter().map(FolderId::as_str))?;
        self.post_plain(&["folder", "delete"], &DeleteBody { identifiers: &ids })
            .await
    }

    /// `POST /api/v1/folder/thumbnail`: set a folder's cover image (JPG, PNG,
    /// GIF or WebP, at most 5 MiB).
    ///
    /// # Errors
    ///
    /// [`ApiError::InvalidArgument`] for an unsupported extension,
    /// [`ApiError::FileTooLarge`] above 5 MiB, otherwise see [`ApiError`].
    pub async fn set_folder_thumbnail(&self, folder: &FolderId, image: &Path) -> Result<()> {
        let ext = image
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp") {
            return Err(ApiError::InvalidArgument(
                "thumbnail must be a JPG, PNG, GIF or WebP image".to_owned(),
            ));
        }
        let source = UploadSource::inspect(image, MAX_THUMBNAIL_BYTES).await?;
        let url = self.endpoint(&["api", "v1", "folder", "thumbnail"])?;
        let (this, source) = (self, &source);
        let resp = self
            .execute(Idempotency::Unsafe, None, move || {
                let url = url.clone();
                async move {
                    let form = Form::new()
                        .text("folder", folder.as_str().to_owned())
                        .part("thumbnail", source.buffered_part().await?);
                    this.authed(this.inner.http.post(url).multipart(form))
                        .map(|rb| rb.timeout(this.inner.config.request_timeout))
                }
            })
            .await?;
        let _: Envelope<serde_json::Value> = self.decode_envelope(resp).await?;
        Ok(())
    }

    // --- Files ----------------------------------------------------------------

    /// `GET /api/v1/files`: one page of files.
    ///
    /// # Errors
    ///
    /// [`ApiError::InvalidArgument`] if `page` is 0 or `per_page` is outside
    /// `1..=100`. Otherwise see [`ApiError`].
    pub async fn list_files(&self, query: &FileQuery) -> Result<FilesPage> {
        if query.page == 0 || !(1..=100).contains(&query.per_page) {
            return Err(ApiError::InvalidArgument(
                "page must be ≥ 1 and per_page within 1..=100".to_owned(),
            ));
        }
        let mut url = self.endpoint(&["api", "v1", "files"])?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("page", &query.page.to_string());
            q.append_pair("per_page", &query.per_page.to_string());
            match &query.folder {
                FolderFilter::Any => {}
                FolderFilter::Root => {
                    q.append_pair("folder", ROOT);
                }
                FolderFilter::Folder(id) => {
                    q.append_pair("folder", id.as_str());
                }
            }
            if let Some(term) = query
                .search
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                q.append_pair("search", term);
            }
        }
        let resp = self
            .execute(Idempotency::Safe, None, || {
                ready(self.authed(self.get(url.clone())))
            })
            .await?;

        let bytes = self.checked_body(resp).await?;
        let env: Envelope<Vec<FileEntry>> = decode_json(&bytes)?;
        let files = envelope_data(env)?;
        let Paged { pagination } = decode_json(&bytes)?;
        Ok(FilesPage { files, pagination })
    }

    /// `GET /api/v1/file/{id}`. `id` may be a slug, UUID or numeric ID.
    ///
    /// # Errors
    ///
    /// See [`ApiError`].
    pub async fn file(&self, id: &str) -> Result<FileEntry> {
        self.get_data(&["api", "v1", "file", id]).await
    }

    /// `POST /api/v1/file/move`. `folder = None` moves to the root.
    ///
    /// # Errors
    ///
    /// See [`ApiError`].
    pub async fn move_file(&self, file: &str, folder: Option<&FolderId>) -> Result<MovedFile> {
        let body = MoveFileBody {
            file,
            folder: dest(folder),
        };
        self.post_data(&["api", "v1", "file", "move"], &body).await
    }

    /// `POST /api/v1/files/move`. Partial failure is reported in the result.
    ///
    /// # Errors
    ///
    /// [`ApiError::InvalidArgument`] for an empty list or more than 200
    /// items. Otherwise see [`ApiError`].
    pub async fn move_files(
        &self,
        files: &[String],
        folder: Option<&FolderId>,
    ) -> Result<BulkMoveReport> {
        let ids = bulk_ids(files.iter().map(String::as_str))?;
        let body = MoveFilesBody {
            files: &ids,
            folder: dest(folder),
        };
        self.post_bulk(&["api", "v1", "files", "move"], &body).await
    }

    /// `POST /file/delete`.
    ///
    /// # Errors
    ///
    /// [`ApiError::InvalidArgument`] for an empty list. Otherwise see
    /// [`ApiError`].
    pub async fn delete_files(&self, files: &[String]) -> Result<DeleteReport> {
        let ids = nonempty_ids(files.iter().map(String::as_str))?;
        self.post_plain(&["file", "delete"], &DeleteBody { identifiers: &ids })
            .await
    }

    // --- Upload ---------------------------------------------------------------

    /// `POST /api/v1/upload`: stream a local file (up to 10 GiB) as
    /// multipart/form-data, optionally into `folder`.
    ///
    /// Works in guest mode (no key). Memory use is independent of file size.
    /// `progress` is called from a worker thread at most once per MiB.
    /// Dropping the returned future aborts the transfer.
    ///
    /// # Errors
    ///
    /// [`ApiError::FileTooLarge`], [`ApiError::Io`] (unreadable, or changed
    /// mid-upload), [`ApiError::Stalled`], or anything on [`ApiError`].
    pub async fn upload(
        &self,
        path: &Path,
        folder: Option<&FolderId>,
        progress: Option<ProgressCallback>,
    ) -> Result<UploadResult> {
        let source = UploadSource::inspect(path, MAX_UPLOAD_BYTES).await?;
        let url = self.endpoint(&["api", "v1", "upload"])?;
        let folder_header = folder
            .map(|f| {
                HeaderValue::from_str(f.as_str()).map_err(|_| {
                    ApiError::InvalidArgument("folder ID is not a valid header value".to_owned())
                })
            })
            .transpose()?;
        let counter = Arc::new(UploadCounter::default());
        let watch = StallWatch {
            counter: Arc::clone(&counter),
            total: source.size,
            idle: self.inner.config.upload_idle_timeout,
            response_timeout: self.inner.config.upload_response_timeout,
        };
        debug!(size = source.size, "starting upload");

        let (this, source, folder_header) = (self, &source, folder_header.as_ref());
        let resp = self
            .execute(Idempotency::Unsafe, Some(&watch), move || {
                let (url, counter, progress) =
                    (url.clone(), Arc::clone(&counter), progress.clone());
                async move {
                    let part = source.streaming_part(counter, progress).await?;
                    let mut rb = this
                        .inner
                        .http
                        .post(url)
                        .multipart(Form::new().part("file", part));
                    if let Some(h) = folder_header {
                        rb = rb.header("X-Folder-ID", h.clone());
                    }
                    if this.inner.key.is_some() {
                        this.authed(rb)
                    } else {
                        Ok(rb)
                    }
                }
            })
            .await?;
        // Upload results are top-level fields next to "success".
        self.decode_plain_checked(resp).await
    }

    /// `GET /api/v1/upload/status?slug=…`.
    ///
    /// # Errors
    ///
    /// See [`ApiError`].
    pub async fn upload_status(&self, slug: &str) -> Result<UploadStatus> {
        let mut url = self.endpoint(&["api", "v1", "upload", "status"])?;
        url.query_pairs_mut().append_pair("slug", slug);
        let resp = self
            .execute(Idempotency::Safe, None, || {
                ready(self.authed(self.get(url.clone())))
            })
            .await?;
        self.decode_plain_checked(resp).await
    }

    // --- Plumbing -------------------------------------------------------------

    /// Base URL with `segments` appended. Each segment is percent-encoded,
    /// so a server-supplied ID can never inject path components or a query.
    fn endpoint(&self, segments: &[&str]) -> Result<Url> {
        let mut url = self.inner.config.base_url.clone();
        url.path_segments_mut()
            .map_err(|()| ApiError::Config("base URL cannot have path segments".to_owned()))?
            .clear()
            .extend(segments);
        Ok(url)
    }

    fn get(&self, url: Url) -> RequestBuilder {
        self.inner
            .http
            .get(url)
            .timeout(self.inner.config.request_timeout)
    }

    fn post_json<B: Serialize + ?Sized>(&self, url: Url, body: &B) -> RequestBuilder {
        self.inner
            .http
            .post(url)
            .timeout(self.inner.config.request_timeout)
            .json(body)
    }

    /// Attach the bearer token, or fail with [`ApiError::NotSignedIn`].
    fn authed(&self, rb: RequestBuilder) -> Result<RequestBuilder> {
        let key = self.inner.key.as_ref().ok_or(ApiError::NotSignedIn)?;
        Ok(rb.header(AUTHORIZATION, key.authorization_header()?))
    }

    async fn get_data<T: DeserializeOwned>(&self, segments: &[&str]) -> Result<T> {
        let url = self.endpoint(segments)?;
        let resp = self
            .execute(Idempotency::Safe, None, || {
                ready(self.authed(self.get(url.clone())))
            })
            .await?;
        envelope_data(self.decode_envelope(resp).await?)
    }

    async fn post_data<B: Serialize, T: DeserializeOwned>(
        &self,
        segments: &[&str],
        body: &B,
    ) -> Result<T> {
        let resp = self.send_post(segments, body).await?;
        envelope_data(self.decode_envelope(resp).await?)
    }

    /// POST whose `data` must be returned even when `success` is false.
    async fn post_bulk<B: Serialize>(&self, segments: &[&str], body: &B) -> Result<BulkMoveReport> {
        let resp = self.send_post(segments, body).await?;
        let env: Envelope<BulkMoveReport> = self.decode_envelope(resp).await?;
        match env.data {
            Some(report) => Ok(report),
            None => Err(rejected(env.message, env.error)),
        }
    }

    /// POST whose result fields sit at the top level.
    async fn post_plain<B: Serialize, T: DeserializeOwned>(
        &self,
        segments: &[&str],
        body: &B,
    ) -> Result<T> {
        let resp = self.send_post(segments, body).await?;
        self.decode_plain_checked(resp).await
    }

    async fn send_post<B: Serialize>(&self, segments: &[&str], body: &B) -> Result<Response> {
        let url = self.endpoint(segments)?;
        self.execute(Idempotency::Unsafe, None, || {
            ready(self.authed(self.post_json(url.clone(), body)))
        })
        .await
    }

    /// Send with rate-limit pacing, retries and an optional stall watchdog.
    async fn execute<F, Fut>(
        &self,
        idempotency: Idempotency,
        watch: Option<&StallWatch>,
        mut build: F,
    ) -> Result<Response>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<RequestBuilder>>,
    {
        let policy = &self.inner.config.retry;
        let max_attempts = policy.max_attempts.max(1);
        let mut retry: u32 = 0;
        loop {
            if let Some(wait) = self.inner.limiter.wait_needed(SystemTime::now()) {
                if wait > policy.max_wait {
                    return Err(ApiError::RateLimited {
                        retry_after: Some(wait),
                    });
                }
                debug!(
                    wait_ms = wait.as_millis(),
                    "rate-limit budget exhausted; pausing"
                );
                tokio::time::sleep(wait).await;
            }

            let request = build().await?;
            let sent = match watch {
                None => request.send().await,
                Some(w) => tokio::select! {
                    r = request.send() => r,
                    e = w.expired() => return Err(e),
                },
            };
            let last_attempt = retry + 1 >= max_attempts;

            let (retry_after, reason) = match sent {
                Ok(resp) => {
                    let now = SystemTime::now();
                    if let Some(info) = RateLimitInfo::from_headers(resp.headers(), now) {
                        self.inner.limiter.observe(info);
                    }
                    let status = resp.status();
                    let retry_after = resp
                        .headers()
                        .get(RETRY_AFTER)
                        .and_then(|v| parse_retry_after(v, now));
                    if status == StatusCode::TOO_MANY_REQUESTS {
                        if let Some(ra) = retry_after {
                            self.inner.limiter.block_until(now + ra);
                        }
                        if last_attempt || retry_after.is_some_and(|ra| ra > policy.max_wait) {
                            return Err(ApiError::RateLimited { retry_after });
                        }
                        (retry_after, "HTTP 429")
                    } else if idempotency == Idempotency::Safe
                        && matches!(
                            status,
                            StatusCode::BAD_GATEWAY
                                | StatusCode::SERVICE_UNAVAILABLE
                                | StatusCode::GATEWAY_TIMEOUT
                        )
                        && !last_attempt
                        && retry_after.is_none_or(|ra| ra <= policy.max_wait)
                    {
                        (retry_after, "server unavailable")
                    } else {
                        return Ok(resp);
                    }
                }
                Err(err) => {
                    let transient =
                        err.is_connect() || (idempotency == Idempotency::Safe && err.is_timeout());
                    if !transient || last_attempt {
                        return Err(ApiError::from_reqwest(err));
                    }
                    (None, "transient network error")
                }
            };

            let delay = policy.delay_for(retry, retry_after, rand::random::<f64>());
            warn!(
                attempt = retry + 1,
                delay_ms = delay.as_millis(),
                reason,
                "retrying request"
            );
            tokio::time::sleep(delay).await;
            retry += 1;
        }
    }

    /// Map non-2xx responses to errors. On success, read the body under the
    /// size cap.
    async fn checked_body(&self, resp: Response) -> Result<Vec<u8>> {
        let status = resp.status();
        if status.is_success() {
            return read_capped(resp, self.inner.config.max_response_bytes).await;
        }
        let retry_after = resp
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| parse_retry_after(v, SystemTime::now()));
        let body = read_capped(resp, MAX_ERROR_BODY_BYTES)
            .await
            .unwrap_or_default();
        let env: Option<Envelope<serde_json::Value>> = serde_json::from_slice(&body).ok();
        let message = env
            .and_then(|e| e.message)
            .map(|m| sanitize_server_text(&m))
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| status.canonical_reason().unwrap_or("no message").to_owned());
        Err(match status {
            StatusCode::UNAUTHORIZED => ApiError::Unauthorized,
            StatusCode::FORBIDDEN => ApiError::Forbidden { message },
            StatusCode::NOT_FOUND => ApiError::NotFound { message },
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
                ApiError::BadRequest { message }
            }
            StatusCode::TOO_MANY_REQUESTS => ApiError::RateLimited { retry_after },
            s if s.is_server_error() => ApiError::Server {
                status: s.as_u16(),
                message,
            },
            s => ApiError::UnexpectedStatus {
                status: s.as_u16(),
                message,
            },
        })
    }

    async fn decode_envelope<T: DeserializeOwned>(&self, resp: Response) -> Result<Envelope<T>> {
        decode_json(&self.checked_body(resp).await?)
    }

    /// Decode a body with no envelope (for example `/health`).
    async fn decode_plain<T: DeserializeOwned>(&self, resp: Response) -> Result<T> {
        decode_json(&self.checked_body(resp).await?)
    }

    /// Decode top-level fields, but first honour `"success": false`.
    async fn decode_plain_checked<T: DeserializeOwned>(&self, resp: Response) -> Result<T> {
        let bytes = self.checked_body(resp).await?;
        let env: Envelope<serde::de::IgnoredAny> = decode_json(&bytes)?;
        if env.success == Some(false) {
            return Err(rejected(env.message, env.error));
        }
        decode_json(&bytes)
    }
}

/// Read a body, refusing to buffer more than `limit` bytes.
async fn read_capped(mut resp: Response, limit: usize) -> Result<Vec<u8>> {
    if resp.content_length().is_some_and(|len| len > limit as u64) {
        return Err(ApiError::ResponseTooLarge { limit });
    }
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(ApiError::from_reqwest)? {
        if buf.len().saturating_add(chunk.len()) > limit {
            return Err(ApiError::ResponseTooLarge { limit });
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

fn decode_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes)
        .map_err(|e| ApiError::Decode(sanitize_server_text(&e.to_string())))
}

/// Unwrap `data`, turning `"success": false` into [`ApiError::Rejected`].
fn envelope_data<T>(env: Envelope<T>) -> Result<T> {
    if env.success == Some(false) {
        return Err(rejected(env.message, env.error));
    }
    env.data
        .ok_or_else(|| ApiError::Decode("response has no \"data\" field".to_owned()))
}

fn rejected(message: Option<String>, code: Option<String>) -> ApiError {
    ApiError::Rejected {
        message: message
            .map(|m| sanitize_server_text(&m))
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| "no message".to_owned()),
        code: code
            .map(|c| sanitize_server_text(&c))
            .filter(|c| !c.is_empty()),
    }
}

/// Wire value for a move destination.
fn dest(folder: Option<&FolderId>) -> &str {
    folder.map_or(ROOT, FolderId::as_str)
}

fn nonempty_ids<'a>(ids: impl Iterator<Item = &'a str>) -> Result<Vec<String>> {
    let ids: Vec<String> = ids.map(str::to_owned).collect();
    if ids.is_empty() {
        return Err(ApiError::InvalidArgument("no items given".to_owned()));
    }
    Ok(ids)
}

fn bulk_ids<'a>(ids: impl Iterator<Item = &'a str>) -> Result<Vec<String>> {
    let ids = nonempty_ids(ids)?;
    if ids.len() > MAX_BULK_ITEMS {
        return Err(ApiError::InvalidArgument(format!(
            "at most {MAX_BULK_ITEMS} items per bulk move (got {})",
            ids.len()
        )));
    }
    Ok(ids)
}

fn validate_folder_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ApiError::InvalidArgument("folder name is empty".to_owned()));
    }
    if name.chars().count() > MAX_FOLDER_NAME_CHARS {
        return Err(ApiError::InvalidArgument(format!(
            "folder name exceeds {MAX_FOLDER_NAME_CHARS} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(ApiError::InvalidArgument(
            "folder name contains control characters".to_owned(),
        ));
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_base_url_is_valid() {
        let cfg = ClientConfig::production().unwrap();
        assert_eq!(cfg.base_url().as_str(), "https://u1.filester.me/");
    }

    #[test]
    fn base_url_validation() {
        assert!(validate_base_url("https://u1.filester.me").is_ok());
        assert!(validate_base_url("http://127.0.0.1:8080").is_ok());
        assert!(validate_base_url("http://[::1]:8080").is_ok());
        assert!(validate_base_url("http://u1.filester.me").is_err());
        assert!(validate_base_url("http://localhost:8080").is_err());
        assert!(validate_base_url("ftp://u1.filester.me").is_err());
        assert!(validate_base_url("https://user:pw@u1.filester.me").is_err());
        assert!(validate_base_url("https://u1.filester.me/?a=b").is_err());
        assert!(validate_base_url("https://u1.filester.me/#x").is_err());
        assert!(validate_base_url("https://u1.filester.me/api").is_err());
        assert!(validate_base_url("not a url").is_err());
    }

    #[test]
    fn endpoint_encodes_segments() {
        let client = FilesterClient::new(ClientConfig::production().unwrap(), None).unwrap();
        let url = client
            .endpoint(&["api", "v1", "folder", "a/b?c#d", "files"])
            .unwrap();
        assert_eq!(
            url.as_str(),
            "https://u1.filester.me/api/v1/folder/a%2Fb%3Fc%23d/files"
        );
        let dots = client.endpoint(&["api", "v1", "file", ".."]).unwrap();
        assert!(dots.path().starts_with("/api/v1/file"));
    }

    #[test]
    fn folder_name_rules() {
        assert_eq!(validate_folder_name("  Photos ").unwrap(), "Photos");
        assert!(validate_folder_name("   ").is_err());
        assert!(validate_folder_name(&"x".repeat(101)).is_err());
        assert!(validate_folder_name(&"é".repeat(100)).is_ok());
        assert!(validate_folder_name("a\nb").is_err());
    }

    #[test]
    fn bulk_limits() {
        let many: Vec<String> = (0..=MAX_BULK_ITEMS).map(|i| i.to_string()).collect();
        assert!(bulk_ids(many.iter().map(String::as_str)).is_err());
        assert!(bulk_ids(many[..MAX_BULK_ITEMS].iter().map(String::as_str)).is_ok());
        assert!(bulk_ids(std::iter::empty()).is_err());
    }

    #[test]
    fn debug_never_shows_key() {
        let key = ApiKey::new(SecretString::from("very-secret-key-123")).unwrap();
        let client = FilesterClient::new(ClientConfig::production().unwrap(), Some(key)).unwrap();
        let dbg = format!("{client:?}");
        assert!(!dbg.contains("very-secret"));
        assert!(dbg.contains("authenticated: true"));
    }
}
