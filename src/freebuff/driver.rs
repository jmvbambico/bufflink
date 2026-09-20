//! `src/freebuff/driver.rs` — the glue that drives freebuff's TUI and implements the ACP Backend.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, Result};
use serde_json::Value;
use tokio::fs;
use tokio::sync::{watch, Mutex};
use tracing::{error, info, trace, warn};

use crate::acp::{
    Backend, SessionUpdate, StopReason, TextContent, ToolCallContentItem, ToolCallStatus,
    UpdateSink,
};
use crate::freebuff::chats::{
    chats_dir, instance_owner, newest_new_chat, snapshot, CHAT_MESSAGES, LOG,
};
use crate::freebuff::log::{outcome, parse_log_line, LogEvent, TurnOutcome};
use crate::freebuff::screen::{
    classify, input_box_is_empty, input_box_text, pasted_chip_chars, ScreenState, SPLASH_ACCEPT_KEY,
};
use crate::freebuff::transcript::{diff, parse_messages, Block, Cursor, Delta, Message};
use crate::freebuff::{CANCEL_KEY, EXIT_COMMAND};
use crate::pty::{Key, Pty, PtyConfig, ScreenSnapshot};

/// Configuration for the freebuff driver.
#[derive(Debug, Clone)]
pub struct DriverConfig {
    /// Program to execute (default: "freebuff", env: BLINK_FREEBUFF_BIN).
    pub program: String,
    /// Extra arguments passed to the program (default: []).
    pub extra_args: Vec<String>,
    /// Extra environment variables for the child (default: []).
    pub env: Vec<(String, String)>,
    /// Override manicode directory (default: None -> chats::manicode_dir()).
    pub manicode_dir: Option<PathBuf>,
    /// Terminal width in columns (default: 120).
    pub cols: u16,
    /// Terminal height in rows (default: 40).
    pub rows: u16,
    /// Startup timeout waiting for Idle (default: 60s).
    pub startup_timeout: Duration,
    /// Per-turn timeout (default: 15min, env: BLINK_TURN_TIMEOUT_S).
    pub turn_timeout: Duration,
    /// Poll interval for screen classification (default: 250ms).
    pub poll_interval: Duration,
    /// Exit timeout for graceful shutdown (default: 3s).
    pub exit_timeout: Duration,
    /// Time to wait for transcript to settle after turn log event (default: 5s, env: BLINK_SETTLE_TIMEOUT_S).
    pub settle_timeout: Duration,
    /// Time to wait after Enter for freebuff to go busy or consume the paste (default: 30s, env: BLINK_SUBMIT_TIMEOUT_S).
    pub submit_timeout: Duration,
}

impl DriverConfig {
    /// Create a DriverConfig from environment variables with sensible defaults.
    pub fn from_env() -> Self {
        let program =
            std::env::var("BLINK_FREEBUFF_BIN").unwrap_or_else(|_| "freebuff".to_string());
        let turn_timeout_secs = std::env::var("BLINK_TURN_TIMEOUT_S")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15 * 60);
        let settle_timeout_secs = std::env::var("BLINK_SETTLE_TIMEOUT_S")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5);
        let submit_timeout =
            parse_submit_timeout(std::env::var("BLINK_SUBMIT_TIMEOUT_S").ok().as_deref());

        Self {
            program,
            extra_args: Vec::new(),
            env: Vec::new(),
            manicode_dir: None,
            cols: 120,
            rows: 40,
            startup_timeout: Duration::from_secs(60),
            turn_timeout: Duration::from_secs(turn_timeout_secs),
            poll_interval: Duration::from_millis(250),
            exit_timeout: Duration::from_secs(3),
            settle_timeout: Duration::from_secs(settle_timeout_secs),
            submit_timeout,
        }
    }
}

/// Parse `BLINK_SUBMIT_TIMEOUT_S`. Default 30 s when the variable is absent or
/// unparseable; floored at 2 s so the 2 s nudge remainder never underflows.
fn parse_submit_timeout(raw: Option<&str>) -> Duration {
    let secs = match raw.and_then(|s| s.parse::<u64>().ok()) {
        Some(v) => v,
        None => return Duration::from_secs(30),
    };
    if secs < 2 {
        warn!("BLINK_SUBMIT_TIMEOUT_S={secs} is below the 2 s floor; using 2 s");
        Duration::from_secs(2)
    } else {
        Duration::from_secs(secs)
    }
}

/// Remainder of `submit_timeout` after reserving the 2 s nudge, saturating at
/// zero so a programmatically built config can never underflow.
fn remainder_after_nudge(submit_timeout: Duration) -> Duration {
    submit_timeout.saturating_sub(Duration::from_secs(2))
}

/// Session state held by the backend.
struct Session {
    pty: Arc<Pty>,
    chats_dir: PathBuf,
    initial_snapshot: std::collections::BTreeSet<String>,
    chat_dir: Arc<Mutex<Option<PathBuf>>>,
    transcript_cursor: Arc<Mutex<Cursor>>,
    log_offset: Arc<Mutex<u64>>,
    cancelled: Arc<AtomicBool>,
    session_id: String,
    /// Count of consecutive transcript parse failures.
    parse_failures: Arc<Mutex<u32>>,
    /// Whether any AgentMessageChunk was emitted during the current turn.
    emitted_message_chunk: Arc<AtomicBool>,
}

/// Backend that drives a single freebuff session per process.
pub struct FreebuffBackend {
    cfg: DriverConfig,
    session: Mutex<Option<Session>>,
}

impl FreebuffBackend {
    /// Create a new backend with the given configuration.
    pub fn new(cfg: DriverConfig) -> Self {
        Self {
            cfg,
            session: Mutex::new(None),
        }
    }

    /// Get the manicode directory for this backend.
    fn manicode_dir(&self) -> PathBuf {
        self.cfg
            .manicode_dir
            .clone()
            .unwrap_or_else(crate::freebuff::chats::manicode_dir)
    }

