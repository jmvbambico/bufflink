//! ACP request/response and session-update types (protocol version 1).
//!
//! Field names match what omnigent reads — see `docs/research/acp-contract.md`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `initialize` request params.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequest {
    pub protocol_version: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_capabilities: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_info: Option<Value>,
}

/// `initialize` result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub protocol_version: u16,
    pub agent_capabilities: AgentCapabilities,
    pub agent_info: AgentInfo,
}

impl InitializeResponse {
    /// Protocol version 1 response advertising blink's fixed capabilities.
    pub fn v1() -> Self {
        Self {
            protocol_version: 1,
            agent_capabilities: AgentCapabilities::default(),
            agent_info: AgentInfo {
                name: "blink".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        }
    }
}

/// Agent capability advertisement.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    pub load_session: bool,
    pub prompt_capabilities: PromptCapabilities,
}

/// Prompt-related capability flags.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PromptCapabilities {
    pub image: bool,
    pub audio: bool,
    pub embedded_context: bool,
}

/// Agent identity in `initialize` result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    pub name: String,
    pub version: String,
}

/// `session/new` request params.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionRequest {
    pub cwd: String,
    #[serde(default)]
    pub mcp_servers: Vec<Value>,
}

/// `session/new` result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResponse {
    pub session_id: String,
}

/// `session/prompt` request params.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PromptRequest {
    pub session_id: String,
    pub prompt: Vec<ContentBlock>,
}

/// A prompt content block. Unknown `type` values are ignored, not rejected.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}

impl Serialize for ContentBlock {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            ContentBlock::Text { text } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "text")?;
                map.serialize_entry("text", text)?;
                map.end()
            }
            // `#[serde(other)]` is deserialize-only; emit a harmless stub for tests.
            ContentBlock::Other => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("type", "unknown")?;
                map.end()
            }
        }
    }
}

impl ContentBlock {
    /// Concatenate all `text` blocks with `"\\n"`.
    pub fn concat_text(blocks: &[ContentBlock]) -> String {
        blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::Other => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// `session/prompt` result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PromptResponse {
    pub stop_reason: StopReason,
}

/// Why a prompt turn ended.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
}

/// `session/cancel` notification params.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CancelNotification {
    pub session_id: String,
}

/// Tool-call lifecycle status on `tool_call` / `tool_call_update`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// Text content payload `{type:"text", text}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TextContent {
    Text { text: String },
}

impl TextContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }
}

/// Nested content item on `tool_call_update`: `{type:"content", content:{type:"text", text}}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolCallContentItem {
    Content { content: TextContent },
}

/// Streaming `session/update` payload (the `update` field).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum SessionUpdate {
    AgentMessageChunk {
        content: TextContent,
    },
    AgentThoughtChunk {
        content: TextContent,
    },
    ToolCall {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        title: String,
        kind: String,
        status: ToolCallStatus,
        #[serde(rename = "rawInput")]
        raw_input: Value,
    },
    ToolCallUpdate {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        status: ToolCallStatus,
        #[serde(rename = "rawOutput", default, skip_serializing_if = "Option::is_none")]
        raw_output: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Vec<ToolCallContentItem>>,
    },
}

