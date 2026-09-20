//! Unit tests for the PTY driver, using real pseudo-terminals on macOS.
//! All children are `/bin/sh` scripts — never the `freebuff` binary.

use std::time::Duration;

use super::{group_target, Key, Pty, PtyConfig};
use tokio::task::JoinHandle;

/// Build a config that runs `script` in `/bin/sh` at the given size.
fn cfg(script: &str, cols: u16, rows: u16) -> PtyConfig {
    PtyConfig {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        cwd: None,
        env: vec![],
        cols,
        rows,
    }
}

#[tokio::test]
async fn spawn_hello_and_wait_exit() {
    let pty = Pty::spawn(cfg("printf 'hello\\n'; sleep 0.2", 80, 24))
        .await
        .expect("spawn");
    let snap = pty
        .wait_for(|s| s.contains("hello"), Duration::from_secs(5))
        .await
        .expect("see hello");
    assert!(snap.contains("hello"));

    let status = pty
        .wait_exit(Duration::from_secs(5))
        .await
        .expect("wait_exit");
    assert!(status.map(|s| s.success()).unwrap_or(false));
}

#[tokio::test]
async fn echo_round_trip() {
    let pty = Pty::spawn(cfg("read x; echo \"got:$x\"", 80, 24))
        .await
        .expect("spawn");
    pty.type_text("abc").await.expect("type_text");
    pty.key(Key::Enter).await.expect("enter");

    let snap = pty
        .wait_for(|s| s.contains("got:abc"), Duration::from_secs(5))
        .await
        .expect("see got:abc");
    assert!(snap.contains("got:abc"));
}

#[tokio::test]
async fn bracketed_paste_bytes() {
    let pty = Pty::spawn(cfg("cat -v", 80, 24)).await.expect("spawn");
    pty.paste("a\nb").await.expect("paste");

    let snap = pty
        .wait_for(
            |s| s.contains("^[[200~a") || s.contains("^[[201~"),
            Duration::from_secs(5),
        )
        .await
        .expect("see paste bytes");
    assert!(
        snap.contains("^[[200~a"),
        "expected `^[[200~a` in:\n{}",
        snap.text()
    );
    assert!(
        snap.contains("^[[201~"),
        "expected `^[[201~` in:\n{}",
        snap.text()
    );
}

// Reply to `ESC[6n` is `ESC[<row>;<col>R` (1-based). We read exactly the 7
// bytes of `ESC[1;1R` with `dd`, since the reply carries no trailing newline
// and a blocking `read` would never return. `cat -v` renders ESC as `^[`.
#[tokio::test]
async fn dsr_auto_reply() {
    let pty = Pty::spawn(cfg(
        "printf '\\033[6n'; dd bs=1 count=7 2>/dev/null | cat -v",
        80,
        24,
    ))
    .await
    .expect("spawn");

    let snap = pty
        .wait_for(
            |s| s.contains("^[[") && s.contains(";"),
            Duration::from_secs(5),
        )
        .await
        .expect("DSR reply echoed");

    let text = snap.text();
    let caret = text.find("^[[").expect("reply contains `^[[`");

    // The reply must read `ESC[<digits>;<digits>R`.
    let body = &text[caret + 3..];
    let digits_then: String = body.chars().take_while(|c| c.is_ascii_digit()).collect();
    assert!(
        !digits_then.is_empty(),
        "expected digits after `^[[` in: {text}"
    );

    let after = &body[digits_then.len()..];
    assert!(after.starts_with(';'), "expected `;` in: {text}");
    assert!(!after[1..].is_empty(), "expected cols after `;` in: {text}");
}

#[tokio::test]
async fn alternate_screen_flag() {
    let pty = Pty::spawn(cfg("printf '\\033[?1049h'; sleep 0.3", 80, 24))
        .await
        .expect("spawn");

    let snap = pty
        .wait_for(|s| s.alternate_screen, Duration::from_secs(5))
        .await
        .expect("alternate screen active while child runs");
    assert!(snap.alternate_screen);
}

#[tokio::test]
async fn kill_terminates_child() {
    let pty = Pty::spawn(cfg("sleep 30", 80, 24)).await.expect("spawn");

    let killed = tokio::time::timeout(Duration::from_secs(2), pty.kill())
        .await
        .expect("kill() returns within 2s");
    killed.expect("kill ok");

    // Poll try_wait until the SIGKILLed child has been reaped.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if pty.try_wait().expect("try_wait").is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "child not reaped after kill within 2s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(pty.try_wait().expect("try_wait").is_some());
}

