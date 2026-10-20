//! Wire types for Filester API requests and responses.
//!
//! The shapes follow <https://filester.me/api-docs>. Deserialisation is
//! deliberately lenient where the docs are ambiguous or the API is in beta:
//!
//! - Optional fields default instead of failing.
//! - Numeric fields accept either JSON numbers or numeric strings.
//! - Booleans accept `true`/`false` or `1`/`0`.
//! - A parent or folder reference of `null`, `""` or `"root"` means the top
//!   level.
//!
//! Server-supplied names and URLs are untrusted. The UI must render them as
//! plain text. The only place a server name reaches the filesystem is the
//! download cache, and only after [`crate::cache::safe_file_name`].

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use url::Url;

/// Longest share slug accepted. Observed slugs are 7 characters.
const MAX_SLUG_LEN: usize = 64;

/// Whether `slug` looks like a share slug: 1 to 64 ASCII letters, digits,
/// `-` or `_`. Slugs name cache directories, so this rules out `.`, `/` and
/// anything else with filesystem meaning.
#[must_use]
pub fn is_valid_slug(slug: &str) -> bool {
    (1..=MAX_SLUG_LEN).contains(&slug.len())
        && slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Opaque folder identifier assigned by the server, for example
/// `"a1b2c3d4e5f6g7h8"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FolderId(String);

impl FolderId {
    /// Wrap a server-issued identifier.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The raw identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FolderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A folder from `GET /api/v1/folders`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "RawFolder")]
pub struct Folder {
    /// Server identifier.
    pub id: FolderId,
    /// Display name (at most 100 characters per the API limits).
    pub name: String,
    /// Whether the folder is publicly viewable.
    pub public: bool,
    /// Number of files directly inside this folder.
    pub file_count: u64,
    /// Number of immediate subfolders.
    pub folder_count: u64,
    /// Creation timestamp (RFC 3339 string as sent by the server).
    pub created_at: Option<String>,
    /// Parent folder, or `None` for a top-level folder.
    pub parent: Option<FolderId>,
}

/// Exact wire shape. The API sends both `parent` and its alias `parent_id`.
#[derive(Deserialize)]
struct RawFolder {
    id: FolderId,
    #[serde(default)]
    name: String,
    #[serde(default, deserialize_with = "flexible_bool")]
    public: bool,
    #[serde(default, deserialize_with = "flexible_u64")]
    file_count: u64,
    #[serde(default, deserialize_with = "flexible_u64")]
    folder_count: u64,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default, deserialize_with = "folder_ref")]
    parent: Option<FolderId>,
    #[serde(default, deserialize_with = "folder_ref")]
    parent_id: Option<FolderId>,
}

impl From<RawFolder> for Folder {
    fn from(raw: RawFolder) -> Self {
        Self {
            id: raw.id,
            name: raw.name,
            public: raw.public,
            file_count: raw.file_count,
            folder_count: raw.folder_count,
            created_at: raw.created_at,
            parent: raw.parent.or(raw.parent_id),
        }
    }
}

/// A file from `GET /api/v1/files`, `GET /api/v1/file/{id}` or
/// `GET /api/v1/folder/{id}/files`. The endpoints return different subsets
/// of these fields, so everything except `id`, `name` and `size` is optional.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FileEntry {
    /// Numeric server identifier.
    #[serde(deserialize_with = "flexible_u64_required")]
    pub id: u64,
    /// UUID identifier.
    #[serde(default)]
    pub uuid: Option<String>,
    /// Short public slug used in share links.
    #[serde(default)]
    pub slug: Option<String>,
    /// File name.
    #[serde(default)]
    pub name: String,
    /// Size in bytes.
    #[serde(default, deserialize_with = "flexible_u64")]
    pub size: u64,
    /// MIME type reported by the server.
    #[serde(default, rename = "type")]
    pub mime_type: Option<String>,
    /// Public share URL.
    #[serde(default)]
    pub url: Option<String>,
    /// Containing folder. `None` means the root, or that the endpoint did not
    /// report it: `GET /api/v1/file/{id}` and `GET /api/v1/folder/{id}/files`
    /// omit this field.
    #[serde(default, deserialize_with = "folder_ref")]
    pub folder_id: Option<FolderId>,
    /// Upload timestamp (RFC 3339 string as sent by the server).
    #[serde(default)]
    pub created_at: Option<String>,
    /// Download count.
    #[serde(default, deserialize_with = "flexible_opt_u64")]
    pub downloads: Option<u64>,
    /// View count.
    #[serde(default, deserialize_with = "flexible_opt_u64")]
    pub views: Option<u64>,
    /// Whether a thumbnail exists.
    #[serde(default, deserialize_with = "flexible_opt_bool")]
    pub has_thumbnail: Option<bool>,
}

