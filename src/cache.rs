//! On-disk cache of downloaded files, used to open public files in local
//! apps.
//!
//! Layout, under a root such as `~/.cache/io.github.scroguard.Filester/files`:
//!
//! ```text
//! <root>/                      mode 0700
//! └─ <slug>/                   mode 0700, one directory per share slug
//!    ├─ <safe file name>       mode 0600, a completed download
//!    └─ .<random>.part         mode 0600, a download in progress
//! ```
//!
//! Rules (see README "Security & Auditing"):
//!
//! - The slug must pass [`is_valid_slug`] and the file name goes through
//!   [`safe_file_name`], so server-supplied text can never escape the root or
//!   create hidden or special names.
//! - Files are created exclusively (`O_EXCL`) and moved into place with
//!   `rename`, so a completed name only ever holds a complete download.
//! - Cleanup inspects entries with `symlink_metadata` and never follows a
//!   symlink. It removes only regular files, symlinks themselves, and the
//!   empty slug directories left behind. It never descends below the slug
//!   level.
//! - A file's age is its modification time. Opening a cached file refreshes
//!   it, so "older than N days" means "not opened for N days".
//!
//! Every method does blocking filesystem I/O. Call them from the worker
//! runtime (for example with `tokio::task::spawn_blocking`), never from the
//! GTK thread.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::api::models::is_valid_slug;
use crate::api::{ApiError, ProgressCallback, PublicDownloader};

/// Partial downloads untouched for this long are abandoned and removed by
/// every cleanup, whatever the retention setting.
pub const STALE_PART_AGE: Duration = Duration::from_hours(24);
/// [`FileCache::clear`] leaves partial downloads younger than this alone,
/// because they probably belong to a transfer that is still running.
const ACTIVE_PART_GRACE: Duration = Duration::from_mins(10);
/// Longest cached file name, in bytes.
const MAX_NAME_BYTES: usize = 200;
/// Longest extension kept when a long name is shortened, in bytes.
const MAX_EXTENSION_BYTES: usize = 16;
/// Suffix of partial downloads.
const PART_SUFFIX: &str = ".part";

/// What a cleanup removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CleanupReport {
    /// Files (and stray symlinks) removed.
    pub files_removed: u64,
    /// Bytes those files occupied.
    pub bytes_freed: u64,
}

/// Handle to the cache directory. Cheap to clone.
#[derive(Debug, Clone)]
pub struct FileCache {
    root: PathBuf,
}

/// A reserved download: write to `part`, then [`FileCache::commit`].
#[derive(Debug)]
pub struct CacheSlot {
    /// Temporary file to download into. It does not exist yet.
    pub part: PathBuf,
    /// Where the completed file will live.
    pub target: PathBuf,
}

impl FileCache {
    /// A cache rooted at `root`. Nothing is created until it is needed.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The cache directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The completed file for (`slug`, `name`), if it exists, is a regular
    /// file (not a symlink) and has `expected_size` bytes. A hit refreshes
    /// the file's modification time.
    #[must_use]
    pub fn lookup(&self, slug: &str, name: &str, expected_size: u64) -> Option<PathBuf> {
        if !is_valid_slug(slug) {
            return None;
        }
        let path = self.root.join(slug).join(safe_file_name(name));
        let meta = fs::symlink_metadata(&path).ok()?;
        if !meta.file_type().is_file() || meta.len() != expected_size {
            return None;
        }
        // Failing to refresh the timestamp only shortens the file's life.
        if let Ok(file) = fs::OpenOptions::new().write(true).open(&path) {
            let _ = file.set_modified(SystemTime::now());
        }
        Some(path)
    }

