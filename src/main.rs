//! Filester desktop client: process entry point.
//!
//! Responsibilities:
//!
//! 1. Initialise structured logging to stderr. Log output never includes
//!    credentials; see the "Security & Auditing" section of `README.md`.
//! 2. Start the background [`Worker`] runtime, which runs all networking and
//!    file I/O.
//! 3. Create the [`adw::Application`], register keyboard shortcuts, and
//!    build the single [`ui::window::MainWindow`] on first activation.

mod ui;

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use filester_app::config;
use filester_app::worker::Worker;
use gtk::{gio, glib};
use tracing::error;
use tracing_subscriber::EnvFilter;

use ui::window::MainWindow;

fn main() -> glib::ExitCode {
    init_tracing();

    let worker = match Worker::new() {
        Ok(worker) => Rc::new(worker),
        Err(err) => {
            error!(error = %err, "could not start background worker");
            return glib::ExitCode::FAILURE;
        }
    };

    let app = adw::Application::builder()
        .application_id(config::APP_ID)
        .build();
    register_app_actions(&app);

    // `GApplication` is single-instance: a second launch re-activates this
    // process, which then raises the existing window.
    let window: Rc<RefCell<Option<Rc<MainWindow>>>> = Rc::default();
    app.connect_activate(move |app| {
        if let Some(existing) = window.borrow().as_ref() {
            existing.present();
            return;
        }
        let main_window = MainWindow::new(app, Rc::clone(&worker));
        main_window.start();
        *window.borrow_mut() = Some(main_window);
    });
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

/// Application-wide actions and keyboard shortcuts.
///
/// Item-level shortcuts (Delete, Ctrl+C) are deliberately absent here: they
/// are scoped to the file grid in `ui::window`, so they cannot fire while a
/// text field has focus.
fn register_app_actions(app: &adw::Application) {
    let quit = gio::SimpleAction::new("quit", None);
    let app_weak = app.downgrade();
    quit.connect_activate(move |_, _| {
        if let Some(app) = app_weak.upgrade() {
            // Route through the window so the active-upload prompt runs.
            match app.active_window() {
                Some(window) => window.close(),
                None => app.quit(),
            }
        }
    });
    app.add_action(&quit);

    app.set_accels_for_action("app.quit", &["<Control>q"]);
    app.set_accels_for_action("window.close", &["<Control>w"]);
    app.set_accels_for_action("win.upload", &["<Control>u"]);
    app.set_accels_for_action("win.new-folder", &["<Control><Shift>n"]);
    app.set_accels_for_action("win.refresh", &["F5", "<Control>r"]);
    app.set_accels_for_action("win.go-up", &["<Alt>Up"]);
}