impl FileEntry {
    /// The identifier to send to move and delete endpoints: the numeric ID.
    ///
    /// The documentation says these endpoints accept a slug, a UUID or a
    /// numeric ID, but `POST /file/delete` rejects UUIDs ("All file
    /// deletions failed"; verified October 2026). The numeric ID is accepted
    /// by every endpoint and is the one field every listing includes.
    #[must_use]
    pub fn reference(&self) -> String {
        self.id.to_string()
    }

    /// The public share slug: the `slug` field when present, otherwise the
    /// last segment of a `https://filester.me/d/<slug>` share URL. Folder and
    /// root listings omit `slug`, but always include `url`.
    #[must_use]
    pub fn share_slug(&self) -> Option<String> {
        if let Some(slug) = self.slug.as_deref().filter(|s| is_valid_slug(s)) {
            return Some(slug.to_owned());
        }
        let url = Url::parse(self.url.as_deref()?).ok()?;
        let host = url.host_str()?;
        if url.scheme() != "https" || !(host == "filester.me" || host.ends_with(".filester.me")) {
            return None;
        }
        let mut segments = url.path_segments()?;
        match (segments.next(), segments.next(), segments.next()) {
            (Some("d"), Some(slug), None) if is_valid_slug(slug) => Some(slug.to_owned()),
            _ => None,
        }
    }
}

/// Pagination block of `GET /api/v1/files`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct Pagination {
    /// Current page (1-based).
    #[serde(default, deserialize_with = "flexible_u64")]
    pub page: u64,
    /// Items per page.
    #[serde(default, deserialize_with = "flexible_u64")]
    pub per_page: u64,
    /// Total matching items.
    #[serde(default, deserialize_with = "flexible_u64")]
    pub total: u64,
    /// Total pages.
    #[serde(default, deserialize_with = "flexible_u64")]
    pub pages: u64,
}

/// One page of `GET /api/v1/files`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesPage {
    /// Files on this page.
    pub files: Vec<FileEntry>,
    /// Paging metadata.
    pub pagination: Pagination,
}

/// Which files `GET /api/v1/files` should return.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FolderFilter {
    /// Files in every folder.
    #[default]
    Any,
    /// Only files at the top level.
    Root,
    /// Only files in this folder.
    Folder(FolderId),
}

/// Query parameters for `GET /api/v1/files`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileQuery {
    /// Page number, 1-based.
    pub page: u32,
    /// Items per page, `1..=100`.
    pub per_page: u8,
    /// Folder restriction.
    pub folder: FolderFilter,
    /// File-name search term.
    pub search: Option<String>,
}

impl Default for FileQuery {
    fn default() -> Self {
        Self {
            page: 1,
            per_page: 100,
            folder: FolderFilter::Any,
            search: None,
        }
    }
}

/// Account information from `GET /api/v1/account`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Account {
    /// Numeric account ID.
    #[serde(default, deserialize_with = "flexible_u64")]
    pub id: u64,
    /// Username.
    #[serde(default)]
    pub username: String,
    /// Bytes used.
    #[serde(default, deserialize_with = "flexible_u64")]
    pub storage_used: u64,
    /// Storage quota in bytes.
    #[serde(default, deserialize_with = "flexible_u64")]
    pub storage_limit: u64,
    /// Number of files.
    #[serde(default, deserialize_with = "flexible_u64")]
    pub files_count: u64,
    /// Number of folders.
    #[serde(default, deserialize_with = "flexible_u64")]
    pub folders_count: u64,
    /// API requests made today.
    #[serde(default, deserialize_with = "flexible_u64")]
    pub api_requests_today: u64,
}

/// Result of `POST /api/v1/upload`. The fields are top-level, not inside `data`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UploadResult {
    /// Public slug of the new file.
    pub slug: String,
    /// Public share URL.
    #[serde(default)]
    pub url: Option<String>,
    /// Numeric file ID.
    #[serde(default, deserialize_with = "flexible_opt_u64")]
    pub file_id: Option<u64>,
    /// Relative thumbnail path, if one was generated.
    #[serde(default)]
    pub thumbnail_url: Option<String>,
}

/// Result of `GET /api/v1/upload/status`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UploadStatus {
    /// File slug.
    pub slug: String,
    /// Processing state, for example `"completed"`.
    #[serde(default)]
    pub status: String,
    /// Upload timestamp.
    #[serde(default)]
    pub upload_date: Option<String>,
    /// Relative thumbnail path.
    #[serde(default)]
    pub thumbnail_url: Option<String>,
}