    /// Create the directories for `slug` and pick a fresh partial-file name.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::InvalidInput`] for a malformed slug, or any error
    /// creating the directories.
    pub fn reserve(&self, slug: &str, name: &str) -> io::Result<CacheSlot> {
        if !is_valid_slug(slug) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "malformed file slug",
            ));
        }
        create_private_dir(&self.root)?;
        let dir = self.root.join(slug);
        create_private_dir(&dir)?;
        let part = dir.join(format!(".{:016x}{PART_SUFFIX}", rand::random::<u64>()));
        Ok(CacheSlot {
            part,
            target: dir.join(safe_file_name(name)),
        })
    }

    /// Move a finished download into place, replacing an older copy.
    ///
    /// # Errors
    ///
    /// Any error from `rename`. The partial file is removed in that case.
    pub fn commit(&self, slot: CacheSlot) -> io::Result<PathBuf> {
        if let Err(err) = fs::rename(&slot.part, &slot.target) {
            let _ = fs::remove_file(&slot.part);
            return Err(err);
        }
        Ok(slot.target)
    }

    /// Delete completed files not opened for longer than `max_age` (skipped
    /// when `None`), and partial downloads older than [`STALE_PART_AGE`].
    ///
    /// A missing cache directory is not an error.
    ///
    /// # Errors
    ///
    /// Only if the root exists but cannot be listed. Individual entries that
    /// cannot be removed are skipped.
    pub fn prune(&self, max_age: Option<Duration>) -> io::Result<CleanupReport> {
        let now = SystemTime::now();
        self.sweep(|is_part, modified| {
            let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
            if is_part {
                age > STALE_PART_AGE
            } else {
                max_age.is_some_and(|max| age > max)
            }
        })
    }

    /// Delete every cached file, except partial downloads touched in the
    /// last few minutes, which probably belong to a running transfer.
    ///
    /// # Errors
    ///
    /// As for [`prune`](Self::prune).
    pub fn clear(&self) -> io::Result<CleanupReport> {
        let now = SystemTime::now();
        self.sweep(|is_part, modified| {
            !is_part || now.duration_since(modified).unwrap_or(Duration::ZERO) > ACTIVE_PART_GRACE
        })
    }

    /// Delete everything, partial downloads included. For sign-out, after
    /// every transfer has been cancelled.
    ///
    /// # Errors
    ///
    /// As for [`prune`](Self::prune).
    pub fn purge(&self) -> io::Result<CleanupReport> {
        self.sweep(|_, _| true)
    }

    /// Total bytes of regular files in the cache.
    ///
    /// # Errors
    ///
    /// Only if the root exists but cannot be listed.
    pub fn usage(&self) -> io::Result<u64> {
        let mut total = 0u64;
        for dir in list(&self.root)? {
            let Ok(meta) = fs::symlink_metadata(&dir) else {
                continue;
            };
            if meta.file_type().is_dir() {
                for entry in list(&dir).unwrap_or_default() {
                    if let Ok(meta) = fs::symlink_metadata(&entry)
                        && meta.file_type().is_file()
                    {
                        total = total.saturating_add(meta.len());
                    }
                }
            } else if meta.file_type().is_file() {
                total = total.saturating_add(meta.len());
            }
        }
        Ok(total)
    }

    /// Download the public file `slug` into the cache and return its path.
    ///
    /// # Errors
    ///
    /// Everything [`PublicDownloader::download`] returns, plus
    /// [`ApiError::Io`] for cache directory failures.
    pub async fn fetch(
        &self,
        downloader: &PublicDownloader,
        slug: &str,
        name: &str,
        size: u64,
        progress: Option<ProgressCallback>,
    ) -> Result<PathBuf, ApiError> {
        let cache = self.clone();
        let (slug_owned, name_owned) = (slug.to_owned(), name.to_owned());
        let slot = blocking(move || cache.reserve(&slug_owned, &name_owned)).await?;
        downloader
            .download(slug, size, &slot.part, progress)
            .await?;
        let cache = self.clone();
        Ok(blocking(move || cache.commit(slot)).await?)
    }

    /// Remove entries for which `doomed(is_partial, modified)` is true, then
    /// any slug directory left empty.
    fn sweep(&self, doomed: impl Fn(bool, SystemTime) -> bool) -> io::Result<CleanupReport> {
        let mut report = CleanupReport::default();
        let mut consider = |path: &Path, meta: &fs::Metadata| {
            let kind = meta.file_type();
            if !(kind.is_file() || kind.is_symlink()) {
                return;
            }
            let is_part = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.') && n.ends_with(PART_SUFFIX));
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if doomed(is_part, modified) && fs::remove_file(path).is_ok() {
                report.files_removed += 1;
                if kind.is_file() {
                    report.bytes_freed = report.bytes_freed.saturating_add(meta.len());
                }
            }
        };
        for top in list(&self.root)? {
            let Ok(meta) = fs::symlink_metadata(&top) else {
                continue;
            };
            if meta.file_type().is_dir() {
                for entry in list(&top).unwrap_or_default() {
                    if let Ok(meta) = fs::symlink_metadata(&entry) {
                        consider(&entry, &meta);
                    }
                }
                // Fails harmlessly unless the directory is now empty.
                let _ = fs::remove_dir(&top);
            } else {
                consider(&top, &meta);
            }
        }
        Ok(report)
    }
}