    /// Build PtyConfig from DriverConfig and cwd.
    fn build_pty_config(&self, cwd: &Path) -> PtyConfig {
        PtyConfig {
            program: self.cfg.program.clone(),
            args: {
                let mut args = vec!["--cwd".to_string(), cwd.to_string_lossy().to_string()];
                args.extend(self.cfg.extra_args.clone());
                args
            },
            cwd: Some(cwd.to_path_buf()),
            env: self.cfg.env.clone(),
            cols: self.cfg.cols,
            rows: self.cfg.rows,
        }
    }

    /// Wait for the pty to reach Idle state.
    async fn wait_for_idle(&self, pty: &Pty, timeout: Duration) -> Result<()> {
        pty.wait_for(|s| matches!(classify(&s.rows), ScreenState::Idle), timeout)
            .await?;
        Ok(())
    }

    /// Wait for the pty to reach Busy state.
    #[allow(dead_code)]
    async fn wait_for_busy(&self, pty: &Pty, timeout: Duration) -> Result<()> {
        pty.wait_for(
            |s| matches!(classify(&s.rows), ScreenState::Busy { .. }),
            timeout,
        )
        .await?;
        Ok(())
    }

    /// Wait for input box to contain expected text.
    #[allow(dead_code)]
    async fn wait_for_input_box_contains(
        &self,
        pty: &Pty,
        expected: &str,
        timeout: Duration,
    ) -> Result<()> {
        let expected = expected.to_string();
        pty.wait_for(
            move |s| {
                input_box_text(&s.rows)
                    .map(|t| t.contains(&expected))
                    .unwrap_or(false)
            },
            timeout,
        )
        .await?;
        Ok(())
    }

    /// Read new log lines and parse events.
    async fn read_log_events(&self, chat_dir: &Path, offset: &mut u64) -> Result<Vec<LogEvent>> {
        let log_path = chat_dir.join(LOG);
        let metadata = fs::metadata(&log_path).await.ok();
        let file_size = metadata.map(|m| m.len()).unwrap_or(0);

        if file_size <= *offset {
            return Ok(Vec::new());
        }

        let content = fs::read(&log_path).await?;
        let new_content = &content[*offset as usize..];
        *offset = file_size;

        let mut events = Vec::new();
        for line in String::from_utf8_lossy(new_content).lines() {
            if !line.trim().is_empty() {
                events.push(parse_log_line(line));
            }
        }
        Ok(events)
    }

    /// Process transcript deltas and send updates.
    /// Returns Ok(()) on success, or an error if parsing fails and this is a final flush attempt.
    async fn process_transcript_deltas(
        &self,
        chat_dir: &Path,
        cursor: &mut Cursor,
        updates: &UpdateSink,
        parse_failures: &Arc<Mutex<u32>>,
        emitted_message_chunk: &Arc<AtomicBool>,
        is_final_flush: bool,
    ) -> Result<()> {
        let transcript_path = chat_dir.join(CHAT_MESSAGES);
        let content = match fs::read_to_string(&transcript_path).await {
            Ok(c) => c,
            Err(_) => return Ok(()), // File doesn't exist yet
        };

        // Try to parse; if it fails, log and count
        let messages = match parse_messages(&content) {
            Ok(m) => {
                // Reset failure count on successful parse
                *parse_failures.lock().await = 0;
                m
            }
            Err(e) => {
                let mut failures = parse_failures.lock().await;
                *failures += 1;
                tracing::warn!(failures = *failures, error = %e, "freebuff transcript parse failed");
                if is_final_flush {
                    return Err(anyhow!("freebuff transcript could not be parsed: {e:#}"));
                }
                return Ok(()); // Torn write, retry next poll
            }
        };

        let (new_cursor, deltas) = diff(cursor, &messages);
        *cursor = new_cursor;

        for delta in deltas {
            match delta {
                Delta::Text { reasoning, delta } => {
                    if reasoning {
                        updates
                            .send(SessionUpdate::AgentThoughtChunk {
                                content: TextContent::text(delta),
                            })
                            .await;
                    } else {
                        updates
                            .send(SessionUpdate::AgentMessageChunk {
                                content: TextContent::text(delta),
                            })
                            .await;
                        // Mark that we've emitted at least one message chunk
                        emitted_message_chunk.store(true, Ordering::SeqCst);
                    }
                }
                Delta::ToolStarted {
                    tool_call_id,
                    tool_name,
                    input,
                } => {
                    // Build title: tool_name + ": " + command/path from input if present
                    let title = build_tool_title(&tool_name, &input);
                    let kind = tool_kind(&tool_name);

                    updates
                        .send(SessionUpdate::ToolCall {
                            tool_call_id,
                            title,
                            kind,
                            status: ToolCallStatus::InProgress,
                            raw_input: input,
                        })
                        .await;
                }
                Delta::ToolFinished {
                    tool_call_id,
                    output,
                } => {
                    updates
                        .send(SessionUpdate::ToolCallUpdate {
                            tool_call_id,
                            status: ToolCallStatus::Completed,
                            raw_output: Some(serde_json::json!({ "output": output })),
                            content: Some(vec![ToolCallContentItem::Content {
                                content: TextContent::text(output),
                            }]),
                        })
                        .await;
                }
            }
        }
        Ok(())
    }

    /// Check if screen shows KickedOut or child exited.
    async fn check_errors(&self, pty: &Pty) -> Result<()> {
        let snap = pty.screen();
        if classify(&snap.rows) == ScreenState::KickedOut {
            return Err(anyhow!("freebuff: another instance took over this account"));
        }

        if pty.try_wait()?.is_some() {
            return Err(anyhow!(
                "freebuff child exited unexpectedly; last screen:\n{}",
                snap.text()
            ));
        }
        Ok(())
    }

