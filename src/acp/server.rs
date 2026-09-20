//! ACP agent-side JSON-RPC server over newline-delimited stdio.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::acp::backend::{Backend, UpdateSink};
use crate::acp::jsonrpc::{parse_line, to_line, Incoming, Outgoing, RpcError};
use crate::acp::types::{
    CancelNotification, ContentBlock, InitializeRequest, InitializeResponse, NewSessionRequest,
    NewSessionResponse, PromptRequest, PromptResponse, SessionUpdate, SessionUpdateParams,
    StopReason,
};

const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// Serve ACP on `reader`/`writer` until stdin EOF, driving `backend`.
pub async fn serve<B: Backend>(
    backend: Arc<B>,
    reader: impl AsyncBufRead + Unpin + Send,
    writer: impl AsyncWrite + Unpin + Send + 'static,
) -> anyhow::Result<()> {
    let (out_tx, mut out_rx) = mpsc::channel::<Outgoing>(64);

    let mut writer = writer;
    let writer_task = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let line = to_line(&msg);
            if let Err(e) = writer.write_all(line.as_bytes()).await {
                warn!(error = %e, "ACP writer failed");
                break;
            }
            if let Err(e) = writer.flush().await {
                warn!(error = %e, "ACP writer flush failed");
                break;
            }
        }
    });

    let result = run_loop(backend, reader, out_tx).await;

    // Dropping the last out_tx happens inside run_loop; wait for writer drain.
    let _ = writer_task.await;
    result
}

async fn run_loop<B: Backend>(
    backend: Arc<B>,
    reader: impl AsyncBufRead + Unpin + Send,
    out_tx: mpsc::Sender<Outgoing>,
) -> anyhow::Result<()> {
    let mut lines = reader.lines();
    let mut session_id: Option<String> = None;
    let mut prompt_cancel: Option<watch::Sender<bool>> = None;
    let mut prompt_task: Option<tokio::task::JoinHandle<()>> = None;

    loop {
        tokio::select! {
            biased;

            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        handle_line(
                            &backend,
                            &line,
                            &out_tx,
                            &mut session_id,
                            &mut prompt_cancel,
                            &mut prompt_task,
                        )
                        .await;
                    }
                    Ok(None) => {
                        info!("ACP stdin EOF");
                        finish_in_flight(&mut prompt_cancel, &mut prompt_task).await;
                        backend.shutdown().await;
                        return Ok(());
                    }
                    Err(e) => {
                        warn!(error = %e, "ACP read error");
                        finish_in_flight(&mut prompt_cancel, &mut prompt_task).await;
                        backend.shutdown().await;
                        return Err(e.into());
                    }
                }
            }

            // Reap completed prompt tasks so `prompt_cancel` clears.
            _ = async {
                if let Some(handle) = prompt_task.as_mut() {
                    let _ = handle.await;
                } else {
                    std::future::pending::<()>().await;
                }
            }, if prompt_task.is_some() => {
                prompt_task = None;
                prompt_cancel = None;
            }
        }
    }
}

async fn finish_in_flight(
    prompt_cancel: &mut Option<watch::Sender<bool>>,
    prompt_task: &mut Option<tokio::task::JoinHandle<()>>,
) {
    if let Some(tx) = prompt_cancel.take() {
        let _ = tx.send(true);
    }
    if let Some(mut handle) = prompt_task.take() {
        match tokio::time::timeout(SHUTDOWN_GRACE, &mut handle).await {
            Ok(_) => {}
            Err(_) => {
                warn!("prompt task did not finish within {SHUTDOWN_GRACE:?} on EOF; aborting");
                handle.abort();
                let _ = handle.await;
            }
        }
    }
}

