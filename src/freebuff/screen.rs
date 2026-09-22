//! Screen classifier for freebuff TUI.
//! The PTY layer supplies `Vec<String>` rows with ANSI already stripped.

/// Possible screen states detected from the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenState {
    /// Still showing the FREEBUFF banner animation, no other markers.
    Booting,
    /// Model picker splash: "Start coding for free", "See all 4 models", "Freebucks/hr".
    ModelSplash,
    /// Expanded model picker: 2+ model boxes, or "Show fewer" visible.
    ModelList,
    /// Previous session was hard-killed: "Session ended · N Freebucks left"
    /// with "Press Enter to continue in a new session". Esc leads to the
    /// normal splash; Enter would resume with the previous model.
    SessionEnded,
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
    // Precedence: FreebucksGate > AlreadyRunning > SessionEnded > KickedOut > Busy > ModelList > ModelSplash > Idle > Booting > Unknown

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

    // Check for SessionEnded: a hard-killed previous session offers
    // "Press Enter to continue in a new session" (resume) or Esc (splash).
    // Never Enter here: resuming spends Freebucks on the previous model.
    let has_session_ended = rows.iter().any(|r| r.contains("Session ended"));
    let has_press_enter = rows.iter().any(|r| r.contains("Press Enter to continue"));
    if has_session_ended && has_press_enter {
        return ScreenState::SessionEnded;
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

    // Check for ModelList (expanded splash) before ModelSplash: 2+ model
    // boxes, or "Show fewer" visible. The collapsed splash has exactly one.
    let has_show_fewer = rows.iter().any(|r| r.contains("Show fewer"));
    if has_show_fewer || model_rows(rows).len() >= 2 {
        return ScreenState::ModelList;
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
            || r.contains("Session ended")
            || r.contains("Press Enter to continue")
            || r.contains("Show fewer")
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

/// One model entry on the splash, collapsed or in the expanded list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRow {
    /// Display name: the text between the focus marker and the first ` · `.
    pub name: String,
    /// True when the row carries the `›` focus marker.
    pub focused: bool,
    /// The `Freebucks/hr` line inside the same box, if visible (an 80x24
    /// viewport can cut the focused last box off mid-render).
    pub cost_line: Option<String>,
}

/// Parse the model rows out of a splash screen (collapsed or expanded).
/// A model row is a `│`-bordered row whose inner text starts with `›`
/// (focused) or directly with the name (unfocused) and holds the name before
/// the first ` · `. Rows mentioning Freebucks are cost lines, not names.
pub fn model_rows(rows: &[String]) -> Vec<ModelRow> {
    let mut out = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let Some(inner) = box_inner(row) else {
            continue;
        };
        let (focused, body) = match inner.strip_prefix('›') {
            Some(rest) => (true, rest.trim()),
            None => (false, inner),
        };
        if body.contains("Freebucks") {
            continue;
        }
        let Some(sep) = body.find(" · ") else {
            continue;
        };
        let name = body[..sep].trim();
        if name.is_empty() {
            continue;
        }
        // Cost line: first "Freebucks/hr" row further down inside the same box.
        let mut cost_line = None;
        for next in rows.iter().skip(i + 1) {
            let Some(next_inner) = box_inner(next) else {
                break;
            };
            if next_inner.contains("Freebucks/hr") {
                cost_line = Some(next_inner.trim().to_string());
                break;
            }
            // A new name row or a box border ends this box.
            if next_inner.contains(" · ") || next.contains('└') || next.contains('┘') {
                break;
            }
        }
        out.push(ModelRow {
            name: name.to_string(),
            focused,
            cost_line,
        });
    }
    out
}

/// Inner text of a `│`-bordered row, trimmed. Returns None for rows without a
/// border. Tolerates a missing right border (partial box at 80x24) and
/// trailing scrollbar glyphs (`█▀▄`).
fn box_inner(row: &str) -> Option<&str> {
    let (_, after_left) = row.split_once('│')?;
    let inner = match after_left.rsplit_once('│') {
        Some((mid, _)) => mid,
        None => after_left,
    };
    Some(inner.trim().trim_end_matches(['█', '▀', '▄']).trim())
}

/// Normalise a model name or `BLINK_MODEL` fragment for comparison:
/// lowercase, ignoring `/`, `-`, `_`, `.` and whitespace. The lineup rotates,
/// so matching is on the display name, never a hardcoded id.
pub fn normalize_model(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '/' | '-' | '_' | '.' | ' ' | '\t'))
        .collect()
}