    /// Handle startup sequence after spawning freebuff.
    async fn run_startup(&self, pty: &Arc<Pty>, _cwd: &Path) -> Result<()> {
        let mut splash_accept_sent = false;

        let startup_deadline = Instant::now() + self.cfg.startup_timeout;

        loop {
            if Instant::now() >= startup_deadline {
                let snap = pty.screen();
                let pid = pty.pid().unwrap_or(0);
                return Err(anyhow!(
                    "startup timeout after {:?} pid={}; last screen:\n{}",
                    self.cfg.startup_timeout,
                    pid,
                    snap.text()
                ));
            }

            // Check if child exited
            if let Some(status) = pty.try_wait()? {
                let snap = pty.screen();
                return Err(anyhow!(
                    "freebuff exited during startup with status {}; last screen:\n{}",
                    status,
                    snap.text()
                ));
            }

            let snap = pty.screen();
            let state = classify(&snap.rows);

            // If we've already sent splash accept, check for Idle markers directly
            // (splash markers may linger in the buffer alongside idle markers)
            if splash_accept_sent {
                let has_time_left = snap
                    .rows
                    .iter()
                    .any(|r| r.contains("·") && (r.contains("h left") || r.contains("m left")));
                let has_placeholder = snap
                    .rows
                    .iter()
                    .any(|r| r.contains("Enter a coding task or / for commands"));
                if has_time_left && has_placeholder {
                    info!("startup: reached Idle (idle markers visible)");
                    return Ok(());
                }
            }

            match state {
                ScreenState::Booting => {
                    trace!("startup: booting");
                }
                ScreenState::ModelSplash => {
                    if !splash_accept_sent {
                        info!(
                            "startup: model splash detected, sending Enter to accept default model"
                        );
                        pty.write(SPLASH_ACCEPT_KEY.as_bytes()).await?;
                        splash_accept_sent = true;
                    } else {
                        trace!("startup: waiting after splash accept");
                    }
                }
                ScreenState::FreebucksGate { message } => {
                    info!("startup: freebucks gate detected");
                    let _ = pty.kill().await;
                    return Err(anyhow!("freebuff: not enough Freebucks — {}", message));
                }
                ScreenState::AlreadyRunning => {
                    info!("startup: already running dialog detected");
                    let _ = pty.kill().await;
                    let owner_pid = instance_owner(&self.manicode_dir())
                        .map(|(_, pid)| pid.to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    return Err(anyhow!(
                        "freebuff: already running (owner pid: {})",
                        owner_pid
                    ));
                }
                ScreenState::KickedOut => {
                    let _ = pty.kill().await;
                    return Err(anyhow!("freebuff: another instance took over this account"));
                }
                ScreenState::Idle => {
                    info!("startup: reached Idle");
                    return Ok(());
                }
                ScreenState::Busy { .. } => {
                    trace!("startup: unexpectedly busy");
                }
                ScreenState::Unknown => {
                    trace!("startup: unknown screen state");
                }
            }

            tokio::time::sleep(self.cfg.poll_interval).await;
        }
    }

    /// Submit a prompt to freebuff.
    async fn submit_prompt(&self, pty: &Arc<Pty>, text: &str) -> Result<()> {
        // Wait for Idle (max 10s)
        let idle_timeout = Duration::from_secs(10);
        if let Err(e) = self.wait_for_idle(pty, idle_timeout).await {
            return Err(anyhow!("freebuff is busy: {}", e));
        }

        // Submit via bracketed paste
        info!("prompt: pasting text ({} chars)", text.len());
        pty.paste(text).await?;

        // Wait for input box to show the text (first 20 chars) or a pasted chip
        let preview: String = text.chars().take(20).collect();
        match pty
            .wait_for(
                |s| {
                    let in_box = input_box_text(&s.rows)
                        .map(|t| t.contains(&preview))
                        .unwrap_or(false);
                    let chip = pasted_chip_chars(&s.rows).is_some();
                    in_box || chip
                },
                Duration::from_secs(5),
            )
            .await
        {
            Ok(snap) => {
                if pasted_chip_chars(&snap.rows).is_some() {
                    let count = pasted_chip_chars(&snap.rows).unwrap();
                    info!("prompt: pasted-text chip appeared ({} chars)", count);
                } else {
                    info!("prompt: text appeared in input box ({} chars)", text.len());
                }
            }
            Err(_) => {
                return Err(anyhow!(
                    "freebuff did not show the prompt text or a pasted-text chip in the input box within 5s; screen:\n{}",
                    pty.screen().text()
                ));
            }
        }

        // Send Enter
        info!("prompt: sending Enter");
        pty.key(Key::Enter).await?;

        // Wait for freebuff to go Busy, or for the paste to be consumed (input
        // box back to the placeholder and no pasted-text chip), up to submit_timeout.
        let submit_timeout = self.cfg.submit_timeout;
        let pred = move |s: &ScreenSnapshot| {
            matches!(classify(&s.rows), ScreenState::Busy { .. })
                || (!input_box_text(&s.rows)
                    .map(|t| t.contains(&preview))
                    .unwrap_or(false)
                    && pasted_chip_chars(&s.rows).is_none()
                    && input_box_is_empty(&s.rows))
        };

        // Short window for the TUI to consume the paste on its own; if the text
        // or chip is still visible after 2s, nudge with one more Enter.
        let pred_first = pred.clone();
        if pty
            .wait_for(move |s| pred_first(s), Duration::from_secs(2))
            .await
            .is_err()
        {
            let last = pty.screen();
            let text_still_visible = input_box_text(&last.rows)
                .map(|t| !t.is_empty() && !t.contains("Enter a coding task"))
                .unwrap_or(false);
            let chip_still_visible = pasted_chip_chars(&last.rows).is_some();
            if text_still_visible || chip_still_visible {
                info!("prompt: still not busy, sending Enter again");
                pty.key(Key::Enter).await?;
            }
        }
        let resolved = match pty
            .wait_for(move |s| pred(s), remainder_after_nudge(submit_timeout))
            .await
        {
            Ok(snap) => snap,
            Err(_) => {
                let snap = pty.screen();
                return Err(anyhow!(
                    "freebuff neither went busy nor consumed the prompt within {:?}; screen:\n{}",
                    submit_timeout,
                    snap.text()
                ));
            }
        };
        if matches!(classify(&resolved.rows), ScreenState::Busy { .. }) {
            info!("prompt: freebuff went busy");
        } else {
            info!("prompt: paste consumed; proceeding to the turn loop");
        }
        Ok(())
    }
}

impl Backend for FreebuffBackend {
    fn new_session(&self, cwd: PathBuf) -> impl Future<Output = Result<String>> + Send {
        let this = self;
        async move {
            // Check if session already exists
            if let Some(session) = this.session.lock().await.as_ref() {
                info!(
                    "new_session: returning existing session {}",
                    session.session_id
                );
                return Ok(session.session_id.clone());
            }

            let manicode_dir_opt = this.cfg.manicode_dir.as_deref();
            let chats_dir_path = chats_dir(&cwd, manicode_dir_opt);
            info!(cwd = %cwd.display(), chats_dir = %chats_dir_path.display(), "new_session: starting");

            // Snapshot chats dir before launch
            let initial_snapshot = snapshot(&chats_dir_path)?;

            // Spawn freebuff in PTY
            let pty_config = this.build_pty_config(&cwd);
            let pty = Arc::new(Pty::spawn(pty_config).await?);
            let child_pid = pty.pid().unwrap_or(0);

            // Run startup sequence
            if let Err(e) = this.run_startup(&pty, &cwd).await {
                let _ = pty.kill().await;
                return Err(e);
            }

            // Create session ID
            let session_id = format!(
                "blink-{}-{}",
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .expect("system time before epoch")
                    .as_millis(),
                child_pid
            );
            info!(session_id = %session_id, "new_session: session created");

            let session = Session {
                pty,
                chats_dir: chats_dir_path,
                initial_snapshot,
                chat_dir: Arc::new(Mutex::new(None)),
                transcript_cursor: Arc::new(Mutex::new(Cursor::new())),
                log_offset: Arc::new(Mutex::new(0)),
                cancelled: Arc::new(AtomicBool::new(false)),
                session_id: session_id.clone(),
                parse_failures: Arc::new(Mutex::new(0)),
                emitted_message_chunk: Arc::new(AtomicBool::new(false)),
            };

            *this.session.lock().await = Some(session);
            Ok(session_id)
        }
    }

