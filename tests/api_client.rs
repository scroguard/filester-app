//! End-to-end tests of `FilesterClient` against a local mock server.
//!
//! The mock listens on plain-HTTP loopback, the only non-HTTPS origin
//! `ClientConfig` accepts. Retry delays are shrunk to milliseconds so the
//! suite stays fast; one test keeps a real 1 s `Retry-After` to prove the
//! floor is honoured.

// Test code may fail loudly; the production deny-list in Cargo.toml does not apply.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use filester_app::api::models::{FileQuery, FolderFilter, FolderId, FolderVisibility};
use filester_app::api::{
    ApiError, ApiKey, ClientConfig, FilesterClient, MAX_DELETE_ITEMS, MAX_UPLOAD_BYTES,
    ProgressCallback, RetryPolicy, TransferProgress, TreeAnomaly,
};
use secrecy::SecretString;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "test-key-1234567890";

fn config(server: &MockServer) -> ClientConfig {
    let mut cfg = ClientConfig::new(&server.uri()).expect("loopback base URL is valid");
    cfg.retry = RetryPolicy {
        max_attempts: 4,
        base_delay: Duration::from_millis(1),
        max_backoff: Duration::from_millis(5),
        max_wait: Duration::from_secs(5),
    };
    cfg
}

fn client(server: &MockServer) -> FilesterClient {
    client_with(config(server), Some(KEY))
}

fn client_with(cfg: ClientConfig, key: Option<&str>) -> FilesterClient {
    let key = key.map(|k| ApiKey::new(SecretString::from(k)).unwrap());
    FilesterClient::new(cfg, key).unwrap()
}

fn ok(body: &serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body)
}

fn folders_body() -> serde_json::Value {
    json!({"success": true, "data": [
        {"id": "p1", "name": "Photos", "public": true, "file_count": 2, "folder_count": 1,
         "parent": null, "parent_id": null},
        {"id": "c1", "name": "Holiday", "public": false, "file_count": 0, "folder_count": 0,
         "parent": "p1", "parent_id": "p1"},
        {"id": "o1", "name": "Orphan", "parent": "gone"}
    ]})
}

// --- Authentication & basic decoding -----------------------------------------

#[tokio::test]
async fn sends_bearer_token_and_decodes_folders() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/folders"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .and(header("user-agent", filester_app::config::USER_AGENT))
        .respond_with(ok(&folders_body()))
        .expect(1)
        .mount(&server)
        .await;

    let folders = client(&server).list_folders().await.unwrap();
    assert_eq!(folders.len(), 3);
    assert_eq!(folders[1].parent, Some(FolderId::new("p1")));
}

#[tokio::test]
async fn folder_tree_repairs_orphans() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/folders"))
        .respond_with(ok(&folders_body()))
        .mount(&server)
        .await;

    let tree = client(&server).folder_tree().await.unwrap();
    let roots: Vec<_> = tree.roots().map(|f| f.name.as_str()).collect();
    assert_eq!(roots, ["Orphan", "Photos"]);
    assert_eq!(tree.children(&FolderId::new("p1")).count(), 1);
    assert_eq!(
        tree.anomalies(),
        [TreeAnomaly::MissingParent {
            folder: FolderId::new("o1"),
            parent: FolderId::new("gone")
        }]
    );
}

#[tokio::test]
async fn guest_client_refuses_authenticated_endpoints_locally() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/account"))
        .respond_with(ok(&json!({})))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(path("/health"))
        .respond_with(ok(
            &json!({"status": "healthy", "timestamp": 1, "service": "storage-api"}),
        ))
        .expect(1)
        .mount(&server)
        .await;

    let guest = client_with(config(&server), None);
    assert!(matches!(guest.account().await, Err(ApiError::NotSignedIn)));
    assert_eq!(guest.health().await.unwrap().status, "healthy");
    let reqs = server.received_requests().await.unwrap();
    assert!(
        reqs.iter()
            .all(|r| !r.headers.contains_key("authorization"))
    );
}