/// Visibility of a new folder.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FolderVisibility {
    /// Anyone with the link can view (the API default).
    #[default]
    Public,
    /// Only the owner can view.
    Private,
}

/// Result of `POST /api/v1/folder`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CreatedFolder {
    /// Identifier of the new folder.
    pub identifier: FolderId,
    /// Name as stored by the server.
    #[serde(default)]
    pub name: String,
    /// Parent folder, or `None` for top level.
    #[serde(default, deserialize_with = "folder_ref")]
    pub parent: Option<FolderId>,
    /// Whether a password was set.
    #[serde(default, rename = "hasPassword", deserialize_with = "flexible_bool")]
    pub has_password: bool,
}

/// Result of `POST /api/v1/file/move`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MovedFile {
    /// The file reference that was moved.
    pub file: String,
    /// Destination, or `None` for root.
    #[serde(default, deserialize_with = "folder_ref")]
    pub folder: Option<FolderId>,
}

/// Result of `POST /api/v1/folder/move`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MovedFolder {
    /// The folder that was moved.
    pub folder: FolderId,
    /// New parent, or `None` for root.
    #[serde(default, deserialize_with = "folder_ref")]
    pub parent: Option<FolderId>,
    /// Number of folders whose paths changed (the folder plus its subtree).
    #[serde(default, deserialize_with = "flexible_u64")]
    pub folders_updated: u64,
}

/// Per-item outcome of a bulk move.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BulkItemResult {
    /// The file or folder identifier this result is for.
    #[serde(alias = "file", alias = "folder")]
    pub id: String,
    /// Whether this item was moved.
    #[serde(default, deserialize_with = "flexible_bool")]
    pub success: bool,
    /// Machine-readable failure code, for example `FOLDER_NOT_FOUND`.
    #[serde(default)]
    pub error: Option<String>,
}

/// Result of `POST /api/v1/files/move` or `POST /api/v1/folders/move`.
///
/// A partial failure is **not** an error. The server answers
/// `"success": false` with this payload, and callers inspect `failed` and
/// `results`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BulkMoveReport {
    /// Destination folder, or `None` for root. The wire key is `folder` for
    /// file moves and `parent` for folder moves.
    #[serde(
        default,
        alias = "folder",
        alias = "parent",
        deserialize_with = "folder_ref"
    )]
    pub destination: Option<FolderId>,
    /// Number of items moved.
    #[serde(default, deserialize_with = "flexible_u64")]
    pub moved: u64,
    /// Number of items that failed.
    #[serde(default, deserialize_with = "flexible_u64")]
    pub failed: u64,
    /// Per-item outcomes.
    #[serde(default)]
    pub results: Vec<BulkItemResult>,
}

/// Result of `POST /file/delete` or `POST /folder/delete`. The fields are
/// top-level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct DeleteReport {
    /// Identifiers processed.
    #[serde(default, deserialize_with = "flexible_u64")]
    pub total_processed: u64,
    /// Successful deletions.
    #[serde(default, deserialize_with = "flexible_u64")]
    pub successful_deletes: u64,
    /// Failed deletions.
    #[serde(default, deserialize_with = "flexible_u64")]
    pub failed_deletes: u64,
    /// Files removed, including files inside deleted folders.
    #[serde(default, deserialize_with = "flexible_u64")]
    pub files_deleted: u64,
}

impl std::ops::AddAssign for DeleteReport {
    /// Combine the reports of a delete split across several requests.
    fn add_assign(&mut self, other: Self) {
        self.total_processed = self.total_processed.saturating_add(other.total_processed);
        self.successful_deletes = self
            .successful_deletes
            .saturating_add(other.successful_deletes);
        self.failed_deletes = self.failed_deletes.saturating_add(other.failed_deletes);
        self.files_deleted = self.files_deleted.saturating_add(other.files_deleted);
    }
}

/// Result of `GET /health`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Health {
    /// `"healthy"` when the service is up.
    #[serde(default)]
    pub status: String,
    /// Server time in Unix seconds.
    #[serde(default)]
    pub timestamp: Option<i64>,
    /// Service name.
    #[serde(default)]
    pub service: Option<String>,
}

/// Standard response envelope: `{ "success", "message", "error", "data" }`.
#[derive(Debug, Deserialize)]
pub(crate) struct Envelope<T> {
    #[serde(default)]
    pub success: Option<bool>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default = "Option::default")]
    pub data: Option<T>,
}

