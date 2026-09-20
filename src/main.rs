//! `blink` binary for bufflink — an ACP bridge for the freebuff CLI.
//!
//! stdout is reserved for newline-delimited JSON-RPC; all human-readable logs
//! go to stderr so they never corrupt the protocol stream.

use std::sync::Arc;

use bufflink::acp::{serve, Backend};
use bufflink::freebuff::{DriverConfig, FreebuffBackend};
use tokio::io::{stdin, stdout, BufReader};
use tokio::signal::unix::{signal, SignalKind};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // stderr logging; stdout stays free for JSON-RPC.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("blink starting");

    // Build driver config from environment
    let cfg = DriverConfig::from_env();
    let backend = Arc::new(FreebuffBackend::new(cfg));

    // Run ACP server with signal handling
    let backend_clone = Arc::clone(&backend);
    let serve_fut = serve(backend_clone, BufReader::new(stdin()), stdout());

    // Set up signal handlers
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    tokio::select! {
        result = serve_fut => {
            // serve() already calls backend.shutdown() on EOF
            result
        }
        _ = sigterm.recv() => {
            tracing::info!("received SIGTERM, shutting down");
            (*backend).shutdown().await;
            // the blocking stdin reader thread keeps the runtime alive; exit explicitly
            tracing::info!("shutdown complete; exiting");
            std::process::exit(0);
        }
        _ = sigint.recv() => {
            tracing::info!("received SIGINT, shutting down");
            (*backend).shutdown().await;
            // the blocking stdin reader thread keeps the runtime alive; exit explicitly
            tracing::info!("shutdown complete; exiting");
            std::process::exit(0);
        }
    }
}