/// The child is spawned as a session leader, so its process group id equals
/// its pid. A grandchild started by the shell lives in the same group.
/// `kill()` must terminate the whole group, not just the direct child.
#[tokio::test]
async fn kill_terminates_grandchild() {
    let pty = Pty::spawn(cfg("sh -c 'sh -c \"exec sleep 30\" & sleep 30'", 80, 24))
        .await
        .expect("spawn");

    // Let the grandchild fork and enter its sleep.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let pid = pty
        .pid()
        .expect("child pid should be available before kill");
    assert!(pid > 0);

    let killed = tokio::time::timeout(Duration::from_secs(2), pty.kill())
        .await
        .expect("kill() returns within 2s");
    killed.expect("kill ok");

    // Poll try_wait until the direct child has been reaped.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if pty.try_wait().expect("try_wait").is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "child not reaped after kill within 2s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The grandchild must be gone too. `pgrep -g <pid>` matches only the
    // child's process group, which is exact — no false positives from other
    // `sleep 30` processes anywhere on the host.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let out = std::process::Command::new("pgrep")
            .args(["-g", &pid.to_string()])
            .output()
            .expect("pgrep should run");
        if out.status.success() {
            // Non-empty: something is still alive in the group.
            let stdout = String::from_utf8_lossy(&out.stdout);
            panic!(
                "grandchild survived kill(); pgrep -g {} returned:\n{}",
                pid, stdout
            );
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// After the child has exited and been reaped, `kill()` must still signal the
/// whole process group so an orphaned grandchild dies. `pid()` is `None` once
/// the child is reaped (`Child::id()` clears after `wait`/`try_wait`), so the
/// group id comes from the spawn pid captured at spawn time.
#[tokio::test]
async fn kill_after_child_exit_still_kills_group() {
    // The parent shell exits immediately, orphaning the inner `sleep 30`
    // grandchild in the same process group.
    let pty = Pty::spawn(cfg("sh -c 'sh -c \"exec sleep 30\" & exit 0'", 80, 24))
        .await
        .expect("spawn");

    let spawn_pid = pty.spawn_pid();
    assert!(spawn_pid > 0);

    // Parent exits on its own; wait_exit reaps it and returns Some.
    let status = pty
        .wait_exit(Duration::from_secs(2))
        .await
        .expect("wait_exit");
    assert!(status.is_some(), "child should have exited on its own");

    // Even though the child is now reaped, kill() must still reach the orphan
    // grandchild via the spawn pid's process group.
    let killed = tokio::time::timeout(Duration::from_secs(2), pty.kill())
        .await
        .expect("kill() returns within 2s");
    killed.expect("kill ok");

    // `pgrep -g <spawn_pid>` matches only the child's process group, which
    // persists as long as the grandchild lives. It must be empty.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let out = std::process::Command::new("pgrep")
            .args(["-g", &spawn_pid.to_string()])
            .output()
            .expect("pgrep should run");
        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            panic!(
                "grandchild survived kill() after child exit; pgrep -g {} returned:\n{}",
                spawn_pid, stdout
            );
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn wait_for_timeout_includes_screen_text() {
    let pty = Pty::spawn(cfg("printf 'abc\\n'; sleep 0.5", 80, 24))
        .await
        .expect("spawn");
    // Wait until the output is on the screen so the timeout path has text.
    pty.wait_for(|s| s.contains("abc"), Duration::from_secs(5))
        .await
        .expect("see abc");

    let err = pty
        .wait_for(|s| s.contains("NEVER"), Duration::from_millis(200))
        .await
        .expect_err("predicate never satisfied");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("abc"),
        "timeout message should include screen text, got: {msg}"
    );
}

#[tokio::test]
async fn resize_inside_runtime_does_not_panic() {
    let pty = Pty::spawn(cfg("sleep 1", 80, 24)).await.expect("spawn");
    pty.resize(100, 30).await.expect("resize inside runtime");
    pty.kill().await.expect("kill");
}

#[tokio::test]
async fn group_target_returns_none_for_invalid_pids() {
    assert_eq!(group_target(None), None);
    assert_eq!(group_target(Some(0)), None);
    assert_eq!(group_target(Some(u32::MAX)), None);
}

/// Stress test: many concurrent PTY spawns must not hit transient ENOENT.
/// Runs 3 rounds of 16 concurrent `sh -c 'printf ok; sleep 0.1'` children.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_spawns_do_not_fail() -> anyhow::Result<()> {
    let script = "printf ok; sleep 0.1";
    let cols = 80;
    let rows = 24;
    const ROUNDS: usize = 3;
    const PER_ROUND: usize = 16;

    for _round in 0..ROUNDS {
        let mut handles: Vec<JoinHandle<anyhow::Result<()>>> = Vec::with_capacity(PER_ROUND);

        for _ in 0..PER_ROUND {
            let script = script.to_string();
            handles.push(tokio::spawn(async move {
                let pty = Pty::spawn(PtyConfig {
                    program: "/bin/sh".to_string(),
                    args: vec!["-c".to_string(), script],
                    cwd: None,
                    env: vec![],
                    cols,
                    rows,
                })
                .await?;

                pty.wait_for(|s| s.contains("ok"), Duration::from_secs(5))
                    .await?;
                let status = pty.wait_exit(Duration::from_secs(3)).await?;
                assert!(
                    status.map(|s| s.success()).unwrap_or(false),
                    "child exited unsuccessfully"
                );
                Ok(())
            }));
        }

        for handle in handles {
            handle.await.expect("task panicked")?;
        }
    }
    Ok(())
}