    fn prompt(
        &self,
        session_id: &str,
        text: String,
        updates: UpdateSink,
        cancel: watch::Receiver<bool>,
    ) -> impl Future<Output = Result<StopReason>> + Send {
        let this = self;
        async move {
            // Get session and verify ID
            let session = {
                let guard = this.session.lock().await;
                guard
                    .as_ref()
                    .ok_or_else(|| anyhow!("no active session"))?
                    .clone_session_ref()
            };

            if session.session_id != session_id {
                return Err(anyhow!(
                    "session id mismatch: expected {}, got {}",
                    session.session_id,
                    session_id
                ));
            }

            info!(session_id = %session_id, "prompt: starting");
            session.emitted_message_chunk.store(false, Ordering::SeqCst);

            let pty = session.pty.clone();

            // Submit the prompt
            this.submit_prompt(&pty, &text).await?;

            // Locate chat dir if not already known
            let chat_dir = {
                let mut chat_dir_guard = session.chat_dir.lock().await;
                if chat_dir_guard.is_none() {
                    info!("prompt: locating new chat directory");
                    let deadline = Instant::now() + Duration::from_secs(30);
                    loop {
                        if let Some(dir) =
                            newest_new_chat(&session.chats_dir, &session.initial_snapshot)?
                        {
                            info!(chat_dir = %dir.display(), "prompt: found chat dir");
                            *chat_dir_guard = Some(dir.clone());
                            break dir;
                        }
                        if Instant::now() >= deadline {
                            return Err(anyhow!("timeout waiting for chat directory to appear"));
                        }
                        tokio::time::sleep(this.cfg.poll_interval).await;
                    }
                } else {
                    chat_dir_guard.as_ref().unwrap().clone()
                }
            };

            // Turn loop
            let turn_deadline = Instant::now() + this.cfg.turn_timeout;
            let mut cancelled_sent = false;
            let mut turn_events = Vec::new();

            loop {
                if Instant::now() >= turn_deadline {
                    return Err(anyhow!("turn timeout after {:?}", this.cfg.turn_timeout));
                }

                // Check for cancel
                if *cancel.borrow() && !cancelled_sent {
                    info!("prompt: cancel requested, sending Esc");
                    pty.write(CANCEL_KEY.as_bytes()).await?;
                    session.cancelled.store(true, Ordering::SeqCst);
                    cancelled_sent = true;
                }

                // Check for errors (kicked out, child exited)
                this.check_errors(&pty).await?;

                // Read log events for this turn
                {
                    let mut offset_guard = session.log_offset.lock().await;
                    let new_events = this.read_log_events(&chat_dir, &mut offset_guard).await?;
                    turn_events.extend(new_events);
                }

                // Check turn outcome from log events
                if let Some(turn_outcome) = outcome(&turn_events) {
                    // Wait for transcript to settle before final flush
                    let settle_start = Instant::now();
                    while settle_start.elapsed() < this.cfg.settle_timeout {
                        let transcript_path = chat_dir.join(CHAT_MESSAGES);
                        if let Ok(content) = fs::read_to_string(&transcript_path).await {
                            if let Ok(messages) = parse_messages(&content) {
                                if let Some(Message::Ai {
                                    blocks,
                                    is_complete,
                                    ..
                                }) = messages.last()
                                {
                                    let has_content = blocks.iter().any(|b| {
                                        matches!(b, Block::Text { text_type, .. } if text_type != "reasoning")
                                            || matches!(b, Block::Tool { .. })
                                    });
                                    match turn_outcome {
                                        TurnOutcome::Completed if *is_complete && has_content => {
                                            break
                                        }
                                        TurnOutcome::Interrupted if !blocks.is_empty() => break,
                                        _ => {}
                                    }
                                }
                            }
                        }
                        tokio::time::sleep(this.cfg.poll_interval).await;
                    }
                    let settle_elapsed = settle_start.elapsed();
                    info!(
                        elapsed_ms = settle_elapsed.as_millis(),
                        "transcript settled"
                    );

                    // Flush one last transcript diff (final flush)
                    {
                        let mut cursor_guard = session.transcript_cursor.lock().await;
                        this.process_transcript_deltas(
                            &chat_dir,
                            &mut cursor_guard,
                            &updates,
                            &session.parse_failures,
                            &session.emitted_message_chunk,
                            true, // is_final_flush
                        )
                        .await?;
                    }

                    match turn_outcome {
                        TurnOutcome::Completed => {
                            info!("prompt: turn completed");
                            // Check if any AgentMessageChunk was emitted during this turn
                            if !session.emitted_message_chunk.load(Ordering::SeqCst) {
                                return Err(anyhow!(
                                    "freebuff finished the turn but no reply text was found in the transcript"
                                ));
                            }
                            // Reset for next turn
                            session.emitted_message_chunk.store(false, Ordering::SeqCst);
                            return Ok(if cancelled_sent {
                                StopReason::Cancelled
                            } else {
                                StopReason::EndTurn
                            });
                        }
                        TurnOutcome::Interrupted => {
                            info!("prompt: turn interrupted");
                            // Reset for next turn
                            session.emitted_message_chunk.store(false, Ordering::SeqCst);
                            return Ok(StopReason::Cancelled);
                        }
                        TurnOutcome::Error { message } => {
                            return Err(anyhow!("freebuff turn failed: {}", message));
                        }
                    }
                }

                // Process transcript deltas (regular poll)
                {
                    let mut cursor_guard = session.transcript_cursor.lock().await;
                    this.process_transcript_deltas(
                        &chat_dir,
                        &mut cursor_guard,
                        &updates,
                        &session.parse_failures,
                        &session.emitted_message_chunk,
                        false, // not final flush
                    )
                    .await?;
                }

                tokio::time::sleep(this.cfg.poll_interval).await;
            }
        }
    }