/// Body of `POST /api/v1/folder`.
#[derive(Serialize)]
pub(crate) struct CreateFolderBody<'a> {
    pub name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<&'a str>,
    pub public: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<&'a str>,
}

/// Body of `POST /api/v1/file/move`.
#[derive(Serialize)]
pub(crate) struct MoveFileBody<'a> {
    pub file: &'a str,
    pub folder: &'a str,
}

/// Body of `POST /api/v1/files/move`.
#[derive(Serialize)]
pub(crate) struct MoveFilesBody<'a> {
    pub files: &'a [String],
    pub folder: &'a str,
}

/// Body of `POST /api/v1/folder/move`.
#[derive(Serialize)]
pub(crate) struct MoveFolderBody<'a> {
    pub folder: &'a str,
    pub parent: &'a str,
}

/// Body of `POST /api/v1/folders/move`.
#[derive(Serialize)]
pub(crate) struct MoveFoldersBody<'a> {
    pub folders: &'a [String],
    pub parent: &'a str,
}

/// Body of `POST /file/delete` and `POST /folder/delete`.
#[derive(Serialize)]
pub(crate) struct DeleteBody<'a> {
    pub identifiers: &'a [String],
}

/// Wire value for "top level" in move requests.
pub(crate) const ROOT: &str = "root";

// --- lenient deserialisers ---------------------------------------------------

#[derive(Deserialize)]
#[serde(untagged)]
enum NumOrString {
    Num(u64),
    Str(String),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BoolOrNum {
    Bool(bool),
    Num(i64),
    Str(String),
}

fn num_or_string_to_u64<E: serde::de::Error>(v: NumOrString) -> Result<u64, E> {
    match v {
        NumOrString::Num(n) => Ok(n),
        NumOrString::Str(s) => s
            .trim()
            .parse()
            .map_err(|_| E::custom("expected an unsigned integer")),
    }
}

fn bool_or_num_to_bool<E: serde::de::Error>(v: BoolOrNum) -> Result<bool, E> {
    match v {
        BoolOrNum::Bool(b) => Ok(b),
        BoolOrNum::Num(n) => Ok(n != 0),
        BoolOrNum::Str(s) => match s.trim() {
            "1" | "true" => Ok(true),
            "0" | "false" | "" => Ok(false),
            _ => Err(E::custom("expected a boolean")),
        },
    }
}

fn flexible_u64_required<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    num_or_string_to_u64(NumOrString::deserialize(d)?)
}

fn flexible_u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    Option::<NumOrString>::deserialize(d)?.map_or(Ok(0), num_or_string_to_u64)
}

fn flexible_opt_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Option::<NumOrString>::deserialize(d)?
        .map(num_or_string_to_u64)
        .transpose()
}

fn flexible_bool<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Option::<BoolOrNum>::deserialize(d)?.map_or(Ok(false), bool_or_num_to_bool)
}

fn flexible_opt_bool<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    Option::<BoolOrNum>::deserialize(d)?
        .map(bool_or_num_to_bool)
        .transpose()
}

