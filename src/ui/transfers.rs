//! Activity panel: uploads, downloads, deletions and moves, each with a
//! progress bar.
//!
//! At most [`MAX_CONCURRENT`] uploads, and separately at most
//! [`MAX_CONCURRENT`] downloads, run at once, enforced by semaphores on the
//! worker side. Deletions and moves run one at a time, in the order they were
//! started. Queued work holds no file handle and sends no requests.
//! Each row shows a live progress bar fed by a lossy channel (see
//! [`filester_app::worker::progress_channel`]): bytes for transfers, items
//! for deletions and moves. The bar pulses while the first batch of a
//! deletion or move is in flight. The stop button aborts the task, which
//! drops the HTTP request; for deletions and moves, batches already sent
//! stay done.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use filester_app::api::models::FolderId;
use filester_app::api::{
    ApiError, FilesterClient, ProgressCallback, PublicDownloader, TransferProgress,
};
use filester_app::cache::FileCache;
use filester_app::worker::{TaskCancelled, Worker, progress_channel};
use gtk::glib;
use tokio::sync::Semaphore;
use tokio::task::AbortHandle;

use super::{format, spawn_ui};

/// Parallel transfers per direction. More rarely helps throughput, and each
/// upload costs part of the hourly request budget.
const MAX_CONCURRENT: usize = 2;
/// Progress events buffered per upload before intermediate ones are dropped.
const PROGRESS_BUFFER: usize = 16;
/// How often an item-counting row's bar pulses while it waits for its first
/// batch to finish.
const PULSE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(120);

/// Which concurrency limit a kind of activity shares.
#[derive(Clone, Copy)]
enum Queue {
    Uploads,
    Downloads,
    Operations,
}

/// What a row's progress counts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Bytes,
    Items,
}

/// Outcome of a deletion or move.
#[derive(Debug, Clone, Copy)]
pub struct OperationReport {
    /// Items the operation was asked to handle.
    pub total: u64,
    /// Items the server reported as failed.
    pub failed: u64,
}

/// One row in the panel.
struct Transfer {
    row: gtk::ListBoxRow,
    bar: gtk::ProgressBar,
    status: gtk::Label,
    cancel: gtk::Button,
    abort: RefCell<Option<AbortHandle>>,
    finished: Cell<bool>,
    unit: Unit,
    /// Set while the bar pulses; cleared by the first real progress.
    pulsing: Cell<bool>,
}

impl Transfer {
    fn new(name: &str, cancel_tooltip: &str, unit: Unit) -> Self {
        let title = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        title.set_text(name);
        title.set_tooltip_text(Some(name));
        let cancel = gtk::Button::builder()
            .icon_name("process-stop-symbolic")
            .tooltip_text(cancel_tooltip)
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
            unit,
            pulsing: Cell::new(false),
        }
    }

    fn progress(self: &Rc<Self>, p: TransferProgress) {
        if self.finished.get() {
            return;
        }
        match self.unit {
            Unit::Bytes => {
                self.bar.set_fraction(p.fraction());
                self.status.set_text(&format!(
                    "{} of {}",
                    glib::format_size(p.sent),
                    glib::format_size(p.total)
                ));
            }
            // Nothing is done yet: pulse until the first batch returns.
            Unit::Items if p.sent == 0 => {
                self.status
                    .set_text(&format!("Working on {}…", items(p.total)));
                self.start_pulsing();
            }
            Unit::Items => {
                self.pulsing.set(false);
                self.bar.set_fraction(p.fraction());
                self.status
                    .set_text(&format!("{} of {}", p.sent, items(p.total)));
            }
        }
    }

    fn start_pulsing(self: &Rc<Self>) {
        if self.pulsing.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(PULSE_INTERVAL, move || match weak.upgrade() {
            Some(t) if t.pulsing.get() && !t.finished.get() => {
                t.bar.pulse();
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        });
    }

    fn finish<T>(&self, outcome: &Result<Result<T, ApiError>, TaskCancelled>, done: &str) {
        self.finished.set(true);
        self.pulsing.set(false);
        self.cancel.set_visible(false);
        match outcome {
            Ok(Ok(_)) => {
                self.bar.set_fraction(1.0);
                self.status.set_text(done);
            }
            Ok(Err(err)) => {
                let text = format::describe_error(err);
                self.status.set_text(&text);
                self.status.set_tooltip_text(Some(&text));
                self.status.remove_css_class("dim-label");
                self.status.add_css_class("error");
            }
            Err(TaskCancelled) => self.status.set_text(match self.unit {
                Unit::Bytes => "Cancelled",
                Unit::Items => "Stopped; items already handled stay done",
            }),
        }
    }

    fn abort(&self) {
        if let Some(handle) = self.abort.borrow().as_ref() {
            handle.abort();
        }
    }
}

/// Collapsible panel listing uploads and downloads.
pub struct TransfersPanel {
    revealer: gtk::Revealer,
    list: gtk::ListBox,
    transfers: RefCell<Vec<Rc<Transfer>>>,
    uploads: Arc<Semaphore>,
    downloads: Arc<Semaphore>,
    operations: Arc<Semaphore>,
}

/// Labels for one kind of activity.
pub struct Kind {
    cancel_tooltip: &'static str,
    /// Finished status; for deletions and moves, the verb ("Deleted").
    done: &'static str,
    unit: Unit,
    queue: Queue,
}

