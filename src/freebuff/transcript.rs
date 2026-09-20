//! Parser for `chat-messages.json` (JSON array).
//! Unknown fields are ignored; unknown block types become `Block::Other`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A message in the chat transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "variant", rename_all = "lowercase")]
pub enum Message {
    /// User message with text content.
    User { content: String },
    /// AI message with blocks.
    Ai {
        blocks: Vec<Block>,
        #[serde(default, rename = "isComplete")]
        is_complete: bool,
        #[serde(skip_serializing_if = "Option::is_none", rename = "completionTime")]
        completion_time: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        credits: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<Value>,
    },
    /// Mode divider or other variant.
    #[serde(other)]
    Other,
}

/// A block within an AI message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Block {
    /// Text block (reasoning or regular text).
    Text {
        #[serde(rename = "textType")]
        text_type: String,
        content: String,
    },
    /// Tool call block.
    Tool {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        input: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(rename = "agentId")]
        agent_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(rename = "includeToolCall")]
        include_tool_call: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(rename = "isCollapsed")]
        is_collapsed: Option<bool>,
    },
    /// Mode divider block.
    #[serde(rename = "mode-divider")]
    ModeDivider { mode: String },
    /// Unknown/other block type.
    #[serde(other)]
    Other,
}

impl Block {
    /// Check if this is a reasoning text block.
    pub fn is_reasoning(&self) -> bool {
        matches!(self, Block::Text { text_type, .. } if text_type == "reasoning")
    }

    /// Get the content of a text block.
    pub fn text_content(&self) -> Option<&str> {
        match self {
            Block::Text { content, .. } => Some(content),
            _ => None,
        }
    }

    /// Get mutable content of a text block.
    pub fn text_content_mut(&mut self) -> Option<&mut String> {
        match self {
            Block::Text { content, .. } => Some(content),
            _ => None,
        }
    }

    /// Get tool call ID if this is a tool block.
    pub fn tool_call_id(&self) -> Option<&str> {
        match self {
            Block::Tool { tool_call_id, .. } => Some(tool_call_id),
            _ => None,
        }
    }

    /// Get tool name if this is a tool block.
    pub fn tool_name(&self) -> Option<&str> {
        match self {
            Block::Tool { tool_name, .. } => Some(tool_name),
            _ => None,
        }
    }

    /// Get tool input if this is a tool block.
    pub fn tool_input(&self) -> Option<&Value> {
        match self {
            Block::Tool { input, .. } => Some(input),
            _ => None,
        }
    }

    /// Get tool output if this is a tool block.
    pub fn tool_output(&self) -> Option<&str> {
        match self {
            Block::Tool { output, .. } => output.as_deref(),
            _ => None,
        }
    }

    /// Set tool output (for when it becomes available).
    pub fn set_tool_output(&mut self, output: String) {
        if let Block::Tool {
            output: ref mut o, ..
        } = self
        {
            *o = Some(output);
        }
    }
}

/// Parse the chat-messages.json content.
pub fn parse_messages(json: &str) -> anyhow::Result<Vec<Message>> {
    let messages: Vec<Message> = serde_json::from_str(json)?;
    Ok(messages)
}

/// Cursor position in the transcript for streaming deltas.
/// Tracks the last seen AI message index and the length of each block's content.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cursor {
    /// Index of the last AI message we've processed.
    last_ai_index: usize,
    /// For each block in that AI message, the length of content we've already seen.
    block_lengths: Vec<usize>,
    /// Set of tool call IDs that have emitted ToolStarted.
    started_tools: Vec<String>,
    /// Set of tool call IDs that have emitted ToolFinished.
    finished_tools: Vec<String>,
}

impl Cursor {
    /// Create a new cursor at the beginning.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Delta emitted when the transcript advances.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Delta {
    /// New text content (reasoning or regular) appended to a block.
    Text { reasoning: bool, delta: String },
    /// A tool block was first seen.
    ToolStarted {
        tool_call_id: String,
        tool_name: String,
        input: Value,
    },
    /// A tool block's output became available.
    ToolFinished {
        tool_call_id: String,
        output: String,
    },
}

