//! `blink` binary for bufflink — an ACP bridge for the freebuff CLI.
//!
//! stdout is reserved for newline-delimited JSON-RPC; all human-readable logs
//! go to stderr so they never corrupt the protocol stream.
//!
//! CLI surface:
//!   blink                 — ACP stdio server (default, unchanged)
//!   blink --version | -V  — print env!("CARGO_PKG_VERSION"), exit 0
//!   blink --help   | -h   — short usage block, exit 0
//!   blink --list-models   — one model id per line on stdout, exit 0
//!   blink --list-models —json
//!                         — JSON catalog on stdout, exit 0
//!   any other flag        — clear error on stderr, non-zero exit

use std::sync::Arc;

use anyhow::{Context, Result};
use bufflink::acp::{serve, Backend};
use bufflink::freebuff::enumerate::{
    enumerate_catalog, plain_model_ids, unmapped_warning, CatalogEntry,
};
use bufflink::freebuff::{DriverConfig, FreebuffBackend};
use bufflink::pty::{Pty, PtyConfig};
use serde::Serialize;
use tokio::io::{stdin, stdout, BufReader};
use tokio::signal::unix::{signal, SignalKind};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Serve,
    Version,
    Help,
    ListModels { json: bool },
}

/// Hand-rolled argv parsing (no new crate: keeps the dependency tree small).
/// Returns an error message for unrecognised flags so `main` can exit
/// non-zero instead of falling through into the silent ACP loop.
fn parse_args(args: &[String]) -> Result<Command, String> {
    let mut json = false;
    let mut list_models = false;
    for arg in &args[1..] {
        match arg.as_str() {
            "--version" | "-V" => return Ok(Command::Version),
            "--help" | "-h" => return Ok(Command::Help),
            "--list-models" => list_models = true,
            "--json" => json = true,
            other => return Err(format!("unknown argument: {other} (try --help)")),
        }
    }
    if list_models {
        Ok(Command::ListModels { json })
    } else {
        Ok(Command::Serve)
    }
}

fn print_help() {
    println!(
        r#"blink — ACP bridge for the freebuff CLI

Usage:
  blink                 Start the ACP stdio server (default)
  blink --version | -V  Print the version and exit
  blink --help   | -h   Print this help and exit
  blink --list-models   Print one model id per line on stdout and exit
  blink --list-models --json
                        Print the model catalog as JSON on stdout and exit

stdout is reserved for protocol data; diagnostics go to stderr.
In --json, an entry's `section` is the picker section it was listed under
(`unlimited` is the free tier) and `price_per_hour` is what the picker states
for an hour on that model: 0 for `unlimited`, null when the picker states no
price."#
    );
}

/// JSON shape emitted by `blink --list-models --json`. Unknown fields are
/// omitted (never invented) via `skip_serializing_if`.
#[derive(Serialize)]
struct CatalogJson {
    schema: u32,
    models: Vec<CatalogEntry>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // stderr logging; stdout stays free for JSON-RPC (and for --list-models data).
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().collect();
    match parse_args(&args) {
        Err(msg) => {
            eprintln!("blink: {msg}");
            std::process::exit(2);
        }
        Ok(Command::Version) => {
            println!("{}", env!("CARGO_PKG_VERSION"));
        }
        Ok(Command::Help) => {
            print_help();
        }
        Ok(Command::ListModels { json }) => run_list_models(json).await?,
        Ok(Command::Serve) => run_serve().await?,
    }
    Ok(())
}

/// Run the ACP stdio server — the hot path omnigent uses. Unchanged behaviour
/// when blink is invoked with no arguments.
async fn run_serve() -> Result<()> {
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

/// Enumerate freebuff's model catalog from its TUI and print it to stdout.
///
/// The PTY child is killed on every exit path (success or error) so no
/// orphan freebuff is left behind. Any failure is surfaced on stderr with a
/// non-zero exit (og treats non-zero as a probe failure and falls back).
async fn run_list_models(json: bool) -> Result<()> {
    let cfg = DriverConfig::from_env();
    let pty_cfg = PtyConfig {
        program: cfg.program.clone(),
        args: Vec::new(),
        cwd: Some(std::env::temp_dir()),
        env: cfg.env.clone(),
        cols: 120,
        rows: 40,
    };

    // Spawn freebuff in a PTY. The binary path is injectable via
    // `BLINK_FREEBUFF_BIN` (DriverConfig.program, default `freebuff`).
    let pty = Pty::spawn(pty_cfg).await.with_context(|| {
        format!(
            "failed to spawn freebuff ({}) for --list-models",
            cfg.program
        )
    })?;

    let catalog = enumerate_catalog(&pty).await;
    // Clean up the PTY/child on every exit path, including errors.
    let _ = pty.kill().await;
    let catalog = catalog.context("freebuff: failed to enumerate the model catalog")?;

    // A display name with no known id (the lineup rotated) is never an id: flag
    // it on stderr so a rotation is visible in og's setup output. stdout stays
    // clean (protocol/data only).
    if let Some(warning) = unmapped_warning(&catalog) {
        eprintln!("{warning}");
    }

    if json {
        let payload = CatalogJson {
            schema: 1u32,
            models: catalog,
        };
        println!("{}", serde_json::to_string(&payload)?);
    } else {
        // og consumes this as pinnable ids: emit ONLY well-formed ids.
        for id in plain_model_ids(&catalog) {
            println!("{id}");
        }
    }
    Ok(())
}
