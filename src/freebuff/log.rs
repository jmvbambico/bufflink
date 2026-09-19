//! Parser for `log.jsonl` (pino lines).
//! Each line: {level, timestamp, pid, hostname, msg, data?}

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A parsed log event from log.jsonl.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LogEvent {
    /// Turn started: `[send-message] Sending message with sdk run config`
    TurnStarted,
    /// Step start: `Start agent <model> step N`
    StepStart { step: u32 },
    /// Step end: `End agent <model> step N`
    StepEnd { step: u32 },
    /// Turn finished: `Main prompt finished` with data.outputType
    Finished { output_type: String },
    /// Agent execution failed: `Agent execution failed` with data.error.message
    Failed { message: String },
    /// Any other line.
    Other,
}

/// Parse a single log.jsonl line.
/// Never errors; malformed lines become `LogEvent::Other`.
pub fn parse_log_line(line: &str) -> LogEvent {
    let line = line.trim();
    if line.is_empty() {
        return LogEvent::Other;
    }

    // Try to parse as JSON
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return LogEvent::Other;
    };

    // Extract msg field
    let msg = match value.get("msg").and_then(|v| v.as_str()) {
        Some(m) => m,
        None => return LogEvent::Other,
    };

    // TurnStarted
    if msg == "[send-message] Sending message with sdk run config" {
        return LogEvent::TurnStarted;
    }

    // StepStart: "Start agent <model> step N"
    if let Some(step) = parse_step_number(msg, "Start agent ") {
        return LogEvent::StepStart { step };
    }

    // StepEnd: "End agent <model> step N"
    if let Some(step) = parse_step_number(msg, "End agent ") {
        return LogEvent::StepEnd { step };
    }

    // Finished: "Main prompt finished" with data.outputType
    if msg == "Main prompt finished" {
        let output_type = value
            .get("data")
            .and_then(|d| d.get("outputType"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        return LogEvent::Finished { output_type };
    }

    // Failed: "Agent execution failed" with data.error.message
    if msg == "Agent execution failed" {
        let message = value
            .get("data")
            .and_then(|d| d.get("error"))
            .and_then(|e| e.get("message"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        return LogEvent::Failed { message };
    }

    LogEvent::Other
}

/// Parse step number from "Start agent <model> step N" or "End agent <model> step N"
fn parse_step_number(msg: &str, prefix: &str) -> Option<u32> {
    if let Some(rest) = msg.strip_prefix(prefix) {
        // rest should be like "GLM 5.3 Flash step 1"
        if let Some(step_str) = rest.split("step ").nth(1) {
            return step_str.trim().parse().ok();
        }
    }
    None
}

/// Outcome of a turn based on log events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed,
    Interrupted,
    Error { message: String },
}

/// Determine the turn outcome from a sequence of log events.
/// Returns None until a Finished event is seen.
/// Finished{lastMessage} -> Completed
/// Finished{error} preceded by Failed{"user-interrupt"} -> Interrupted
/// Finished{error} otherwise -> Error{last Failed message or "unknown"}
pub fn outcome(events: &[LogEvent]) -> Option<TurnOutcome> {
    let mut last_failed_message: Option<String> = None;

    for event in events {
        match event {
            LogEvent::Failed { message } => {
                last_failed_message = Some(message.clone());
            }
            LogEvent::Finished { output_type } => {
                if output_type == "lastMessage" {
                    return Some(TurnOutcome::Completed);
                } else if output_type == "error" {
                    if let Some(ref msg) = last_failed_message {
                        if msg == "user-interrupt" {
                            return Some(TurnOutcome::Interrupted);
                        }
                    }
                    return Some(TurnOutcome::Error {
                        message: last_failed_message.unwrap_or_else(|| "unknown".to_string()),
                    });
                } else {
                    // Unknown outputType, treat as completed
                    return Some(TurnOutcome::Completed);
                }
            }
            _ => {}
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_log_line_turn_started() {
        let line = r#"{"level":30,"timestamp":"2026-09-19T14:37:47.705Z","pid":88366,"hostname":"mac","msg":"[send-message] Sending message with sdk run config","data":{}}"#;
        assert_eq!(parse_log_line(line), LogEvent::TurnStarted);
    }

    #[test]
    fn parse_log_line_step_start() {
        let line = r#"{"level":30,"timestamp":"2026-09-19T14:37:48.000Z","pid":88366,"hostname":"mac","msg":"Start agent GLM 5.3 Flash step 1","data":{}}"#;
        assert_eq!(parse_log_line(line), LogEvent::StepStart { step: 1 });
    }

    #[test]
    fn parse_log_line_step_end() {
        let line = r#"{"level":30,"timestamp":"2026-09-19T14:37:50.000Z","pid":88366,"hostname":"mac","msg":"End agent GLM 5.3 Flash step 1","data":{}}"#;
        assert_eq!(parse_log_line(line), LogEvent::StepEnd { step: 1 });
    }

    #[test]
    fn parse_log_line_finished_success() {
        let line = r#"{"level":30,"timestamp":"2026-09-19T14:37:56.000Z","pid":88366,"hostname":"mac","msg":"Main prompt finished","data":{"outputType":"lastMessage"}}"#;
        assert_eq!(
            parse_log_line(line),
            LogEvent::Finished {
                output_type: "lastMessage".to_string()
            }
        );
    }

    #[test]
    fn parse_log_line_finished_error() {
        let line = r#"{"level":30,"timestamp":"2026-09-19T14:41:07.000Z","pid":88366,"hostname":"mac","msg":"Main prompt finished","data":{"outputType":"error"}}"#;
        assert_eq!(
            parse_log_line(line),
            LogEvent::Finished {
                output_type: "error".to_string()
            }
        );
    }

    #[test]
    fn parse_log_line_failed_user_interrupt() {
        let line = r#"{"level":30,"timestamp":"2026-09-19T14:41:06.000Z","pid":88366,"hostname":"mac","msg":"Agent execution failed","data":{"error":{"name":"Error","message":"user-interrupt"}}}"#;
        assert_eq!(
            parse_log_line(line),
            LogEvent::Failed {
                message: "user-interrupt".to_string()
            }
        );
    }

    #[test]
    fn parse_log_line_failed_other() {
        let line = r#"{"level":30,"timestamp":"2026-09-19T14:41:06.000Z","pid":88366,"hostname":"mac","msg":"Agent execution failed","data":{"error":{"name":"Error","message":"timeout"}}}"#;
        assert_eq!(
            parse_log_line(line),
            LogEvent::Failed {
                message: "timeout".to_string()
            }
        );
    }

    #[test]
    fn parse_log_line_malformed_becomes_other() {
        assert_eq!(parse_log_line("not json"), LogEvent::Other);
        assert_eq!(parse_log_line(""), LogEvent::Other);
        assert_eq!(parse_log_line("{}"), LogEvent::Other);
    }

    #[test]
    fn outcome_completed() {
        let events = vec![
            LogEvent::TurnStarted,
            LogEvent::StepStart { step: 1 },
            LogEvent::StepEnd { step: 1 },
            LogEvent::Finished {
                output_type: "lastMessage".to_string(),
            },
        ];
        assert_eq!(outcome(&events), Some(TurnOutcome::Completed));
    }

    #[test]
    fn outcome_interrupted() {
        let events = vec![
            LogEvent::TurnStarted,
            LogEvent::StepStart { step: 1 },
            LogEvent::Failed {
                message: "user-interrupt".to_string(),
            },
            LogEvent::Finished {
                output_type: "error".to_string(),
            },
        ];
        assert_eq!(outcome(&events), Some(TurnOutcome::Interrupted));
    }

    #[test]
    fn outcome_error() {
        let events = vec![
            LogEvent::TurnStarted,
            LogEvent::StepStart { step: 1 },
            LogEvent::Failed {
                message: "timeout".to_string(),
            },
            LogEvent::Finished {
                output_type: "error".to_string(),
            },
        ];
        assert_eq!(
            outcome(&events),
            Some(TurnOutcome::Error {
                message: "timeout".to_string()
            })
        );
    }

    #[test]
    fn outcome_none_until_finished() {
        let events = vec![
            LogEvent::TurnStarted,
            LogEvent::StepStart { step: 1 },
            LogEvent::StepEnd { step: 1 },
        ];
        assert_eq!(outcome(&events), None);
    }

    #[test]
    fn parse_log_success_fixture() {
        let content = include_str!("../../tests/fixtures/log/log-success.jsonl");
        let events: Vec<LogEvent> = content.lines().map(parse_log_line).collect();
        assert_eq!(events.len(), 6);
        assert_eq!(events[0], LogEvent::TurnStarted);
        assert_eq!(events[1], LogEvent::StepStart { step: 1 });
        assert_eq!(events[2], LogEvent::StepEnd { step: 1 });
        assert_eq!(events[3], LogEvent::StepStart { step: 2 });
        assert_eq!(events[4], LogEvent::StepEnd { step: 2 });
        assert_eq!(
            events[5],
            LogEvent::Finished {
                output_type: "lastMessage".to_string()
            }
        );
        assert_eq!(outcome(&events), Some(TurnOutcome::Completed));
    }

    #[test]
    fn parse_log_interrupted_fixture() {
        let content = include_str!("../../tests/fixtures/log/log-interrupted.jsonl");
        let events: Vec<LogEvent> = content.lines().map(parse_log_line).collect();
        assert_eq!(events.len(), 6);
        assert_eq!(events[0], LogEvent::TurnStarted);
        assert_eq!(events[1], LogEvent::StepStart { step: 1 });
        assert_eq!(events[2], LogEvent::StepEnd { step: 1 });
        assert_eq!(events[3], LogEvent::StepStart { step: 2 });
        assert_eq!(
            events[4],
            LogEvent::Failed {
                message: "user-interrupt".to_string()
            }
        );
        assert_eq!(
            events[5],
            LogEvent::Finished {
                output_type: "error".to_string()
            }
        );
        assert_eq!(outcome(&events), Some(TurnOutcome::Interrupted));
    }
}