#[tokio::test]
async fn maps_http_errors_with_sanitised_messages() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "success": false, "message": "Authentication required", "error": "UNAUTHORIZED"})))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/file/nope"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "success": false, "message": "File\r\nnot found\u{0007}"})))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/folder/x/files"))
        .respond_with(ResponseTemplate::new(418).set_body_string("<html>teapot</html>"))
        .mount(&server)
        .await;

    let c = client(&server);
    assert!(matches!(c.account().await, Err(ApiError::Unauthorized)));
    match c.file("nope").await {
        Err(ApiError::NotFound { message }) => assert_eq!(message, "File not found"),
        other => panic!("unexpected: {other:?}"),
    }
    assert!(matches!(
        c.folder_files(&FolderId::new("x")).await,
        Err(ApiError::UnexpectedStatus { status: 418, .. })
    ));
}

#[tokio::test]
async fn success_false_is_rejected_with_code() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/folder"))
        .and(body_json(
            json!({"name": "Dup", "parent": "p1", "public": 0}),
        ))
        .respond_with(ok(&json!({
            "success": false, "message": "A folder with\nthat name exists", "error": "DUPLICATE"})))
        .expect(1)
        .mount(&server)
        .await;

    let err = client(&server)
        .create_folder(
            " Dup ",
            Some(&FolderId::new("p1")),
            FolderVisibility::Private,
            None,
        )
        .await
        .unwrap_err();
    match err {
        ApiError::Rejected { message, code } => {
            assert_eq!(message, "A folder with that name exists");
            assert_eq!(code.as_deref(), Some("DUPLICATE"));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn invalid_arguments_never_reach_the_server() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ok(&json!({})))
        .expect(0)
        .mount(&server)
        .await;
    let c = client(&server);

    let long = "x".repeat(101);
    assert!(matches!(
        c.create_folder(&long, None, FolderVisibility::Public, None)
            .await,
        Err(ApiError::InvalidArgument(_))
    ));
    let too_many: Vec<String> = (0..201).map(|i| i.to_string()).collect();
    assert!(matches!(
        c.move_files(&too_many, None).await,
        Err(ApiError::InvalidArgument(_))
    ));
    assert!(matches!(
        c.delete_files(&[]).await,
        Err(ApiError::InvalidArgument(_))
    ));
    let bad_page = FileQuery {
        per_page: 0,
        ..FileQuery::default()
    };
    assert!(matches!(
        c.list_files(&bad_page).await,
        Err(ApiError::InvalidArgument(_))
    ));
}

// --- Endpoint shapes ---------------------------------------------------------------