/// Params for the `session/update` notification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdateParams {
    pub session_id: String,
    pub update: SessionUpdate,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn initialize_response_json_keys() {
        let resp = InitializeResponse::v1();
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["protocolVersion"], 1);
        assert_eq!(v["agentCapabilities"]["loadSession"], false);
        assert_eq!(v["agentCapabilities"]["promptCapabilities"]["image"], false);
        assert_eq!(v["agentCapabilities"]["promptCapabilities"]["audio"], false);
        assert_eq!(
            v["agentCapabilities"]["promptCapabilities"]["embeddedContext"],
            false
        );
        assert_eq!(v["agentInfo"]["name"], "blink");
        assert_eq!(v["agentInfo"]["version"], env!("CARGO_PKG_VERSION"));
        let round: InitializeResponse = serde_json::from_value(v).unwrap();
        assert_eq!(round, resp);
    }

    #[test]
    fn initialize_request_round_trip() {
        let raw = json!({
            "protocolVersion": 1,
            "clientCapabilities": {"fs": {"readTextFile": true}},
            "clientInfo": {"name": "omnigent"}
        });
        let req: InitializeRequest = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(req.protocol_version, 1);
        let back = serde_json::to_value(&req).unwrap();
        assert_eq!(back["protocolVersion"], 1);
        assert!(back.get("clientCapabilities").is_some());
    }

    #[test]
    fn new_session_json_keys() {
        let req = NewSessionRequest {
            cwd: "/tmp".into(),
            mcp_servers: vec![],
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["cwd"], "/tmp");
        assert_eq!(v["mcpServers"], json!([]));
        let resp = NewSessionResponse {
            session_id: "sess-1".into(),
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["sessionId"], "sess-1");
    }

    #[test]
    fn prompt_request_ignores_unknown_blocks() {
        let raw = json!({
            "sessionId": "s1",
            "prompt": [
                {"type": "text", "text": "hello"},
                {"type": "image", "data": "xxx"},
                {"type": "text", "text": "world"}
            ]
        });
        let req: PromptRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.session_id, "s1");
        assert_eq!(req.prompt.len(), 3);
        assert_eq!(ContentBlock::concat_text(&req.prompt), "hello\nworld");
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["sessionId"], "s1");
    }

    #[test]
    fn stop_reason_snake_case() {
        for (reason, expected) in [
            (StopReason::EndTurn, "end_turn"),
            (StopReason::MaxTokens, "max_tokens"),
            (StopReason::MaxTurnRequests, "max_turn_requests"),
            (StopReason::Refusal, "refusal"),
            (StopReason::Cancelled, "cancelled"),
        ] {
            let v = serde_json::to_value(PromptResponse {
                stop_reason: reason,
            })
            .unwrap();
            assert_eq!(v["stopReason"], expected);
            let back: PromptResponse = serde_json::from_value(v).unwrap();
            assert_eq!(back.stop_reason, reason);
        }
    }

    #[test]
    fn cancel_notification_json_keys() {
        let n = CancelNotification {
            session_id: "s1".into(),
        };
        let v = serde_json::to_value(&n).unwrap();
        assert_eq!(v["sessionId"], "s1");
    }

    #[test]
    fn session_update_agent_message_chunk_keys() {
        let u = SessionUpdate::AgentMessageChunk {
            content: TextContent::text("hi"),
        };
        let v = serde_json::to_value(&u).unwrap();
        assert_eq!(v["sessionUpdate"], "agent_message_chunk");
        assert_eq!(v["content"]["type"], "text");
        assert_eq!(v["content"]["text"], "hi");
        let back: SessionUpdate = serde_json::from_value(v).unwrap();
        assert_eq!(back, u);
    }

    #[test]
    fn session_update_agent_thought_chunk_keys() {
        let u = SessionUpdate::AgentThoughtChunk {
            content: TextContent::text("thinking"),
        };
        let v = serde_json::to_value(&u).unwrap();
        assert_eq!(v["sessionUpdate"], "agent_thought_chunk");
        assert_eq!(v["content"]["type"], "text");
        assert_eq!(v["content"]["text"], "thinking");
    }

    #[test]
    fn session_update_tool_call_keys() {
        let u = SessionUpdate::ToolCall {
            tool_call_id: "tc1".into(),
            title: "run".into(),
            kind: "execute".into(),
            status: ToolCallStatus::Pending,
            raw_input: json!({"cmd": "ls"}),
        };
        let v = serde_json::to_value(&u).unwrap();
        assert_eq!(v["sessionUpdate"], "tool_call");
        assert_eq!(v["toolCallId"], "tc1");
        assert_eq!(v["title"], "run");
        assert_eq!(v["kind"], "execute");
        assert_eq!(v["status"], "pending");
        assert_eq!(v["rawInput"]["cmd"], "ls");
        let back: SessionUpdate = serde_json::from_value(v).unwrap();
        assert_eq!(back, u);
    }

    #[test]
    fn session_update_tool_call_update_keys() {
        let u = SessionUpdate::ToolCallUpdate {
            tool_call_id: "tc1".into(),
            status: ToolCallStatus::Completed,
            raw_output: Some(json!({"ok": true})),
            content: Some(vec![ToolCallContentItem::Content {
                content: TextContent::text("done"),
            }]),
        };
        let v = serde_json::to_value(&u).unwrap();
        assert_eq!(v["sessionUpdate"], "tool_call_update");
        assert_eq!(v["toolCallId"], "tc1");
        assert_eq!(v["status"], "completed");
        assert_eq!(v["rawOutput"]["ok"], true);
        assert_eq!(v["content"][0]["type"], "content");
        assert_eq!(v["content"][0]["content"]["type"], "text");
        assert_eq!(v["content"][0]["content"]["text"], "done");
        let back: SessionUpdate = serde_json::from_value(v).unwrap();
        assert_eq!(back, u);
    }

    #[test]
    fn session_update_params_json_keys() {
        let p = SessionUpdateParams {
            session_id: "s1".into(),
            update: SessionUpdate::AgentMessageChunk {
                content: TextContent::text("x"),
            },
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["sessionId"], "s1");
        assert_eq!(v["update"]["sessionUpdate"], "agent_message_chunk");
    }

    #[test]
    fn tool_call_status_values() {
        assert_eq!(
            serde_json::to_value(ToolCallStatus::InProgress).unwrap(),
            json!("in_progress")
        );
        assert_eq!(
            serde_json::to_value(ToolCallStatus::Failed).unwrap(),
            json!("failed")
        );
    }
}
