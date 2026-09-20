//! Newline-delimited JSON-RPC 2.0 framing for ACP stdio.
//!
//! Only JSON-RPC messages are written to stdout; use `tracing` (stderr) for logs.

use serde_json::{json, Value};

/// A parsed inbound JSON-RPC message.
#[derive(Debug, Clone)]
pub enum Incoming {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
}

/// An outbound JSON-RPC message.
#[derive(Debug, Clone)]
pub enum Outgoing {
    Response {
        id: Value,
        result: Value,
    },
    Error {
        id: Value,
        code: i64,
        message: String,
        data: Option<Value>,
    },
    Notification {
        method: String,
        params: Value,
    },
}

/// JSON-RPC error returned by [`parse_line`].
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn parse_error(message: impl Into<String>) -> Self {
        Self {
            code: -32700,
            message: message.into(),
            data: None,
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            code: -32600,
            message: message.into(),
            data: None,
        }
    }
}

/// Parse one newline-delimited JSON-RPC line into an [`Incoming`] message.
///
/// * Invalid JSON → `-32700`
/// * Missing / invalid `jsonrpc: "2.0"` or `method` → `-32600`
pub fn parse_line(line: &str) -> Result<Incoming, RpcError> {
    let value: Value = serde_json::from_str(line.trim())
        .map_err(|e| RpcError::parse_error(format!("Parse error: {e}")))?;

    let obj = value
        .as_object()
        .ok_or_else(|| RpcError::invalid_request("JSON-RPC message must be an object"))?;

    match obj.get("jsonrpc") {
        Some(Value::String(v)) if v == "2.0" => {}
        _ => {
            return Err(RpcError::invalid_request(
                "missing or invalid jsonrpc version (expected \"2.0\")",
            ));
        }
    }

    let method = match obj.get("method") {
        Some(Value::String(m)) if !m.is_empty() => m.clone(),
        _ => {
            return Err(RpcError::invalid_request(
                "missing or invalid method (expected non-empty string)",
            ));
        }
    };

    let params = obj.get("params").cloned().unwrap_or(Value::Null);

    match obj.get("id") {
        Some(id) => Ok(Incoming::Request {
            id: id.clone(),
            method,
            params,
        }),
        None => Ok(Incoming::Notification { method, params }),
    }
}

/// Serialize an [`Outgoing`] message to a single line ending with `\\n`.
pub fn to_line(msg: &Outgoing) -> String {
    let value = match msg {
        Outgoing::Response { id, result } => {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result,
            })
        }
        Outgoing::Error {
            id,
            code,
            message,
            data,
        } => {
            let mut err = json!({
                "code": code,
                "message": message,
            });
            if let Some(data) = data {
                err.as_object_mut()
                    .expect("error object")
                    .insert("data".into(), data.clone());
            }
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": err,
            })
        }
        Outgoing::Notification { method, params } => {
            json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
            })
        }
    };
    let mut line = value.to_string();
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_request() {
        let msg =
            parse_line(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#).unwrap();
        match msg {
            Incoming::Request { id, method, .. } => {
                assert_eq!(id, json!(1));
                assert_eq!(method, "initialize");
            }
            _ => panic!("expected request"),
        }
    }

    #[test]
    fn parse_notification() {
        let msg =
            parse_line(r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s"}}"#)
                .unwrap();
        match msg {
            Incoming::Notification { method, .. } => assert_eq!(method, "session/cancel"),
            _ => panic!("expected notification"),
        }
    }

    #[test]
    fn parse_invalid_json() {
        let err = parse_line("not-json").unwrap_err();
        assert_eq!(err.code, -32700);
    }

    #[test]
    fn parse_missing_jsonrpc() {
        let err = parse_line(r#"{"id":1,"method":"initialize"}"#).unwrap_err();
        assert_eq!(err.code, -32600);
    }

    #[test]
    fn to_line_has_trailing_newline() {
        let line = to_line(&Outgoing::Response {
            id: json!(1),
            result: json!({}),
        });
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
    }
}