#[tokio::test]
async fn list_files_sends_query_and_reads_pagination() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/files"))
        .and(query_param("page", "2"))
        .and(query_param("per_page", "50"))
        .and(query_param("folder", "root"))
        .and(query_param("search", "cat pics"))
        .respond_with(ok(&json!({
            "success": true,
            "data": [{"id": 1, "name": "cat.jpg", "size": 10, "type": "image/jpeg",
                      "folder_id": null, "created_at": "2026-03-15T10:30:00Z", "downloads": 3}],
            "pagination": {"page": 2, "per_page": 50, "total": 51, "pages": 2}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let page = client(&server)
        .list_files(&FileQuery {
            page: 2,
            per_page: 50,
            folder: FolderFilter::Root,
            search: Some("  cat pics ".to_owned()),
        })
        .await
        .unwrap();
    assert_eq!(page.files[0].name, "cat.jpg");
    assert_eq!(page.pagination.total, 51);
}

#[tokio::test]
async fn path_segments_are_percent_encoded() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/folder/a%2F..%2Fb/files"))
        .respond_with(ok(&json!({"success": true, "data": []})))
        .expect(1)
        .mount(&server)
        .await;
    let files = client(&server)
        .folder_files(&FolderId::new("a/../b"))
        .await
        .unwrap();
    assert_eq!(files, []);
}

#[tokio::test]
async fn bulk_move_partial_failure_is_a_report_not_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/folders/move"))
        .and(body_json(
            json!({"folders": ["9f8e", "bad0"], "parent": "root"}),
        ))
        .respond_with(ok(&json!({
            "success": false, "message": "Moved 1 of 2 folders",
            "data": {"parent": "root", "moved": 1, "failed": 1, "results": [
                {"folder": "9f8e", "success": true, "folders_updated": 2},
                {"folder": "bad0", "success": false, "error": "FOLDER_NOT_FOUND"}]}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let report = client(&server)
        .move_folders(&[FolderId::new("9f8e"), FolderId::new("bad0")], None)
        .await
        .unwrap();
    assert_eq!((report.moved, report.failed), (1, 1));
    assert_eq!(report.destination, None);
    assert_eq!(report.results[1].error.as_deref(), Some("FOLDER_NOT_FOUND"));
}

#[tokio::test]
async fn delete_uses_legacy_paths_and_top_level_report() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/file/delete"))
        .and(body_json(json!({"identifiers": ["aBcDeFg", "xYzAbCd"]})))
        .respond_with(ok(&json!({
            "success": true, "message": "Files deleted successfully", "total_processed": 2,
            "successful_deletes": 2, "failed_deletes": 0, "files_deleted": 2})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/folder/delete"))
        .respond_with(ok(
            &json!({"success": false, "message": "Folder is locked"}),
        ))
        .expect(1)
        .mount(&server)
        .await;

    let c = client(&server);
    let report = c
        .delete_files(&["aBcDeFg".into(), "xYzAbCd".into()])
        .await
        .unwrap();
    assert_eq!(report.successful_deletes, 2);
    assert!(matches!(
        c.delete_folders(&[FolderId::new("f")]).await,
        Err(ApiError::Rejected { .. })
    ));
}

/// The API accepts at most 100 identifiers per delete; longer lists are
/// split into consecutive requests and the reports summed.
#[tokio::test]
async fn delete_splits_into_requests_of_at_most_100() {
    let server = MockServer::start().await;
    let ids: Vec<String> = (0..250).map(|i| format!("f{i:03}")).collect();
    for chunk in ids.chunks(MAX_DELETE_ITEMS) {
        Mock::given(method("POST"))
            .and(path("/file/delete"))
            .and(body_json(json!({ "identifiers": chunk })))
            .respond_with(ok(&json!({
                "success": true, "total_processed": chunk.len(),
                "successful_deletes": chunk.len() - 1, "failed_deletes": 1,
                "files_deleted": chunk.len() - 1})))
            .expect(1)
            .mount(&server)
            .await;
    }
    let report = client(&server).delete_files(&ids).await.unwrap();
    assert_eq!(MAX_DELETE_ITEMS, 100);
    assert_eq!(report.total_processed, 250);
    assert_eq!(report.successful_deletes, 247);
    assert_eq!(report.failed_deletes, 3);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
}

/// A failed batch stops the delete; batches already sent stay deleted.
#[tokio::test]
async fn delete_stops_at_the_first_failed_batch() {
    let server = MockServer::start().await;
    let ids: Vec<String> = (0..150).map(|i| format!("d{i:03}")).collect();
    Mock::given(method("POST"))
        .and(path("/folder/delete"))
        .and(body_json(json!({ "identifiers": &ids[..100] })))
        .respond_with(ok(&json!({"success": true, "total_processed": 100,
            "successful_deletes": 100, "failed_deletes": 0})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/folder/delete"))
        .and(body_json(json!({ "identifiers": &ids[100..] })))
        .respond_with(ok(
            &json!({"success": false, "message": "Folder is locked"}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let folders: Vec<FolderId> = ids.iter().map(FolderId::new).collect();
    assert!(matches!(
        client(&server).delete_folders(&folders).await,
        Err(ApiError::Rejected { .. })
    ));
}

#[tokio::test]
async fn move_file_to_root_sends_root_literal() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/file/move"))
        .and(body_json(json!({"file": "aBcDeFg", "folder": "root"})))
        .respond_with(ok(
            &json!({"success": true, "data": {"file": "aBcDeFg", "folder": "root"}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let moved = client(&server).move_file("aBcDeFg", None).await.unwrap();
    assert_eq!(moved.folder, None);
}

// --- Rate limiting & retries -----------------------------------------------------

#[tokio::test]
async fn retries_429_then_succeeds_and_records_budget() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(2)
        .with_priority(1)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/account"))
        .respond_with(
            ok(&json!({"success": true, "data": {"id": 42, "username": "johndoe"}}))
                .insert_header("x-ratelimit-limit", "1000")
                .insert_header("x-ratelimit-remaining", "997")
                .insert_header("x-ratelimit-reset", "3600"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let c = client(&server);
    assert_eq!(c.account().await.unwrap().username, "johndoe");
    let rl = c.rate_limit().unwrap();
    assert_eq!((rl.limit, rl.remaining), (Some(1000), Some(997)));
}

#[tokio::test]
async fn retry_after_is_a_lower_bound() {
    let server = MockServer::start().await;
    Mock::given(path("/health"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(path("/health"))
        .respond_with(ok(&json!({"status": "healthy"})))
        .mount(&server)
        .await;

    let started = Instant::now();
    client(&server).health().await.unwrap();
    assert!(started.elapsed() >= Duration::from_secs(1));
}

#[tokio::test]
async fn long_retry_after_fails_fast_and_blocks_further_requests() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "3600"))
        .expect(1)
        .mount(&server)
        .await;

    let c = client(&server);
    match c.account().await {
        Err(ApiError::RateLimited { retry_after }) => {
            assert_eq!(retry_after, Some(Duration::from_secs(3600)));
        }
        other => panic!("unexpected: {other:?}"),
    }
    // The limiter now knows the budget is gone; no request is spent.
    assert!(matches!(
        c.account().await,
        Err(ApiError::RateLimited { .. })
    ));
}

#[tokio::test]
async fn exhausted_budget_header_pauses_proactively() {
    let server = MockServer::start().await;
    let reset = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    Mock::given(path("/api/v1/folders"))
        .respond_with(
            ok(&json!({"success": true, "data": []}))
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset", reset.to_string().as_str()),
        )
        .expect(1)
        .mount(&server)
        .await;

    let c = client(&server);
    c.list_folders().await.unwrap();
    assert!(matches!(
        c.list_folders().await,
        Err(ApiError::RateLimited {
            retry_after: Some(_)
        })
    ));
}

#[tokio::test]
async fn persistent_429_gives_up_after_max_attempts() {
    let server = MockServer::start().await;
    Mock::given(path("/health"))
        .respond_with(ResponseTemplate::new(429))
        .expect(4)
        .mount(&server)
        .await;
    assert!(matches!(
        client(&server).health().await,
        Err(ApiError::RateLimited { retry_after: None })
    ));
}

#[tokio::test]
async fn get_retries_503_but_post_does_not() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .respond_with(ok(&json!({"success": true, "data": {"username": "u"}})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/file/move"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;

    let c = client(&server);
    assert_eq!(c.account().await.unwrap().username, "u");
    assert!(matches!(
        c.move_file("abc", None).await,
        Err(ApiError::Server { status: 503, .. })
    ));
}

#[tokio::test]
async fn request_timeout_maps_to_timeout() {
    let server = MockServer::start().await;
    Mock::given(path("/health"))
        .respond_with(ok(&json!({"status": "healthy"})).set_delay(Duration::from_secs(5)))
        .mount(&server)
        .await;
    let mut cfg = config(&server);
    cfg.request_timeout = Duration::from_millis(100);
    cfg.retry.max_attempts = 1;
    assert!(matches!(
        client_with(cfg, None).health().await,
        Err(ApiError::Timeout)
    ));
}

// --- Transport hardening ---------------------------------------------------------

#[tokio::test]
async fn cross_origin_redirect_is_refused() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/account"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "https://attacker.invalid/x"),
        )
        .mount(&server)
        .await;
    assert!(matches!(
        client(&server).account().await,
        Err(ApiError::CrossOriginRedirect)
    ));
}

#[tokio::test]
async fn same_origin_redirect_is_followed_with_auth() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(307).insert_header("location", "/api/v1/account2"))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/account2"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .respond_with(ok(&json!({"success": true, "data": {"username": "moved"}})))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(client(&server).account().await.unwrap().username, "moved");
}

#[tokio::test]
async fn oversized_response_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/folders"))
        .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(10_000)))
        .mount(&server)
        .await;
    let mut cfg = config(&server);
    cfg.max_response_bytes = 1_000;
    assert!(matches!(
        client_with(cfg, Some(KEY)).list_folders().await,
        Err(ApiError::ResponseTooLarge { limit: 1_000 })
    ));
}

#[tokio::test]
async fn malformed_json_is_a_decode_error() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{not json"))
        .mount(&server)
        .await;
    assert!(matches!(
        client(&server).account().await,
        Err(ApiError::Decode(_))
    ));
}

// --- Uploads -------------------------------------------------------------------------

/// A temp file with a recognisable byte pattern.
fn temp_file(name: &str, len: usize) -> (tempfile::TempDir, std::path::PathBuf, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    #[allow(clippy::cast_possible_truncation)]
    let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
    std::fs::File::create(&path)
        .unwrap()
        .write_all(&data)
        .unwrap();
    (dir, path, data)
}

fn recorder() -> (ProgressCallback, Arc<Mutex<Vec<TransferProgress>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    (Arc::new(move |p| sink.lock().unwrap().push(p)), log)
}

fn upload_ok() -> ResponseTemplate {
    ok(
        &json!({"success": true, "message": "File uploaded successfully", "slug": "aBcDeFg",
               "url": "https://filester.me/d/aBcDeFg", "file_id": 12345,
               "thumbnail_url": "/t/UUID"}),
    )
}

#[tokio::test]
async fn upload_streams_multipart_with_folder_header_and_progress() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/upload"))
        .and(header("x-folder-id", "a1b2c3d4e5f6g7h8"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .respond_with(upload_ok())
        .expect(1)
        .mount(&server)
        .await;

    let len = 3 * 1024 * 1024 + 123;
    let (_dir, path, data) = temp_file("holiday video.mp4", len);
    let (cb, log) = recorder();
    let result = client(&server)
        .upload(&path, Some(&FolderId::new("a1b2c3d4e5f6g7h8")), Some(cb))
        .await
        .unwrap();
    assert_eq!(result.slug, "aBcDeFg");
    assert_eq!(result.file_id, Some(12345));

    let progress = log.lock().unwrap().clone();
    assert_eq!(progress.first().map(|p| p.sent), Some(0));
    assert_eq!(progress.last().map(|p| p.sent), Some(len as u64));
    assert!(progress.windows(2).all(|w| w[0].sent <= w[1].sent));
    assert!(progress.iter().all(|p| p.total == len as u64));

    let req = &server.received_requests().await.unwrap()[0];
    let content_type = req.headers.get("content-type").unwrap().to_str().unwrap();
    assert!(content_type.starts_with("multipart/form-data; boundary="));
    let body = &req.body;
    let header_end = body.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let part_headers = String::from_utf8_lossy(&body[..header_end]);
    assert!(part_headers.contains("name=\"file\""));
    assert!(part_headers.contains("filename=\"holiday video.mp4\""));
    assert!(part_headers.contains("Content-Type: video/mp4"));
    assert_eq!(&body[header_end..header_end + len], data.as_slice());
}

#[tokio::test]
async fn guest_upload_sends_no_authorization() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/upload"))
        .respond_with(upload_ok())
        .expect(1)
        .mount(&server)
        .await;
    let (_dir, path, _) = temp_file("a.txt", 10);
    client_with(config(&server), None)
        .upload(&path, None, None)
        .await
        .unwrap();
    let req = &server.received_requests().await.unwrap()[0];
    assert!(!req.headers.contains_key("authorization"));
    assert!(!req.headers.contains_key("x-folder-id"));
}

#[tokio::test]
async fn upload_retries_429_and_restarts_progress() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/upload"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/upload"))
        .respond_with(upload_ok())
        .mount(&server)
        .await;

    let len = 2 * 1024 * 1024;
    let (_dir, path, _) = temp_file("b.bin", len);
    let (cb, log) = recorder();
    client(&server).upload(&path, None, Some(cb)).await.unwrap();

    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    let restarts = log.lock().unwrap().iter().filter(|p| p.sent == 0).count();
    assert_eq!(restarts, 2);
}

#[tokio::test]
async fn upload_is_not_retried_on_server_error() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/upload"))
        .respond_with(ResponseTemplate::new(502))
        .expect(1)
        .mount(&server)
        .await;
    let (_dir, path, _) = temp_file("c.bin", 100);
    assert!(matches!(
        client(&server).upload(&path, None, None).await,
        Err(ApiError::Server { status: 502, .. })
    ));
}

#[tokio::test]
async fn upload_rejects_oversize_sparse_file_without_request() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(upload_ok())
        .expect(0)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("huge.iso");
    // Sparse: allocates no disk blocks.
    std::fs::File::create(&path)
        .unwrap()
        .set_len(MAX_UPLOAD_BYTES + 1)
        .unwrap();
    assert!(matches!(
        client(&server).upload(&path, None, None).await,
        Err(ApiError::FileTooLarge { .. })
    ));
}

#[tokio::test]
async fn upload_success_false_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/upload"))
        .respond_with(ok(
            &json!({"success": false, "message": "Storage quota exceeded"}),
        ))
        .mount(&server)
        .await;
    let (_dir, path, _) = temp_file("d.bin", 10);
    match client(&server).upload(&path, None, None).await {
        Err(ApiError::Rejected { message, .. }) => assert_eq!(message, "Storage quota exceeded"),
        other => panic!("unexpected: {other:?}"),
    }
}
