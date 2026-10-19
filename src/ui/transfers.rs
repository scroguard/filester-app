//! Upload queue with per-file progress bars.
//!
//! At most [`MAX_CONCURRENT_UPLOADS`] uploads run at once, enforced by a
//! semaphore on the worker side. Queued uploads hold no file handle and use
//! no bandwidth. Each row shows a live progress bar fed by a lossy channel
//! (see [`filester_app::worker::progress_channel`]), and its cancel button
//! aborts the task, which drops the HTTP request.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use filester_app::api::models::{FolderId, UploadResult};
use filester_app::api::{ApiError, FilesterClient, TransferProgress};
use filester_app::worker::{TaskCancelled, Worker, progress_channel};
use gtk::glib;
use tokio::sync::Semaphore;
use tokio::task::AbortHandle;

use super::{format, spawn_ui};

/// Parallel uploads. More rarely helps throughput, and each upload costs
/// part of the hourly request budget.
const MAX_CONCURRENT_UPLOADS: usize = 2;
/// Progress events buffered per upload before intermediate ones are dropped.
const PROGRESS_BUFFER: usize = 16;

/// One row in the panel.
struct Transfer {
    row: gtk::ListBoxRow,
    bar: gtk::ProgressBar,
    status: gtk::Label,
    cancel: gtk::Button,
    abort: RefCell<Option<AbortHandle>>,
    finished: Cell<bool>,
}

impl Transfer {
    fn new(name: &str) -> Self {
        let title = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        title.set_text(name);
        title.set_tooltip_text(Some(name));
        let cancel = gtk::Button::builder()
            .icon_name("process-stop-symbolic")
            .tooltip_text("Cancel Upload")
            .valign(gtk::Align::Center)
            .build();
        cancel.add_css_class("flat");
        cancel.add_css_class("circular");
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        top.append(&title);
        top.append(&cancel);

        let bar = gtk::ProgressBar::new();
        bar.update_property(&[gtk::accessible::Property::Label(name)]);
        let status = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .label("Waiting…")
            .build();
        status.add_css_class("caption");
        status.add_css_class("dim-label");

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&top);
        content.append(&bar);
        content.append(&status);
        let row = gtk::ListBoxRow::builder()
            .child(&content)
            .activatable(false)
            .build();

        Self {
            row,
            bar,
            status,
            cancel,
            abort: RefCell::new(None),
            finished: Cell::new(false),
        }
    }

    fn progress(&self, p: TransferProgress) {
        if self.finished.get() {
            return;
        }
        self.bar.set_fraction(p.fraction());
        self.status.set_text(&format!(
            "{} of {}",
            glib::format_size(p.sent),
            glib::format_size(p.total)
        ));
    }

    fn finish(&self, outcome: &Result<Result<UploadResult, ApiError>, TaskCancelled>) {
        self.finished.set(true);
        self.cancel.set_visible(false);
        match outcome {
            Ok(Ok(_)) => {
                self.bar.set_fraction(1.0);
                self.status.set_text("Uploaded");
            }
            Ok(Err(err)) => {
                let text = format::describe_error(err);
                self.status.set_text(&text);
                self.status.set_tooltip_text(Some(&text));
                self.status.remove_css_class("dim-label");
                self.status.add_css_class("error");
            }
            Err(TaskCancelled) => self.status.set_text("Cancelled"),
        }
    }

    fn abort(&self) {
        if let Some(handle) = self.abort.borrow().as_ref() {
            handle.abort();
        }
    }
}

/// Collapsible panel listing uploads.
pub struct TransfersPanel {
    revealer: gtk::Revealer,
    list: gtk::ListBox,
    transfers: RefCell<Vec<Rc<Transfer>>>,
    queue: Arc<Semaphore>,
}

impl TransfersPanel {
    pub fn new() -> Rc<Self> {
        let heading = gtk::Label::builder()
            .label("Uploads")
            .xalign(0.0)
            .hexpand(true)
            .build();
        heading.add_css_class("heading");
        let clear = gtk::Button::with_label("Clear Finished");
        clear.add_css_class("flat");
        let header = gtk::Box::builder()
            .spacing(6)
            .margin_start(12)
            .margin_end(6)
            .margin_top(6)
            .build();
        header.append(&heading);
        header.append(&clear);

        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        list.add_css_class("background");
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .max_content_height(220)
            .propagate_natural_height(true)
            .child(&list)
            .build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        content.append(&header);
        content.append(&scrolled);
        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .child(&content)
            .build();

        let panel = Rc::new(Self {
            revealer,
            list,
            transfers: RefCell::new(Vec::new()),
            queue: Arc::new(Semaphore::new(MAX_CONCURRENT_UPLOADS)),
        });
        let weak = Rc::downgrade(&panel);
        clear.connect_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.clear_finished();
            }
        });
        panel
    }

    /// The widget to place at the bottom of the content pane.
    pub fn widget(&self) -> &gtk::Revealer {
        &self.revealer
    }

    /// Queue `path` for upload into `folder`. `on_done(true)` runs on the
    /// main thread after a successful upload, `on_done(false)` otherwise.
    pub fn enqueue(
        self: &Rc<Self>,
        worker: &Worker,
        client: FilesterClient,
        path: PathBuf,
        folder: Option<FolderId>,
        on_done: impl FnOnce(bool) + 'static,
    ) {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let transfer = Rc::new(Transfer::new(&name));
        self.list.append(&transfer.row);
        self.transfers.borrow_mut().push(Rc::clone(&transfer));
        self.revealer.set_reveal_child(true);

        let (callback, progress_rx) = progress_channel(PROGRESS_BUFFER);
        let queue = Arc::clone(&self.queue);
        let done_transfer = Rc::clone(&transfer);
        let abort = spawn_ui(
            worker,
            async move {
                // The semaphore is never closed, so `acquire` cannot fail. If
                // it somehow did, proceeding unthrottled is the safe fallback.
                let _permit = queue.acquire_owned().await.ok();
                client.upload(&path, folder.as_ref(), Some(callback)).await
            },
            move |outcome| {
                let ok = matches!(outcome, Ok(Ok(_)));
                done_transfer.finish(&outcome);
                on_done(ok);
            },
        );
        *transfer.abort.borrow_mut() = Some(abort);

        let weak = Rc::downgrade(&transfer);
        transfer.cancel.connect_clicked(move |_| {
            if let Some(t) = weak.upgrade() {
                t.abort();
            }
        });

        // Ends when the upload future drops its callback (finished, failed or
        // aborted), which closes the channel.
        let progress_transfer = Rc::clone(&transfer);
        glib::spawn_future_local(async move {
            while let Ok(p) = progress_rx.recv().await {
                progress_transfer.progress(p);
            }
        });
    }

    /// Number of uploads not yet finished.
    pub fn active_count(&self) -> usize {
        self.transfers
            .borrow()
            .iter()
            .filter(|t| !t.finished.get())
            .count()
    }

    /// Cancel every unfinished upload.
    pub fn abort_all(&self) {
        for transfer in self.transfers.borrow().iter() {
            if !transfer.finished.get() {
                transfer.abort();
            }
        }
    }

    /// Remove finished rows. Hides the panel if nothing remains.
    pub fn clear_finished(&self) {
        let mut transfers = self.transfers.borrow_mut();
        transfers.retain(|t| {
            if t.finished.get() {
                self.list.remove(&t.row);
                false
            } else {
                true
            }
        });
        if transfers.is_empty() {
            self.revealer.set_reveal_child(false);
        }
    }
}