/// Compute deltas between a previous cursor and the current messages.
/// Only emits deltas for blocks in the LAST AI message.
/// Text deltas are the suffix beyond the previously seen length.
/// Tool blocks emit ToolStarted when first seen and ToolFinished when output first becomes Some.
pub fn diff(prev: &Cursor, messages: &[Message]) -> (Cursor, Vec<Delta>) {
    let mut deltas = Vec::new();

    // Find the last AI message
    let mut last_ai_idx = None;
    let mut last_ai_blocks = Vec::new();
    for (i, msg) in messages.iter().enumerate() {
        if let Message::Ai { blocks, .. } = msg {
            last_ai_idx = Some(i);
            last_ai_blocks = blocks.clone();
        }
    }

    // If no AI message, return empty
    let Some(ai_idx) = last_ai_idx else {
        return (prev.clone(), deltas);
    };

    // If this is a different AI message than before, reset tracking
    let mut cursor = if prev.last_ai_index == ai_idx {
        prev.clone()
    } else {
        Cursor {
            last_ai_index: ai_idx,
            block_lengths: vec![0; last_ai_blocks.len()],
            started_tools: Vec::new(),
            finished_tools: Vec::new(),
        }
    };

    // Ensure block_lengths matches current block count
    if cursor.block_lengths.len() != last_ai_blocks.len() {
        cursor.block_lengths.resize(last_ai_blocks.len(), 0);
    }

    // Process each block in the last AI message
    for (block_idx, block) in last_ai_blocks.iter().enumerate() {
        match block {
            Block::Text { text_type, content } => {
                let prev_len = cursor.block_lengths.get(block_idx).copied().unwrap_or(0);
                if content.len() > prev_len {
                    let delta = &content[prev_len..];
                    if !delta.is_empty() {
                        deltas.push(Delta::Text {
                            reasoning: text_type == "reasoning",
                            delta: delta.to_string(),
                        });
                    }
                    cursor.block_lengths[block_idx] = content.len();
                }
            }
            Block::Tool {
                tool_call_id,
                tool_name,
                input,
                output,
                ..
            } => {
                // Emit ToolStarted if not yet started
                if !cursor.started_tools.contains(tool_call_id) {
                    deltas.push(Delta::ToolStarted {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        input: input.clone(),
                    });
                    cursor.started_tools.push(tool_call_id.clone());
                }
                // Emit ToolFinished if output just appeared
                if output.is_some() && !cursor.finished_tools.contains(tool_call_id) {
                    if let Some(out) = output {
                        deltas.push(Delta::ToolFinished {
                            tool_call_id: tool_call_id.clone(),
                            output: out.clone(),
                        });
                        cursor.finished_tools.push(tool_call_id.clone());
                    }
                }
            }
            Block::ModeDivider { .. } | Block::Other => {
                // No deltas for these
            }
        }
    }

    (cursor, deltas)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_messages_loads_fixture() {
        let json = include_str!("../../tests/fixtures/transcript/chat-messages-small.json");
        let messages = parse_messages(json).unwrap();

        assert_eq!(messages.len(), 3);

        // First: AI message with mode-divider block
        match &messages[0] {
            Message::Ai {
                blocks,
                is_complete,
                ..
            } => {
                assert!(*is_complete);
                assert_eq!(blocks.len(), 1);
                assert!(matches!(&blocks[0], Block::ModeDivider { mode } if mode == "LITE"));
            }
            _ => panic!("expected AI message with mode-divider"),
        }

        // Second: user message
        assert!(matches!(&messages[1], Message::User { content } if content == "Hello, world!"));

        // Third: AI message with blocks
        match &messages[2] {
            Message::Ai {
                blocks,
                is_complete,
                ..
            } => {
                assert!(*is_complete);
                assert_eq!(blocks.len(), 3);

                // Block 0: reasoning text
                assert!(
                    matches!(&blocks[0], Block::Text { text_type, content } if text_type == "reasoning" && content == "The user said hello.")
                );

                // Block 1: tool with output
                assert!(
                    matches!(&blocks[1], Block::Tool { tool_call_id, tool_name, output, .. } if tool_call_id == "tool-123" && tool_name == "run_terminal_command" && *output == Some("hello\n".to_string()))
                );

                // Block 2: regular text
                assert!(
                    matches!(&blocks[2], Block::Text { text_type, content } if text_type == "text" && content == "Hello there!")
                );
            }
            _ => panic!("expected AI message"),
        }
    }

    #[test]
    fn diff_emits_text_deltas_as_content_grows() {
        let json = include_str!("../../tests/fixtures/transcript/chat-messages-small.json");
        let messages = parse_messages(json).unwrap();

        // First diff: cursor at start, should see all content from LAST AI message only
        let cursor = Cursor::new();
        let (cursor1, deltas1) = diff(&cursor, &messages);

        // Last AI message has 3 blocks: reasoning, tool (with output already), text
        // Tool block emits both ToolStarted and ToolFinished since output is present
        assert_eq!(deltas1.len(), 4);
        // Reasoning text delta
        assert!(
            matches!(&deltas1[0], Delta::Text { reasoning: true, delta } if delta == "The user said hello.")
        );
        // Tool started
        assert!(
            matches!(&deltas1[1], Delta::ToolStarted { tool_call_id, tool_name, .. } if tool_call_id == "tool-123" && tool_name == "run_terminal_command")
        );
        // Tool finished (output already present in fixture)
        assert!(
            matches!(&deltas1[2], Delta::ToolFinished { tool_call_id, output } if tool_call_id == "tool-123" && output == "hello\n")
        );
        // Text delta
        assert!(
            matches!(&deltas1[3], Delta::Text { reasoning: false, delta } if delta == "Hello there!")
        );

        // Second diff: same messages, no new content
        let (cursor2, deltas2) = diff(&cursor1, &messages);
        assert!(deltas2.is_empty());
        assert_eq!(cursor1, cursor2);

        // Third diff: simulate growing text in the last block
        let mut messages2 = messages.clone();
        if let Message::Ai { blocks, .. } = &mut messages2[2] {
            if let Block::Text { content, .. } = &mut blocks[2] {
                content.push_str(" How are you?");
            }
        }
        let (_cursor3, deltas3) = diff(&cursor2, &messages2);
        assert_eq!(deltas3.len(), 1);
        assert!(
            matches!(&deltas3[0], Delta::Text { reasoning: false, delta } if delta == " How are you?")
        );
    }

    #[test]
    fn diff_emits_tool_started_and_finished_once_each() {
        // Create a scenario where tool output appears later
        let json = r#"[
            {"variant": "ai", "blocks": [{"type": "tool", "toolCallId": "tool-1", "toolName": "run_terminal_command", "input": {"cmd": "echo hi"}}], "isComplete": false}
        ]"#;
        let messages1 = parse_messages(json).unwrap();

        let cursor = Cursor::new();
        let (cursor1, deltas1) = diff(&cursor, &messages1);
        assert_eq!(deltas1.len(), 1);
        assert!(matches!(&deltas1[0], Delta::ToolStarted { .. }));

        // Now with output
        let json2 = r#"[
            {"variant": "ai", "blocks": [{"type": "tool", "toolCallId": "tool-1", "toolName": "run_terminal_command", "input": {"cmd": "echo hi"}, "output": "hi\n"}], "isComplete": true}
        ]"#;
        let messages2 = parse_messages(json2).unwrap();
        let (cursor2, deltas2) = diff(&cursor1, &messages2);
        assert_eq!(deltas2.len(), 1);
        assert!(
            matches!(&deltas2[0], Delta::ToolFinished { tool_call_id, output } if tool_call_id == "tool-1" && output == "hi\n")
        );

        // Third diff: no new deltas
        let (_, deltas3) = diff(&cursor2, &messages2);
        assert!(deltas3.is_empty());
    }

    #[test]
    fn block_helpers() {
        let block = Block::Text {
            text_type: "reasoning".to_string(),
            content: "thinking".to_string(),
        };
        assert!(block.is_reasoning());
        assert_eq!(block.text_content(), Some("thinking"));

        let mut block2 = Block::Tool {
            tool_call_id: "id1".to_string(),
            tool_name: "test".to_string(),
            input: Value::Null,
            output: None,
            agent_id: None,
            include_tool_call: None,
            is_collapsed: None,
        };
        assert_eq!(block2.tool_call_id(), Some("id1"));
        assert_eq!(block2.tool_output(), None);
        block2.set_tool_output("done".to_string());
        assert_eq!(block2.tool_output(), Some("done"));
    }

    #[test]
    fn real_pong_transcript_parses_and_diffs() {
        let json = include_str!("../../tests/fixtures/transcript/chat-messages-real-pong.json");
        let result = parse_messages(json);

        if let Err(ref e) = result {
            eprintln!("Parse error: {e:#}");
        }

        let messages = result.expect("real pong transcript should parse");

        eprintln!("Parsed messages: {:#?}", messages);

        assert_eq!(messages.len(), 3, "expected 3 messages");

        // First: AI message with mode-divider block (may be Other or Ai with ModeDivider)
        match &messages[0] {
            Message::Ai { blocks, .. } => {
                assert_eq!(blocks.len(), 1);
                assert!(matches!(&blocks[0], Block::ModeDivider { mode } if mode == "LITE"));
            }
            Message::Other => {
                // Also acceptable per task description
            }
            _ => panic!(
                "expected AI message with mode-divider or Other, got {:?}",
                messages[0]
            ),
        }

        // Second: User message
        match &messages[1] {
            Message::User { content } => {
                assert_eq!(
                    content,
                    "Reply with exactly the single word PONG and nothing else."
                );
            }
            _ => panic!("expected User message, got {:?}", messages[1]),
        }

        // Third: AI message with reasoning and text blocks
        match &messages[2] {
            Message::Ai {
                blocks,
                is_complete,
                ..
            } => {
                assert!(*is_complete, "is_complete should be true");
                assert_eq!(blocks.len(), 2, "expected 2 blocks (reasoning + text)");

                // Block 0: reasoning text
                assert!(
                    matches!(&blocks[0], Block::Text { text_type, content } if text_type == "reasoning" && content == "The user wants exactly the single word PONG, nothing else. Simple compliance is right here — no tools needed, no extra text.")
                );

                // Block 1: regular text "PONG"
                assert!(
                    matches!(&blocks[1], Block::Text { text_type, content } if text_type == "text" && content == "PONG")
                );
            }
            _ => panic!("expected AI message, got {:?}", messages[2]),
        }

        // Now simulate driver's polling: diffs with carried cursor
        let mut cursor = Cursor::new();
        let mut all_deltas = Vec::new();

        // Snapshot 1: first message only
        let (c1, d1) = diff(&cursor, &messages[..1]);
        cursor = c1;
        all_deltas.extend(d1);

        // Snapshot 2: first two messages
        let (c2, d2) = diff(&cursor, &messages[..2]);
        cursor = c2;
        all_deltas.extend(d2);

        // Snapshot 3: all three messages
        let (c3, d3) = diff(&cursor, &messages[..3]);
        let _ = c3;
        all_deltas.extend(d3);

        // The union of deltas must contain a reasoning Text delta and a non-reasoning Text delta containing "PONG", each exactly once
        let reasoning_deltas: Vec<_> = all_deltas
            .iter()
            .filter(|d| {
                matches!(
                    d,
                    Delta::Text {
                        reasoning: true,
                        ..
                    }
                )
            })
            .collect();
        let text_deltas: Vec<_> = all_deltas
            .iter()
            .filter(|d| {
                matches!(
                    d,
                    Delta::Text {
                        reasoning: false,
                        ..
                    }
                )
            })
            .collect();

        assert_eq!(
            reasoning_deltas.len(),
            1,
            "expected exactly 1 reasoning delta, got {}",
            reasoning_deltas.len()
        );
        assert_eq!(
            text_deltas.len(),
            1,
            "expected exactly 1 text delta, got {}",
            text_deltas.len()
        );

        // Check the text delta contains "PONG"
        if let Delta::Text { delta, .. } = text_deltas[0] {
            assert!(
                delta.contains("PONG"),
                "text delta should contain PONG, got: {}",
                delta
            );
        }

        // Check reasoning delta has expected content
        if let Delta::Text { delta, .. } = reasoning_deltas[0] {
            assert!(
                delta.contains("Simple compliance"),
                "reasoning delta should contain expected text, got: {}",
                delta
            );
        }
    }
}
