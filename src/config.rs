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
