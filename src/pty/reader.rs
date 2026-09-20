//! The background reader task.
//!
//! Consumes the PTY master's byte stream, feeds it into a [`vt100::Parser`],
//! auto-replies to a small set of terminal capability queries the TUI is known
//! to send, bumps a generation counter and wakes waiters after every chunk.
//! Terminates cleanly on EOF or child exit.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::Shared;

/// `ESC[6n` — cursor position report request.
const DSR_CURSOR: &[u8] = b"\x1b[6n";
/// `ESC[?u` — kitty keyboard protocol request.
const KITTY_KEYBOARD: &[u8] = b"\x1b[?u";
/// `ESC]11;?` + BEL — background colour query.
const OSC11_BEL: &[u8] = b"\x1b]11;?\x07";
/// `ESC]11;?` + ST — background colour query.
const OSC11_ST: &[u8] = b"\x1b]11;?\x1b\\";
/// `ESC]10;?` + BEL — foreground colour query.
const OSC10_BEL: &[u8] = b"\x1b]10;?\x07";
/// `ESC]10;?` + ST — foreground colour query.
const OSC10_ST: &[u8] = b"\x1b]10;?\x1b\\";

/// The recognised query sequences, in a stable order for scanning.
const QUERIES: &[&[u8]] = &[
    DSR_CURSOR,
    KITTY_KEYBOARD,
    OSC11_BEL,
    OSC11_ST,
    OSC10_BEL,
    OSC10_ST,
];

/// The distinct query kinds we know how to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueryKind {
    CursorReport,
    KittyKeyboard,
    SetFg,
    SetBg,
}

/// Drive the read loop until EOF or an error. Runs forever in the background.
pub(crate) async fn reader_task(mut read: pty_process::OwnedReadPty, shared: Arc<Shared>) {
    let mut carry: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = match read.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(ref e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof
                ) =>
            {
                break;
            }
            Err(e) => {
                tracing::warn!(error = %e, "pty read error");
                break;
            }
        };

        // Combine with any leftover from the previous chunk, scan for the
        // queries, and determine how far forward we've consumed.
        let mut combined = std::mem::take(&mut carry);
        combined.extend_from_slice(&buf[..n]);

        let mut queries = Vec::new();
        let consumed = scan(&combined, &mut queries);

        // Feed the consumed bytes (ordinary output, plus any ignorable query
        // bytes) to the parser.
        if consumed > 0 {
            shared.parser.lock().unwrap().process(&combined[..consumed]);
        }
        // Hold any unconsumed tail (a possible split query) for the next chunk.
        if consumed < combined.len() {
            carry = combined[consumed..].to_vec();
        }

        // Build and write replies from the current parser state so a DSR
        // reply's cursor position reflects the bytes just parsed.
        if !queries.is_empty() {
            let replies: Vec<u8> = queries
                .iter()
                .filter_map(|q| reply_for(*q, &shared))
                .flatten()
                .collect();
            if !replies.is_empty() {
                let mut w = shared.write.lock().await;
                if w.write_all(&replies).await.is_err() {
                    tracing::warn!("failed to write pty auto-reply");
                }
            }
        }

        // Bump the generation and wake waiters.
        let gen = *shared.gen.borrow() + 1;
        let _ = shared.gen.send(gen);
    }
    shared.child_exited.store(true, Ordering::SeqCst);
}

/// Scan `combined` left-to-right, recording any recognised queries in `out`.
/// Ordinary bytes are skipped; the return value is the number of bytes
/// consumed from the front of `combined`. The unconsumed tail is a possible
/// query that is split across chunk boundaries.
fn scan(combined: &[u8], out: &mut Vec<QueryKind>) -> usize {
    let mut i = 0;
    while i < combined.len() {
        let rest = &combined[i..];

        // Full query match first: consume it and record the kind.
        let mut matched = false;
        for q in QUERIES {
            if rest.starts_with(q) {
                out.push(kind_for(q));
                i += q.len();
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }

        // Otherwise this position might start a query that is split across
        // chunk boundaries: hold the tail for the next call.
        let mut prefix = false;
        for q in QUERIES {
            if strict_prefix(rest, q) {
                prefix = true;
                break;
            }
        }
        if prefix {
            break;
        }

        // Ordinary byte: consume it (stays in the parser stream).
        i += 1;
    }
    i
}

/// Whether `rest` is a non-empty strict prefix of `q` (a possible split query).
fn strict_prefix(rest: &[u8], q: &[u8]) -> bool {
    !rest.is_empty() && rest.len() < q.len() && q.starts_with(rest)
}

/// Map a recognised query byte-sequence to its [`QueryKind`].
fn kind_for(q: &[u8]) -> QueryKind {
    match q {
        DSR_CURSOR => QueryKind::CursorReport,
        KITTY_KEYBOARD => QueryKind::KittyKeyboard,
        OSC11_BEL | OSC11_ST => QueryKind::SetBg,
        OSC10_BEL | OSC10_ST => QueryKind::SetFg,
        _ => unreachable!("scan only emits recognised queries"),
    }
}

/// Build the byte reply for a recognised query kind.
fn reply_for(kind: QueryKind, shared: &Shared) -> Option<Vec<u8>> {
    Some(match kind {
        QueryKind::CursorReport => {
            // Cursor position reply is 1-based.
            let (row, col) = shared.parser.lock().unwrap().screen().cursor_position();
            format!("\x1b[{};{}R", row + 1, col + 1).into_bytes()
        }
        QueryKind::KittyKeyboard => b"\x1b[?0u".to_vec(),
        QueryKind::SetBg => b"\x1b]11;rgb:0000/0000/0000\x1b\\".to_vec(),
        QueryKind::SetFg => b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\".to_vec(),
    })
}