    fn shutdown(&self) -> impl Future<Output = ()> + Send {
        let this = self;
        async move {
            let session = {
                let mut guard = this.session.lock().await;
                guard.take()
            };

            let Some(session) = session else {
                info!("shutdown: no session to shut down");
                return;
            };

            info!(session_id = %session.session_id, "shutdown: starting");
            let pty = session.pty;

            // If busy, send Esc and wait up to 2s
            let snap = pty.screen();
            if matches!(classify(&snap.rows), ScreenState::Busy { .. }) {
                info!("shutdown: busy, sending Esc");
                let _ = pty.write(CANCEL_KEY.as_bytes()).await;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }

            // Send /exit and Enter
            info!("shutdown: sending /exit");
            let _ = pty.type_text(EXIT_COMMAND).await;
            let _ = pty.key(Key::Enter).await;

            // Wait for exit, then always kill the whole process group.
            // `pty.kill()` is a no-op once the child is reaped, but the
            // launcher forwards no signals to the real binary, so a clean
            // exit of the launcher script can leave an orphan grandchild
            // alive and holding the single-instance lock.
            match pty.wait_exit(this.cfg.exit_timeout).await {
                Ok(Some(status)) => {
                    info!(?status, "shutdown: child exited cleanly");
                }
                Ok(None) => {
                    warn!(
                        "shutdown: child did not exit within {:?}, killing",
                        this.cfg.exit_timeout
                    );
                }
                Err(e) => {
                    error!(error = %e, "shutdown: error waiting for exit");
                }
            }
            let _ = pty.kill().await;

            info!("shutdown: complete");
        }
    }
}

// Helper to allow cloning a reference to the session for the prompt future
impl Session {
    fn clone_session_ref(&self) -> SessionRef {
        SessionRef {
            pty: self.pty.clone(),
            chats_dir: self.chats_dir.clone(),
            initial_snapshot: self.initial_snapshot.clone(),
            chat_dir: self.chat_dir.clone(),
            transcript_cursor: self.transcript_cursor.clone(),
            log_offset: self.log_offset.clone(),
            cancelled: self.cancelled.clone(),
            session_id: self.session_id.clone(),
            parse_failures: self.parse_failures.clone(),
            emitted_message_chunk: self.emitted_message_chunk.clone(),
        }
    }
}

/// A reference-counted handle to session internals for use in prompt future.
#[derive(Clone)]
struct SessionRef {
    pty: Arc<Pty>,
    chats_dir: PathBuf,
    initial_snapshot: std::collections::BTreeSet<String>,
    chat_dir: Arc<Mutex<Option<PathBuf>>>,
    transcript_cursor: Arc<Mutex<Cursor>>,
    log_offset: Arc<Mutex<u64>>,
    cancelled: Arc<AtomicBool>,
    session_id: String,
    parse_failures: Arc<Mutex<u32>>,
    emitted_message_chunk: Arc<AtomicBool>,
}

/// Build tool title: tool_name + ": " + command/path from input
fn build_tool_title(tool_name: &str, input: &Value) -> String {
    let extra = input
        .get("command")
        .and_then(|v| v.as_str())
        .or_else(|| input.get("path").and_then(|v| v.as_str()))
        .unwrap_or("");
    if extra.is_empty() {
        tool_name.to_string()
    } else {
        format!("{}: {}", tool_name, extra)
    }
}

/// Map tool_name to kind
fn tool_kind(tool_name: &str) -> String {
    match tool_name {
        "run_terminal_command" => "execute".to_string(),
        "write_file" | "str_replace" | "create_file" => "edit".to_string(),
        "read_files" | "read_file" | "code_search" | "find_files" => "read".to_string(),
        _ => "other".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use tokio::time::timeout;

    /// Get the path to the fake-freebuff script.
    fn fake_freebuff_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("fake-freebuff")
            .join("fake-freebuff.sh")
    }

    /// RAII guard that removes its directory on drop.
    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Create a unique temp directory for testing. Returns an RAII guard
    /// that removes the directory when dropped, so driver tests never leak.
    fn test_temp_dir() -> TempDir {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "bufflink-driver-{}-{}-{}",
            std::process::id(),
            nanos,
            id
        ));
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    /// Create a DriverConfig pointed at the fake binary with a temp manicode dir.
    fn test_config(manicode_dir: &Path, mode: Option<&str>) -> DriverConfig {
        let mut cfg = DriverConfig::from_env();
        cfg.program = fake_freebuff_path().to_string_lossy().to_string();
        cfg.manicode_dir = Some(manicode_dir.to_path_buf());
        cfg.env = vec![(
            "BLINK_MANICODE_DIR".to_string(),
            manicode_dir.to_string_lossy().to_string(),
        )];
        if let Some(m) = mode {
            cfg.env
                .push(("FAKE_FREEBUFF_MODE".to_string(), m.to_string()));
        }
        cfg.startup_timeout = Duration::from_secs(10);
        cfg.turn_timeout = Duration::from_secs(30);
        cfg.poll_interval = Duration::from_millis(100);
        cfg.exit_timeout = Duration::from_secs(2);
        cfg.submit_timeout = Duration::from_secs(10);
        cfg
    }