async fn handle_line<B: Backend>(
    backend: &Arc<B>,
    line: &str,
    out_tx: &mpsc::Sender<Outgoing>,
    session_id: &mut Option<String>,
    prompt_cancel: &mut Option<watch::Sender<bool>>,
    prompt_task: &mut Option<tokio::task::JoinHandle<()>>,
) {
    let incoming = match parse_line(line) {
        Ok(msg) => msg,
        Err(err) => {
            send_rpc_error(out_tx, Value::Null, &err).await;
            return;
        }
    };

    match incoming {
        Incoming::Request { id, method, params } => {
            dispatch_request(
                backend,
                id,
                &method,
                params,
                out_tx,
                session_id,
                prompt_cancel,
                prompt_task,
            )
            .await;
        }
        Incoming::Notification { method, params } => {
            dispatch_notification(&method, params, session_id, prompt_cancel).await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_request<B: Backend>(
    backend: &Arc<B>,
    id: Value,
    method: &str,
    params: Value,
    out_tx: &mpsc::Sender<Outgoing>,
    session_id: &mut Option<String>,
    prompt_cancel: &mut Option<watch::Sender<bool>>,
    prompt_task: &mut Option<tokio::task::JoinHandle<()>>,
) {
    match method {
        "initialize" => {
            let req: InitializeRequest = match serde_json::from_value(params) {
                Ok(r) => r,
                Err(e) => {
                    send_error(out_tx, id, -32602, format!("Invalid params: {e}"), None).await;
                    return;
                }
            };
            info!(
                client_protocol_version = req.protocol_version,
                "ACP initialize"
            );
            let result = serde_json::to_value(InitializeResponse::v1())
                .expect("InitializeResponse serializes");
            send_result(out_tx, id, result).await;
        }
        "session/new" => {
            let req: NewSessionRequest = match serde_json::from_value(params) {
                Ok(r) => r,
                Err(e) => {
                    send_error(out_tx, id, -32602, format!("Invalid params: {e}"), None).await;
                    return;
                }
            };
            if let Some(existing) = session_id.as_ref() {
                warn!(
                    session_id = %existing,
                    cwd = %req.cwd,
                    "session/new while a session already exists; returning the same id"
                );
                let result = serde_json::to_value(NewSessionResponse {
                    session_id: existing.clone(),
                })
                .expect("NewSessionResponse serializes");
                send_result(out_tx, id, result).await;
                return;
            }
            match backend.new_session(PathBuf::from(&req.cwd)).await {
                Ok(sid) => {
                    *session_id = Some(sid.clone());
                    let result = serde_json::to_value(NewSessionResponse { session_id: sid })
                        .expect("NewSessionResponse serializes");
                    send_result(out_tx, id, result).await;
                }
                Err(e) => {
                    send_error(out_tx, id, -32603, e.to_string(), None).await;
                }
            }
        }
        "session/prompt" => {
            let req: PromptRequest = match serde_json::from_value(params) {
                Ok(r) => r,
                Err(e) => {
                    send_error(out_tx, id, -32602, format!("Invalid params: {e}"), None).await;
                    return;
                }
            };
            let Some(current) = session_id.as_ref() else {
                send_error(
                    out_tx,
                    id,
                    -32602,
                    "no session; call session/new first",
                    None,
                )
                .await;
                return;
            };
            if req.session_id != *current {
                send_error(
                    out_tx,
                    id,
                    -32602,
                    format!("unknown session id: {}", req.session_id),
                    None,
                )
                .await;
                return;
            }
            // Reap a finished prompt before accepting a new one.
            if prompt_task.as_ref().is_some_and(|h| h.is_finished()) {
                if let Some(h) = prompt_task.take() {
                    let _ = h.await;
                }
                *prompt_cancel = None;
            }
            if prompt_task.is_some() {
                send_error(out_tx, id, -32000, "prompt already in flight", None).await;
                return;
            }

            let text = ContentBlock::concat_text(&req.prompt);
            let (cancel_tx, cancel_rx) = watch::channel(false);
            let (update_tx, update_rx) = mpsc::channel::<SessionUpdate>(64);
            let sink = UpdateSink::new(update_tx);
            let backend = Arc::clone(backend);
            let sid = req.session_id.clone();
            let out = out_tx.clone();

            *prompt_cancel = Some(cancel_tx);
            *prompt_task = Some(tokio::spawn(async move {
                run_prompt(backend, sid, text, sink, cancel_rx, update_rx, out, id).await;
            }));
        }
        _ => {
            send_error(
                out_tx,
                id,
                -32601,
                format!("Method not found: {method}"),
                None,
            )
            .await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_prompt<B: Backend>(
    backend: Arc<B>,
    session_id: String,
    text: String,
    sink: UpdateSink,
    cancel_rx: watch::Receiver<bool>,
    mut update_rx: mpsc::Receiver<SessionUpdate>,
    out_tx: mpsc::Sender<Outgoing>,
    id: Value,
) {
    let sid_for_prompt = session_id.clone();
    let prompt_fut = backend.prompt(&sid_for_prompt, text, sink, cancel_rx);
    tokio::pin!(prompt_fut);
    let mut outcome: Option<anyhow::Result<StopReason>> = None;

    loop {
        tokio::select! {
            biased;

            Some(update) = update_rx.recv() => {
                send_session_update(&out_tx, &session_id, update).await;
            }

            result = &mut prompt_fut, if outcome.is_none() => {
                outcome = Some(result);
            }

            else => break,
        }
    }

    // Drain any updates still queued after the backend dropped the sink.
    while let Some(update) = update_rx.recv().await {
        send_session_update(&out_tx, &session_id, update).await;
    }

    match outcome.expect("prompt future completed") {
        Ok(stop_reason) => {
            let result = serde_json::to_value(PromptResponse { stop_reason })
                .expect("PromptResponse serializes");
            send_result(&out_tx, id, result).await;
        }
        Err(e) => {
            send_error(&out_tx, id, -32603, e.to_string(), None).await;
        }
    }
}

async fn dispatch_notification(
    method: &str,
    params: Value,
    session_id: &Option<String>,
    prompt_cancel: &mut Option<watch::Sender<bool>>,
) {
    match method {
        "session/cancel" => {
            let req: CancelNotification = match serde_json::from_value(params) {
                Ok(r) => r,
                Err(e) => {
                    debug!(error = %e, "ignoring malformed session/cancel");
                    return;
                }
            };
            match session_id {
                Some(current) if current == &req.session_id => {
                    if let Some(tx) = prompt_cancel.as_ref() {
                        let _ = tx.send(true);
                    } else {
                        debug!("session/cancel with no prompt in flight");
                    }
                }
                Some(_) => {
                    debug!(
                        session_id = %req.session_id,
                        "session/cancel for unknown session id; ignored"
                    );
                }
                None => {
                    debug!("session/cancel with no session; ignored");
                }
            }
        }
        other => {
            debug!(method = other, "ignoring unknown notification");
        }
    }
}

async fn send_session_update(
    out_tx: &mpsc::Sender<Outgoing>,
    session_id: &str,
    update: SessionUpdate,
) {
    let params = SessionUpdateParams {
        session_id: session_id.to_string(),
        update,
    };
    let params = serde_json::to_value(params).expect("SessionUpdateParams serializes");
    let _ = out_tx
        .send(Outgoing::Notification {
            method: "session/update".into(),
            params,
        })
        .await;
}

async fn send_result(out_tx: &mpsc::Sender<Outgoing>, id: Value, result: Value) {
    let _ = out_tx.send(Outgoing::Response { id, result }).await;
}

async fn send_error(
    out_tx: &mpsc::Sender<Outgoing>,
    id: Value,
    code: i64,
    message: impl Into<String>,
    data: Option<Value>,
) {
    let _ = out_tx
        .send(Outgoing::Error {
            id,
            code,
            message: message.into(),
            data,
        })
        .await;
}

async fn send_rpc_error(out_tx: &mpsc::Sender<Outgoing>, id: Value, err: &RpcError) {
    send_error(out_tx, id, err.code, err.message.clone(), err.data.clone()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::types::TextContent;
    use serde_json::json;
    use std::future::Future;
    use std::sync::Mutex;
    use tokio::io::duplex;
    use tokio::io::AsyncWriteExt;
    use tokio::time::{sleep, timeout};

    #[derive(Default)]
    struct FakeState {
        sessions: Vec<PathBuf>,
        prompts: Vec<(String, String)>,
        shutdowns: usize,
        /// When set, prompt waits until cancel becomes true, then returns Cancelled.
        wait_for_cancel: bool,
        /// When set, prompt ignores cancel and never returns.
        ignore_cancel: bool,
        /// Scripted updates to emit before completing.
        updates: Vec<SessionUpdate>,
        /// If set, prompt returns this error.
        prompt_error: Option<String>,
        /// Extra delay before completing a normal prompt.
        prompt_delay: Option<Duration>,
        last_cancel_seen: bool,
    }

    struct FakeBackend {
        state: Mutex<FakeState>,
        session_id: String,
    }

    impl FakeBackend {
        fn new(session_id: impl Into<String>) -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::new(FakeState::default()),
                session_id: session_id.into(),
            })
        }

        fn with(self: Arc<Self>, f: impl FnOnce(&mut FakeState)) -> Arc<Self> {
            f(&mut self.state.lock().unwrap());
            self
        }

        fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
            self.state.lock().unwrap()
        }
    }

    impl Backend for FakeBackend {
        fn new_session(&self, cwd: PathBuf) -> impl Future<Output = anyhow::Result<String>> + Send {
            let id = self.session_id.clone();
            async move {
                self.state.lock().unwrap().sessions.push(cwd);
                Ok(id)
            }
        }

        fn prompt(
            &self,
            session_id: &str,
            text: String,
            updates: UpdateSink,
            mut cancel: watch::Receiver<bool>,
        ) -> impl Future<Output = anyhow::Result<StopReason>> + Send {
            let sid = session_id.to_string();
            async move {
                {
                    let mut st = self.state.lock().unwrap();
                    st.prompts.push((sid, text));
                }

                let (wait_cancel, ignore_cancel, scripted, err, delay) = {
                    let st = self.state.lock().unwrap();
                    (
                        st.wait_for_cancel,
                        st.ignore_cancel,
                        st.updates.clone(),
                        st.prompt_error.clone(),
                        st.prompt_delay,
                    )
                };

                for u in scripted {
                    updates.send(u).await;
                }

                if let Some(msg) = err {
                    return Err(anyhow::anyhow!(msg));
                }

                if let Some(d) = delay {
                    sleep(d).await;
                }

                if ignore_cancel {
                    std::future::pending::<()>().await;
                    unreachable!()
                }

                if wait_cancel {
                    loop {
                        if *cancel.borrow() {
                            self.state.lock().unwrap().last_cancel_seen = true;
                            return Ok(StopReason::Cancelled);
                        }
                        if cancel.changed().await.is_err() {
                            return Ok(StopReason::Cancelled);
                        }
                    }
                }

                Ok(StopReason::EndTurn)
            }
        }

        #[allow(clippy::manual_async_fn)]
        fn shutdown(&self) -> impl Future<Output = ()> + Send {
            async move {
                self.state.lock().unwrap().shutdowns += 1;
            }
        }
    }

    async fn collect_lines(mut reader: impl AsyncBufRead + Unpin, n: usize) -> Vec<Value> {
        let mut out = Vec::new();
        let mut buf = String::new();
        while out.len() < n {
            buf.clear();
            let nread = reader.read_line(&mut buf).await.unwrap();
            if nread == 0 {
                break;
            }
            if buf.trim().is_empty() {
                continue;
            }
            out.push(serde_json::from_str(buf.trim()).unwrap());
        }
        out
    }

    async fn write_line(writer: &mut (impl AsyncWrite + Unpin), v: Value) {
        let mut s = v.to_string();
        s.push('\n');
        writer.write_all(s.as_bytes()).await.unwrap();
        writer.flush().await.unwrap();
    }

    fn init_req(id: u64) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": { "protocolVersion": 1 }
        })
    }

    fn session_new(id: u64, cwd: &str) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/new",
            "params": { "cwd": cwd, "mcpServers": [] }
        })
    }

    fn prompt_req(id: u64, sid: &str, text: &str) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/prompt",
            "params": {
                "sessionId": sid,
                "prompt": [{"type": "text", "text": text}]
            }
        })
    }

    type DuplexRead = tokio::io::BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>;
    type DuplexWrite = tokio::io::WriteHalf<tokio::io::DuplexStream>;

    fn open_pipe() -> (DuplexWrite, DuplexRead, DuplexRead, DuplexWrite) {
        let (client, server) = duplex(64 * 1024);
        let (server_r, server_w) = tokio::io::split(server);
        let (client_r, client_w) = tokio::io::split(client);
        (
            client_w,
            tokio::io::BufReader::new(client_r),
            tokio::io::BufReader::new(server_r),
            server_w,
        )
    }

    async fn shutdown_serve(
        client_w: DuplexWrite,
        client_r: DuplexRead,
        serve: tokio::task::JoinHandle<anyhow::Result<()>>,
    ) {
        drop(client_w);
        drop(client_r);
        timeout(Duration::from_secs(2), serve)
            .await
            .expect("serve timed out")
            .expect("serve join")
            .expect("serve error");
    }

    #[tokio::test]
    async fn initialize_handshake_json_shape() {
        let backend = FakeBackend::new("s1");
        let (mut client_w, mut client_r, server_r, server_w) = open_pipe();

        let serve = tokio::spawn(serve(backend, server_r, server_w));
        write_line(&mut client_w, init_req(1)).await;
        let lines = collect_lines(&mut client_r, 1).await;
        assert_eq!(lines[0]["id"], 1);
        assert_eq!(lines[0]["result"]["protocolVersion"], 1);
        assert_eq!(lines[0]["result"]["agentInfo"]["name"], "blink");
        assert_eq!(
            lines[0]["result"]["agentCapabilities"]["loadSession"],
            false
        );
        assert_eq!(
            lines[0]["result"]["agentCapabilities"]["promptCapabilities"]["image"],
            false
        );
        shutdown_serve(client_w, client_r, serve).await;
    }

    #[tokio::test]
    async fn session_new_returns_id_and_repeats_it() {
        let backend = FakeBackend::new("fixed-session");
        let (mut client_w, mut client_r, server_r, server_w) = open_pipe();

        let serve = tokio::spawn(serve(Arc::clone(&backend), server_r, server_w));
        write_line(&mut client_w, init_req(1)).await;
        write_line(&mut client_w, session_new(2, "/tmp/a")).await;
        write_line(&mut client_w, session_new(3, "/tmp/b")).await;
        let lines = collect_lines(&mut client_r, 3).await;
        assert_eq!(lines[1]["result"]["sessionId"], "fixed-session");
        assert_eq!(lines[2]["result"]["sessionId"], "fixed-session");
        assert_eq!(backend.state().sessions.len(), 1);
        shutdown_serve(client_w, client_r, serve).await;
    }

    #[tokio::test]
    async fn prompt_streams_updates_before_response() {
        let backend = FakeBackend::new("s1").with(|st| {
            st.updates = vec![
                SessionUpdate::AgentThoughtChunk {
                    content: TextContent::text("thinking"),
                },
                SessionUpdate::AgentMessageChunk {
                    content: TextContent::text("hello"),
                },
            ];
        });
        let (mut client_w, mut client_r, server_r, server_w) = open_pipe();

        let serve = tokio::spawn(serve(Arc::clone(&backend), server_r, server_w));
        write_line(&mut client_w, init_req(1)).await;
        write_line(&mut client_w, session_new(2, "/tmp")).await;
        write_line(&mut client_w, prompt_req(3, "s1", "hi")).await;
        let lines = collect_lines(&mut client_r, 5).await;
        // 0 init, 1 session/new, then two notifications, then prompt response
        assert_eq!(lines[2]["method"], "session/update");
        assert_eq!(
            lines[2]["params"]["update"]["sessionUpdate"],
            "agent_thought_chunk"
        );
        assert_eq!(lines[3]["method"], "session/update");
        assert_eq!(
            lines[3]["params"]["update"]["sessionUpdate"],
            "agent_message_chunk"
        );
        assert_eq!(lines[4]["id"], 3);
        assert_eq!(lines[4]["result"]["stopReason"], "end_turn");
        // notifications must appear before the response
        assert!(lines[2].get("method").is_some());
        assert!(lines[4].get("result").is_some());
        shutdown_serve(client_w, client_r, serve).await;
    }

    #[tokio::test]
    async fn cancel_mid_prompt_flips_watch_and_returns_cancelled() {
        let backend = FakeBackend::new("s1").with(|st| {
            st.wait_for_cancel = true;
        });
        let (mut client_w, mut client_r, server_r, server_w) = open_pipe();

        let serve = tokio::spawn(serve(Arc::clone(&backend), server_r, server_w));
        write_line(&mut client_w, init_req(1)).await;
        write_line(&mut client_w, session_new(2, "/tmp")).await;
        write_line(&mut client_w, prompt_req(3, "s1", "long")).await;
        // drain init + session/new
        let _ = collect_lines(&mut client_r, 2).await;
        sleep(Duration::from_millis(50)).await;
        write_line(
            &mut client_w,
            json!({
                "jsonrpc": "2.0",
                "method": "session/cancel",
                "params": { "sessionId": "s1" }
            }),
        )
        .await;
        let lines = collect_lines(&mut client_r, 1).await;
        assert_eq!(lines[0]["id"], 3);
        assert_eq!(lines[0]["result"]["stopReason"], "cancelled");
        assert!(backend.state().last_cancel_seen);
        shutdown_serve(client_w, client_r, serve).await;
    }

    #[tokio::test]
    async fn backend_error_surfaces_as_32603() {
        let backend = FakeBackend::new("s1").with(|st| {
            st.prompt_error = Some("TUI shape changed".into());
        });
        let (mut client_w, mut client_r, server_r, server_w) = open_pipe();

        let serve = tokio::spawn(serve(Arc::clone(&backend), server_r, server_w));
        write_line(&mut client_w, init_req(1)).await;
        write_line(&mut client_w, session_new(2, "/tmp")).await;
        write_line(&mut client_w, prompt_req(3, "s1", "x")).await;
        let lines = collect_lines(&mut client_r, 3).await;
        assert_eq!(lines[2]["id"], 3);
        assert_eq!(lines[2]["error"]["code"], -32603);
        assert!(lines[2]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("TUI shape changed"));
        shutdown_serve(client_w, client_r, serve).await;
    }

    #[tokio::test]
    async fn malformed_line_returns_32700_and_loop_continues() {
        let backend = FakeBackend::new("s1");
        let (mut client_w, mut client_r, server_r, server_w) = open_pipe();

        let serve = tokio::spawn(serve(backend, server_r, server_w));
        client_w.write_all(b"not-json\n").await.unwrap();
        write_line(&mut client_w, init_req(1)).await;
        let lines = collect_lines(&mut client_r, 2).await;
        assert_eq!(lines[0]["error"]["code"], -32700);
        assert_eq!(lines[1]["id"], 1);
        assert_eq!(lines[1]["result"]["protocolVersion"], 1);
        shutdown_serve(client_w, client_r, serve).await;
    }

    #[tokio::test]
    async fn unknown_method_returns_32601() {
        let backend = FakeBackend::new("s1");
        let (mut client_w, mut client_r, server_r, server_w) = open_pipe();

        let serve = tokio::spawn(serve(backend, server_r, server_w));
        write_line(
            &mut client_w,
            json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "nope/unknown",
                "params": {}
            }),
        )
        .await;
        let lines = collect_lines(&mut client_r, 1).await;
        assert_eq!(lines[0]["error"]["code"], -32601);
        shutdown_serve(client_w, client_r, serve).await;
    }

    #[tokio::test]
    async fn eof_calls_shutdown() {
        let backend = FakeBackend::new("s1");
        let (client_w, client_r, server_r, server_w) = open_pipe();

        let serve = tokio::spawn(serve(Arc::clone(&backend), server_r, server_w));
        shutdown_serve(client_w, client_r, serve).await;
        assert_eq!(backend.state().shutdowns, 1);
    }

    #[tokio::test]
    async fn second_prompt_while_in_flight_errors() {
        let backend = FakeBackend::new("s1").with(|st| {
            st.wait_for_cancel = true;
        });
        let (mut client_w, mut client_r, server_r, server_w) = open_pipe();

        let serve = tokio::spawn(serve(Arc::clone(&backend), server_r, server_w));
        write_line(&mut client_w, init_req(1)).await;
        write_line(&mut client_w, session_new(2, "/tmp")).await;
        write_line(&mut client_w, prompt_req(3, "s1", "a")).await;
        let _ = collect_lines(&mut client_r, 2).await;
        sleep(Duration::from_millis(30)).await;
        write_line(&mut client_w, prompt_req(4, "s1", "b")).await;
        let lines = collect_lines(&mut client_r, 1).await;
        assert_eq!(lines[0]["id"], 4);
        assert_eq!(lines[0]["error"]["code"], -32000);
        // cancel to unblock
        write_line(
            &mut client_w,
            json!({
                "jsonrpc": "2.0",
                "method": "session/cancel",
                "params": { "sessionId": "s1" }
            }),
        )
        .await;
        let _ = collect_lines(&mut client_r, 1).await;
        shutdown_serve(client_w, client_r, serve).await;
    }

    #[tokio::test]
    async fn eof_aborts_prompt_that_ignores_cancel() {
        let backend = FakeBackend::new("s1").with(|st| {
            st.ignore_cancel = true;
        });
        let (mut client_w, mut client_r, server_r, server_w) = open_pipe();

        let serve = tokio::spawn(serve(Arc::clone(&backend), server_r, server_w));
        write_line(&mut client_w, init_req(1)).await;
        write_line(&mut client_w, session_new(2, "/tmp")).await;
        write_line(&mut client_w, prompt_req(3, "s1", "hang")).await;
        let _ = collect_lines(&mut client_r, 2).await;
        sleep(Duration::from_millis(50)).await;

        drop(client_w);
        drop(client_r);

        timeout(Duration::from_secs(5), serve)
            .await
            .expect("serve should return within ~4s")
            .expect("serve join")
            .expect("serve error");
        assert_eq!(backend.state().shutdowns, 1);
    }
}