/// `null`, `""` and `"root"` all mean "top level".
fn folder_ref<'de, D: Deserializer<'de>>(d: D) -> Result<Option<FolderId>, D::Error> {
    Ok(Option::<String>::deserialize(d)?
        .filter(|s| {
            let s = s.trim();
            !s.is_empty() && !s.eq_ignore_ascii_case(ROOT)
        })
        .map(FolderId))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_with(slug: Option<&str>, url: Option<&str>) -> FileEntry {
        FileEntry {
            id: 1,
            uuid: None,
            slug: slug.map(str::to_owned),
            name: "a.txt".to_owned(),
            size: 1,
            mime_type: None,
            url: url.map(str::to_owned),
            folder_id: None,
            created_at: None,
            downloads: None,
            views: None,
            has_thumbnail: None,
        }
    }

    #[test]
    fn slug_validation() {
        assert!(is_valid_slug("aBcDeFg"));
        assert!(is_valid_slug("a-b_c9"));
        for bad in ["", ".", "..", "a/b", "a b", "a.b", "é", &"x".repeat(65)] {
            assert!(!is_valid_slug(bad), "{bad:?}");
        }
    }

    #[test]
    fn share_slug_prefers_field_then_parses_url() {
        let f = file_with(Some("Slug123"), Some("https://filester.me/d/Other"));
        assert_eq!(f.share_slug().as_deref(), Some("Slug123"));
        let f = file_with(None, Some("https://filester.me/d/cUxkqEh"));
        assert_eq!(f.share_slug().as_deref(), Some("cUxkqEh"));
        for url in [
            "http://filester.me/d/abc",
            "https://evil.example/d/abc",
            "https://filester.me.evil.example/d/abc",
            "https://filester.me/x/abc",
            "https://filester.me/d/abc/extra",
            "https://filester.me/d/..",
            "https://filester.me/d/",
        ] {
            assert_eq!(file_with(None, Some(url)).share_slug(), None, "{url}");
        }
        assert_eq!(file_with(Some("../x"), None).share_slug(), None);
    }

    #[test]
    fn folder_parses_documented_example_and_merges_parent_alias() {
        let json = r#"[
            {"id":"a1b2c3d4e5f6g7h8","name":"My Photos","public":true,"file_count":24,
             "folder_count":2,"created_at":"2026-03-10T08:00:00Z","parent":null,"parent_id":null},
            {"id":"9f8e7d6c5b4a3210","name":"Episodes","public":false,"file_count":12,
             "folder_count":0,"created_at":"2026-03-11T09:30:00Z",
             "parent":"a1b2c3d4e5f6g7h8","parent_id":"a1b2c3d4e5f6g7h8"},
            {"id":"only-alias","name":"X","parent_id":"a1b2c3d4e5f6g7h8"},
            {"id":"root-literal","name":"Y","parent":"root","public":1}
        ]"#;
        let folders: Vec<Folder> = serde_json::from_str(json).unwrap();
        assert_eq!(folders[0].parent, None);
        assert!(folders[0].public);
        assert_eq!(folders[0].file_count, 24);
        assert_eq!(folders[1].parent, Some(FolderId::new("a1b2c3d4e5f6g7h8")));
        assert_eq!(folders[2].parent, Some(FolderId::new("a1b2c3d4e5f6g7h8")));
        assert_eq!(folders[3].parent, None);
        assert!(folders[3].public);
    }

    #[test]
    fn file_entry_tolerates_missing_fields_and_string_numbers() {
        let json = r#"{"id":"12345","name":"photo1.jpg","size":"2048000","type":"image/jpeg"}"#;
        let f: FileEntry = serde_json::from_str(json).unwrap();
        assert_eq!(f.id, 12345);
        assert_eq!(f.size, 2_048_000);
        assert_eq!(f.mime_type.as_deref(), Some("image/jpeg"));
        assert_eq!(f.folder_id, None);
        assert_eq!(f.reference(), "12345");
    }

    #[test]
    fn file_reference_is_always_the_numeric_id() {
        // Top-level listings carry a UUID, which the delete endpoint refuses.
        let f: FileEntry =
            serde_json::from_str(r#"{"id":1,"uuid":"u-1","slug":"aBcDeFg","name":"a"}"#).unwrap();
        assert_eq!(f.reference(), "1");
    }

    #[test]
    fn invalid_numbers_are_rejected() {
        assert!(serde_json::from_str::<FileEntry>(r#"{"id":"abc","name":"a"}"#).is_err());
        assert!(serde_json::from_str::<FileEntry>(r#"{"id":-1,"name":"a"}"#).is_err());
    }

    #[test]
    fn bulk_report_accepts_both_destination_keys() {
        let files: BulkMoveReport = serde_json::from_str(
            r#"{"folder":"f1","moved":2,"failed":0,
                "results":[{"file":"a","success":true},{"file":"b","success":true}]}"#,
        )
        .unwrap();
        assert_eq!(files.destination, Some(FolderId::new("f1")));
        assert_eq!(files.results[1].id, "b");

        let folders: BulkMoveReport = serde_json::from_str(
            r#"{"parent":"root","moved":1,"failed":1,"results":[
                {"folder":"9f8e","success":true,"folders_updated":2},
                {"folder":"bad0","success":false,"error":"FOLDER_NOT_FOUND"}]}"#,
        )
        .unwrap();
        assert_eq!(folders.destination, None);
        assert_eq!(folders.failed, 1);
        assert_eq!(
            folders.results[1].error.as_deref(),
            Some("FOLDER_NOT_FOUND")
        );
    }

    #[test]
    fn envelope_handles_missing_data() {
        let env: Envelope<Account> =
            serde_json::from_str(r#"{"success":false,"message":"nope","error":"X"}"#).unwrap();
        assert_eq!(env.success, Some(false));
        assert!(env.data.is_none());
        assert_eq!(env.error.as_deref(), Some("X"));
    }

    #[test]
    fn create_folder_body_omits_absent_fields() {
        let body = CreateFolderBody {
            name: "N",
            parent: None,
            public: 0,
            password: None,
        };
        assert_eq!(
            serde_json::to_string(&body).unwrap(),
            r#"{"name":"N","public":0}"#
        );
    }
}
