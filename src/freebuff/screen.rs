//! Screen classifier for freebuff TUI.
//! The PTY layer supplies `Vec<String>` rows with ANSI already stripped.

/// Possible screen states detected from the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenState {
    /// Still showing the FREEBUFF banner animation, no other markers.
    Booting,
    /// Model picker splash: "Start coding for free", "See all 4 models", "Freebucks/hr".
    ModelSplash,
    /// Freebucks gate interstitial: "Not enough Freebucks — ...".
    FreebucksGate { message: String },
    /// "Freebuff is already running" dialog with Take over / Exit buttons.
    AlreadyRunning,
    /// Idle prompt ready for input: status shows "· <n>h/m left" and input placeholder visible.
    Idle,
    /// Generating a reply: status bar shows "thinking... <N>s" or "working... <N>s".
    Busy { elapsed_s: Option<u32> },
    /// "Another freebuff instance took over this account."
    KickedOut,
    /// None of the above patterns matched.
    Unknown,
}

/// Key pressed to accept the model splash (Enter).
pub const SPLASH_ACCEPT_KEY: &str = "\r";

/// Classify the current screen state from a list of text rows (ANSI already stripped).
pub fn classify(rows: &[String]) -> ScreenState {
    // Precedence: FreebucksGate > AlreadyRunning > KickedOut > Busy > ModelSplash > Idle > Booting > Unknown

    // Check for FreebucksGate first (inside the model box)
    for row in rows {
        if row.contains("Not enough Freebucks") {
            return ScreenState::FreebucksGate {
                message: row.trim().to_string(),
            };
        }
    }

    // Check for AlreadyRunning
    for row in rows {
        if row.contains("Freebuff is already running") {
            return ScreenState::AlreadyRunning;
        }
    }

    // Check for KickedOut
    for row in rows {
        if row.contains("Another freebuff instance took over this account") {
            return ScreenState::KickedOut;
        }
    }

    // Check for Busy: status bar matches "(thinking|working)\.\.\. (\d+)s"
    for row in rows {
        if let Some(elapsed) = parse_busy_elapsed(row) {
            return ScreenState::Busy {
                elapsed_s: Some(elapsed),
            };
        }
        // Also match without captured seconds (just in case)
        if (row.contains("thinking...") || row.contains("working...")) && row.contains("Esc") {
            return ScreenState::Busy { elapsed_s: None };
        }
    }

    // Check for ModelSplash
    let has_start_coding = rows.iter().any(|r| r.contains("Start coding for free"));
    let has_see_all_models = rows.iter().any(|r| r.contains("See all 4 models"));
    let has_freebucks_hr = rows.iter().any(|r| r.contains("Freebucks/hr"));
    if has_start_coding || has_see_all_models || has_freebucks_hr {
        return ScreenState::ModelSplash;
    }

    // Check for Idle: status bar has "· <n>h left" or "· <n>m left" AND
    // (input placeholder visible OR "✕ End session" visible) AND not Busy
    let has_time_left = rows
        .iter()
        .any(|r| r.contains("·") && (r.contains("h left") || r.contains("m left")));
    let has_placeholder = rows
        .iter()
        .any(|r| r.contains("Enter a coding task or / for commands"));
    let has_end_session = rows.iter().any(|r| r.contains("✕ End session"));
    if has_time_left && (has_placeholder || has_end_session) {
        return ScreenState::Idle;
    }

    // Check for Booting: only banner (█ characters) with no other markers
    let has_banner = rows.iter().any(|r| r.contains('█'));
    let has_any_marker = rows.iter().any(|r| {
        r.contains("Start coding for free")
            || r.contains("Freebuff will run commands")
            || r.contains("Directory ")
            || r.contains("Enter a coding task")
            || r.contains("thinking...")
            || r.contains("working...")
            || r.contains("Not enough Freebucks")
            || r.contains("Freebuff is already running")
            || r.contains("Another freebuff instance")
            || r.contains("See all 4 models")
            || r.contains("Freebucks/hr")
    });
    if has_banner && !has_any_marker && !rows.is_empty() {
        return ScreenState::Booting;
    }

    // Empty rows also count as Booting (very early launch)
    if rows.is_empty() || rows.iter().all(|r| r.trim().is_empty()) {
        return ScreenState::Booting;
    }

    ScreenState::Unknown
}

