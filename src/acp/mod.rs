//! ACP server: JSON-RPC framing, method dispatch, and the freebuff backend seam.
//!
//! Speaks Agent Client Protocol version 1 over newline-delimited JSON-RPC 2.0
//! on stdio. stdout is reserved for JSON-RPC; logs go to stderr via `tracing`.

mod backend;
mod jsonrpc;
mod server;
mod types;

pub use backend::{Backend, UpdateSink};
pub use server::serve;
pub use types::*;