/// Outcome of matching `BLINK_MODEL` against the splash's model rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelMatch {
    /// Exactly one row matched.
    One(usize),
    /// Nothing matched; carries the offered names for the error message.
    None { offered: Vec<String> },
    /// Two or more rows matched; carries their names for the error message.
    Ambiguous(Vec<String>),
}

/// Match a `BLINK_MODEL` target against model display names. A row matches
/// when the normalised target is a substring of the normalised model NAME,
/// or the normalised name is a substring of the normalised last `/` segment
/// of the target — so `deepseek/deepseek-v4.1-flash`, `deepseek-v4-1-flash`
/// and `DeepSeek V4.1 Flash` all resolve, while `deepseek-v4-pro` does not.
pub fn match_model(target: &str, names: &[String]) -> ModelMatch {
    let want = normalize_model(target);
    let last_seg = target.rsplit('/').next().unwrap_or(target);
    let want_seg = normalize_model(last_seg);
    let mut hits = Vec::new();
    if !want.is_empty() {
        for (i, name) in names.iter().enumerate() {
            let norm = normalize_model(name);
            if !norm.is_empty() && (norm.contains(&want) || want_seg.contains(&norm)) {
                hits.push(i);
            }
        }
    }
    match hits.len() {
        0 => ModelMatch::None {
            offered: names.to_vec(),
        },
        1 => ModelMatch::One(hits[0]),
        _ => ModelMatch::Ambiguous(hits.into_iter().map(|i| names[i].clone()).collect()),
    }
}

/// Active model and time left from the Idle screen's status row, e.g.
/// ` MiMo 2.5 · 58m left … ✕ End session` -> `("MiMo 2.5", "58m left")`.
/// The name is the trimmed text before the first ` · `; the time is the
/// `\d+[hm] left` fragment. Rows mentioning Freebucks are splash cost lines,
/// not the status row. Returns None when no row parses: within a running
/// hour freebuff skips the splash and resumes on the hour's model, so the
/// status row is the only place the active model is shown — callers must
/// fail loudly rather than assume the requested model is active.
pub fn active_model(rows: &[String]) -> Option<(String, String)> {
    for row in rows {
        if row.contains("Freebucks") {
            continue;
        }
        if !row.contains('·') || !row.contains("left") {
            continue;
        }
        let Some(sep) = row.find(" · ") else {
            continue;
        };
        let name = row[..sep].trim();
        if name.is_empty() {
            continue;
        }
        // The status row is `<Name> · <N>[hm] left`: the time fragment sits
        // immediately after the separator, so anything else there (a mode
        // word, a transcript line) is not the status row.
        let after = row[sep + " · ".len()..].trim_start();
        let Some(time) = time_left_at_start(after) else {
            continue;
        };
        // After the time only the usage suffix (`· …`) or `✕ End session`
        // may follow; trailing prose (`5m left on the problem`) is a
        // transcript line, not the status row.
        let rest = after[time.len()..].trim_start();
        if !(rest.is_empty() || rest.starts_with('·') || rest.starts_with('✕')) {
            continue;
        }
        return Some((name.to_string(), time.to_string()));
    }
    None
}

/// The `\d+[hm] left` fragment at the START of `s` (`58m left`, `1h left`).
/// Anchored so callers can require the time immediately after the ` · `
/// separator instead of matching it anywhere in the row.
fn time_left_at_start(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut j = 0;
    while j < bytes.len() && bytes[j].is_ascii_digit() {
        j += 1;
    }
    if j == 0 || j >= bytes.len() {
        return None;
    }
    if bytes[j] != b'h' && bytes[j] != b'm' {
        return None;
    }
    if s[j + 1..].starts_with(" left") {
        Some(&s[..j + 1 + " left".len()])
    } else {
        None
    }
}

/// Decide whether an already-running hour satisfies `BLINK_MODEL`, using the
/// same [`match_model`] rule as the splash: against a single active name the
/// outcome is match or already-running error (never ambiguous).
pub fn check_active_model(target: &str, active: &str, time_left: &str) -> Result<(), String> {
    match match_model(target, &[active.to_string()]) {
        ModelMatch::One(_) => Ok(()),
        _ => Err(format!(
            "freebuff: an hour on '{active}' is already running ({time_left}); requested '{target}' — wait for it to end or unset BLINK_MODEL"
        )),
    }
}

/// One step of the model-selection walk over the expanded list: pure so it is
/// unit-testable without a PTY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectStep {
    /// Focus is on the uniquely matching row: press Enter to start the hour.
    Accept,
    /// Move focus one row down, then re-read the screen.
    Down,
    /// Focus reached `Show fewer` with no match: fail with "not offered".
    NotOffered { offered: Vec<String> },
    /// Two or more rows match: fail, naming them.
    Ambiguous(Vec<String>),
}

