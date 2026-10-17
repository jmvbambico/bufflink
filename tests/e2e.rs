use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::{Command as StdCommand, ExitStatus};
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time;

pub struct AcpClient {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    recent: VecDeque<String>,
    pub workdir: PathBuf,
}

impl Drop for AcpClient {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.start_kill();
            // We own the only freebuff on a dev box, but don't kill
            // unrelated processes by default — only when explicitly asked.
            if std::env::var("BLINK_E2E_CLEANUP").as_deref() == Ok("1") {
                let _ = std::process::Command::new("pkill")
                    .args(["-TERM", "-f", "manicode/freebuff"])
                    .status();
            }
        }
    }
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// The model the e2e run pins through bufflink's normal `BLINK_MODEL` ->
/// `settings.json` path. freebuff otherwise picks its own default (currently
/// `MiMo Fast`, which bills 2 hours/hour), and the test account has 1 hour, so
/// without an explicit pin the run can never pass. `z-ai/glm-5.3-flash` (GLM
/// 5.3 Flash) is the cheapest commonly-available model and the one the user's
/// own settings.json already selects — pinning it here makes the run
/// deterministic AND exercises the settings-pin path.
const E2E_PINNED_MODEL: &str = "z-ai/glm-5.3-flash";

/// Path to freebuff's live `settings.json` — the file a `BLINK_MODEL` pin
/// rewrites and restores. Mirrors `chats::manicode_dir()`: `$BLINK_MANICODE_DIR`
/// when set, else `$HOME/.config/manicode`.
fn manicode_settings_path() -> PathBuf {
    let base = match std::env::var("BLINK_MANICODE_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config").join("manicode")
        }
    };
    base.join("settings.json")
}