/// Parse the elapsed seconds from a busy status bar row.
/// Matches "thinking... <N>s" or "working... <N>s" where N is digits.
fn parse_busy_elapsed(row: &str) -> Option<u32> {
    // Look for "thinking... " or "working... " followed by digits and 's'
    let prefixes = ["thinking... ", "working... "];
    for prefix in prefixes {
        if let Some(idx) = row.find(prefix) {
            let after = &row[idx + prefix.len()..];
            // Find digits followed by 's'
            let mut num_str = String::new();
            for ch in after.chars() {
                if ch.is_ascii_digit() {
                    num_str.push(ch);
                } else if ch == 's' && !num_str.is_empty() {
                    return num_str.parse().ok();
                } else if !num_str.is_empty() {
                    // Digits ended but not followed by 's'
                    break;
                }
            }
        }
    }
    None
}

/// Extract the text inside the bordered input box.
/// The box is bordered by ╭…╮ (top) and ╰…╯ (bottom).
/// Returns the text content trimmed, without the ▍ cursor glyph.
/// Returns None if no input box is found.
pub fn input_box_text(rows: &[String]) -> Option<String> {
    let mut in_box = false;
    let mut box_lines = Vec::new();

    for row in rows {
        if row.contains('╭') && row.contains('╮') {
            in_box = true;
            continue;
        }
        if row.contains('╰') && row.contains('╯') && in_box {
            break;
        }
        if in_box {
            // Strip border characters (│) and surrounding whitespace
            let stripped = row.trim_start_matches('│').trim_end_matches('│').trim();
            box_lines.push(stripped.to_string());
        }
    }

    if box_lines.is_empty() {
        return None;
    }

    // Join lines, remove the cursor glyph ▍, trim
    let text = box_lines.join("\n").replace('▍', "").trim().to_string();

    if text.is_empty() {
        Some(String::new())
    } else {
        Some(text)
    }
}

/// Parse the pasted-text chip count from rows.
/// Finds a row containing `Pasted text (` and parses the number before ` chars)`.
/// Digits may include thousands separators (`,`). Returns `Some(count)` or `None`.
pub fn pasted_chip_chars(rows: &[String]) -> Option<u64> {
    for row in rows {
        if let Some(start) = row.find("Pasted text (") {
            let after = &row[start + "Pasted text (".len()..];
            if let Some(end) = after.find(" chars)") {
                let num_str = after[..end].replace(',', "");
                return num_str.parse::<u64>().ok();
            }
        }
    }
    None
}

