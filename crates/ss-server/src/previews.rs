//! Link-preview jobs. T-029 replaces the drain task with fetch workers.

use ss_core::Id;
use tokio::sync::mpsc;

/// One message whose body may contain URLs.
#[derive(Debug)]
pub struct PreviewJob {
    /// Stored message.
    pub message_id: Id,
    /// Raw markdown body.
    pub content: String,
}

/// Bounded queue. A full queue drops the job instead of waiting on a worker.
#[derive(Clone)]
pub struct PreviewQueue {
    tx: mpsc::Sender<PreviewJob>,
}

impl PreviewQueue {
    /// Starts a task that discards jobs until T-029 attaches workers.
    #[must_use]
    pub fn new() -> Self {
        let (tx, mut rx) = mpsc::channel::<PreviewJob>(32);
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                tracing::debug!(message_id = %job.message_id, "link preview job discarded until workers exist");
            }
        });
        Self { tx }
    }

    /// Queues `job` when there is room.
    pub fn enqueue(&self, job: PreviewJob) {
        let _ = self.tx.try_send(job);
    }
}

impl Default for PreviewQueue {
    fn default() -> Self {
        Self::new()
    }
}
