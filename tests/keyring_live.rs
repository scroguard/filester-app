//! Manual smoke test against the real desktop Secret Service.
//!
//! Ignored by default because CI has no keyring daemon. It is read-only: it
//! looks up this application's item and never writes or deletes anything.
//!
//! ```sh
//! cargo test --test keyring_live -- --ignored --nocapture
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::time::{Duration, Instant};

use filester_app::secrets::TokenStore;

const STEP_TIMEOUT: Duration = Duration::from_secs(15);

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a desktop Secret Service; run manually"]
async fn secret_service_round_trip_is_responsive() {
    let started = Instant::now();
    let store = tokio::time::timeout(STEP_TIMEOUT, TokenStore::open())
        .await
        .expect("TokenStore::open timed out")
        .expect("TokenStore::open failed");
    eprintln!("open: {:?}", started.elapsed());

    let started = Instant::now();
    let loaded = tokio::time::timeout(STEP_TIMEOUT, store.load())
        .await
        .expect("TokenStore::load timed out")
        .expect("TokenStore::load failed");
    eprintln!(
        "load: {:?} (key present: {})",
        started.elapsed(),
        loaded.is_some()
    );
}
