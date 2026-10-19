//! Compile-time application identity shared by every module.
//!
//! Values here are baked into the binary and must match the installed
//! desktop metadata in `data/` (the `.desktop` file name, the `AppStream`
//! `<id>`, and the icon name are all derived from [`APP_ID`]).

/// Reverse-DNS application identifier, used for the `GApplication` D-Bus name,
/// the desktop entry, the icon, and the Secret Service attribute namespace.
pub const APP_ID: &str = "io.github.scroguard.Filester";

/// Human-readable application name shown in window titles.
pub const APP_NAME: &str = "Filester";

/// Environment variable that controls log verbosity, using `tracing`
/// `EnvFilter` syntax (for example `FILESTER_LOG=filester_app=debug`).
pub const LOG_ENV_VAR: &str = "FILESTER_LOG";

/// Origin of the Filester REST API.
///
/// The public website is `filester.me`, but the REST API is served from
/// `u1.filester.me` (documented at <https://filester.me/api-docs>). Requests
/// to `https://filester.me/api/v1/...` return 404. This is a compile-time
/// constant with no runtime override, so the API key cannot be redirected
/// to another host through configuration.
pub const API_BASE_URL: &str = "https://u1.filester.me";

/// `User-Agent` sent with every request. It identifies only the client name
/// and version.
pub const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

/// Endpoint that issues short-lived download tokens for **public** files.
///
/// This is not part of the documented REST API. It is the route the share
/// page's Download button calls (`/js/file_dl.js` on `filester.me`). It
/// takes no API key and refuses private files with HTTP 403. The API key is
/// never sent here; see [`crate::api::PublicDownloader`].
pub const PUBLIC_DOWNLOAD_TOKEN_URL: &str = "https://filester.me/v2/api/public/download";

/// Hosts allowed to serve file contents named in a download-token response,
/// matched exactly or as a parent domain (`cdn.cr` admits `fsc2.cdn.cr`).
/// Observed in October 2026: the token names `*.cdn.cr` servers, and the
/// site's script falls back to `*.filester.me` mirrors. A server outside
/// this list is refused rather than contacted.
pub const DOWNLOAD_HOSTS: &[&str] = &["filester.me", "cdn.cr"];

/// Download servers used when a token response names none, mirroring the
/// share page's own fallback list.
pub const DOWNLOAD_FALLBACK_SERVERS: &[&str] = &[
    "https://cn1.filester.me",
    "https://p1.filester.me",
    "https://rs2.filester.me",
];
