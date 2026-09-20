//! `src/pty/` — spawning freebuff in a PTY, screen model, input injection.
//!
//! A small, generic async PTY driver. Nothing here knows about freebuff; it
//! just spawns a child in a pseudo-terminal, keeps a readable screen model
//! (backed by a [`vt100`] parser), auto-replies to a handful of well-known
//! terminal capability queries, and offers async input injection primitives.

mod reader;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use tokio::io::AsyncWriteExt;
use tracing::warn;

/// Configuration for spawning a child inside a new pseudo-terminal.
#[derive(Debug, Clone)]
pub struct PtyConfig {
    /// The program to exec.
    pub program: String,
    /// Arguments passed to `program`.
    pub args: Vec<String>,
    /// Working directory for the child; `None` inherits ours.
    pub cwd: Option<PathBuf>,
    /// Extra environment, **added to** the inherited environment. A `TERM`
    /// entry here overrides our default; otherwise `TERM=xterm-256color` is
    /// always set.
    pub env: Vec<(String, String)>,
    /// Initial terminal width in columns.
    pub cols: u16,
    /// Initial terminal height in rows.
    pub rows: u16,
}

/// A single key press, mapped to the raw byte sequence we send to the PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Enter,
    Escape,
    CtrlC,
    CtrlD,
    Up,
    Down,
    Left,
    Right,
    Tab,
    Backspace,
}

impl Key {
    /// The raw bytes sent for this key.
    fn bytes(self) -> &'static [u8] {
        match self {
            Key::Enter => b"\r",
            Key::Escape => b"\x1b",
            Key::CtrlC => b"\x03",
            Key::CtrlD => b"\x04",
            Key::Up => b"\x1b[A",
            Key::Down => b"\x1b[B",
            Key::Right => b"\x1b[C",
            Key::Left => b"\x1b[D",
            Key::Tab => b"\t",
            Key::Backspace => b"\x7f",
        }
    }
}

/// A cheap snapshot of the current terminal screen model.
#[derive(Debug, Clone)]
pub struct ScreenSnapshot {
    /// One `String` per terminal row, in top-to-bottom order.
    pub rows: Vec<String>,
    /// The (0-based) row/col of the cursor.
    pub cursor: (u16, u16),
    /// Whether the alternate screen buffer is active.
    pub alternate_screen: bool,
    /// Monotonic counter bumped by the reader after every processed chunk.
    pub generation: u64,
}

