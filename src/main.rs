//! `blink` binary for bufflink — an ACP bridge for the freebuff CLI.
//!
//! stdout is reserved for newline-delimited JSON-RPC; all human-readable logs
//! go to stderr so they never corrupt the protocol stream.

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // stderr logging; stdout stays free for JSON-RPC.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("blink starting");

    Ok(())
}