/// Make a server-supplied file name safe to use as one path component.
///
/// Path separators, NUL and other control characters become `_`. Leading
/// dots and surrounding whitespace are removed, so the result is never `.`,
/// `..`, hidden, or a partial-download name. Long names are cut to 200 bytes
/// at a character boundary, keeping a short extension so the file still
/// opens with the right app. An empty result becomes `file`.
#[must_use]
pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.').trim_start();
    if trimmed.is_empty() {
        return "file".to_owned();
    }
    if trimmed.len() <= MAX_NAME_BYTES {
        return trimmed.to_owned();
    }
    let extension = trimmed
        .rfind('.')
        .map(|i| &trimmed[i..])
        .filter(|ext| ext.len() <= MAX_EXTENSION_BYTES)
        .unwrap_or("");
    let mut cut = MAX_NAME_BYTES - extension.len();
    while !trimmed.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{extension}", trimmed[..cut].trim_end())
}

/// Create `dir` (and missing parents) and make sure it is owner-only.
fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        builder.mode(0o700);
        builder.create(dir)?;
        let meta = fs::symlink_metadata(dir)?;
        if !meta.file_type().is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "cache path exists and is not a directory",
            ));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    builder.create(dir)
}

/// Entries of `dir`; a missing directory is empty.
fn list(dir: &Path) -> io::Result<Vec<PathBuf>> {
    match fs::read_dir(dir) {
        Ok(entries) => Ok(entries.filter_map(|e| e.ok().map(|e| e.path())).collect()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err),
    }
}