    /// Helper to run a prompt and collect updates.
    async fn run_prompt_collect(
        backend: &FreebuffBackend,
        session_id: &str,
        text: &str,
    ) -> (Result<StopReason>, Vec<SessionUpdate>) {
        let (tx, mut rx) = mpsc::channel(64);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let sink = UpdateSink::new(tx);

        let prompt_fut = backend.prompt(session_id, text.to_string(), sink, cancel_rx);
        tokio::pin!(prompt_fut);

        let mut updates = Vec::new();
        let outcome;

        loop {
            tokio::select! {
                biased;
                Some(u) = rx.recv() => {
                    updates.push(u);
                }
                result = &mut prompt_fut => {
                    outcome = Some(result);
                    break;
                }
            }
        }

        // Drain any remaining updates after prompt completes
        while let Ok(u) = rx.try_recv() {
            updates.push(u);
        }

        (outcome.expect("prompt completed"), updates)
    }

    #[tokio::test]
    async fn t1_new_session_reaches_idle_and_returns_id() {
        let tmp = test_temp_dir();
        let temp_dir = tmp.0.clone();
        let cfg = test_config(&temp_dir, None);
        let backend = FreebuffBackend::new(cfg);

        // First new_session
        let id1 = backend.new_session(temp_dir.clone()).await.unwrap();
        assert!(id1.starts_with("blink-"));
        assert!(id1.contains("-"));

        // Second new_session should return the same ID
        let id2 = backend.new_session(temp_dir.clone()).await.unwrap();
        assert_eq!(id1, id2);

        // Cleanup
        backend.shutdown().await;
    }

    #[tokio::test]
    async fn t2_prompt_reply_pong_streams_updates_and_returns_endturn() {
        let tmp = test_temp_dir();
        let temp_dir = tmp.0.clone();
        let cfg = test_config(&temp_dir, None);
        let backend = FreebuffBackend::new(cfg);

        let session_id = backend.new_session(temp_dir.clone()).await.unwrap();

        let (result, updates) = run_prompt_collect(&backend, &session_id, "Reply PONG").await;

        assert!(result.is_ok(), "prompt failed: {:?}", result.err());
        assert_eq!(result.unwrap(), StopReason::EndTurn);

        // Check update sequence: AgentThoughtChunk, ToolCall, ToolCallUpdate, AgentMessageChunk with PONG
        assert!(!updates.is_empty(), "no updates streamed");

        let mut saw_thought = false;
        let mut saw_tool_call = false;
        let mut saw_tool_update = false;
        let mut saw_message_with_pong = false;

        for u in &updates {
            match u {
                SessionUpdate::AgentThoughtChunk { .. } => saw_thought = true,
                SessionUpdate::ToolCall {
                    tool_call_id,
                    title,
                    kind,
                    status,
                    ..
                } => {
                    assert_eq!(*status, ToolCallStatus::InProgress);
                    assert!(!tool_call_id.is_empty());
                    assert!(title.contains("run_terminal_command"));
                    assert_eq!(*kind, "execute");
                    saw_tool_call = true;
                }
                SessionUpdate::ToolCallUpdate {
                    tool_call_id,
                    status,
                    ..
                } => {
                    assert_eq!(*status, ToolCallStatus::Completed);
                    assert!(!tool_call_id.is_empty());
                    saw_tool_update = true;
                }
                SessionUpdate::AgentMessageChunk { content } => {
                    let TextContent::Text { text } = content;
                    if text.contains("PONG") {
                        saw_message_with_pong = true;
                    }
                }
            }
        }

        assert!(saw_thought, "missing AgentThoughtChunk");
        assert!(saw_tool_call, "missing ToolCall");
        assert!(saw_tool_update, "missing ToolCallUpdate");
        assert!(saw_message_with_pong, "missing AgentMessageChunk with PONG");

        backend.shutdown().await;
    }

    #[tokio::test]
    async fn t3_prompt_slow_count_with_cancel_returns_cancelled() {
        let tmp = test_temp_dir();
        let temp_dir = tmp.0.clone();
        let cfg = test_config(&temp_dir, None);
        let backend = FreebuffBackend::new(cfg);

        let session_id = backend.new_session(temp_dir.clone()).await.unwrap();

        let (tx, _rx) = mpsc::channel(64);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let sink = UpdateSink::new(tx);

        let prompt_fut = backend.prompt(&session_id, "slow count".to_string(), sink, cancel_rx);
        tokio::pin!(prompt_fut);

        // Wait a bit then flip cancel
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel_tx.send(true).unwrap();

        // Wait for completion with timeout
        let result = timeout(Duration::from_secs(5), prompt_fut).await;
        assert!(result.is_ok(), "prompt timed out");
        let result = result.unwrap();
        assert!(result.is_ok(), "prompt error: {:?}", result.err());
        assert_eq!(result.unwrap(), StopReason::Cancelled);

        backend.shutdown().await;
    }

    #[tokio::test]
    async fn t4_shutdown_makes_child_exit() {
        let tmp = test_temp_dir();
        let temp_dir = tmp.0.clone();
        let cfg = test_config(&temp_dir, None);
        let backend = FreebuffBackend::new(cfg);

        let session_id = backend.new_session(temp_dir.clone()).await.unwrap();
        assert!(!session_id.is_empty());

        // Get the session to check child status
        let session_guard = backend.session.lock().await;
        let session = session_guard.as_ref().unwrap();
        let pty = session.pty.clone();
        drop(session_guard);

        // Shutdown
        let shutdown_fut = backend.shutdown();
        let result = timeout(Duration::from_secs(4), shutdown_fut).await;
        assert!(result.is_ok(), "shutdown timed out");

        // Verify child exited
        let exit_status = pty.try_wait().unwrap();
        assert!(exit_status.is_some(), "child did not exit");
    }