/// Check if the input box is empty (showing the placeholder).
pub fn input_box_is_empty(rows: &[String]) -> bool {
    input_box_text(rows)
        .map(|t| t.is_empty() || t.contains("Enter a coding task or / for commands"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:expr) => {
            include_str!(concat!("../../tests/fixtures/screens/", $name, ".txt"))
        };
    }

    fn load_fixture(name: &str) -> Vec<String> {
        let content = match name {
            "splash-120x40" => fixture!("splash-120x40"),
            "splash-80x24" => fixture!("splash-80x24"),
            "idle-120x40" => fixture!("idle-120x40"),
            "busy-120x40" => fixture!("busy-120x40"),
            "after-esc" => fixture!("after-esc"),
            "kicked-out" => fixture!("kicked-out"),
            "freebucks-gate-80x24" => fixture!("freebucks-gate-80x24"),
            "slash-menu" => fixture!("slash-menu"),
            "paste-chip-120x40" => fixture!("paste-chip-120x40"),
            _ => panic!("unknown fixture: {}", name),
        };
        content.lines().map(|s| s.to_string()).collect()
    }

    #[test]
    fn splash_120x40_classifies_as_model_splash() {
        let rows = load_fixture("splash-120x40");
        assert_eq!(classify(&rows), ScreenState::ModelSplash);
    }

    #[test]
    fn splash_80x24_classifies_as_model_splash() {
        let rows = load_fixture("splash-80x24");
        assert_eq!(classify(&rows), ScreenState::ModelSplash);
    }

    #[test]
    fn idle_120x40_classifies_as_idle() {
        let rows = load_fixture("idle-120x40");
        assert_eq!(classify(&rows), ScreenState::Idle);
    }

    #[test]
    fn busy_120x40_classifies_as_busy() {
        let rows = load_fixture("busy-120x40");
        let state = classify(&rows);
        assert!(matches!(state, ScreenState::Busy { .. }));
        // Check elapsed is parsed
        if let ScreenState::Busy { elapsed_s } = state {
            assert_eq!(elapsed_s, Some(4));
        }
    }

    #[test]
    fn after_esc_classifies_as_idle() {
        let rows = load_fixture("after-esc");
        assert_eq!(classify(&rows), ScreenState::Idle);
    }

    #[test]
    fn already_running_dialog_classifies_as_already_running() {
        // We don't have a fixture for this exact screen, but we can test the logic
        let rows = vec![
            "Freebuff is already running".to_string(),
            "Only one freebuff instance is allowed at a time.".to_string(),
        ];
        assert_eq!(classify(&rows), ScreenState::AlreadyRunning);
    }

    #[test]
    fn kicked_out_classifies_as_kicked_out() {
        let rows = load_fixture("kicked-out");
        assert_eq!(classify(&rows), ScreenState::KickedOut);
    }

    #[test]
    fn freebucks_gate_classifies_as_freebucks_gate() {
        let rows = load_fixture("freebucks-gate-80x24");
        let state = classify(&rows);
        assert!(matches!(state, ScreenState::FreebucksGate { .. }));
        if let ScreenState::FreebucksGate { message } = state {
            assert!(message.contains("Not enough Freebucks"));
            assert!(message.contains("5 Freebucks/hr against 0 left"));
        }
    }

    #[test]
    fn slash_menu_classifies_as_idle_or_unknown() {
        let rows = load_fixture("slash-menu");
        let state = classify(&rows);
        // Slash menu shows "GLM 5.3 Flash · 56m left · 20.7K (2%)" and "✕ End session"
        // and the input box with "/" - this matches Idle criteria
        assert_eq!(state, ScreenState::Idle);
    }

    #[test]
    fn input_box_text_extracts_content() {
        let rows = load_fixture("idle-120x40");
        let text = input_box_text(&rows);
        assert_eq!(
            text,
            Some("Enter a coding task or / for commands".to_string())
        );
    }

    #[test]
    fn input_box_text_empty_when_placeholder() {
        let rows = load_fixture("idle-120x40");
        assert!(input_box_is_empty(&rows));
    }

    #[test]
    fn input_box_text_with_user_input() {
        let rows = vec![
            "╭────────────────────────────────────────╮".to_string(),
            "│  ▍some user input                      │".to_string(),
            "│                                        │".to_string(),
            "╰────────────────────────────────────────╯".to_string(),
        ];
        let text = input_box_text(&rows);
        assert_eq!(text, Some("some user input".to_string()));
        assert!(!input_box_is_empty(&rows));
    }

    #[test]
    fn input_box_text_none_when_no_box() {
        let rows = vec!["Just some text".to_string(), "More text".to_string()];
        assert_eq!(input_box_text(&rows), None);
    }

    #[test]
    fn paste_chip_parses_count() {
        let rows = load_fixture("paste-chip-120x40");
        assert_eq!(pasted_chip_chars(&rows), Some(5001));
    }

    #[test]
    fn paste_chip_none_when_no_chip() {
        let rows = load_fixture("idle-120x40");
        assert_eq!(pasted_chip_chars(&rows), None);
    }

    #[test]
    fn paste_chip_classifies_as_idle() {
        let rows = load_fixture("paste-chip-120x40");
        assert_eq!(classify(&rows), ScreenState::Idle);
    }

    #[test]
    fn pasted_chip_chars_edge_cases() {
        let cases = [
            (" 📋 Pasted text (12,345 chars)", Some(12345)),
            ("Pasted text (0 chars)", Some(0)),
            ("note before 📋 Pasted text (7 chars) and after", Some(7)),
            ("Pasted text (chars)", None),
            ("nothing here", None),
        ];
        for (input, expected) in cases {
            assert_eq!(
                pasted_chip_chars(&[input.to_string()]),
                expected,
                "input: {input}"
            );
        }
    }
}
