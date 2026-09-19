//! Backend seam implemented by the freebuff driver.

use std::future::Future;
use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::acp::types::{SessionUpdate, StopReason};

/// Sink for streaming [`SessionUpdate`]s during a prompt.
///
/// Wraps a `tokio::sync::mpsc::Sender<SessionUpdate>`. The ACP server forwards
/// each item as a `session/update` notification before answering the prompt.
#[derive(Debug, Clone)]
pub struct UpdateSink {
    tx: mpsc::Sender<SessionUpdate>,
}

impl UpdateSink {
    /// Create a sink that sends into `tx`.
    pub fn new(tx: mpsc::Sender<SessionUpdate>) -> Self {
        Self { tx }
    }

    /// Forward one session update to the ACP server.
    pub async fn send(&self, u: SessionUpdate) {
        // Ignore send failures (server shut down / channel closed).
        let _ = self.tx.send(u).await;
    }
}

/// Driver seam: one freebuff session per process.
///
/// Uses return-position `impl Future + Send` (RPITIT) rather than boxed
/// futures, so implementors stay zero-cost and `tokio::spawn` works when the
/// caller owns `Arc<Self>` and owned string arguments.
pub trait Backend: Send + Sync + 'static {
    /// Create (or return) the single session for this process. Returns the session id.
    fn new_session(&self, cwd: PathBuf) -> impl Future<Output = anyhow::Result<String>> + Send;

    /// Run one prompt turn. Stream updates through `updates`. Watch `cancel`
    /// for `true` (set by `session/cancel` or stdin EOF); return
    /// [`StopReason::Cancelled`] when aborted that way.
    fn prompt(
        &self,
        session_id: &str,
        text: String,
        updates: UpdateSink,
        cancel: tokio::sync::watch::Receiver<bool>,
    ) -> impl Future<Output = anyhow::Result<StopReason>> + Send;

    /// Tear down the backend. Must return within ~3 s (stdin EOF / SIGTERM).
    fn shutdown(&self) -> impl Future<Output = ()> + Send;
}
