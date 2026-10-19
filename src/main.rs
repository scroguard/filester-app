//! Filester desktop client: process entry point.
//!
//! Responsibilities are deliberately narrow:
//!
//! 1. Initialise structured logging to stderr. Log output never includes
//!    credentials; see the "Security & Auditing" section of `README.md`.
//! 2. Create the [`adw::Application`] and hand control to the GTK main loop.
//!
//! Networking and other blocking work never run on this thread. The
//! background worker pool arrives with the API engine (Phase 2), and the main
//! window module (Phase 3) replaces [`build_shell`].

mod config;

use adw::prelude::*;
use gtk::glib;
use tracing_subscriber::EnvFilter;

fn main() -> glib::ExitCode {
    init_tracing();

    let app = adw::Application::builder()
        .application_id(config::APP_ID)
        .build();
    app.connect_activate(build_shell);
    app.run()
}

/// Install a stderr `tracing` subscriber.
///
/// Verbosity comes from [`config::LOG_ENV_VAR`]. A missing or malformed value
/// falls back to `info` rather than failing startup.
fn init_tracing() {
    let filter =
        EnvFilter::try_from_env(config::LOG_ENV_VAR).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .init();
}

/// Present the application window on `activate`.
///
/// `GApplication` is single-instance, so a second launch re-activates the
/// running process. In that case the existing window is raised and no
/// second one is created.
fn build_shell(app: &adw::Application) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }

    let status = adw::StatusPage::builder()
        .icon_name(config::APP_ID)
        .title(config::APP_NAME)
        .description("Not connected to a Filester account.")
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&status));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(config::APP_NAME)
        .default_width(1000)
        .default_height(700)
        .content(&toolbar)
        .build();
    window.present();
}
