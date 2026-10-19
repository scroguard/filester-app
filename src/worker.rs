//! Background runtime and the bridge back to the GTK main loop.
//!
//! The GTK main thread must never block. All networking, file I/O and tree
//! building run on a dedicated multi-threaded tokio runtime owned by
//! [`Worker`]. The UI interacts with it in two ways:
//!
//! - [`Worker::spawn`] runs a future on the pool and returns a [`Task`].
//!   Awaiting [`Task::join`] from `glib::spawn_future_local` delivers the
//!   result on the main thread. The receiver is executor-agnostic, so
//!   nothing tokio-specific runs on the GTK thread.
//! - [`progress_channel`] creates a bounded, lossy channel for high-rate
//!   progress events. Workers never block on a slow UI. When the channel is
//!   full, intermediate updates are dropped, and the task's final result
//!   remains authoritative.
//!
//! Cancellation: [`Task::abort`] cancels the future at its next `.await`.
//! For uploads, that drops the HTTP request and closes the connection.

use std::future::Future;
use std::io;
use std::sync::Arc;

use thiserror::Error;
use tokio::runtime::{Builder, Handle, Runtime};
use tokio::sync::oneshot;
use tokio::task::AbortHandle;

use crate::api::{ProgressCallback, TransferProgress};

/// Worker threads. Uploads are I/O-bound; a small pool keeps CPU and memory
/// use modest while letting several transfers and listings overlap.
const WORKER_THREADS: usize = 4;

/// The task ended without producing a value.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("background task was cancelled")]
pub struct TaskCancelled;

/// Owner of the background tokio runtime.
///
/// Create exactly one, on the main thread, at startup, and drop it at
/// shutdown. Dropping it cancels all outstanding tasks without blocking.
#[derive(Debug)]
pub struct Worker {
    runtime: Option<Runtime>,
}

impl Worker {
    /// Start the runtime.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the OS refuses to create threads or the
    /// reactor.
    pub fn new() -> io::Result<Self> {
        let runtime = Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .thread_name("filester-worker")
            .enable_all()
            .build()?;
        Ok(Self {
            runtime: Some(runtime),
        })
    }

    /// Handle for code that needs to enter the runtime directly.
    ///
    /// # Panics
    ///
    /// Never in practice. The runtime is present from `new()` until `drop`.
    #[must_use]
    #[allow(clippy::missing_panics_doc)]
    pub fn handle(&self) -> Handle {
        match &self.runtime {
            Some(rt) => rt.handle().clone(),
            None => unreachable!("runtime is only taken in Drop"),
        }
    }

    /// Run `future` on the pool.
    pub fn spawn<F>(&self, future: F) -> Task<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let join = self.handle().spawn(async move {
            if tx.send(future.await).is_err() {
                tracing::trace!("task result discarded: receiver dropped");
            }
        });
        Task {
            rx,
            abort: join.abort_handle(),
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(rt) = self.runtime.take() {
            // Non-blocking: dropping a Runtime normally waits for blocking
            // tasks, which would freeze the UI at exit.
            rt.shutdown_background();
        }
    }
}

/// A running background task.
#[derive(Debug)]
pub struct Task<T> {
    rx: oneshot::Receiver<T>,
    abort: AbortHandle,
}

impl<T> Task<T> {
    /// Request cancellation. Idempotent.
    pub fn abort(&self) {
        self.abort.abort();
    }

    /// Handle that can cancel the task after `self` has been moved into an
    /// awaiting future.
    #[must_use]
    pub fn abort_handle(&self) -> AbortHandle {
        self.abort.clone()
    }

    /// Wait for the result. Safe to await from the `GLib` main context.
    ///
    /// # Errors
    ///
    /// [`TaskCancelled`] if the task was aborted, panicked, or the worker
    /// shut down.
    pub async fn join(self) -> Result<T, TaskCancelled> {
        self.rx.await.map_err(|_| TaskCancelled)
    }
}

/// Create a progress callback for the API engine and the receiver the UI
/// drains with `glib::spawn_future_local`.
///
/// The channel holds at most `capacity` updates. When it is full, new
/// updates are dropped rather than blocking the upload.
#[must_use]
pub fn progress_channel(
    capacity: usize,
) -> (ProgressCallback, async_channel::Receiver<TransferProgress>) {
    let (tx, rx) = async_channel::bounded(capacity.max(1));
    let callback: ProgressCallback = Arc::new(move |p| {
        // Full or closed: dropping an intermediate progress tick is harmless.
        if tx.try_send(p).is_err() {
            tracing::trace!("progress update dropped");
        }
    });
    (callback, rx)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Grace period for observing cancellation.
    const TEST_GRACE: Duration = Duration::from_secs(5);

    /// Await a future from a plain thread, the way the `GLib` main loop would,
    /// without entering the worker runtime.
    fn block_on<F: Future>(f: F) -> F::Output {
        Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn spawn_delivers_result_off_runtime() {
        let worker = Worker::new().unwrap();
        let task = worker.spawn(async {
            let name = std::thread::current().name().map(str::to_owned);
            (21 * 2, name)
        });
        let (value, thread) = block_on(task.join()).unwrap();
        assert_eq!(value, 42);
        assert_eq!(thread.as_deref(), Some("filester-worker"));
    }

    #[test]
    fn abort_yields_cancelled() {
        let worker = Worker::new().unwrap();
        let task = worker.spawn(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        });
        task.abort();
        let res = block_on(async { tokio::time::timeout(TEST_GRACE, task.join()).await });
        assert_eq!(res.unwrap(), Err(TaskCancelled));
    }

    #[test]
    fn dropping_worker_cancels_tasks_without_blocking() {
        let worker = Worker::new().unwrap();
        let task = worker.spawn(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        });
        let started = std::time::Instant::now();
        drop(worker);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(block_on(task.join()), Err(TaskCancelled));
    }

    #[test]
    fn progress_channel_is_lossy_not_blocking() {
        let (cb, rx) = progress_channel(2);
        for sent in 0..10 {
            cb(TransferProgress { sent, total: 10 });
        }
        assert_eq!(rx.len(), 2);
        assert_eq!(rx.try_recv().unwrap().sent, 0);
        drop(rx);
        cb(TransferProgress {
            sent: 10,
            total: 10,
        }); // closed: must not panic
    }
}