impl ScreenSnapshot {
    /// All visible rows joined by `\n`, trailing spaces trimmed per row.
    pub fn text(&self) -> String {
        self.rows
            .iter()
            .map(|r| r.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Whether the rendered text contains the given substring.
    pub fn contains(&self, s: &str) -> bool {
        self.text().contains(s)
    }

    /// Index of the first row whose text contains `s`.
    pub fn find_row(&self, s: &str) -> Option<usize> {
        self.rows.iter().position(|r| r.contains(s))
    }
}

/// Shared state between the `Pty` handle and the background reader task.
pub(crate) struct Shared {
    /// Guarded by a plain `std` mutex; critical sections are tiny.
    pub parser: Mutex<vt100::Parser>,
    /// Broadcast by the reader after each processed chunk so `wait_for` wakes.
    pub gen: tokio::sync::watch::Sender<u64>,
    /// Set by the reader when the child process / PTY reaches EOF.
    pub child_exited: AtomicBool,
    /// Write half of the PTY; shared so the reader can auto-reply to queries
    /// while the caller is typing. Serialises writes to the line.
    pub write: tokio::sync::Mutex<pty_process::OwnedWritePty>,
}
/// An async handle to a child running inside a pseudo-terminal.
pub struct Pty {
    shared: Arc<Shared>,
    /// Guarded because `tokio::process::Child` isn't `Sync`.
    child: Mutex<Option<tokio::process::Child>>,
}

impl Pty {
    /// Spawn `cfg.program` in a fresh PTY of size `rows x cols` and start the
    /// background reader task that feeds the screen model and auto-replies to
    /// terminal capability queries.
    pub async fn spawn(cfg: PtyConfig) -> Result<Pty> {
        let max_attempts = 5;
        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 1..=max_attempts {
            let (pty, pts) = match pty_process::open().context("failed to open pty") {
                Ok(p) => p,
                Err(e) => {
                    last_err = Some(e);
                    if attempt < max_attempts {
                        warn!(attempt, "pty open failed, retrying");
                        tokio::time::sleep(Duration::from_millis(25 * attempt)).await;
                        continue;
                    }
                    break;
                }
            };

            if let Err(e) = pty
                .resize(pty_process::Size::new(cfg.rows, cfg.cols))
                .context("failed to size pty")
            {
                last_err = Some(e);
                if attempt < max_attempts {
                    warn!(attempt, "pty resize failed, retrying");
                    tokio::time::sleep(Duration::from_millis(25 * attempt)).await;
                    continue;
                }
                break;
            }

            let mut cmd = pty_process::Command::new(&cfg.program);
            cmd = cmd.args(&cfg.args);
            if let Some(dir) = &cfg.cwd {
                cmd = cmd.current_dir(dir);
            }
            cmd = cmd.envs(build_env(&cfg));

            match cmd.spawn(pts) {
                Ok(child) => {
                    let (read_half, write_half) = pty.into_split();

                    let shared = Arc::new(Shared {
                        parser: Mutex::new(vt100::Parser::new(cfg.rows, cfg.cols, 0)),
                        gen: tokio::sync::watch::channel(0u64).0,
                        child_exited: AtomicBool::new(false),
                        write: tokio::sync::Mutex::new(write_half),
                    });

                    let reader_shared = Arc::clone(&shared);
                    tokio::spawn(reader::reader_task(read_half, reader_shared));

                    return Ok(Pty {
                        shared,
                        child: Mutex::new(Some(child)),
                    });
                }
                Err(e) => {
                    let anyhow_err: anyhow::Error = e.into();
                    let is_not_found = anyhow_err.chain().any(|c| {
                        c.downcast_ref::<std::io::Error>()
                            .map(|io| io.kind() == std::io::ErrorKind::NotFound)
                            .unwrap_or(false)
                    });
                    let program_exists = Path::new(&cfg.program).exists();

                    if is_not_found && program_exists && attempt < max_attempts {
                        warn!(attempt, program = %cfg.program, "spawn failed with ENOENT, retrying");
                        tokio::time::sleep(Duration::from_millis(25 * attempt)).await;
                        last_err = Some(anyhow_err);
                        continue;
                    } else if !program_exists {
                        last_err = Some(
                            anyhow_err.context(format!("program `{}` does not exist", cfg.program)),
                        );
                        break;
                    } else {
                        last_err = Some(
                            anyhow_err.context(format!("failed to spawn `{}` in pty", cfg.program)),
                        );
                        break;
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| {
            anyhow!(
                "failed to spawn `{}` in pty after {} attempts",
                cfg.program,
                max_attempts
            )
        }))
    }

    /// Write raw bytes to the child's terminal input.
    pub async fn write(&self, bytes: &[u8]) -> Result<()> {
        let mut w = self.shared.write.lock().await;
        w.write_all(bytes).await.context("failed to write to pty")
    }

    /// Type text as raw bytes (no trailing CR added).
    pub async fn type_text(&self, s: &str) -> Result<()> {
        self.write(s.as_bytes()).await
    }

    /// Paste text using bracketed paste mode: `ESC[200~` + `s` + `ESC[201~`.
    pub async fn paste(&self, s: &str) -> Result<()> {
        let mut bytes = Vec::with_capacity(s.len() + 8);
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(s.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        self.write(&bytes).await
    }

    /// Send a single key press.
    pub async fn key(&self, k: Key) -> Result<()> {
        self.write(k.bytes()).await
    }

    /// Cheap snapshot of the current screen grid.
    pub fn screen(&self) -> ScreenSnapshot {
        let parser = self.shared.parser.lock().unwrap();
        let (_scr_rows, scr_cols) = parser.screen().size();
        let rows = parser.screen().rows(0, scr_cols).collect();
        let cursor = parser.screen().cursor_position();
        let alternate_screen = parser.screen().alternate_screen();
        let generation = *self.shared.gen.borrow();
        ScreenSnapshot {
            rows,
            cursor,
            alternate_screen,
            generation,
        }
    }
    /// Wait until `pred` is satisfied by the screen, or `timeout` elapses.
    /// On timeout, returns an error that embeds the last snapshot's text.
    pub async fn wait_for<F: Fn(&ScreenSnapshot) -> bool>(
        &self,
        pred: F,
        timeout: Duration,
    ) -> Result<ScreenSnapshot> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut rx = self.shared.gen.subscribe();
        loop {
            {
                let snap = self.screen();
                if pred(&snap) {
                    return Ok(snap);
                }
            }
            match tokio::time::timeout_at(deadline, rx.changed()).await {
                Ok(Ok(())) => {
                    rx.borrow_and_update();
                    continue;
                }
                Ok(Err(_)) => {
                    // Sender dropped: the reader task has finished.
                    let exc = self.shared.child_exited.load(Ordering::SeqCst);
                    let snap = self.screen();
                    if pred(&snap) {
                        return Ok(snap);
                    }
                    return Err(anyhow!(
                        "pty reader closed (child_exited={exc}) before predicate held; \
                         last screen:\n{}",
                        snap.text()
                    ));
                }
                Err(_elapsed) => {
                    let snap = self.screen();
                    if pred(&snap) {
                        return Ok(snap);
                    }
                    return Err(anyhow!(
                        "wait_for timed out after {timeout:?}; last screen:\n{}",
                        snap.text()
                    ));
                }
            }
        }
    }

    /// Non-blocking check of whether the child has exited.
    pub fn try_wait(&self) -> Result<Option<ExitStatus>> {
        let mut guard = self.child.lock().unwrap();
        match guard.as_mut() {
            Some(child) => child.try_wait().map_err(Into::into),
            None => Ok(None),
        }
    }

    /// Block until the child exits or `timeout` elapses. Returns `Ok(None)` on
    /// timeout.
    pub async fn wait_exit(&self, timeout: Duration) -> Result<Option<ExitStatus>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Terminate the child and its whole process group. The child is spawned as
    /// a session leader by pty-process (`setsid`), so its process group id
    /// equals its pid. freebuff's launcher spawns the real Bun binary as a
    /// grandchild and forwards no signals, so signalling only the direct
    /// child leaves the grandchild alive and holding the single-instance
    /// lock. Sequence: SIGTERM the group, poll `try_wait` for up to 1 s, then
    /// SIGKILL the group if anything is still alive, then reap.
    pub async fn kill(&self) -> Result<()> {
        let pid = self.pid();
        if let Some(pid) = pid {
            // SIGTERM the whole process group first.
            let term_rc = unsafe { libc::kill(-(pid as i32), libc::SIGTERM) };
            if term_rc < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::ESRCH) {
                    warn!(
                        pid,
                        errno = err.raw_os_error(),
                        "SIGTERM on process group failed; falling back to Child::kill"
                    );
                }
            }

            // Poll for up to 1 s for the group to die from SIGTERM.
            let deadline = std::time::Instant::now() + Duration::from_secs(1);
            let mut exited = false;
            while std::time::Instant::now() < deadline {
                if self.try_wait()?.is_some() {
                    exited = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }

            if !exited {
                // SIGKILL the group.
                let kill_rc = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
                if kill_rc < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() != Some(libc::ESRCH) {
                        warn!(
                            pid,
                            errno = err.raw_os_error(),
                            "SIGKILL on process group failed; falling back to Child::kill"
                        );
                    }
                }
            }
        }

        // Fall back to Child::kill for any direct child still alive.
        let mut child = {
            let mut guard = self.child.lock().unwrap();
            match guard.take() {
                Some(child) => child,
                None => return Ok(()),
            }
        }; // guard dropped here, before the await below.
        child.kill().await.context("failed to kill child")?;
        // Put the (dead) child back so `pid()`/`try_wait()` still work and can
        // reap it.
        *self.child.lock().unwrap() = Some(child);
        Ok(())
    }

    /// Resize the PTY's terminal to `cols` x `rows`.
    pub async fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let write = self.shared.write.lock().await;
        write
            .resize(pty_process::Size::new(rows, cols))
            .context("failed to resize pty")
    }

    /// The child's OS pid, if a child is still tracked.
    pub fn pid(&self) -> Option<u32> {
        self.child.lock().unwrap().as_ref().and_then(|c| c.id())
    }
}

/// Build the child environment: inherited vars, plus `cfg.env` overrides, with
/// `TERM=xterm-256color` forced unless the caller overrode `TERM`.
fn build_env(cfg: &PtyConfig) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    for (k, v) in &cfg.env {
        upsert(&mut env, k.clone(), v.clone());
    }
    let caller_set_term = cfg.env.iter().any(|(k, _)| k == "TERM");
    if !caller_set_term {
        upsert(&mut env, "TERM".to_string(), "xterm-256color".to_string());
    }
    env
}

fn upsert(env: &mut Vec<(String, String)>, key: String, value: String) {
    if let Some(entry) = env.iter_mut().find(|(k, _)| *k == key) {
        entry.1 = value;
    } else {
        env.push((key, value));
    }
}
