//! GTK front end.
//!
//! Every type here lives on the GTK main thread. Work that touches the
//! network or the filesystem goes through [`spawn_ui`]: the future runs on
//! the background [`Worker`], and the completion callback runs back on the
//! main thread, where it may touch widgets.
//!
//! Components are plain structs that own their widgets. There is no `GObject`
//! subclassing. Signal handlers hold `Weak` references to their owners, so
//! the widget tree never keeps its controllers alive in a cycle.
//!
//! Untrusted text (file and folder names, server error messages) is only
//! ever placed in plain-text widget properties (`Label::set_text`, tooltips,
//! `AlertDialog` heading and body). The two libadwaita properties that
//! interpret Pango markup by default, `AdwToast:title` and
//! `AdwStatusPage:description`, are handled in [`format`].

mod file_grid;
mod format;
mod sidebar;
mod transfers;
pub mod window;

use std::future::Future;

use filester_app::api::models::FolderId;
use filester_app::worker::{TaskCancelled, Worker};
use gtk::glib;
use tokio::task::AbortHandle;

/// What the content pane is showing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Location {
    /// Top-level files and folders.
    #[default]
    Root,
    /// Inside a folder.
    Folder(FolderId),
}

impl Location {
    /// The folder ID, or `None` at the top level.
    pub fn folder_id(&self) -> Option<&FolderId> {
        match self {
            Self::Root => None,
            Self::Folder(id) => Some(id),
        }
    }
}

/// Run `future` on the worker pool and deliver its result to `on_done` on
/// the GTK main thread.
///
/// Returns a handle that cancels the future. `on_done` then receives
/// `Err(TaskCancelled)`.
pub fn spawn_ui<F, T, C>(worker: &Worker, future: F, on_done: C) -> AbortHandle
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
    C: FnOnce(Result<T, TaskCancelled>) + 'static,
{
    let task = worker.spawn(future);
    let abort = task.abort_handle();
    glib::spawn_future_local(async move { on_done(task.join().await) });
    abort
}