impl AcpClient {
    pub async fn spawn() -> Result<Self> {
        let workdir = std::env::temp_dir().join(format!(
            "blink-e2e-{}-{}",
            std::process::id(),
            unix_millis()
        ));
        std::fs::create_dir_all(&workdir)?;
        let _ = StdCommand::new("git")
            .args(["init", "-q"])
            .current_dir(&workdir)
            .status();

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blink"));
        cmd.current_dir(&workdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        match std::env::var("RUST_LOG") {
            Ok(log) => cmd.env("RUST_LOG", log),
            Err(_) => cmd.env("RUST_LOG", "info"),
        };
        // Pin an inexpensive model through bufflink's normal BLINK_MODEL ->
        // settings.json path so the run is deterministic (freebuff's default
        // model costs more hours than the test account has) and the settings
        // pin is exercised.
        cmd.env("BLINK_MODEL", E2E_PINNED_MODEL);
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 0,
            recent: VecDeque::with_capacity(20),
            workdir,
        })
    }

    pub async fn request(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string();
        let stdin = self.stdin.as_mut().ok_or_else(|| anyhow!("stdin closed"))?;
        stdin.write_all(msg.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(id)
    }

    pub async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = json!({"jsonrpc":"2.0","method":method,"params":params}).to_string();
        let stdin = self.stdin.as_mut().ok_or_else(|| anyhow!("stdin closed"))?;
        stdin.write_all(msg.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    pub async fn next_message(&mut self, timeout: Duration) -> Result<Value> {
        match self.next_message_within(timeout).await? {
            Some(msg) => Ok(msg),
            None => Err(anyhow!(
                "timeout; recent: {}",
                self.recent.iter().cloned().collect::<Vec<_>>().join(" | ")
            )),
        }
    }

    /// Read the next non-empty JSON line, waiting at most `timeout`. Returns
    /// `Ok(None)` when the timeout elapses first, so a caller can poll the
    /// stream mid-turn without treating a quiet period as a hard error.
    pub async fn next_message_within(&mut self, timeout: Duration) -> Result<Option<Value>> {
        let deadline = std::time::Instant::now() + timeout;
        let mut buf = String::new();
        loop {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .unwrap_or(Duration::ZERO);
            let result = time::timeout(remaining, self.stdout.read_line(&mut buf)).await;
            match result {
                Ok(Ok(0)) => return Err(anyhow!("blink closed stdout")),
                Ok(Ok(_)) => {
                    let line = buf.trim_end().to_string();
                    buf.clear();
                    if line.is_empty() {
                        continue;
                    }
                    self.recent.push_back(line.clone());
                    if self.recent.len() > 20 {
                        self.recent.pop_front();
                    }
                    return serde_json::from_str(&line)
                        .map(Some)
                        .map_err(|e| anyhow!("parse JSON: {}", e));
                }
                Ok(Err(e)) => return Err(anyhow!("read error: {}", e)),
                Err(_) => return Ok(None),
            }
        }
    }

    pub async fn call(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<(Value, Vec<Value>)> {
        let id = self.request(method, params).await?;
        let mut updates = Vec::new();
        loop {
            let msg = self.next_message(timeout).await?;
            if msg.get("id").and_then(|v| v.as_u64()) == Some(id) {
                return Ok((msg, updates));
            }
            if msg.get("method").and_then(|v| v.as_str()) == Some("session/update") {
                updates.push(msg.clone());
            }
        }
    }

    pub async fn close_stdin(&mut self) {
        self.stdin.take();
    }

    pub async fn wait_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        time::timeout(timeout, self.child.wait())
            .await
            .ok()
            .and_then(|r| r.ok())
    }

    pub fn freebuff_processes() -> String {
        freebuff_processes()
    }
}

fn freebuff_processes() -> String {
    match StdCommand::new("pgrep")
        .args(["-f", "manicode/freebuff"])
        .output()
    {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).to_string()
        }
        _ => String::new(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn blink_initialize_without_session_does_not_spawn_freebuff() {
    let before = freebuff_processes();
    let mut client = AcpClient::spawn().await.unwrap();
    let workdir = client.workdir.clone();
    let (resp, _updates) = client
        .call(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": {"readTextFile": true, "writeTextFile": true},
                    "terminal": false
                },
                "clientInfo": {"name": "e2e", "version": "0"}
            }),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert_eq!(resp["result"]["protocolVersion"], 1);
    assert_eq!(resp["result"]["agentInfo"]["name"], "blink");
    let after = freebuff_processes();
    assert_eq!(after, before, "pgrep set changed after initialize");
    client.close_stdin().await;
    let status = client.wait_exit(Duration::from_secs(5)).await;
    assert!(status.is_some());
    assert!(status.unwrap().success());
    let after_exit = freebuff_processes();
    assert_eq!(after_exit, before, "pgrep set changed after exit");
    let _ = std::fs::remove_dir_all(&workdir);
}

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn e2e_single_launch_pong_cancel_exit() {
    let t0 = std::time::Instant::now();

    eprintln!("[e2e +{}ms] start", t0.elapsed().as_millis());
    if std::env::var("BUFFLINK_E2E").as_deref() != Ok("1") {
        eprintln!("skipping: BUFFLINK_E2E!=1");
        return;
    }

    // The BLINK_MODEL pin rewrites the user's live settings.json and restores
    // the original on shutdown. The user runs freebuff interactively, so this
    // test must leave that file byte-identical: capture it before anything
    // spawns and compare after the child has exited.
    let settings_path = manicode_settings_path();
    let settings_before = std::fs::read(&settings_path).ok();

    eprintln!("[e2e +{}ms] spawn", t0.elapsed().as_millis());
    let mut c = AcpClient::spawn().await.unwrap();

    eprintln!("[e2e +{}ms] initialize", t0.elapsed().as_millis());
    let (resp, _updates) = c
        .call(
            "initialize",
            json!({"protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":true,"writeTextFile":true},"terminal":false},"clientInfo":{"name":"e2e","version":"0"}}),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert_eq!(resp["result"]["protocolVersion"], 1);
    assert_eq!(resp["result"]["agentInfo"]["name"], "blink");

    eprintln!("[e2e +{}ms] session/new", t0.elapsed().as_millis());
    let (resp, _updates) = c
        .call(
            "session/new",
            json!({"cwd": c.workdir.to_string_lossy(), "mcpServers": []}),
            Duration::from_secs(120),
        )
        .await
        .unwrap();
    if resp.get("error").is_some() {
        panic!("session/new failed: {}", resp["error"]);
    }
    let sid = resp["result"]["sessionId"].as_str().unwrap().to_string();
    assert!(!sid.is_empty());

    eprintln!("[e2e +{}ms] prompt pong", t0.elapsed().as_millis());
    let (resp, updates) = c
        .call(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type":"text","text":"Reply with exactly the single word PONG and nothing else."}]}),
            Duration::from_secs(180),
        )
        .await
        .unwrap();
    assert_eq!(
        resp["result"]["stopReason"],
        "end_turn",
        "unexpected prompt response: {}",
        serde_json::to_string_pretty(&resp).unwrap()
    );
    let text = message_text(&updates);
    assert!(text.contains("PONG"));
    for update in &updates {
        assert_eq!(update["params"]["sessionId"].as_str(), Some(sid.as_str()));
    }
    eprintln!(
        "[e2e +{}ms] received {} notifications",
        t0.elapsed().as_millis(),
        updates.len()
    );

    eprintln!("[e2e +{}ms] prompt cancel", t0.elapsed().as_millis());
    let id = c
        .request(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type":"text","text":"Count from 1 to 400, one number per line, with a short comment after each."}]}),
        )
        .await
        .unwrap();
    // Trigger the cancel on ELAPSED TIME, not on a `session/update`. freebuff
    // writes `chat-messages.json` once, at the END of the turn (see
    // docs/research/freebuff-tui.md), so no update is emitted while it is
    // thinking. Waiting for one blocks until the turn has already finished,
    // and the cancel then lands after the fact: the turn reports `end_turn`,
    // never `cancelled`. The fake streams updates mid-turn, which is exactly
    // why only the real run failed. Do NOT "helpfully" change this back to
    // waiting for an update — against real freebuff there will never be one.
    // freebuff goes busy ~0.7-2.0s after Enter, a turn takes 8-14s and cancel
    // takes effect in <2s, so 3s lands safely mid-turn. While we wait we keep
    // draining the stream and fail loudly if the turn manages to finish first,
    // preserving the intent of the old `turn finished before we could cancel`
    // guard rather than silently cancelling a completed turn.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        if remaining.is_zero() {
            break;
        }
        if let Some(msg) = c.next_message_within(remaining).await.unwrap() {
            if msg.get("id").and_then(Value::as_u64) == Some(id) {
                panic!("turn finished before we could cancel");
            }
        }
    }
    c.notify("session/cancel", json!({"sessionId": sid}))
        .await
        .unwrap();
    loop {
        let msg = c.next_message(Duration::from_secs(60)).await.unwrap();
        if msg.get("id").and_then(Value::as_u64) == Some(id) {
            assert_eq!(
                msg["result"]["stopReason"],
                "cancelled",
                "cancel produced the wrong response: {}",
                serde_json::to_string_pretty(&msg).unwrap()
            );
            break;
        }
    }

    eprintln!("[e2e +{}ms] prompt ping", t0.elapsed().as_millis());
    let (resp, updates) = c
        .call(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type":"text","text":"Reply with exactly the single word PING and nothing else."}]}),
            Duration::from_secs(180),
        )
        .await
        .unwrap();
    assert_eq!(
        resp["result"]["stopReason"],
        "end_turn",
        "unexpected prompt response: {}",
        serde_json::to_string_pretty(&resp).unwrap()
    );
    assert!(message_text(&updates).contains("PING"));
    for update in &updates {
        assert_eq!(update["params"]["sessionId"].as_str(), Some(sid.as_str()));
    }

    eprintln!("[e2e +{}ms] close stdin", t0.elapsed().as_millis());
    c.close_stdin().await;
    let st = c.wait_exit(Duration::from_secs(10)).await;
    assert!(st.is_some() && st.unwrap().success());

    // The pin was restored on shutdown: the user's settings.json is untouched.
    eprintln!(
        "[e2e +{}ms] verify settings restored",
        t0.elapsed().as_millis()
    );
    assert_eq!(
        std::fs::read(&settings_path).ok(),
        settings_before,
        "bufflink must leave {} exactly as it found it",
        settings_path.display()
    );

    eprintln!("[e2e +{}ms] verify cleanup", t0.elapsed().as_millis());
    assert!(
        AcpClient::freebuff_processes().trim().is_empty(),
        "leaked freebuff process"
    );
    let _ = std::fs::remove_dir_all(&c.workdir);
}

fn message_text(notes: &[Value]) -> String {
    notes
        .iter()
        .filter(|note| {
            note["params"]["update"]["sessionUpdate"].as_str() == Some("agent_message_chunk")
        })
        .filter_map(|note| note["params"]["update"]["content"]["text"].as_str())
        .collect()
}