/// Run blocking filesystem work off the async executor.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| io::Error::other(e.to_string()))?
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    const DAY: Duration = Duration::from_hours(24);

    fn age(path: &Path, by: Duration) {
        let file = fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - by).unwrap();
    }

    fn put(cache: &FileCache, slug: &str, name: &str, bytes: &[u8]) -> PathBuf {
        let slot = cache.reserve(slug, name).unwrap();
        fs::write(&slot.part, bytes).unwrap();
        cache.commit(slot).unwrap()
    }

    #[test]
    fn file_names_are_sanitised() {
        assert_eq!(safe_file_name("photo.jpg"), "photo.jpg");
        assert_eq!(safe_file_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(safe_file_name(".bashrc"), "bashrc");
        assert_eq!(safe_file_name(".."), "file");
        assert_eq!(safe_file_name("  "), "file");
        assert_eq!(safe_file_name("a\0b\nc"), "a_b_c");
        assert_eq!(safe_file_name("..x.part"), "x.part");
        let long = format!("{}.mkv", "é".repeat(150));
        let short = safe_file_name(&long);
        assert!(short.len() <= MAX_NAME_BYTES);
        assert_eq!(
            Path::new(&short).extension().and_then(|e| e.to_str()),
            Some("mkv")
        );
    }

    #[test]
    fn reserve_commit_lookup_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = FileCache::new(tmp.path().join("files"));
        let path = put(&cache, "aBc123", "../x.txt", b"hello");
        assert_eq!(path, cache.root().join("aBc123").join("_x.txt"));
        assert_eq!(cache.lookup("aBc123", "../x.txt", 5), Some(path.clone()));
        assert_eq!(
            cache.lookup("aBc123", "../x.txt", 6),
            None,
            "size must match"
        );
        assert_eq!(cache.lookup("..", "x", 5), None);
        assert!(cache.reserve("../evil", "x").is_err());
        let root_mode = fs::metadata(cache.root()).unwrap().permissions().mode();
        assert_eq!(root_mode & 0o777, 0o700);
        assert_eq!(cache.usage().unwrap(), 5);
    }

    #[test]
    fn lookup_refreshes_age() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = FileCache::new(tmp.path().to_path_buf());
        let path = put(&cache, "s1", "a.txt", b"a");
        age(&path, 10 * DAY);
        assert!(cache.lookup("s1", "a.txt", 1).is_some());
        assert_eq!(cache.prune(Some(DAY)).unwrap().files_removed, 0);
    }

    #[test]
    fn prune_removes_only_old_files_and_stale_parts() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = FileCache::new(tmp.path().to_path_buf());
        let old = put(&cache, "old", "a.txt", b"12345");
        let fresh = put(&cache, "new", "b.txt", b"1");
        age(&old, 8 * DAY);
        let stale = cache.reserve("new", "c").unwrap().part;
        fs::write(&stale, b"xx").unwrap();
        age(&stale, 2 * DAY);
        let active = cache.reserve("new", "d").unwrap().part;
        fs::write(&active, b"y").unwrap();

        // Retention off: only the stale partial goes.
        let report = cache.prune(None).unwrap();
        assert_eq!(report.files_removed, 1);
        assert!(!stale.exists() && old.exists());

        let report = cache.prune(Some(7 * DAY)).unwrap();
        assert_eq!(
            report,
            CleanupReport {
                files_removed: 1,
                bytes_freed: 5
            }
        );
        assert!(!old.exists() && !old.parent().unwrap().exists());
        assert!(fresh.exists() && active.exists());
    }

    #[test]
    fn cleanup_never_follows_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let precious = outside.join("precious.txt");
        fs::write(&precious, b"keep").unwrap();

        let cache = FileCache::new(tmp.path().join("cache"));
        put(&cache, "s1", "a.txt", b"a");
        std::os::unix::fs::symlink(&precious, cache.root().join("s1").join("link")).unwrap();
        std::os::unix::fs::symlink(&outside, cache.root().join("dirlink")).unwrap();

        assert_eq!(cache.lookup("s1", "link", 4), None, "symlinks are not hits");
        let report = cache.clear().unwrap();
        assert_eq!(report.files_removed, 3); // a.txt, link, dirlink
        assert_eq!(report.bytes_freed, 1);
        assert!(precious.exists() && outside.is_dir());
    }

    #[test]
    fn clear_spares_active_partials() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = FileCache::new(tmp.path().to_path_buf());
        put(&cache, "s1", "a.txt", b"a");
        let active = cache.reserve("s1", "b").unwrap().part;
        fs::write(&active, b"b").unwrap();
        assert_eq!(cache.clear().unwrap().files_removed, 1);
        assert!(active.exists());
        assert_eq!(cache.purge().unwrap().files_removed, 1);
        assert!(!active.exists() && !cache.root().join("s1").exists());
    }

    #[test]
    fn missing_root_is_empty() {
        let cache = FileCache::new(PathBuf::from("/nonexistent/filester-cache-test"));
        assert_eq!(cache.usage().unwrap(), 0);
        assert_eq!(cache.prune(Some(DAY)).unwrap(), CleanupReport::default());
    }
}
