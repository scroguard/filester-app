//! Integration tests for public-file downloads and the download cache,
//! against a local mock server.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use filester_app::api::{ApiError, DownloadConfig, PublicDownloader, TransferProgress};
use filester_app::cache::FileCache;
use serde_json::json;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SLUG: &str = "aBcDeFg";
const TOKEN_PATH: &str = "/v2/api/public/download";

fn downloader(server: &MockServer) -> PublicDownloader {
    let cfg =
        DownloadConfig::new(&format!("{}{TOKEN_PATH}", server.uri()), &["127.0.0.1"]).unwrap();
    PublicDownloader::new(cfg).unwrap()
}

async fn mount_token(server: &MockServer, server_field: &str) {
    Mock::given(method("POST"))
        .and(path(TOKEN_PATH))
        .and(body_json(json!({ "file_slug": SLUG })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "expires_in": 1800,
            "file": "uuid-1234.txt",
            "name": "report.txt",
            "server": server_field,
            "token": "t0k/en+=",
        })))
        .mount(server)
        .await;
}

async fn mount_file(server: &MockServer, body: &[u8]) {
    Mock::given(method("GET"))
        .and(path("/v2/uuid-1234.txt"))
        .and(query_param("token", "t0k/en+="))
        .and(query_param("download", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
        .mount(server)
        .await;
}

#[tokio::test]
async fn downloads_into_cache_without_ever_sending_credentials() {
    let server = MockServer::start().await;
    mount_token(&server, &server.uri()).await;
    let body = vec![7u8; 3 * 1024 * 1024 + 11];
    mount_file(&server, &body).await;

    let tmp = tempfile::tempdir().unwrap();
    let cache = FileCache::new(tmp.path().join("files"));
    let events: Arc<Mutex<Vec<TransferProgress>>> = Arc::default();
    let sink = Arc::clone(&events);
    let path = cache
        .fetch(
            &downloader(&server),
            SLUG,
            "report.txt",
            body.len() as u64,
            Some(Arc::new(move |p| sink.lock().unwrap().push(p))),
        )
        .await
        .unwrap();

    assert_eq!(path, cache.root().join(SLUG).join("report.txt"));
    assert_eq!(std::fs::read(&path).unwrap(), body);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "cached files are owner-only");
    assert_eq!(
        cache.lookup(SLUG, "report.txt", body.len() as u64),
        Some(path)
    );

    {
        let events = events.lock().unwrap();
        assert_eq!(events.first().unwrap().sent, 0);
        assert_eq!(events.last().unwrap().sent, body.len() as u64);
    }

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert!(
            !request.headers.contains_key("authorization"),
            "download requests must never carry an Authorization header"
        );
        assert!(!request.headers.contains_key("cookie"));
    }
}

#[tokio::test]
async fn private_file_is_reported_as_not_public() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(TOKEN_PATH))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "success": false, "error": "E2007", "message": "Access denied"
        })))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("out");
    let err = downloader(&server)
        .download(SLUG, 4, &dest, None)
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::NotPublic), "{err:?}");
    assert!(!dest.exists());
}

#[tokio::test]
async fn untrusted_server_is_never_contacted() {
    let server = MockServer::start().await;
    mount_token(&server, "https://evil.example").await;
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("out");
    let err = downloader(&server)
        .download(SLUG, 4, &dest, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ApiError::UntrustedDownloadServer(_)),
        "{err:?}"
    );
    assert!(!dest.exists());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn size_mismatch_fails_and_removes_partial_file() {
    let server = MockServer::start().await;
    mount_token(&server, &server.uri()).await;
    mount_file(&server, b"short").await;
    let tmp = tempfile::tempdir().unwrap();
    let cache = FileCache::new(tmp.path().to_path_buf());

    for expected in [4u64, 9] {
        let err = cache
            .fetch(&downloader(&server), SLUG, "x.bin", expected, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ApiError::SizeMismatch { expected: e, received: 5 } if e == expected),
            "{err:?}"
        );
    }
    assert_eq!(cache.usage().unwrap(), 0, "no partial files left behind");
}

#[tokio::test]
async fn oversized_and_malformed_requests_are_refused_locally() {
    let server = MockServer::start().await;
    let mut cfg =
        DownloadConfig::new(&format!("{}{TOKEN_PATH}", server.uri()), &["127.0.0.1"]).unwrap();
    cfg.max_bytes = 10;
    let dl = PublicDownloader::new(cfg).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("out");

    let err = dl.download(SLUG, 11, &dest, None).await.unwrap_err();
    assert!(
        matches!(
            err,
            ApiError::DownloadTooLarge {
                size: 11,
                limit: 10
            }
        ),
        "{err:?}"
    );
    let err = dl.download("../etc", 1, &dest, None).await.unwrap_err();
    assert!(matches!(err, ApiError::InvalidArgument(_)), "{err:?}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn existing_destination_is_never_overwritten() {
    let server = MockServer::start().await;
    mount_token(&server, &server.uri()).await;
    mount_file(&server, b"data").await;
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("existing");
    std::fs::write(&dest, b"keep").unwrap();
    let err = downloader(&server)
        .download(SLUG, 4, &dest, None)
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Io(_)), "{err:?}");
    assert_eq!(std::fs::read(&dest).unwrap(), b"keep");
}