/// Decide the next keystroke for `BLINK_MODEL` on the expanded model list.
/// `presses` is how many Down presses have been sent so far (cap: 12).
pub fn plan_select_step(
    target: &str,
    rows: &[ModelRow],
    show_fewer: bool,
    presses: u32,
) -> SelectStep {
    const MAX_DOWN_PRESSES: u32 = 12;
    let names: Vec<String> = rows.iter().map(|r| r.name.clone()).collect();
    match match_model(target, &names) {
        ModelMatch::Ambiguous(names) => SelectStep::Ambiguous(names),
        ModelMatch::None { offered } => {
            if presses >= MAX_DOWN_PRESSES || (show_fewer && !rows.iter().any(|r| r.focused)) {
                SelectStep::NotOffered { offered }
            } else {
                SelectStep::Down
            }
        }
        ModelMatch::One(i) => {
            if rows[i].focused {
                SelectStep::Accept
            } else if presses >= MAX_DOWN_PRESSES || (show_fewer && !rows.iter().any(|r| r.focused))
            {
                // Focus never landed on the match (e.g. the walk reached
                // `Show fewer`, or the list kept scrolling): fail loudly
                // rather than pressing Enter on the wrong row.
                SelectStep::NotOffered { offered: names }
            } else {
                SelectStep::Down
            }
        }
    }
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
            "session-ended-120x40" => fixture!("session-ended-120x40"),
            "splash-list-120x40" => fixture!("splash-list-120x40"),
            "splash-list-focus4-120x40" => fixture!("splash-list-focus4-120x40"),
            "splash-list-showfewer-120x40" => fixture!("splash-list-showfewer-120x40"),
            "splash-list-80x24-focus4" => fixture!("splash-list-80x24-focus4"),
            "splash-list-80x24-focus1" => fixture!("splash-list-80x24-focus1"),
            "idle-120x40" => fixture!("idle-120x40"),
            "idle-midhour-120x40" => fixture!("idle-midhour-120x40"),
            "busy-120x40" => fixture!("busy-120x40"),
            "busy-first-instant-120x40" => fixture!("busy-first-instant-120x40"),
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
    fn session_ended_classifies_as_session_ended() {
        let rows = load_fixture("session-ended-120x40");
        assert_eq!(classify(&rows), ScreenState::SessionEnded);
    }

    #[test]
    fn session_ended_needs_both_markers() {
        let only_banner = vec!["Session ended  ·  25 Freebucks left".to_string()];
        assert_ne!(classify(&only_banner), ScreenState::SessionEnded);
        let only_enter = vec!["Press Enter to continue in a new session".to_string()];
        assert_ne!(classify(&only_enter), ScreenState::SessionEnded);
    }

    #[test]
    fn expanded_list_classifies_as_model_list() {
        for name in [
            "splash-list-120x40",
            "splash-list-focus4-120x40",
            "splash-list-showfewer-120x40",
            "splash-list-80x24-focus4",
            "splash-list-80x24-focus1",
        ] {
            let rows = load_fixture(name);
            assert_eq!(classify(&rows), ScreenState::ModelList, "{name}");
        }
    }

    #[test]
    fn collapsed_splash_still_classifies_as_model_splash() {
        // model_rows() sees exactly one box on the collapsed splash, so it
        // must not be promoted to ModelList.
        for name in ["splash-120x40", "splash-80x24"] {
            let rows = load_fixture(name);
            assert_eq!(model_rows(&rows).len(), 1, "{name}");
            assert_eq!(classify(&rows), ScreenState::ModelSplash, "{name}");
        }
    }

    #[test]
    fn model_rows_parses_expanded_list() {
        let rows = load_fixture("splash-list-120x40");
        let models = model_rows(&rows);
        let names: Vec<&str> = models.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "GLM 5.3 Flash",
                "MiMo 2.5",
                "Solar Pro 4",
                "DeepSeek V4.1 Flash"
            ]
        );
        assert!(models[0].focused);
        assert!(models[1..].iter().all(|m| !m.focused));
        assert_eq!(models[0].cost_line.as_deref(), Some("5 Freebucks/hr"));
        assert_eq!(models[1].cost_line.as_deref(), Some("10 Freebucks/hr"));
        assert!(
            models[3]
                .cost_line
                .as_deref()
                .is_some_and(|c| c.contains("Off-peak")),
            "deepseek cost line: {:?}",
            models[3].cost_line
        );
    }

    #[test]
    fn model_rows_tracks_focus_on_fourth() {
        let rows = load_fixture("splash-list-focus4-120x40");
        let models = model_rows(&rows);
        assert_eq!(models.len(), 4);
        assert_eq!(models[3].name, "DeepSeek V4.1 Flash");
        assert!(models[3].focused);
        assert!(models[..3].iter().all(|m| !m.focused));
    }

    #[test]
    fn model_rows_tolerates_partial_box_at_80x24() {
        // Focus on the 4th model: its box is cut off mid-render, no cost line.
        let rows = load_fixture("splash-list-80x24-focus4");
        let models = model_rows(&rows);
        let names: Vec<&str> = models.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "GLM 5.3 Flash",
                "MiMo 2.5",
                "Solar Pro 4",
                "DeepSeek V4.1 Flash"
            ]
        );
        assert!(models[3].focused);
        assert_eq!(models[3].cost_line, None);
        // Focus on the 1st model: the 4th box has not scrolled into view.
        let rows = load_fixture("splash-list-80x24-focus1");
        let models = model_rows(&rows);
        assert_eq!(models.len(), 3);
        assert!(models[0].focused);
    }

    #[test]
    fn model_rows_empty_on_show_fewer_focus() {
        // Down past the last model: no `›` on any model row.
        let rows = load_fixture("splash-list-showfewer-120x40");
        let models = model_rows(&rows);
        assert_eq!(models.len(), 4);
        assert!(models.iter().all(|m| !m.focused));
        assert!(rows.iter().any(|r| r.contains("Show fewer")));
    }

    #[test]
    fn normalize_model_ignores_case_and_separators() {
        assert_eq!(normalize_model("DeepSeek V4.1 Flash"), "deepseekv41flash");
        assert_eq!(
            normalize_model("deepseek/deepseek-v4.1-flash"),
            "deepseekdeepseekv41flash"
        );
        assert_eq!(normalize_model("z-ai/glm-5.3-flash"), "zaiglm53flash");
        assert_eq!(normalize_model("MiMo_2.5"), "mimo25");
    }

    #[test]
    fn match_model_table() {
        let offered = [
            "GLM 5.3 Flash",
            "MiMo 2.5",
            "Solar Pro 4",
            "DeepSeek V4.1 Flash",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
        let cases: &[(&str, ModelMatch)] = &[
            ("deepseek/deepseek-v4.1-flash", ModelMatch::One(3)),
            ("deepseek-v4-1-flash", ModelMatch::One(3)),
            ("DeepSeek V4.1 Flash", ModelMatch::One(3)),
            ("glm-5.3-flash", ModelMatch::One(0)),
            ("z-ai/glm-5.3-flash", ModelMatch::One(0)),
            ("mimo-2.5", ModelMatch::One(1)),
            ("solar", ModelMatch::One(2)),
            (
                "deepseek-v4-pro",
                ModelMatch::None {
                    offered: offered.clone(),
                },
            ),
            (
                "",
                ModelMatch::None {
                    offered: offered.clone(),
                },
            ),
            (
                "flash",
                ModelMatch::Ambiguous(vec![
                    "GLM 5.3 Flash".to_string(),
                    "DeepSeek V4.1 Flash".to_string(),
                ]),
            ),
        ];
        for (target, expected) in cases {
            assert_eq!(&match_model(target, &offered), expected, "target: {target}");
        }
    }

    #[test]
    fn plan_select_step_table() {
        let rows = |focused: Option<usize>| {
            [
                "GLM 5.3 Flash",
                "MiMo 2.5",
                "Solar Pro 4",
                "DeepSeek V4.1 Flash",
            ]
            .iter()
            .enumerate()
            .map(|(i, n)| ModelRow {
                name: n.to_string(),
                focused: Some(i) == focused,
                cost_line: None,
            })
            .collect::<Vec<_>>()
        };
        // Focus already on the match: accept.
        assert_eq!(
            plan_select_step("mimo", &rows(Some(1)), false, 0),
            SelectStep::Accept
        );
        // Match below focus: move down.
        assert_eq!(
            plan_select_step("deepseek", &rows(Some(0)), false, 0),
            SelectStep::Down
        );
        // Show fewer visible but a model still focused: keep walking.
        assert_eq!(
            plan_select_step("deepseek", &rows(Some(2)), true, 3),
            SelectStep::Down
        );
        // No match, list still walkable: move down.
        assert!(matches!(
            plan_select_step("claude", &rows(Some(0)), false, 0),
            SelectStep::Down
        ));
        // Focus reached Show fewer with no match: fail, naming the lineup.
        match plan_select_step("claude", &rows(None), true, 5) {
            SelectStep::NotOffered { offered } => assert_eq!(offered.len(), 4),
            other => panic!("expected NotOffered, got {other:?}"),
        }
        // Focus reached Show fewer past a missed match: fail as well.
        match plan_select_step("deepseek", &rows(None), true, 5) {
            SelectStep::NotOffered { offered } => assert_eq!(offered.len(), 4),
            other => panic!("expected NotOffered, got {other:?}"),
        }
        // 12 Down presses without landing: fail rather than Enter blindly.
        match plan_select_step("deepseek", &rows(Some(0)), false, 12) {
            SelectStep::NotOffered { offered } => assert_eq!(offered.len(), 4),
            other => panic!("expected NotOffered, got {other:?}"),
        }
        // Two rows match: fail, naming both.
        match plan_select_step("flash", &rows(Some(0)), false, 0) {
            SelectStep::Ambiguous(names) => assert_eq!(names.len(), 2),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn idle_120x40_classifies_as_idle() {
        let rows = load_fixture("idle-120x40");
        assert_eq!(classify(&rows), ScreenState::Idle);
    }

    #[test]
    fn active_model_reads_both_idle_fixtures() {
        let rows = load_fixture("idle-midhour-120x40");
        assert_eq!(classify(&rows), ScreenState::Idle);
        assert_eq!(
            active_model(&rows),
            Some(("MiMo 2.5".to_string(), "58m left".to_string()))
        );
        let rows = load_fixture("idle-120x40");
        assert_eq!(
            active_model(&rows),
            Some(("GLM 5.3 Flash".to_string(), "1h left".to_string()))
        );
    }

    #[test]
    fn active_model_table() {
        // Multi-segment status rows (usage suffix) still parse.
        let cases: &[(&str, Option<(&str, &str)>)] = &[
            (
                " GLM 5.3 Flash · 59m left · 16.4K (2%)      ✕ End session",
                Some(("GLM 5.3 Flash", "59m left")),
            ),
            (
                " MiMo 2.5 · 58m left                    ✕ End session",
                Some(("MiMo 2.5", "58m left")),
            ),
            (
                " GLM 5.3 Flash · 1h left                    ✕ End session",
                Some(("GLM 5.3 Flash", "1h left")),
            ),
            // Splash cost lines are not the status row.
            (" Session ended · 25 Freebucks left", None),
            (" 5 Freebucks/hr", None),
            // No time-left token, no separator, empty name.
            (" MiMo 2.5 · Balanced · Images", None),
            ("MiMo 2.5 58m left", None),
            (" · 58m left", None),
            // Transcript-like row: time-shaped text with trailing prose is
            // not the status row.
            (" Thinking · 5m left on the problem", None),
            ("nothing here", None),
        ];
        for (input, expected) in cases {
            let rows = vec![input.to_string()];
            let expected = expected.map(|(n, t)| (n.to_string(), t.to_string()));
            assert_eq!(active_model(&rows), expected, "input: {input}");
        }
        // Splash fixtures carry no status row.
        for name in ["splash-120x40", "splash-list-120x40"] {
            let rows = load_fixture(name);
            assert_eq!(active_model(&rows), None, "{name}");
        }
    }

    #[test]
    fn active_model_rejects_transcript_like_row() {
        // A transcript line can carry both `·` and a time-shaped fragment;
        // only `<Name> · <N>[hm] left` with nothing prose-like after it is
        // the status row.
        let rows = vec![" Thinking · 5m left on the problem".to_string()];
        assert_eq!(active_model(&rows), None);
    }

    #[test]
    fn check_active_model_table() {
        // Match reuses the match_model rule (normalised substring).
        assert_eq!(
            check_active_model("mimo-2.5", "MiMo 2.5", "58m left"),
            Ok(())
        );
        assert_eq!(
            check_active_model("glm-5.3-flash", "GLM 5.3 Flash", "1h left"),
            Ok(())
        );
        assert_eq!(
            check_active_model("deepseek-v4-pro", "MiMo 2.5", "58m left"),
            Err("freebuff: an hour on 'MiMo 2.5' is already running (58m left); requested 'deepseek-v4-pro' — wait for it to end or unset BLINK_MODEL".to_string())
        );
        assert_eq!(
            check_active_model("mimo", "GLM 5.3 Flash", "1h left"),
            Err("freebuff: an hour on 'GLM 5.3 Flash' is already running (1h left); requested 'mimo' — wait for it to end or unset BLINK_MODEL".to_string())
        );
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
    fn busy_first_instant_classifies_busy_without_elapsed() {
        let rows = load_fixture("busy-first-instant-120x40");
        let state = classify(&rows);
        assert_eq!(state, ScreenState::Busy { elapsed_s: None });
        assert!(input_box_is_empty(&rows));
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