const UPLOAD: Kind = Kind {
    cancel_tooltip: "Cancel Upload",
    done: "Uploaded",
    unit: Unit::Bytes,
    queue: Queue::Uploads,
};
const DOWNLOAD: Kind = Kind {
    cancel_tooltip: "Cancel Download",
    done: "Downloaded",
    unit: Unit::Bytes,
    queue: Queue::Downloads,
};
/// A deletion of one or more items.
pub const DELETE: Kind = Kind {
    cancel_tooltip: "Stop Deleting",
    done: "Deleted",
    unit: Unit::Items,
    queue: Queue::Operations,
};
/// A move of one or more items.
pub const MOVE: Kind = Kind {
    cancel_tooltip: "Stop Moving",
    done: "Moved",
    unit: Unit::Items,
    queue: Queue::Operations,
};

/// "1 item", "150 items".
fn items(n: u64) -> String {
    if n == 1 {
        "1 item".to_owned()
    } else {
        format!("{n} items")
    }
}

impl TransfersPanel {
    pub fn new() -> Rc<Self> {
        let heading = gtk::Label::builder()
            .label("Activity")
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
            uploads: Arc::new(Semaphore::new(MAX_CONCURRENT)),
            downloads: Arc::new(Semaphore::new(MAX_CONCURRENT)),
            operations: Arc::new(Semaphore::new(1)),
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
    pub fn enqueue_upload(
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
        self.run(
            worker,
            &name,
            &UPLOAD,
            move |progress| async move { client.upload(&path, folder.as_ref(), Some(progress)).await },
            |_| UPLOAD.done.to_owned(),
            move |outcome| on_done(matches!(outcome, Ok(Ok(_)))),
        );
    }

    /// Queue a download of the public file `slug` into `cache`. `on_done`
    /// receives the cached path, the error, or `Err(TaskCancelled)`.
    #[allow(clippy::too_many_arguments)] // all distinct, all required
    pub fn enqueue_download(
        self: &Rc<Self>,
        worker: &Worker,
        downloader: PublicDownloader,
        cache: FileCache,
        slug: String,
        name: String,
        size: u64,
        on_done: impl FnOnce(Result<Result<PathBuf, ApiError>, TaskCancelled>) + 'static,
    ) {
        let label = name.clone();
        self.run(
            worker,
            &label,
            &DOWNLOAD,
            move |progress| async move {
                cache
                    .fetch(&downloader, &slug, &name, size, Some(progress))
                    .await
            },
            |_| DOWNLOAD.done.to_owned(),
            on_done,
        );
    }

    /// Queue a deletion or move titled `title`. `work` reports progress as
    /// items done out of the total, and runs after earlier deletions and
    /// moves have finished.
    pub fn enqueue_operation<W, Fut>(
        self: &Rc<Self>,
        worker: &Worker,
        title: &str,
        kind: &'static Kind,
        work: W,
        on_done: impl FnOnce(Result<Result<OperationReport, ApiError>, TaskCancelled>) + 'static,
    ) where
        W: FnOnce(ProgressCallback) -> Fut + Send + 'static,
        Fut: Future<Output = Result<OperationReport, ApiError>> + Send,
    {
        self.run(
            worker,
            title,
            kind,
            work,
            move |report: &OperationReport| match report.failed {
                0 => format!("{} {}", kind.done, items(report.total)),
                failed => format!(
                    "{} {} of {}; {failed} failed",
                    kind.done,
                    report.total.saturating_sub(failed),
                    items(report.total)
                ),
            },
            on_done,
        );
    }

    /// Add a row for `name` and run `work` on the worker once `queue` has a
    /// free slot.
    fn run<T, W, Fut>(
        self: &Rc<Self>,
        worker: &Worker,
        name: &str,
        kind: &'static Kind,
        work: W,
        done_text: impl FnOnce(&T) -> String + 'static,
        on_done: impl FnOnce(Result<Result<T, ApiError>, TaskCancelled>) + 'static,
    ) where
        T: Send + 'static,
        W: FnOnce(ProgressCallback) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, ApiError>> + Send,
    {
        let queue = Arc::clone(match kind.queue {
            Queue::Uploads => &self.uploads,
            Queue::Downloads => &self.downloads,
            Queue::Operations => &self.operations,
        });
        let transfer = Rc::new(Transfer::new(name, kind.cancel_tooltip, kind.unit));
        self.list.append(&transfer.row);
        self.transfers.borrow_mut().push(Rc::clone(&transfer));
        self.revealer.set_reveal_child(true);

        let (callback, progress_rx) = progress_channel(PROGRESS_BUFFER);
        let done_transfer = Rc::clone(&transfer);
        let abort = spawn_ui(
            worker,
            async move {
                // The semaphore is never closed, so `acquire` cannot fail. If
                // it somehow did, proceeding unthrottled is the safe fallback.
                let _permit = queue.acquire_owned().await.ok();
                work(callback).await
            },
            move |outcome| {
                let done = match &outcome {
                    Ok(Ok(value)) => done_text(value),
                    _ => String::new(),
                };
                done_transfer.finish(&outcome, &done);
                on_done(outcome);
            },
        );
        *transfer.abort.borrow_mut() = Some(abort);

        let weak = Rc::downgrade(&transfer);
        transfer.cancel.connect_clicked(move |_| {
            if let Some(t) = weak.upgrade() {
                t.abort();
            }
        });

        // Ends when the work future drops its callback (finished, failed or
        // aborted), which closes the channel.
        let progress_transfer = Rc::clone(&transfer);
        glib::spawn_future_local(async move {
            while let Ok(p) = progress_rx.recv().await {
                progress_transfer.progress(p);
            }
        });
    }

    /// Number of transfers not yet finished.
    pub fn active_count(&self) -> usize {
        self.transfers
            .borrow()
            .iter()
            .filter(|t| !t.finished.get())
            .count()
    }

    /// Cancel every unfinished transfer.
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
