//! User preferences, stored as a small key file.
//!
//! Location: `$XDG_CONFIG_HOME/io.github.scroguard.Filester/settings.ini`
//! (inside Flatpak, the app's private config directory). A key file rather
//! than `GSettings` keeps `cargo run` working without installing a compiled
//! schema.
//!
//! The file holds no secrets. Missing, unreadable or out-of-range values fall
//! back to the defaults, so a damaged file can never stop the app starting.
//! Loading reads a few hundred bytes synchronously at startup (as
//! `GSettings` does). Saving is written off the GTK thread through
//! [`write_atomically`].

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use filester_app::config;
use gtk::glib;
use tracing::warn;

const GROUP_OPEN: &str = "open";
const KEY_MODE: &str = "mode";
const GROUP_CACHE: &str = "cache";
const KEY_AUTO_CLEAN: &str = "auto-clean";
const KEY_MAX_AGE_DAYS: &str = "max-age-days";

/// Shortest retention the UI offers, in days.
pub const MIN_RETENTION_DAYS: u32 = 1;
/// Longest retention the UI offers, in days.
pub const MAX_RETENTION_DAYS: u32 = 365;
const DEFAULT_RETENTION_DAYS: u32 = 7;
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// What opening a file does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OpenMode {
    /// Open the share page in the web browser. Nothing is stored locally.
    #[default]
    Browser,
    /// Download public files to the cache and open them with the default
    /// app for their type. Private files still open in the browser.
    LocalApp,
}

impl OpenMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::LocalApp => "local-app",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "browser" => Some(Self::Browser),
            "local-app" => Some(Self::LocalApp),
            _ => None,
        }
    }
}

/// All user preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// What opening a file does.
    pub open_mode: OpenMode,
    /// Whether old cached files are removed when the app starts.
    pub auto_clean: bool,
    /// Files not opened for this many days are removed by the cleanup.
    pub retention_days: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            open_mode: OpenMode::Browser,
            auto_clean: true,
            retention_days: DEFAULT_RETENTION_DAYS,
        }
    }
}

impl Settings {
    /// Load from `path`, falling back to defaults for anything missing or
    /// invalid.
    pub fn load(path: &Path) -> Self {
        let file = glib::KeyFile::new();
        match file.load_from_file(path, glib::KeyFileFlags::NONE) {
            Ok(()) => Self::from_key_file(&file),
            Err(err) if err.matches(glib::FileError::Noent) => Self::default(),
            Err(err) => {
                warn!(error = %err, "could not read settings; using defaults");
                Self::default()
            }
        }
    }

    fn from_key_file(file: &glib::KeyFile) -> Self {
        let defaults = Self::default();
        let open_mode = file
            .string(GROUP_OPEN, KEY_MODE)
            .ok()
            .and_then(|s| OpenMode::parse(s.as_str()))
            .unwrap_or(defaults.open_mode);
        let auto_clean = file
            .boolean(GROUP_CACHE, KEY_AUTO_CLEAN)
            .unwrap_or(defaults.auto_clean);
        let retention_days = file
            .integer(GROUP_CACHE, KEY_MAX_AGE_DAYS)
            .ok()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| (MIN_RETENTION_DAYS..=MAX_RETENTION_DAYS).contains(n))
            .unwrap_or(defaults.retention_days);
        Self {
            open_mode,
            auto_clean,
            retention_days,
        }
    }

    /// Serialise to key-file text.
    pub fn to_data(self) -> String {
        let file = glib::KeyFile::new();
        file.set_comment(
            None,
            None,
            " Filester preferences. Edit from the app's Preferences window.",
        )
        .ok();
        file.set_string(GROUP_OPEN, KEY_MODE, self.open_mode.as_str());
        file.set_boolean(GROUP_CACHE, KEY_AUTO_CLEAN, self.auto_clean);
        file.set_integer(
            GROUP_CACHE,
            KEY_MAX_AGE_DAYS,
            i32::try_from(self.retention_days).unwrap_or(i32::MAX),
        );
        file.to_data().to_string()
    }

    /// Retention for the startup cleanup, or `None` when it is off.
    pub fn retention(self) -> Option<Duration> {
        self.auto_clean
            .then(|| Duration::from_secs(u64::from(self.retention_days) * SECONDS_PER_DAY))
    }
}

/// `$XDG_CONFIG_HOME/<app id>/settings.ini`.
pub fn settings_path() -> PathBuf {
    glib::user_config_dir()
        .join(config::APP_ID)
        .join("settings.ini")
}

/// `$XDG_CACHE_HOME/<app id>/files`.
pub fn cache_dir() -> PathBuf {
    glib::user_cache_dir().join(config::APP_ID).join("files")
}

/// Write `data` to `path` via a temporary file and `rename`, so a crash
/// never leaves a half-written file. The directory is created owner-only.
/// Blocking: run it on the worker.
pub fn write_atomically(path: &Path, data: &str) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let tmp = path.with_extension("ini.tmp");
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(data.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Settings {
        let file = glib::KeyFile::new();
        file.load_from_data(text, glib::KeyFileFlags::NONE).unwrap();
        Settings::from_key_file(&file)
    }

    #[test]
    fn defaults_are_browser_and_weekly_cleanup() {
        let d = Settings::default();
        assert_eq!(d.open_mode, OpenMode::Browser);
        assert_eq!(
            d.retention(),
            Some(Duration::from_secs(7 * SECONDS_PER_DAY))
        );
    }

    #[test]
    fn round_trip() {
        let s = Settings {
            open_mode: OpenMode::LocalApp,
            auto_clean: false,
            retention_days: 30,
        };
        assert_eq!(parse(&s.to_data()), s);
        assert_eq!(s.retention(), None);
    }

    #[test]
    fn invalid_values_fall_back_per_key() {
        let s = parse("[open]\nmode=telnet\n[cache]\nauto-clean=maybe\nmax-age-days=0\n");
        assert_eq!(s, Settings::default());
        let s = parse("[open]\nmode=local-app\n[cache]\nmax-age-days=99999\n");
        assert_eq!(s.open_mode, OpenMode::LocalApp);
        assert_eq!(s.retention_days, DEFAULT_RETENTION_DAYS);
    }

    #[test]
    fn missing_file_gives_defaults_and_save_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("app").join("settings.ini");
        assert_eq!(Settings::load(&path), Settings::default());
        let s = Settings {
            retention_days: 3,
            ..Settings::default()
        };
        write_atomically(&path, &s.to_data()).unwrap();
        assert_eq!(Settings::load(&path), s);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
