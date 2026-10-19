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
