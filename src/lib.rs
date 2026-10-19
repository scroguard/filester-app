//! Filester desktop client library.
//!
//! The crate is split so that everything except the GTK front end can be
//! built and tested headlessly:
//!
//! - [`api`]: async REST engine (HTTP, rate limiting, uploads, folder tree).
//!   It has no GTK dependency.
//! - [`secrets`]: API key persistence in the desktop Secret Service.
//! - [`worker`]: the background tokio runtime and the bridge that hands
//!   results back to the GTK main loop.
//! - [`config`]: compile-time identity and endpoints.
//!
//! The binary (`src/main.rs`) owns all GTK code.

pub mod api;
pub mod config;
pub mod secrets;
pub mod worker;