    #[tokio::test]
    async fn t5_fake_freebuff_mode_gate_errors_with_freebucks() {
        let tmp = test_temp_dir();
        let temp_dir = tmp.0.clone();
        let cfg = test_config(&temp_dir, Some("gate"));
        let backend = FreebuffBackend::new(cfg);

        let result = backend.new_session(temp_dir.clone()).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Freebucks"),
            "error should mention Freebucks: {}",
            err
        );
    }

    #[tokio::test]
    async fn t6_fake_freebuff_mode_running_errors_with_already_running() {
        let tmp = test_temp_dir();
        let temp_dir = tmp.0.clone();
        let cfg = test_config(&temp_dir, Some("running"));
        let backend = FreebuffBackend::new(cfg);

        let result = backend.new_session(temp_dir.clone()).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("already running"),
            "error should mention already running: {}",
            err
        );
    }

    #[tokio::test]
    async fn t7_fake_freebuff_mode_unparsable_errors_with_parse_failure() {
        let tmp = test_temp_dir();
        let temp_dir = tmp.0.clone();
        let cfg = test_config(&temp_dir, Some("unparsable"));
        let backend = FreebuffBackend::new(cfg);

        let session_id = backend.new_session(temp_dir.clone()).await.unwrap();

        let (result, _updates) = run_prompt_collect(&backend, &session_id, "test").await;

        assert!(
            result.is_err(),
            "prompt should fail with unparsable transcript"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("freebuff transcript could not be parsed"),
            "error should mention transcript parse failure: {}",
            err
        );

        backend.shutdown().await;
    }

    #[tokio::test]
    async fn t8_late_transcript_is_still_streamed() {
        let tmp = test_temp_dir();
        let temp_dir = tmp.0.clone();
        let cfg = test_config(&temp_dir, Some("late-transcript"));
        let backend = FreebuffBackend::new(cfg);

        let session_id = backend.new_session(temp_dir.clone()).await.unwrap();

        let (result, updates) = run_prompt_collect(&backend, &session_id, "Reply PONG").await;

        assert!(result.is_ok(), "prompt failed: {:?}", result.err());
        assert_eq!(result.unwrap(), StopReason::EndTurn);

        // Check update sequence: AgentThoughtChunk, ToolCall, ToolCallUpdate, AgentMessageChunk with PONG
        assert!(!updates.is_empty(), "no updates streamed");

        let mut saw_thought = false;
        let mut saw_tool_call = false;
        let mut saw_tool_update = false;
        let mut saw_message_with_pong = false;

        for u in &updates {
            match u {
                SessionUpdate::AgentThoughtChunk { .. } => saw_thought = true,
                SessionUpdate::ToolCall {
                    tool_call_id,
                    title,
                    kind,
                    status,
                    ..
                } => {
                    assert_eq!(*status, ToolCallStatus::InProgress);
                    assert!(!tool_call_id.is_empty());
                    assert!(title.contains("run_terminal_command"));
                    assert_eq!(*kind, "execute");
                    saw_tool_call = true;
                }
                SessionUpdate::ToolCallUpdate {
                    tool_call_id,
                    status,
                    ..
                } => {
                    assert_eq!(*status, ToolCallStatus::Completed);
                    assert!(!tool_call_id.is_empty());
                    saw_tool_update = true;
                }
                SessionUpdate::AgentMessageChunk { content } => {
                    let TextContent::Text { text } = content;
                    if text.contains("PONG") {
                        saw_message_with_pong = true;
                    }
                }
            }
        }

        assert!(saw_thought, "missing AgentThoughtChunk");
        assert!(saw_tool_call, "missing ToolCall");
        assert!(saw_tool_update, "missing ToolCallUpdate");
        assert!(saw_message_with_pong, "missing AgentMessageChunk with PONG");

        backend.shutdown().await;
    }

    /// freebuff's launcher spawns the real Bun binary as a grandchild and
    /// forwards no signals to it. When the launcher script exits on `/exit`,
    /// the orphan grandchild survives and keeps the single-instance lock, so
    /// `shutdown()` must kill the whole process group even on a clean exit.
    #[tokio::test]
    async fn t9_shutdown_kills_grandchild() {
        let tmp = test_temp_dir();
        let temp_dir = tmp.0.clone();
        let cfg = test_config(&temp_dir, Some("grandchild"));
        let backend = FreebuffBackend::new(cfg);

        let _session_id = backend.new_session(temp_dir.clone()).await.unwrap();

        // Capture the child pid before shutdown takes the session.
        let child_pid = {
            let guard = backend.session.lock().await;
            guard.as_ref().unwrap().pty.pid().unwrap_or(0)
        };
        assert!(child_pid > 0, "child pid should be available");

        // Shutdown: the fake exits cleanly on /exit, but must still kill the
        // orphan grandchild (`sh -c 'exec sleep 60' &`).
        let shutdown_fut = backend.shutdown();
        let result = timeout(Duration::from_secs(5), shutdown_fut).await;
        assert!(result.is_ok(), "shutdown timed out");

        // The grandchild must be gone. `pgrep -g <pid>` matches only the
        // child's process group, which is exact.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let out = std::process::Command::new("pgrep")
                .args(["-g", &child_pid.to_string()])
                .output()
                .expect("pgrep should run");
            if out.status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout);
                panic!(
                    "grandchild survived shutdown; pgrep -g {} returned:\n{}",
                    child_pid, stdout
                );
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn t10_startup_timeout_kills_child() {
        let tmp = test_temp_dir();
        let temp_dir = tmp.0.clone();
        let mut cfg = test_config(&temp_dir, Some("hang-splash"));
        cfg.startup_timeout = Duration::from_secs(1);
        let backend = FreebuffBackend::new(cfg);

        let result = backend.new_session(temp_dir.clone()).await;
        assert!(
            result.is_err(),
            "new_session should fail on startup timeout"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("startup timeout"),
            "error should mention timeout: {}",
            err
        );
        // Extract pid from error message: "pid=<n>"
        let pid_str = err.split("pid=").nth(1).and_then(|s| {
            s.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse::<u32>()
                .ok()
        });
        assert!(pid_str.is_some(), "error should contain pid=<n>: {}", err);
        let pid = pid_str.unwrap();

        // Wait briefly for the process group to be reaped.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let out = std::process::Command::new("pgrep")
                .args(["-g", &pid.to_string()])
                .output()
                .expect("pgrep should run");
            if !out.status.success() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("child pid {} still alive after startup timeout kill", pid);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    #[tokio::test]
    async fn t11_long_prompt_is_submitted_via_paste_chip() {
        let tmp = test_temp_dir();
        let temp_dir = tmp.0.clone();
        let cfg = test_config(&temp_dir, Some("chip"));
        let backend = FreebuffBackend::new(cfg);

        let session_id = backend.new_session(temp_dir.clone()).await.unwrap();

        // Build a 1,500-char prompt by repeating a sentence (no "slow").
        let sentence = "A quick brown fox jumps. ";
        let prompt = sentence.repeat(60);
        assert_eq!(prompt.len(), 1500, "prompt must be exactly 1500 chars");
        assert!(!prompt.contains("slow"), "prompt must not contain 'slow'");

        let (result, updates) = run_prompt_collect(&backend, &session_id, &prompt).await;

        assert!(result.is_ok(), "prompt failed: {:?}", result.err());
        assert_eq!(result.unwrap(), StopReason::EndTurn);

        let mut saw_thought = false;
        let mut saw_tool_call = false;
        let mut saw_tool_update = false;
        let mut saw_message_with_pong = false;

        for u in &updates {
            match u {
                SessionUpdate::AgentThoughtChunk { .. } => saw_thought = true,
                SessionUpdate::ToolCall {
                    tool_call_id,
                    title,
                    kind,
                    status,
                    ..
                } => {
                    assert_eq!(*status, ToolCallStatus::InProgress);
                    assert!(!tool_call_id.is_empty());
                    assert!(title.contains("run_terminal_command"));
                    assert_eq!(*kind, "execute");
                    saw_tool_call = true;
                }
                SessionUpdate::ToolCallUpdate {
                    tool_call_id,
                    status,
                    ..
                } => {
                    assert_eq!(*status, ToolCallStatus::Completed);
                    assert!(!tool_call_id.is_empty());
                    saw_tool_update = true;
                }
                SessionUpdate::AgentMessageChunk { content } => {
                    let TextContent::Text { text } = content;
                    if text.contains("PONG") {
                        saw_message_with_pong = true;
                    }
                }
            }
        }

        assert!(saw_thought, "missing AgentThoughtChunk");
        assert!(saw_tool_call, "missing ToolCall");
        assert!(saw_tool_update, "missing ToolCallUpdate");
        assert!(saw_message_with_pong, "missing AgentMessageChunk with PONG");

        backend.shutdown().await;
    }

    #[tokio::test]
    async fn t12_slow_busy_prompt_still_completes() {
        // A prompt in slow-busy mode: the fake shows the placeholder box (the
        // paste is 'consumed') for 6 s before going busy. The prompt must still
        // complete EndTurn, passing through the 'paste consumed' signal.
        let tmp = test_temp_dir();
        let temp_dir = tmp.0.clone();
        let cfg = test_config(&temp_dir, Some("slow-busy"));
        let backend = FreebuffBackend::new(cfg);

        let session_id = backend.new_session(temp_dir.clone()).await.unwrap();

        let (result, updates) = run_prompt_collect(&backend, &session_id, "Reply PONG").await;

        assert!(result.is_ok(), "prompt failed: {:?}", result.err());
        assert_eq!(result.unwrap(), StopReason::EndTurn);

        // Check update sequence: AgentThoughtChunk, ToolCall, ToolCallUpdate, AgentMessageChunk with PONG
        assert!(!updates.is_empty(), "no updates streamed");

        let mut saw_thought = false;
        let mut saw_tool_call = false;
        let mut saw_tool_update = false;
        let mut saw_message_with_pong = false;

        for u in &updates {
            match u {
                SessionUpdate::AgentThoughtChunk { .. } => saw_thought = true,
                SessionUpdate::ToolCall {
                    tool_call_id,
                    title,
                    kind,
                    status,
                    ..
                } => {
                    assert_eq!(*status, ToolCallStatus::InProgress);
                    assert!(!tool_call_id.is_empty());
                    assert!(title.contains("run_terminal_command"));
                    assert_eq!(*kind, "execute");
                    saw_tool_call = true;
                }
                SessionUpdate::ToolCallUpdate {
                    tool_call_id,
                    status,
                    ..
                } => {
                    assert_eq!(*status, ToolCallStatus::Completed);
                    assert!(!tool_call_id.is_empty());
                    saw_tool_update = true;
                }
                SessionUpdate::AgentMessageChunk { content } => {
                    let TextContent::Text { text } = content;
                    if text.contains("PONG") {
                        saw_message_with_pong = true;
                    }
                }
            }
        }

        assert!(saw_thought, "missing AgentThoughtChunk");
        assert!(saw_tool_call, "missing ToolCall");
        assert!(saw_tool_update, "missing ToolCallUpdate");
        assert!(saw_message_with_pong, "missing AgentMessageChunk with PONG");

        backend.shutdown().await;
    }

    #[test]
    fn submit_timeout_floor_is_two_seconds() {
        // A programmatically built config with a 1 s submit_timeout must never
        // underflow the 2 s nudge remainder; it saturates to zero.
        assert_eq!(
            remainder_after_nudge(Duration::from_secs(1)),
            Duration::ZERO
        );

        // Env parsing floors any value below 2 s up to 2 s (and defaults to 30).
        assert_eq!(parse_submit_timeout(Some("1")), Duration::from_secs(2));
        assert_eq!(parse_submit_timeout(Some("0")), Duration::from_secs(2));
        assert_eq!(parse_submit_timeout(Some("30")), Duration::from_secs(30));
        assert_eq!(parse_submit_timeout(None), Duration::from_secs(30));
    }
}
