//! Screen classifier for freebuff TUI.
//! The PTY layer supplies `Vec<String>` rows with ANSI already stripped.

/// Possible screen states detected from the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenState {
    /// Still showing the FREEBUFF banner animation, no other markers.
    Booting,
    /// Model picker splash (collapsed): the templated `See all <N> models`
    /// toggle (any count), the older `Start coding for free` copy, or the
    /// `Choose a model to start a new session` title, alongside `Freebucks/hr`.
    ModelSplash,
    /// Expanded model picker: 2+ model boxes, or "Show fewer" visible.
    ModelList,
    /// Previous session was hard-killed: "Session ended · N Freebucks left"
    /// with "Press Enter to continue in a new session". Esc leads to the
    /// normal splash; Enter would resume with the previous model.
    SessionEnded,
    /// Freebucks gate interstitial: "Not enough Freebucks — ...".
    FreebucksGate { message: String },
    /// Hours gate interstitial (freebuff 0.2.x bills a session in HOURS):
    /// "Not enough hours for <Model>: an hour on it takes N hours." with an
    /// `Enter: retry  Esc: back to draft` affordance. A hard precondition the
    /// agent must NOT satisfy itself — pressing Enter would start a session the
    /// user cannot afford (AGENTS.md: never skip or auto-dismiss an
    /// interstitial). `model`/`hours` are parsed out when the shape allows; the
    /// raw `message` is always carried so the error is never vague.
    HoursGate {
        model: Option<String>,
        hours: Option<u32>,
        message: String,
    },
    /// "Freebuff is already running" dialog with Take over / Exit buttons.
    AlreadyRunning,
    /// Interactive login gate: "Press ENTER to login ...". A hard precondition
    /// the agent must NOT satisfy itself — the human logs in (AGENTS.md: never
    /// reuse credentials, never auto-dismiss an interstitial).
    Login,
    /// Idle prompt ready for input: a live session status row (active model,
    /// optionally with "· <n>h/m left") and input placeholder visible.
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
    // Precedence: FreebucksGate > HoursGate > AlreadyRunning > Login > SessionEnded > KickedOut > Busy > ModelList > ModelSplash > Idle > Booting > Unknown
    //
    // Both terminal gates sit ahead of the picker states (ModelList/ModelSplash):
    // a gate renders modally over the session — and can render OVER the picker
    // when the user picks an unaffordable model — so it must win there. Each gate
    // is keyed on its FULL shape rather than a bare phrase a per-model picker
    // warning also carries, so a healthy picker is never misread as a terminal
    // gate and a fine session never aborts as unaffordable.

    // Check for FreebucksGate first (inside the model box). A v0.2.x picker can
    // also show a per-model insufficient-balance warning, so require the gate's
    // fuller message shape (`... against 0 left ...` / `Enter opens plans`)
    // rather than the bare phrase, or the picker would be misread as the gate.
    for row in rows {
        if row.contains("Not enough Freebucks")
            && (row.contains("against") || row.contains("Enter opens plans"))
        {
            return ScreenState::FreebucksGate {
                message: row.trim().to_string(),
            };
        }
    }

    // Check for the freebuff 0.2.x HOURS gate: 0.2.29 bills a session in hours
    // and refuses a turn with `Not enough hours for <Model>: an hour on it takes
    // N hours.` A v0.2.x picker can show the SAME phrase as a per-model
    // affordability warning label, so — exactly like FreebucksGate above —
    // require the gate's fuller shape (the shortfall sentence) before calling it
    // terminal. The model and figure are parsed where possible and the raw line
    // is always carried, so a renamed model or a different figure still
    // classifies; the shape may soft-wrap, so it is matched screen-wide.
    if has_hours_gate_shape(rows) {
        if let Some((model, hours, message)) = rows.iter().find_map(|r| parse_hours_gate(r)) {
            // The figure can sit on a different physical row than the marker
            // when the gate soft-wraps, so fall back to a screen-wide search.
            let hours = hours.or_else(|| rows.iter().find_map(|r| parse_takes_hours(r)));
            return ScreenState::HoursGate {
                model,
                hours,
                message,
            };
        }
    }

    // Check for AlreadyRunning
    for row in rows {
        if row.contains("Freebuff is already running") {
            return ScreenState::AlreadyRunning;
        }
    }

    // Check for the interactive login gate. A hard precondition: the human
    // logs in, the agent never drives it.
    for row in rows {
        if row.contains("Press ENTER to login") || row.contains("Press Enter to login") {
            return ScreenState::Login;
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

    // Check for ModelList (expanded picker) before ModelSplash: "Show fewer",
    // or 2+ model boxes. v0.2.x renders each model in its own single-border box
    // under UPPERCASE section headers, so count parsed catalog entries too; the
    // collapsed splash shows exactly one box plus the templated toggle. The
    // v0.0.x `model_rows` walk is kept so an older freebuff still classifies.
    let has_show_fewer = rows.iter().any(|r| r.contains("Show fewer"));
    let catalog_models = parse_catalog(rows).entries.len();
    if has_show_fewer || catalog_models >= 2 || model_rows(rows).len() >= 2 {
        return ScreenState::ModelList;
    }

    // Check for ModelSplash. v0.2.27 dropped "Start coding for free" and the
    // fixed "See all 4 models" copy: the toggle is templated with the model
    // count, so match any `See all <N> models`. Keep the older anchors and add
    // the new picker title as a fallback so a copy tweak cannot break it.
    let has_start_coding = rows.iter().any(|r| r.contains("Start coding for free"));
    let has_see_all_models = advertised_model_count(rows).is_some();
    let has_freebucks_hr = rows.iter().any(|r| r.contains("Freebucks/hr"));
    let has_choose_model = rows
        .iter()
        .any(|r| r.contains("Choose a model to start a new session"));
    if has_start_coding || has_see_all_models || has_freebucks_hr || has_choose_model {
        return ScreenState::ModelSplash;
    }

    // Check for Idle: a live session status row (active model; the plan may
    // report remaining time or omit it, e.g. `GLM 5.3 Flash • max · ...`) AND
    // an input affordance (placeholder or "✕ End session"). Keying on the
    // status row — not a time figure, and not merely the placeholder — keeps
    // this specific enough that screens matched earlier in the precedence
    // chain (splash, login) are never misread as Idle.
    let has_status_row = active_model(rows).is_some();
    let has_placeholder = rows
        .iter()
        .any(|r| r.contains("Enter a coding task or / for commands"));
    let has_end_session = rows.iter().any(|r| r.contains("✕ End session"));
    if has_status_row && (has_placeholder || has_end_session) {
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
            || r.contains("Not enough hours")
            || r.contains("Freebuff is already running")
            || r.contains("Another freebuff instance")
            || r.contains("See all ")
            || r.contains("Choose a model to start a new session")
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

/// Whether the screen carries the hours gate's FULL, distinctive shape rather
/// than a bare `Not enough hours for` phrase. A v0.2.x model picker can show
/// that phrase as a per-model affordability warning LABEL — which is why
/// `FreebucksGate` likewise requires its fuller message shape — so the marker
/// alone must never be read as the terminal gate. The genuine gate (the verbatim
/// `0229-real-hours-gate-120x40` frame, and the fake's `hours-gate` screen) also
/// emits the shortfall sentence `an hour on it takes N hours.`; a picker label
/// does not. The marker and the sentence may soft-wrap onto different rows, so
/// both are matched screen-wide.
fn has_hours_gate_shape(rows: &[String]) -> bool {
    let has_marker = rows.iter().any(|r| r.contains("Not enough hours for"));
    let has_shortfall = rows.iter().any(|r| r.contains("an hour on it takes"));
    has_marker && has_shortfall
}

/// Parse the freebuff 0.2.x HOURS gate from a single row. Detected by the
/// `Not enough hours for` phrase (the shape 0.2.29 emits: `Not enough hours for
/// <Model>: an hour on it takes N hours.`). Callers must gate this on
/// [`has_hours_gate_shape`] so a per-model picker warning label is not read as
/// the terminal gate. The model name — the text between the phrase and the first
/// `:` — and the hours figure (from `takes N hour(s)`) are returned when they
/// parse, else `None`, so the caller still surfaces the raw line rather than a
/// vague error. Returns `None` when the row carries no gate at all. Deliberately
/// tolerant: neither the model nor the figure is hardcoded.
fn parse_hours_gate(row: &str) -> Option<(Option<String>, Option<u32>, String)> {
    const MARKER: &str = "Not enough hours for";
    let idx = row.find(MARKER)?;
    let rest = row[idx + MARKER.len()..].trim_start();
    // The model name runs to the first `:` that separates it from the shortfall
    // explanation; a bare phrase with no `:` yields no name.
    let model = match rest.split_once(':') {
        Some((name, _)) => {
            let name = name.trim();
            (!name.is_empty()).then(|| name.to_string())
        }
        None => None,
    };
    Some((model, parse_takes_hours(rest), row.trim().to_string()))
}

/// `N` from a `takes N hour(s)` fragment anywhere in `text`. The `hour`/`hours`
/// unit is required right after the digits so a stray number is not read as the
/// figure.
fn parse_takes_hours(text: &str) -> Option<u32> {
    let idx = text.find("takes ")?;
    let after = &text[idx + "takes ".len()..];
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    if after[digits.len()..].trim_start().starts_with("hour") {
        digits.parse().ok()
    } else {
        None
    }
}

/// One model entry on the splash, collapsed or in the expanded list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRow {
    /// Display name: the text before the first tagline separator (` · ` or the
    /// ` • <effort>` form freebuff 0.2.29 renders). The ` • <effort>` suffix is
    /// NOT part of the name.
    pub name: String,
    /// Section the row appeared under — the last `UNLIMITED`/`OPTIMIZED`/
    /// `POWERFUL` header before it, or [`ModelSection::Unknown`] for the flat
    /// pickers that render no section labels.
    pub section: ModelSection,
    /// True when the row carries the `›` focus marker.
    pub focused: bool,
    /// The `Freebucks/hr` line inside the same box, if visible (an 80x24
    /// viewport can cut the focused last box off mid-render).
    pub cost_line: Option<String>,
}

/// Parse the model rows out of a splash screen (collapsed or expanded).
/// A model row is a `│`-bordered NAME row ([`name_row_body`]) that carries a
/// tagline separator (` · `, or the ` • <effort>` form). Cost lines (they
/// mention `Freebucks`) and chrome (no separator) are not names.
pub fn model_rows(rows: &[String]) -> Vec<ModelRow> {
    let mut out = Vec::new();
    let mut section = ModelSection::Unknown;
    for (i, row) in rows.iter().enumerate() {
        // Section labels are borderless UPPERCASE rows; they set the section for
        // the model rows that follow.
        let framed = strip_frame(row);
        if is_toggle_line(&framed) {
            continue;
        }
        if let Some(sec) = section_from_header(&framed) {
            section = sec;
            continue;
        }
        let Some(inner) = box_inner_untrimmed(row) else {
            continue;
        };
        let Some((body, focused)) = name_row_body(inner) else {
            continue;
        };
        if body.contains("Freebucks") {
            continue;
        }
        if !(body.contains(" · ") || body.contains(" • ")) {
            continue;
        }
        let name = model_name(body);
        if name.is_empty() || !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
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
            name,
            section,
            focused,
            cost_line,
        });
    }
    out
}

/// The name body and focus of a `│`-bordered model NAME row, or `None` when the
/// row is not a name row.
///
/// freebuff indents a model NAME to the same column the focus marker sits in —
/// `│ › Name …` (focused) or `│   Name …` (unfocused). A box's OTHER content
/// rows are laid out differently: a description or status row is LEFT-aligned
/// at a single space (`│ detail. …`, `│ NVIDIA's …`) and a centered row has
/// many. That distinction is the only thing that separates a real name from a
/// wrapped DESCRIPTION that happens to contain the tagline separator, e.g.
/// freebuff 0.2.29's
/// `│ detail. Claude Haiku 5.5 · 100k context is the same model at a lower hourly │`,
/// which was previously read as a model named `detail. Claude Haiku 5.5`.
///
/// `inner` must be the raw inner text, BEFORE leading whitespace is trimmed.
fn name_row_body(inner: &str) -> Option<(&str, bool)> {
    let lead = inner.len() - inner.trim_start_matches(' ').len();
    let trimmed = &inner[lead..];
    if let Some(body) = trimmed.strip_prefix("› ") {
        // The marker may be preceded by the same small indent in every layout
        // seen (1 space at 120x40, 1–3 in the 80x24 fixtures).
        return (lead <= 3).then(|| (body.trim(), true));
    }
    // Unfocused: the name sits exactly at the marker's column.
    (lead == 3).then(|| (trimmed.trim(), false))
}

/// Inner text of a `│`-bordered row, trimmed. Returns None for rows without a
/// border. Tolerates a missing right border (partial box at 80x24) and
/// trailing scrollbar glyphs (`█▀▄`).
fn box_inner(row: &str) -> Option<&str> {
    Some(
        box_inner_untrimmed(row)?
            .trim()
            .trim_end_matches(['█', '▀', '▄'])
            .trim(),
    )
}

/// The inner text of a `│`-bordered row WITHOUT trimming the leading padding:
/// the name-row indent ([`name_row_body`]) is only visible before trimming.
/// Tolerates a missing right border (partial box at 80x24).
fn box_inner_untrimmed(row: &str) -> Option<&str> {
    let (_, after_left) = row.split_once('│')?;
    Some(match after_left.rsplit_once('│') {
        Some((mid, _)) => mid,
        None => after_left,
    })
}

/// Section of the freebuff v0.2.x expanded model picker. The renderer emits
/// UPPERCASE section labels as their own text rows ahead of that section's
/// model boxes (`unlimited`, `optimized`, `powerful`, plus `LIMITED TRIAL` when
/// a trial model is offered).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSection {
    Unlimited,
    Optimized,
    Powerful,
    LimitedTrial,
    Unknown,
}

/// One model entry parsed from the freebuff v0.2.x picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEntry {
    /// Display name (freebuff never renders the model id on screen).
    pub display_name: String,
    /// Section the entry appeared under.
    pub section: ModelSection,
    /// Hourly price parsed from an `N Freebucks/hr` line, when shown.
    pub price_per_hour: Option<u32>,
    /// True when a `Recommended` status line is shown for this entry.
    pub recommended: bool,
    /// False when a locked/warning label (e.g. `Not enough Freebucks`) is shown.
    pub available: bool,
    /// True when the row carries the `›` focus marker.
    pub focused: bool,
}

/// Parsed freebuff v0.2.x picker: the model entries visible in this render, in
/// screen order, plus the advertised total from the toggle line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelCatalog {
    pub entries: Vec<ModelEntry>,
    /// `N` from a `See all <N> models` toggle row, when rendered. A caller
    /// compares `entries.len()` with this to learn whether it has seen every
    /// model or still needs to scroll.
    pub advertised: Option<usize>,
}

/// Navigation ring of the freebuff v0.2.x picker. Every model row plus the
/// toggle row participates in one wrapping ring
/// (`(index + direction + len) % len`); the toggle row carries the internal id
/// `__freebuff_toggle__`. A caller walks this instead of a fixed max-presses
/// constant: after at most `ring_len` presses it has visited every stop, so it
/// can never wrap forever without landing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NavRing {
    /// Number of model rows visible in this render (fewer than the advertised
    /// total when the list is scrolled).
    pub visible_models: usize,
    /// Advertised total from the toggle line, when rendered.
    pub advertised_models: Option<usize>,
    /// Index of the toggle row within the ring, when the toggle is rendered.
    /// Models occupy `0..models`; the toggle sits last.
    pub toggle_index: Option<usize>,
    /// Total number of focusable stops: every model (the advertised total when
    /// known, else the visible count) plus the toggle row.
    pub ring_len: usize,
}

/// Parse the freebuff v0.2.x model picker from rendered rows. Section-header
/// rows and the `See all <N> models` / `Show fewer` toggle row are never
/// emitted as models. The bordered-box layout is handled structurally: each
/// model owns a single-border box whose FIRST content row carries the name,
/// and any later rows in the same box (centred price/status lines, wrapped
/// taglines) attach to that entry. A compacted layout that puts the price on
/// the name row works too, because status parsing runs over the name row as
/// well. Soft-wrapping and trailing padding are tolerated.
pub fn parse_catalog(rows: &[String]) -> ModelCatalog {
    let mut catalog = ModelCatalog {
        entries: Vec::new(),
        advertised: advertised_model_count(rows),
    };
    let mut section = ModelSection::Unknown;
    let mut current: Option<ModelEntry> = None;
    // Whether the previous row was content inside a box, and whether that box
    // turned out to be a model box (as opposed to an input box, paste chip or
    // slash-menu box). A new `│` row after box content continues the box.
    let mut in_box = false;
    let mut box_is_model = false;

    for row in rows {
        // A `│`-bordered row is box content.
        if let Some(inner) = box_inner(row) {
            let inner = inner.trim();
            if inner.is_empty() {
                in_box = false;
                continue;
            }
            if is_toggle_line(inner) {
                in_box = false;
                continue;
            }
            if let Some(sec) = section_from_header(inner) {
                if let Some(e) = current.take() {
                    catalog.entries.push(e);
                }
                section = sec;
                in_box = false;
                continue;
            }
            if !in_box {
                // First content row of a box. It is a model only when the row
                // carries a tagline separator (` · ` or its ` • <effort>` form);
                // an input box, paste chip, slash-menu box or a square-bordered
                // CHROME box (e.g. the info box's `⎘ Copy invite link  Learn
                // More ↵`, which has a double space but no tagline) has none and
                // is skipped whole. The v0.2.29 `/model` picker also draws a
                // promo/ad box inside it, so this keeps an ad out of the catalog.
                let (focused, body) = strip_focus(inner);
                if !(body.contains(" · ") || body.contains(" • ")) {
                    in_box = true;
                    box_is_model = false;
                    continue;
                }
                let name = model_name(body);
                // A name starts with a letter. A scrolled picker can leave only
                // a box's CENTERED continuation row visible (`3x · Recommended ·
                // …`), whose derived "name" starts with a digit: that is not a
                // model, so it is not reported as one.
                if name.is_empty() || !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
                    in_box = true;
                    box_is_model = false;
                    continue;
                }
                if let Some(e) = current.take() {
                    catalog.entries.push(e);
                }
                let mut entry = ModelEntry {
                    display_name: name,
                    section,
                    price_per_hour: None,
                    recommended: false,
                    available: true,
                    focused,
                };
                apply_status(&mut entry, body);
                current = Some(entry);
                in_box = true;
                box_is_model = true;
                continue;
            }
            // Continuation rows attach to the current model box only.
            if box_is_model {
                if let Some(e) = current.as_mut() {
                    apply_status(e, inner);
                }
            }
            continue;
        }

        // A borderless row: only section headers and the toggle are meaningful.
        let text = strip_frame(row);
        if text.is_empty() || is_toggle_line(&text) {
            in_box = false;
            continue;
        }
        if let Some(sec) = section_from_header(&text) {
            if let Some(e) = current.take() {
                catalog.entries.push(e);
            }
            section = sec;
            in_box = false;
            continue;
        }
        // Any other chrome (banner rows, prose between boxes) ends the box run.
        in_box = false;
    }

    if let Some(e) = current.take() {
        catalog.entries.push(e);
    }
    catalog
}

/// `N` from a `See all <N> models` toggle row, matching ANY count (freebuff
/// v0.2.x templates the line as `↓  See all ${LT.length} models`). Returns None
/// for the `Show fewer` expanded form (which carries no count) and when no
/// toggle row is present.
pub fn advertised_model_count(rows: &[String]) -> Option<usize> {
    for row in rows {
        let text = strip_frame(row);
        let Some(idx) = text.find("See all ") else {
            continue;
        };
        let rest = &text[idx + "See all ".len()..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        if rest[digits.len()..].trim_start().starts_with("models") {
            if let Ok(n) = digits.parse::<usize>() {
                return Some(n);
            }
        }
    }
    None
}

/// Navigation ring of the freebuff v0.2.x picker, derived from the rendered
/// rows: the index of the toggle row and the total ring length. Returns None
/// when neither a model row nor the toggle is rendered. Section headers are
/// treated as labels, not focus stops (they render as plain text rows).
pub fn nav_ring(rows: &[String]) -> Option<NavRing> {
    let catalog = parse_catalog(rows);
    let visible_models = catalog.entries.len();
    let advertised_models = catalog.advertised;
    let toggle_present = rows.iter().any(|r| is_toggle_line(&strip_frame(r)));
    if visible_models == 0 && !toggle_present {
        return None;
    }
    let models = advertised_models.unwrap_or(visible_models);
    let ring_len = models + usize::from(toggle_present);
    Some(NavRing {
        visible_models,
        advertised_models,
        toggle_index: toggle_present.then_some(models),
        ring_len,
    })
}

/// Section from a header row, ignoring an optional ` · <hint>` suffix. Only an
/// EXACT UPPERCASE label as the row's first segment matches, so a model row
/// whose tagline merely mentions `Powerful` is not read as a header.
fn section_from_header(text: &str) -> Option<ModelSection> {
    let upper = text.trim().to_ascii_uppercase();
    let head = upper.split(" · ").next().unwrap_or("").trim();
    match head {
        "UNLIMITED" => Some(ModelSection::Unlimited),
        "OPTIMIZED" => Some(ModelSection::Optimized),
        "POWERFUL" => Some(ModelSection::Powerful),
        "LIMITED TRIAL" => Some(ModelSection::LimitedTrial),
        _ => None,
    }
}

/// Split a model box's inner text into (focused, body) around the `›` marker.
fn strip_focus(inner: &str) -> (bool, &str) {
    match inner.strip_prefix('›') {
        Some(rest) => (true, rest.trim()),
        None => (false, inner.trim()),
    }
}

/// Strip border/scrollbar characters and padding from any row.
fn strip_frame(row: &str) -> String {
    row.trim()
        .trim_matches(|c: char| matches!(c, '│' | '┃' | '█' | '▀' | '▄'))
        .trim()
        .to_string()
}

/// Display name from a model box body: the text before the first tagline
/// separator — ` · `, a ` • <effort>` suffix, or a run of 2+ padding spaces.
fn model_name(body: &str) -> String {
    let mut end = body.len();
    for sep in [" · ", " • ", "  "] {
        if let Some(i) = body.find(sep) {
            end = end.min(i);
        }
    }
    body[..end].trim().to_string()
}

/// Whether a row is the picker toggle (collapsed `See all <N> models` or
/// expanded `Show fewer`).
fn is_toggle_line(text: &str) -> bool {
    text.contains("See all ") || text.contains("Show fewer")
}

/// Whether `row` is the picker toggle line AND carries the `›` focus marker on
/// that same line. The freebuff 0.2.x renderer marks a focused toggle only by
/// colour/bold (which the PTY layer strips), so this is not the only way to
/// establish toggle focus — but when a renderer does emit a marker, it is
/// positive evidence that the toggle, not a model row, holds focus.
pub fn toggle_row_has_focus_marker(row: &str) -> bool {
    let text = strip_frame(row);
    text.contains('›') && is_toggle_line(&text)
}

/// `N` from an `N Freebucks/hr` price fragment.
fn parse_price_per_hour(text: &str) -> Option<u32> {
    let idx = text.find("Freebucks/hr")?;
    let before = text[..idx].trim_end();
    let digits: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    digits.parse().ok()
}

/// Fold a status/price line (or a compacted name row) into `entry`.
fn apply_status(entry: &mut ModelEntry, text: &str) {
    if entry.price_per_hour.is_none() {
        if let Some(p) = parse_price_per_hour(text) {
            entry.price_per_hour = Some(p);
        }
    }
    if text.contains("Recommended") {
        entry.recommended = true;
    }
    if is_unavailable_label(text) {
        entry.available = false;
    }
}

/// Whether `text` carries a locked/warning label or an insufficient-balance
/// state (the picker shows these instead of a plain price for unusable models).
fn is_unavailable_label(text: &str) -> bool {
    const MARKERS: [&str; 6] = [
        "Not enough Freebucks",
        "Insufficient",
        "Locked",
        "Unlock",
        "Trial ended",
        "Unavailable",
    ];
    MARKERS.iter().any(|m| text.contains(m))
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

/// Canonical 0.2.x model ids and the display name freebuff renders for each in
/// the idle status row. freebuff 0.2.x persists the active model as an id
/// (`freebuffModel` in `settings.json`) but still shows a display name, and the
/// two are not derivable from one another: `deepseek/deepseek-v4-flash` shows
/// as `DeepSeek V4.1 Flash`, and `mimo/mimo-v2.5` as `MiMo 2.6 Flash`. Since
/// `BLINK_MODEL` now carries ids, matching a requested id against the running
/// row must go through this table. Extracted from the freebuff 0.2.27 bundle;
/// update when the lineup rotates (see docs/research).
const MODEL_ID_DISPLAY: &[(&str, &str)] = &[
    ("z-ai/glm-5.3-flash", "GLM 5.3 Flash"),
    ("z-ai/glm-5.2", "GLM 5.2"),
    ("mimo/mimo-v2.5", "MiMo 2.6 Flash"),
    ("mimo/mimo-v2.6-pro", "MiMo 2.6 Pro"),
    ("openai/gpt-6-luna", "GPT-6 Luna"),
    ("openai/gpt-6.1-sol", "GPT-6.1 Sol"),
    ("openai/gpt-5.6-luna", "GPT-5.6 Luna"),
    ("deepseek/deepseek-v4-flash", "DeepSeek V4.1 Flash"),
    (
        "deepseek/deepseek-v4-flash-fast",
        "DeepSeek V4.1 Flash Fast",
    ),
    ("deepseek/deepseek-v4-pro", "DeepSeek V4 Pro"),
    ("meta/muse-spark-1.3-contributor", "Muse Spark 1.3"),
    ("meta/muse-spark-1.2-contributor", "Muse Spark 1.2"),
    ("upstage/solar-pro4", "Solar Pro 4"),
    ("upstage/solar-mini4", "Solar Mini 4"),
    ("google/gemini-3.8-flash", "Gemini 3.8 Flash"),
    ("minimax/minimax-m3", "MiniMax M3"),
    ("crof/kimi-k3-eco", "Kimi K3"),
    ("anthropic/claude-fable-5.1", "Claude Fable 5.1"),
    ("stealth/ox-alpha", "Ox Alpha"),
    ("stealth/space-bunny-alpha", "Space Bunny Alpha"),
];

/// The display name freebuff renders for a known 0.2.x model id. Matches on
/// the whole id or its last `/` segment, under [`normalize_model`] (case- and
/// separator-insensitive). Returns None for an id freebuff did not ship in the
/// 0.2.27 lineup — callers treat that as an unrecognized id.
pub fn model_display_name(id: &str) -> Option<&'static str> {
    let want = normalize_model(id);
    let want_seg = normalize_model(id.rsplit('/').next().unwrap_or(id));
    MODEL_ID_DISPLAY.iter().find_map(|(cid, name)| {
        let cid_norm = normalize_model(cid);
        let cid_seg = normalize_model(cid.rsplit('/').next().unwrap_or(cid));
        if cid_norm == want || (!want_seg.is_empty() && cid_seg == want_seg) {
            Some(*name)
        } else {
            None
        }
    })
}

/// Resolve a `BLINK_MODEL` value — a provider-prefixed id, a bare id, or a
/// display name — to the canonical catalog id to write into `settings.json`.
/// Returns None when the value names no model in the 0.2.27 lineup.
pub fn resolve_model_id(target: &str) -> Option<&'static str> {
    let want = normalize_model(target);
    let want_seg = normalize_model(target.rsplit('/').next().unwrap_or(target));
    MODEL_ID_DISPLAY.iter().find_map(|(id, name)| {
        let id_norm = normalize_model(id);
        let id_seg = normalize_model(id.rsplit('/').next().unwrap_or(id));
        if id_norm == want
            || (!want_seg.is_empty() && id_seg == want_seg)
            || normalize_model(name) == want
        {
            Some(*id)
        } else {
            None
        }
    })
}

/// Active model and (optional) time left from the Idle screen's status row.
/// Two shapes are accepted: current plans mark the row with the `/model`
/// command hint and may report no remaining time (e.g. `GLM 5.3 Flash • max ·
/// /dir · /model to change · Chat: New chat`), while older plans carry a
/// `\d+[hm] left` figure. The name is the trimmed text before the first ` · `
/// or ` • `; the time is the `\d+[hm] left` fragment when present. Rows
/// mentioning Freebucks are splash cost lines, not the status row. Returns
/// None when no row parses: within a running hour freebuff skips the splash
/// and resumes on the hour's model, so the status row is the only place the
/// active model is shown — callers must fail loudly rather than assume the
/// requested model is active.
pub fn active_model(rows: &[String]) -> Option<(String, Option<String>)> {
    rows.iter().find_map(|row| parse_status_row(row))
}

/// Parse a single physical row as the live-session status row. Shape A:
/// current plans mark it with the `/model` command hint and the remaining-time
/// figure may be absent (`• max`) or present. Shape B: older plans carry
/// `<Name> · <N>[hm] left`, with nothing prose-like after the figure so
/// transcript lines stay rejected.
///
/// Why `/model` and not the full `/model to change`: freebuff soft-wraps at a
/// WORD boundary, so with a long working directory the marker is split across
/// two physical rows (`... · /model` / `to change · Chat: New chat`). Every
/// row is padded to the PTY width, so a soft-wrapped row is byte-indistinguishable
/// from an ordinary padded row and the wrap cannot be reconstructed reliably.
/// `/model` sits on the first physical row in both the wrapped and unwrapped
/// forms. It cannot false-match on its own: the name must be followed by a
/// ` · `/` • ` separator (so a bare `/model` mention is rejected), and Idle is
/// only reached after all higher-precedence states (gates, login, busy, the
/// splash shapes) have failed.
fn parse_status_row(row: &str) -> Option<(String, Option<String>)> {
    if row.contains("Freebucks") {
        return None;
    }
    if row.contains("/model") {
        let sep = first_separator(row)?;
        let name = row[..sep].trim();
        if name.is_empty() {
            return None;
        }
        return Some((name.to_string(), time_left_anywhere(row)));
    }
    if !row.contains('·') {
        return None;
    }
    let sep = row.find(" · ")?;
    let name = row[..sep].trim();
    if name.is_empty() {
        return None;
    }
    // The time fragment sits immediately after the separator, so anything
    // else there (a mode word, a transcript line) is not the status row.
    let after = row[sep + " · ".len()..].trim_start();
    let time = time_left_at_start(after)?;
    // After the time only the usage suffix (`· …`) or `✕ End session` may
    // follow; trailing prose (`5m left on the problem`) is a transcript line,
    // not the status row.
    let rest = after[time.len()..].trim_start();
    if !(rest.is_empty() || rest.starts_with('·') || rest.starts_with('✕')) {
        return None;
    }
    Some((name.to_string(), Some(time.to_string())))
}

/// Byte index of the first status-row separator, ` · ` or ` • `.
fn first_separator(row: &str) -> Option<usize> {
    [row.find(" · "), row.find(" • ")]
        .into_iter()
        .flatten()
        .min()
}

/// The first `\d+[hm] left` fragment anywhere in the row, if present.
fn time_left_anywhere(row: &str) -> Option<String> {
    let bytes = row.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i < bytes.len() && (bytes[i] == b'h' || bytes[i] == b'm') {
            let unit_end = i + 1;
            if row[unit_end..].starts_with(" left") {
                return Some(row[start..unit_end + " left".len()].to_string());
            }
        }
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

/// Decide whether an already-running hour satisfies `BLINK_MODEL`. `target` may
/// be an id, a bare id fragment or a display name (see [`models_match`]); the
/// outcome is match or already-running error (never ambiguous). The error
/// distinguishes a recognized id that is simply not the running model from a
/// value freebuff would not recognize at all.
pub fn check_active_model(
    target: &str,
    active: &str,
    time_left: Option<&str>,
) -> Result<(), String> {
    if models_match(target, active) {
        return Ok(());
    }
    let time = time_left.map(|t| format!(" ({t})")).unwrap_or_default();
    if resolve_model_id(target).is_some() {
        Err(format!(
            "freebuff: an hour on '{active}' is already running{time}; requested '{target}' is not that model — the BLINK_MODEL pin may be stale; wait for the hour to end or unset BLINK_MODEL"
        ))
    } else {
        Err(format!(
            "freebuff: an hour on '{active}' is already running{time}; requested '{target}' is not a freebuff 0.2.x model id — check BLINK_MODEL against freebuff's /model picker"
        ))
    }
}

/// Do a requested `BLINK_MODEL` value and the model name shown on the idle
/// status row denote the same model? Accepts a direct name/normalized-substring
/// match, and otherwise resolves the request to its canonical id via
/// [`MODEL_ID_DISPLAY`] and compares the id and its display name against the
/// running row. This is what lets an id such as `z-ai/glm-5.3-flash` be
/// confirmed against a row that reads `GLM 5.3 Flash`.
fn models_match(target: &str, active: &str) -> bool {
    let running = [active.to_string()];
    if let ModelMatch::One(_) = match_model(target, &running) {
        return true;
    }
    let Some(id) = resolve_model_id(target) else {
        return false;
    };
    if let ModelMatch::One(_) = match_model(id, &running) {
        return true;
    }
    match model_display_name(id) {
        Some(name) => matches!(match_model(name, &running), ModelMatch::One(_)),
        None => false,
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
/// `presses` is how many Down presses have been sent so far; `ring_len` is the
/// focus-ring length (models plus the toggle) reported by [`nav_ring`]. The
/// walk gives up once it has pressed a full ring's worth without landing,
/// instead of a fixed cap: the old `MAX_DOWN_PRESSES = 12` could never reach a
/// model at index >= 12, so a 13-model list wrapped forever and never landed.
pub fn plan_select_step(
    target: &str,
    rows: &[ModelRow],
    show_fewer: bool,
    ring_len: u32,
    presses: u32,
) -> SelectStep {
    let max_presses = ring_len.max(1);
    let names: Vec<String> = rows.iter().map(|r| r.name.clone()).collect();
    match match_model(target, &names) {
        ModelMatch::Ambiguous(names) => SelectStep::Ambiguous(names),
        ModelMatch::None { offered } => {
            if presses >= max_presses || (show_fewer && !rows.iter().any(|r| r.focused)) {
                SelectStep::NotOffered { offered }
            } else {
                SelectStep::Down
            }
        }
        ModelMatch::One(i) => {
            if rows[i].focused {
                SelectStep::Accept
            } else if presses >= max_presses || (show_fewer && !rows.iter().any(|r| r.focused)) {
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

/// Extract the text inside the freebuff input box.
/// The input box is the `╭…╮ … ╰…╯` box nearest the BOTTOM of the screen,
/// located structurally by its border glyphs rather than by row index. This
/// matters because freebuff v0.2.29 renders a sponsored promo banner in a
/// rounded box of the SAME shape above the input box: a scan that returned the
/// FIRST box returned the banner and never saw the typed prompt, so
/// submit-confirmation timed out with the text plainly on screen. Every closed
/// box is collected and the last one wins.
///
/// Returns the box content trimmed, without the `▍` cursor glyph, with its
/// wrapped rows joined by `\n`. Returns None when no rounded box is present,
/// and Some("") for a present-but-empty box (callers treat that as showing no
/// prompt).
pub fn input_box_text(rows: &[String]) -> Option<String> {
    let mut boxes: Vec<Vec<String>> = Vec::new();
    let mut current: Option<Vec<String>> = None;

    for row in rows {
        if row.contains('╭') && row.contains('╮') {
            current = Some(Vec::new());
            continue;
        }
        if row.contains('╰') && row.contains('╯') {
            if let Some(lines) = current.take() {
                boxes.push(lines);
            }
            continue;
        }
        if let Some(lines) = current.as_mut() {
            // Strip border characters (│) and surrounding whitespace.
            let stripped = row.trim_start_matches('│').trim_end_matches('│').trim();
            lines.push(stripped.to_string());
        }
    }
    // An unterminated box (bottom border scrolled off) still yields its rows.
    if let Some(lines) = current.take() {
        boxes.push(lines);
    }

    let box_lines = boxes.into_iter().next_back()?;
    if box_lines.is_empty() {
        return None;
    }

    // Join lines, remove the cursor glyph ▍, trim
    let text = box_lines.join("\n").replace('▍', "").trim().to_string();
    Some(text)
}

/// Collapse every run of whitespace to a single space, then trim. The newlines
/// that join a box's wrapped rows, and the padding inside a row, both become
/// ordinary single spaces, so an unwrapped needle can be compared against a
/// wrapped box.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether the input box currently displays `needle` (a prefix of the prompt),
/// tolerating soft-wrapping. The box's rows are joined and their whitespace
/// collapsed before the comparison, so a prompt the TUI wrapped across two rows
/// still matches its unwrapped prefix; a whitespace-insensitive fallback
/// catches a wrap that splits a token in two. Only the input box — the
/// bottom-most rounded box — is examined, so a promo banner above it cannot
/// shadow it.
///
/// An empty `needle`, or an empty box, matches nothing: a dropped prompt must
/// never look submitted.
pub fn input_box_shows(rows: &[String], needle: &str) -> bool {
    let needle = collapse_ws(needle);
    if needle.is_empty() {
        return false;
    }
    let Some(text) = input_box_text(rows) else {
        return false;
    };
    let text = collapse_ws(&text);
    if text.is_empty() {
        return false;
    }
    if text.contains(&needle) {
        return true;
    }
    // A wrap that splits mid-token leaves no space to collapse; compare with
    // ALL whitespace removed so `...number`/`per...` still joins.
    let squish = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    squish(&text).contains(&squish(&needle))
}

/// Whether the input box's TYPED content is EXACTLY `cmd` — nothing else.
///
/// [`input_box_shows`] is a SUBSTRING match (it exists to prefix-match a long
/// prompt, a different job), so a box holding `foo/model` or `/model/model`
/// satisfies it — and submitting such a box sends a plain MESSAGE, which starts
/// a billed session. This predicate is the strict one the `/model` submit path
/// uses instead: after normalising the box the way [`input_box_shows`] does
/// (bottom-most rounded box, wrapped rows joined, cursor glyph stripped,
/// whitespace collapsed) the remaining typed lines must be exactly one line
/// equal to `cmd`.
///
/// The one thing tolerated is CHROME the TUI renders INSIDE the box: the
/// slash-command autocomplete popup draws `/command   Description` rows above
/// the typed text ([`slash_popup_shows`]). Those are not typed content, so they
/// are skipped; every other non-empty line must be exactly `cmd`. An empty box,
/// a box with extra text, or no box at all, all return `false`.
pub fn input_box_is_exactly(rows: &[String], cmd: &str) -> bool {
    let want = collapse_ws(cmd);
    if want.is_empty() {
        return false;
    }
    let Some(text) = input_box_text(rows) else {
        return false;
    };
    let mut typed: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || is_slash_menu_row(line) {
            continue;
        }
        typed.push(collapse_ws(line));
    }
    typed.len() == 1 && typed[0] == want
}

/// Whether the input box shows freebuff's slash-command AUTOCOMPLETE popup.
///
/// The popup renders INSIDE the input box, above the typed text, as
/// `/command   Description` rows (captured live from 0.2.29:
/// `/model      Choose the model for your next message`). It is the ONLY
/// documented reason a first Enter is swallowed
/// (`docs/research/freebuff-tui.md`), so it is the positive evidence a REPEAT
/// Enter requires — a repeat must never be sent merely because "the picker is
/// not up yet".
pub fn slash_popup_shows(rows: &[String]) -> bool {
    let Some(text) = input_box_text(rows) else {
        return false;
    };
    text.lines().any(is_slash_menu_row)
}

/// `  /model      Choose the model for your next message` — a slash command
/// followed by a description, i.e. an autocomplete menu row. The typed line
/// itself (`/model`) has no description, so it never matches.
fn is_slash_menu_row(line: &str) -> bool {
    let line = line.trim();
    let Some(rest) = line.strip_prefix('/') else {
        return false;
    };
    let name_len = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
        .count();
    if name_len == 0 {
        return false;
    }
    let after = &rest[name_len..];
    let spaces = after.len() - after.trim_start_matches(' ').len();
    spaces >= 2 && !after.trim().is_empty()
}

/// Whether the screen confirms a submitted prompt: the typed text's prefix is
/// visible in the input box (tolerating wrapped rows and a banner above it) or
/// a pasted-text chip is shown. This is the exact predicate the driver's submit
/// wait uses, so a fixture pins the behaviour the driver depends on.
pub fn prompt_visible(rows: &[String], preview: &str) -> bool {
    input_box_shows(rows, preview) || pasted_chip_chars(rows).is_some()
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
            "idle-max-plan-120x40" => fixture!("idle-max-plan-120x40"),
            "busy-120x40" => fixture!("busy-120x40"),
            "busy-first-instant-120x40" => fixture!("busy-first-instant-120x40"),
            "after-esc" => fixture!("after-esc"),
            "kicked-out" => fixture!("kicked-out"),
            "freebucks-gate-80x24" => fixture!("freebucks-gate-80x24"),
            "slash-menu" => fixture!("slash-menu"),
            "paste-chip-120x40" => fixture!("paste-chip-120x40"),
            // freebuff v0.2.x picker — SYNTHETIC fixtures, hand-built from the
            // 0.2.27 bundle's render code (see 0227-synthetic-NOTE.md), not
            // captured from a live run.
            "0227-synthetic-splash-collapsed-120x40" => {
                fixture!("0227-synthetic-splash-collapsed-120x40")
            }
            "0227-synthetic-splash-expanded-120x40" => {
                fixture!("0227-synthetic-splash-expanded-120x40")
            }
            "0227-synthetic-splash-trial-120x40" => {
                fixture!("0227-synthetic-splash-trial-120x40")
            }
            "0227-synthetic-splash-scrolled-120x40" => {
                fixture!("0227-synthetic-splash-scrolled-120x40")
            }
            "0227-synthetic-splash-focus-120x40" => {
                fixture!("0227-synthetic-splash-focus-120x40")
            }
            "0227-synthetic-splash-narrow-80x24" => {
                fixture!("0227-synthetic-splash-narrow-80x24")
            }
            // Submit-confirmation fixtures. The real one is a VERBATIM frame
            // captured from the 2026-10-16 live run (freebuff v0.2.29) where a
            // promo banner in a rounded box shadowed the input box; the rest
            // are synthetic, derived from that frame's layout.
            "0227-real-banner-and-input-120x40" => fixture!("0227-real-banner-and-input-120x40"),
            // The 2026-10-16 live run's hours gate: freebuff v0.2.29 bills in
            // hours and refused the turn. VERBATIM frame from that run.
            "0229-real-hours-gate-120x40" => fixture!("0229-real-hours-gate-120x40"),
            // VERBATIM 120x40 frames captured from the 2026-10-17 live probe of
            // freebuff v0.2.29 (real-derived): the startup screen with NO model
            // picker, and the `/model` picker it opens. See 0229-real-NOTE.md.
            "0229-real-startup-no-picker-120x40" => fixture!("0229-real-startup-no-picker-120x40"),
            "0229-real-slash-model-picker-120x40" => {
                fixture!("0229-real-slash-model-picker-120x40")
            }
            // Consecutive frames from the SAME live scroll of that picker: the
            // `POWERFUL` section with its header on screen, the next frame whose
            // header has scrolled off (and which carries a wrapped DESCRIPTION
            // row), and the tail whose models never had a header on screen.
            "0229-real-picker-powerful-section-120x40" => {
                fixture!("0229-real-picker-powerful-section-120x40")
            }
            "0229-real-picker-wrapped-description-120x40" => {
                fixture!("0229-real-picker-wrapped-description-120x40")
            }
            "0229-real-picker-tail-120x40" => fixture!("0229-real-picker-tail-120x40"),
            // The slash-command AUTOCOMPLETE popup, captured live right after
            // typing `/model` (before any Enter).
            "0229-real-slash-popup-120x40" => fixture!("0229-real-slash-popup-120x40"),
            "0227-synthetic-wrapped-input-120x40" => {
                fixture!("0227-synthetic-wrapped-input-120x40")
            }
            "0227-synthetic-wrapped-banner-120x40" => {
                fixture!("0227-synthetic-wrapped-banner-120x40")
            }
            "0227-synthetic-unrelated-input-120x40" => {
                fixture!("0227-synthetic-unrelated-input-120x40")
            }
            "0227-synthetic-empty-input-120x40" => {
                fixture!("0227-synthetic-empty-input-120x40")
            }
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
                section: ModelSection::Unknown,
                focused: Some(i) == focused,
                cost_line: None,
            })
            .collect::<Vec<_>>()
        };
        // A four-model list plus the toggle: a five-stop ring.
        const RING: u32 = 5;
        // Focus already on the match: accept.
        assert_eq!(
            plan_select_step("mimo", &rows(Some(1)), false, RING, 0),
            SelectStep::Accept
        );
        // Match below focus: move down.
        assert_eq!(
            plan_select_step("deepseek", &rows(Some(0)), false, RING, 0),
            SelectStep::Down
        );
        // Show fewer visible but a model still focused: keep walking.
        assert_eq!(
            plan_select_step("deepseek", &rows(Some(2)), true, RING, 3),
            SelectStep::Down
        );
        // No match, list still walkable: move down.
        assert!(matches!(
            plan_select_step("claude", &rows(Some(0)), false, RING, 0),
            SelectStep::Down
        ));
        // Focus reached Show fewer with no match: fail, naming the lineup.
        match plan_select_step("claude", &rows(None), true, RING, 5) {
            SelectStep::NotOffered { offered } => assert_eq!(offered.len(), 4),
            other => panic!("expected NotOffered, got {other:?}"),
        }
        // Focus reached Show fewer past a missed match: fail as well.
        match plan_select_step("deepseek", &rows(None), true, RING, 5) {
            SelectStep::NotOffered { offered } => assert_eq!(offered.len(), 4),
            other => panic!("expected NotOffered, got {other:?}"),
        }
        // A full ring's worth of Down presses without landing: fail rather than
        // Enter blindly.
        match plan_select_step("deepseek", &rows(Some(0)), false, RING, RING) {
            SelectStep::NotOffered { offered } => assert_eq!(offered.len(), 4),
            other => panic!("expected NotOffered, got {other:?}"),
        }
        // Two rows match: fail, naming both.
        match plan_select_step("flash", &rows(Some(0)), false, RING, 0) {
            SelectStep::Ambiguous(names) => assert_eq!(names.len(), 2),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn idle_120x40_classifies_as_idle() {
        let rows = load_fixture("idle-120x40");
        assert_eq!(classify(&rows), ScreenState::Idle);
    }

    /// Regression guard for the production bug: this account's ready-session
    /// status row carries `• max` with NO `h left`/`m left` figure, so the old
    /// Idle predicate could never match and `session/new` burned the whole
    /// budget. The row below is the real captured one.
    #[test]
    fn idle_plan_without_time_left_classifies_as_idle() {
        let rows = vec![
            "  GLM 5.3 Flash • max · /private/var/folders/abc/blink-e2e-42350-1 · /model to change · Chat: New chat"
                .to_string(),
            "╭────────────────────────────────────────╮".to_string(),
            "│  ▍Enter a coding task or / for commands  │".to_string(),
            "╰────────────────────────────────────────╯".to_string(),
        ];
        assert_eq!(classify(&rows), ScreenState::Idle);
        assert_eq!(
            active_model(&rows),
            Some(("GLM 5.3 Flash".to_string(), None))
        );
    }

    /// The real screen captured via BLINK_DUMP_SCREEN on this plan: all 40
    /// rows byte-for-byte, trailing padding included. The status row soft-wraps
    /// (`... · /model` / `to change · Chat: New chat`), so the literal
    /// `/model to change` is on neither row alone — the regression the
    /// hand-written fixtures never caught.
    #[test]
    fn idle_max_plan_fixture_classifies_as_idle() {
        let rows = load_fixture("idle-max-plan-120x40");
        // Padding survives the fixture file and `.lines()`.
        assert_eq!(rows.len(), 40);
        assert!(rows.iter().all(|r| r.chars().count() == 120));
        // The marker really is split across two physical rows.
        assert!(
            rows.iter()
                .any(|r| r.contains("/model") && !r.contains("/model to change")),
            "fixture must contain the soft-wrapped status row"
        );
        assert_eq!(classify(&rows), ScreenState::Idle);
        assert_eq!(
            active_model(&rows),
            Some(("GLM 5.3 Flash".to_string(), None))
        );
    }

    /// The time-left plan shape must still classify as Idle.
    #[test]
    fn idle_time_left_shapes_classify_as_idle() {
        for status in [
            " GLM 5.3 Flash · 1h left                    ✕ End session",
            " MiMo 2.5 · 58m left                    ✕ End session",
        ] {
            let rows = vec![
                status.to_string(),
                "│  ▍Enter a coding task or / for commands  │".to_string(),
            ];
            assert_eq!(classify(&rows), ScreenState::Idle, "status: {status}");
        }
    }

    /// Idle must key on a live session status row, not merely the input
    /// placeholder: a bare box must NOT be read as Idle.
    #[test]
    fn idle_requires_a_status_row_not_just_a_placeholder() {
        let rows = vec!["│  ▍Enter a coding task or / for commands  │".to_string()];
        assert_ne!(classify(&rows), ScreenState::Idle);
        assert_eq!(classify(&rows), ScreenState::Unknown);
    }

    #[test]
    fn login_gate_classifies_as_login() {
        for text in ["Press ENTER to login to continue", "Press Enter to login"] {
            let rows = vec![String::new(), text.to_string()];
            assert_eq!(classify(&rows), ScreenState::Login, "text: {text}");
        }
    }

    #[test]
    fn active_model_reads_both_idle_fixtures() {
        let rows = load_fixture("idle-midhour-120x40");
        assert_eq!(classify(&rows), ScreenState::Idle);
        assert_eq!(
            active_model(&rows),
            Some(("MiMo 2.5".to_string(), Some("58m left".to_string())))
        );
        let rows = load_fixture("idle-120x40");
        assert_eq!(
            active_model(&rows),
            Some(("GLM 5.3 Flash".to_string(), Some("1h left".to_string())))
        );
    }

    #[test]
    fn active_model_table() {
        // Shape B (older plans): a `<Name> · <N>[hm] left` status row.
        // Shape A (current plans): the `/model` command hint marks the row;
        // the remaining-time figure may be present or absent.
        type Case<'a> = (&'a str, Option<(&'a str, Option<&'a str>)>);
        let cases: &[Case] = &[
            (
                " GLM 5.3 Flash · 59m left · 16.4K (2%)      ✕ End session",
                Some(("GLM 5.3 Flash", Some("59m left"))),
            ),
            (
                " MiMo 2.5 · 58m left                    ✕ End session",
                Some(("MiMo 2.5", Some("58m left"))),
            ),
            (
                " GLM 5.3 Flash · 1h left                    ✕ End session",
                Some(("GLM 5.3 Flash", Some("1h left"))),
            ),
            // Real captured status row on a plan that reports no remaining
            // time (`• max`).
            (
                "  GLM 5.3 Flash • max · /private/var/folders/abc/blink-e2e-42350-1 · /model to change · Chat: New chat",
                Some(("GLM 5.3 Flash", None)),
            ),
            // Shape A with a time figure still reports it.
            (
                " MiMo 2.5 · 58m left · /model to change",
                Some(("MiMo 2.5", Some("58m left"))),
            ),
            // Splash cost lines are not the status row.
            (" Session ended · 25 Freebucks left", None),
            (" 5 Freebucks/hr", None),
            // No `/model`, no time token, no separator, empty name.
            (" MiMo 2.5 · Balanced · Images", None),
            ("MiMo 2.5 58m left", None),
            (" · 58m left", None),
            // The `/model` marker alone is not enough: a name plus a separator
            // is still required.
            (" /model to change", None),
            (" /model", None),
            // Transcript-like row: time-shaped text with trailing prose is
            // not the status row.
            (" Thinking · 5m left on the problem", None),
            ("nothing here", None),
        ];
        for (input, expected) in cases {
            let rows = vec![input.to_string()];
            let expected = expected.map(|(n, t)| (n.to_string(), t.map(|t| t.to_string())));
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
        // Ok: a bare id fragment or display name matches a running name.
        assert_eq!(
            check_active_model("mimo-2.5", "MiMo 2.5", Some("58m left")),
            Ok(())
        );
        assert_eq!(
            check_active_model("glm-5.3-flash", "GLM 5.3 Flash", Some("1h left")),
            Ok(())
        );
        assert_eq!(
            check_active_model("glm-5.3-flash", "GLM 5.3 Flash", None),
            Ok(())
        );
        // Ok: a 0.2.x provider id matches its display name via the table, even
        // when the id and the name share no substring.
        assert_eq!(
            check_active_model("z-ai/glm-5.3-flash", "GLM 5.3 Flash", Some("1h left")),
            Ok(())
        );
        assert_eq!(
            check_active_model("deepseek/deepseek-v4-flash", "DeepSeek V4.1 Flash", None),
            Ok(())
        );
        assert_eq!(
            check_active_model("mimo/mimo-v2.5", "MiMo 2.6 Flash", Some("42m left")),
            Ok(())
        );
        // Ok: a display name resolves through the table too.
        assert_eq!(
            check_active_model("GLM 5.3 Flash", "GLM 5.3 Flash", None),
            Ok(())
        );

        // Err, recognized id that is not the running model: names the staleness
        // explicitly.
        let err = check_active_model("deepseek-v4-pro", "MiMo 2.5", Some("58m left"))
            .expect_err("different model must error");
        assert!(err.contains("an hour on 'MiMo 2.5' is already running (58m left)"));
        assert!(err.contains("requested 'deepseek-v4-pro' is not that model"));
        assert!(err.contains("BLINK_MODEL pin may be stale"));
        assert!(err.contains("unset BLINK_MODEL"));

        // Err, unrecognized id (the reported 'mimo-2.6'): a distinct message.
        let err = check_active_model("mimo-2.6", "GLM 5.3 Flash", Some("1h left"))
            .expect_err("unrecognized model must error");
        assert!(err.contains("an hour on 'GLM 5.3 Flash' is already running (1h left)"));
        assert!(err.contains("requested 'mimo-2.6' is not a freebuff 0.2.x model id"));
        assert!(err.contains("/model picker"));

        // Enforcement still fires when the plan reports no time left.
        let err = check_active_model("deepseek-v4-pro", "GLM 5.3 Flash", None)
            .expect_err("different model must error");
        assert!(err.contains("an hour on 'GLM 5.3 Flash' is already running;"));
        assert!(err.contains("not that model"));
    }

    #[test]
    fn model_id_display_resolves_ids_and_names() {
        assert_eq!(
            model_display_name("z-ai/glm-5.3-flash"),
            Some("GLM 5.3 Flash")
        );
        assert_eq!(model_display_name("glm-5.3-flash"), Some("GLM 5.3 Flash"));
        assert_eq!(model_display_name("mimo/mimo-v2.5"), Some("MiMo 2.6 Flash"));
        assert_eq!(
            model_display_name("deepseek/deepseek-v4-flash"),
            Some("DeepSeek V4.1 Flash")
        );
        assert_eq!(model_display_name("mimo-2.6"), None);
        assert_eq!(model_display_name(""), None);

        assert_eq!(
            resolve_model_id("z-ai/glm-5.3-flash"),
            Some("z-ai/glm-5.3-flash")
        );
        assert_eq!(
            resolve_model_id("GLM 5.3 Flash"),
            Some("z-ai/glm-5.3-flash")
        );
        assert_eq!(resolve_model_id("mimo/mimo-v2.5"), Some("mimo/mimo-v2.5"));
        assert_eq!(
            resolve_model_id("deepseek/deepseek-v4-flash-fast"),
            Some("deepseek/deepseek-v4-flash-fast")
        );
        assert_eq!(resolve_model_id("mimo-2.6"), None);
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

    // --- freebuff v0.2.29 HOURS gate --------------------------------------
    //
    // The 2026-10-16 live run showed an interstitial bufflink did not classify:
    // freebuff 0.2.29 bills a session in HOURS and refuses a turn with
    // `Not enough hours for <Model>: an hour on it takes N hours.`. Instead of
    // a clear error the turn ended in an opaque null. The fixture is the
    // VERBATIM frame from that run.

    #[test]
    fn v0229_real_hours_gate_fixture_classifies_as_hours_gate() {
        let rows = load_fixture("0229-real-hours-gate-120x40");
        match classify(&rows) {
            ScreenState::HoursGate {
                model,
                hours,
                message,
            } => {
                assert_eq!(model.as_deref(), Some("MiMo Fast"));
                assert_eq!(hours, Some(2));
                assert!(message.contains("Not enough hours for"));
            }
            other => panic!("expected HoursGate, got {other:?}"),
        }
    }

    #[test]
    fn v0229_hours_gate_parses_varying_model_and_figure() {
        let gate = |line: &str| classify(&[line.to_string()]);
        // Neither the model nor the figure is hardcoded.
        match gate("Not enough hours for Optima Prime: an hour on it takes 9 hours.") {
            ScreenState::HoursGate { model, hours, .. } => {
                assert_eq!(model.as_deref(), Some("Optima Prime"));
                assert_eq!(hours, Some(9));
            }
            other => panic!("expected HoursGate, got {other:?}"),
        }
        // A single-hour figure is tolerated.
        match gate("│ Not enough hours for Tiny: an hour on it takes 1 hour. │") {
            ScreenState::HoursGate { hours, .. } => assert_eq!(hours, Some(1)),
            other => panic!("expected HoursGate, got {other:?}"),
        }
        // Parsing fails: STILL a gate (the shortfall sentence is present but its
        // figures do not parse), raw line carried, never a vague error.
        let rows = vec![
            "Not enough hours for".to_string(),
            "an hour on it takes hours.".to_string(),
        ];
        match classify(&rows) {
            ScreenState::HoursGate {
                model,
                hours,
                message,
            } => {
                assert_eq!(model, None);
                assert_eq!(hours, None);
                assert!(message.contains("Not enough hours for"));
            }
            other => panic!("expected HoursGate, got {other:?}"),
        }
    }

    /// BLOCKING regression: a model-picker screen whose rows carry a per-model
    /// `Not enough hours for <Model>` affordability warning label must NOT be
    /// read as the terminal hours gate. The bare phrase alone no longer
    /// classifies as a gate — only the gate's full shape (its shortfall
    /// sentence) does — so a healthy picker is never aborted as unaffordable.
    #[test]
    fn v0229_picker_per_model_hours_warning_is_not_the_hours_gate() {
        // An expanded picker (2+ model boxes + a `Show fewer` toggle) whose
        // second model shows the warning label instead of a plain price.
        let rows: Vec<String> = [
            "                    UNLIMITED",
            "                    ┌────────────────────────────────────────────────┐",
            "                    │ › GLM 5.3 Flash · Deep reasoning · Images      │",
            "                    │                      5 Freebucks/hr · Recommended │",
            "                    └────────────────────────────────────────────────┘",
            "                    ┌────────────────────────────────────────────────┐",
            "                    │   MiMo Fast · Smart & Fast · Images            │",
            "                    │            Not enough hours for MiMo Fast      │",
            "                    └────────────────────────────────────────────────┘",
            "                    ↑  Show fewer",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        // The fixture really does carry the phrase; only the shape is missing.
        assert!(
            rows.iter().any(|r| r.contains("Not enough hours for")),
            "the picker warning label must be present for this test to bite"
        );
        let state = classify(&rows);
        assert!(
            !matches!(state, ScreenState::HoursGate { .. }),
            "a per-model picker warning label must not be the terminal gate: {state:?}"
        );
        assert!(
            matches!(state, ScreenState::ModelList | ScreenState::ModelSplash),
            "the picker must still classify as a picker state: {state:?}"
        );
    }

    /// The older Freebucks gate must still classify as ITS gate, never the new
    /// hours gate (regression guard for the 0.2.29 addition).
    #[test]
    fn v0229_freebucks_gate_is_not_the_hours_gate() {
        let rows = load_fixture("freebucks-gate-80x24");
        assert!(matches!(classify(&rows), ScreenState::FreebucksGate { .. }));
        assert!(!matches!(classify(&rows), ScreenState::HoursGate { .. }));
    }

    /// Fail-loud guard: a normal idle screen must never be misread as the gate.
    #[test]
    fn v0229_idle_screen_is_not_the_hours_gate() {
        for name in ["idle-120x40", "idle-max-plan-120x40"] {
            let rows = load_fixture(name);
            assert_eq!(classify(&rows), ScreenState::Idle, "{name}");
            assert!(
                !matches!(classify(&rows), ScreenState::HoursGate { .. }),
                "{name} must not be the hours gate"
            );
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

    // --- freebuff v0.2.x picker -------------------------------------------
    //
    // The `0227-synthetic-*` fixtures are HAND-CONSTRUCTED from freebuff
    // 0.2.27's own render code (bordered single-box model rows, UPPERCASE
    // section headers, the templated `See all <N> models` / `Show fewer`
    // toggle, centre-padded price rows). They are SYNTHETIC, derived from the
    // bundle, not captured from a live run — replace them with real captures
    // when available. See tests/fixtures/screens/0227-synthetic-NOTE.md.

    #[test]
    fn v0227_collapsed_picker_classifies_as_model_splash() {
        let rows = load_fixture("0227-synthetic-splash-collapsed-120x40");
        assert_eq!(classify(&rows), ScreenState::ModelSplash);
    }

    #[test]
    fn v0227_expanded_picker_variants_classify_as_model_list() {
        for name in [
            "0227-synthetic-splash-expanded-120x40",
            "0227-synthetic-splash-trial-120x40",
            "0227-synthetic-splash-scrolled-120x40",
            "0227-synthetic-splash-focus-120x40",
            "0227-synthetic-splash-narrow-80x24",
        ] {
            let rows = load_fixture(name);
            assert_eq!(classify(&rows), ScreenState::ModelList, "{name}");
        }
    }

    #[test]
    fn v0227_counting_toggle_carries_splash_without_old_copy() {
        // v0.2.27 dropped "Start coding for free" and "Select a model": the
        // templated toggle must carry the splash for ANY count.
        for n in [1_usize, 4, 12, 13, 100] {
            let rows = vec![format!("↓  See all {n} models")];
            assert_eq!(classify(&rows), ScreenState::ModelSplash, "n={n}");
        }
        // The new picker title is a fallback even with no toggle line.
        let rows = vec!["Choose a model to start a new session".to_string()];
        assert_eq!(classify(&rows), ScreenState::ModelSplash);
    }

    #[test]
    fn advertised_model_count_parses_any_count() {
        assert_eq!(
            advertised_model_count(&["↓  See all 12 models".to_string()]),
            Some(12)
        );
        assert_eq!(
            advertised_model_count(&["↓  See all 4 models".to_string()]),
            Some(4)
        );
        assert_eq!(
            advertised_model_count(&["   banner".to_string(), "↓  See all 13 models".to_string(),]),
            Some(13)
        );
        // The expanded form carries no count.
        assert_eq!(advertised_model_count(&["↑  Show fewer".to_string()]), None);
        assert_eq!(advertised_model_count(&["nothing here".to_string()]), None);
    }

    #[test]
    fn v0227_collapsed_catalog_parses_single_model() {
        let rows = load_fixture("0227-synthetic-splash-collapsed-120x40");
        let catalog = parse_catalog(&rows);
        assert_eq!(catalog.advertised, Some(12));
        assert_eq!(catalog.entries.len(), 1);
        let m = &catalog.entries[0];
        assert_eq!(m.display_name, "GLM 5.3 Flash");
        assert_eq!(m.section, ModelSection::Unknown);
        assert_eq!(m.price_per_hour, Some(5));
        assert!(m.recommended);
        assert!(m.available);
        assert!(m.focused);
    }

    #[test]
    fn v0227_expanded_catalog_sections_names_and_status() {
        let rows = load_fixture("0227-synthetic-splash-expanded-120x40");
        let catalog = parse_catalog(&rows);
        // The `Show fewer` toggle carries no advertised count.
        assert_eq!(catalog.advertised, None);
        let got: Vec<(&str, ModelSection, Option<u32>)> = catalog
            .entries
            .iter()
            .map(|e| (e.display_name.as_str(), e.section, e.price_per_hour))
            .collect();
        assert_eq!(
            got,
            vec![
                ("GLM 5.3 Flash", ModelSection::Unlimited, Some(5)),
                ("Muse Spark 1.3", ModelSection::Unlimited, Some(5)),
                ("MiMo 2.6 Pro", ModelSection::Optimized, Some(10)),
                ("GPT-6 Luna", ModelSection::Optimized, Some(10)),
                ("GPT-6.1 Sol", ModelSection::Powerful, Some(15)),
                ("Claude Fable 5.1", ModelSection::Powerful, Some(15)),
            ]
        );
        // Recommended flag on the first entry only.
        assert!(catalog.entries[0].recommended);
        assert!(catalog.entries[1..].iter().all(|e| !e.recommended));
        // The `Not enough Freebucks` warning marks GPT-6.1 Sol unavailable.
        assert!(!catalog.entries[4].available);
        assert!(catalog
            .entries
            .iter()
            .enumerate()
            .all(|(i, e)| e.available == (i != 4)));
        // Focus is on the first entry.
        assert!(catalog.entries[0].focused);
        assert!(catalog.entries[1..].iter().all(|e| !e.focused));
    }

    #[test]
    fn v0227_catalog_never_emits_headers_or_toggle() {
        let rows = load_fixture("0227-synthetic-splash-expanded-120x40");
        let catalog = parse_catalog(&rows);
        for e in &catalog.entries {
            let upper = e.display_name.to_ascii_uppercase();
            assert_ne!(upper, "UNLIMITED");
            assert_ne!(upper, "OPTIMIZED");
            assert_ne!(upper, "POWERFUL");
            assert!(!e.display_name.contains("See all"));
            assert!(!e.display_name.contains("Show fewer"));
        }
    }

    #[test]
    fn v0227_trial_section_and_locked_model() {
        let rows = load_fixture("0227-synthetic-splash-trial-120x40");
        let catalog = parse_catalog(&rows);
        let sections: Vec<ModelSection> = catalog.entries.iter().map(|e| e.section).collect();
        assert_eq!(
            sections,
            vec![
                ModelSection::Unlimited,
                ModelSection::Unlimited,
                ModelSection::LimitedTrial,
                ModelSection::LimitedTrial,
            ]
        );
        // The locked model is unavailable, and its `Unlock` label is not read
        // as a price.
        let locked = &catalog.entries[3];
        assert_eq!(locked.display_name, "Ox Alpha");
        assert!(!locked.available);
        assert_eq!(locked.price_per_hour, None);
        // The trial model itself is usable.
        assert!(catalog.entries[2].available);
        assert_eq!(catalog.entries[2].price_per_hour, Some(1));
    }

    #[test]
    fn v0227_scrolled_catalog_visible_lt_advertised() {
        let rows = load_fixture("0227-synthetic-splash-scrolled-120x40");
        let catalog = parse_catalog(&rows);
        assert_eq!(catalog.entries.len(), 4);
        assert_eq!(catalog.advertised, Some(13));
        assert!(catalog.entries.len() < catalog.advertised.unwrap());
        let ring = nav_ring(&rows).expect("ring");
        assert_eq!(ring.visible_models, 4);
        assert_eq!(ring.advertised_models, Some(13));
        assert_eq!(ring.toggle_index, Some(13));
        assert_eq!(ring.ring_len, 14);
    }

    #[test]
    fn v0227_focus_tracks_second_entry() {
        let rows = load_fixture("0227-synthetic-splash-focus-120x40");
        let catalog = parse_catalog(&rows);
        assert_eq!(catalog.entries.len(), 3);
        assert_eq!(catalog.entries[1].display_name, "Muse Spark 1.3");
        assert!(catalog.entries[1].focused);
        assert!(!catalog.entries[0].focused);
        assert!(!catalog.entries[2].focused);
    }

    #[test]
    fn v0227_narrow_compacted_catalog() {
        let rows = load_fixture("0227-synthetic-splash-narrow-80x24");
        let catalog = parse_catalog(&rows);
        let got: Vec<(&str, ModelSection, Option<u32>)> = catalog
            .entries
            .iter()
            .map(|e| (e.display_name.as_str(), e.section, e.price_per_hour))
            .collect();
        assert_eq!(
            got,
            vec![
                ("GLM 5.3 Flash", ModelSection::Unlimited, Some(5)),
                ("MiMo 2.6 Pro", ModelSection::Optimized, Some(10)),
                // Cut-off box with no right border still parses.
                ("DeepSeek V4 Pro", ModelSection::Optimized, Some(15)),
            ]
        );
        assert!(catalog.entries[0].focused);
    }

    #[test]
    fn nav_ring_reports_toggle_and_ring_length() {
        // Collapsed: one visible model, 12 advertised, toggle sits last.
        let rows = load_fixture("0227-synthetic-splash-collapsed-120x40");
        let ring = nav_ring(&rows).expect("ring");
        assert_eq!(ring.visible_models, 1);
        assert_eq!(ring.advertised_models, Some(12));
        assert_eq!(ring.toggle_index, Some(12));
        assert_eq!(ring.ring_len, 13);
        // No count and no toggle row: the ring is just the visible models.
        let rows = vec![
            "┌──────────────────────────┐".to_string(),
            "│ MiMo 2.6 Pro · Balanced │".to_string(),
            "└──────────────────────────┘".to_string(),
        ];
        let ring = nav_ring(&rows).expect("ring");
        assert_eq!(ring.visible_models, 1);
        assert_eq!(ring.advertised_models, None);
        assert_eq!(ring.toggle_index, None);
        assert_eq!(ring.ring_len, 1);
    }

    #[test]
    fn nav_ring_none_without_models_or_toggle() {
        assert_eq!(nav_ring(&[]), None);
        assert_eq!(nav_ring(&["Freebuff will run commands".to_string()]), None);
    }

    #[test]
    fn plan_select_step_walks_past_the_old_fixed_cap() {
        // 13 models plus the toggle: a 14-stop ring. The old
        // MAX_DOWN_PRESSES = 12 wrapped forever without landing; the
        // ring-derived bound covers the whole ring.
        let rows: Vec<ModelRow> = (0..13)
            .map(|i| ModelRow {
                name: format!("Model {i:02}"),
                section: ModelSection::Unknown,
                focused: i == 0,
                cost_line: None,
            })
            .collect();
        assert_eq!(
            plan_select_step("Model 12", &rows, true, 14, 12),
            SelectStep::Down,
            "12 presses must not cap the walk short of the 14-stop ring"
        );
        assert_eq!(
            plan_select_step("Model 12", &rows, true, 14, 13),
            SelectStep::Down
        );
        match plan_select_step("Model 12", &rows, true, 14, 14) {
            SelectStep::NotOffered { offered } => assert_eq!(offered.len(), 13),
            other => panic!("expected NotOffered, got {other:?}"),
        }
    }

    #[test]
    fn v0227_keeps_old_v0_detectors() {
        // The v0.0.x anchors still classify an older freebuff.
        let collapsed = vec![
            "Start coding for free".to_string(),
            "↓  See all 4 models".to_string(),
            "5 Freebucks/hr".to_string(),
        ];
        assert_eq!(classify(&collapsed), ScreenState::ModelSplash);
        assert_eq!(
            classify(&load_fixture("splash-120x40")),
            ScreenState::ModelSplash
        );
        assert_eq!(
            classify(&load_fixture("splash-list-120x40")),
            ScreenState::ModelList
        );
    }

    // --- Submit confirmation (0229) ---------------------------------------
    //
    // The 2026-10-16 live run failed to confirm a submitted prompt although the
    // text was plainly in the input box: freebuff v0.2.29 draws a promo banner
    // in a ROUNDED box (`╭…╮`/`╰…╯`) ABOVE the input box, and `input_box_text`
    // returned the first rounded box — the banner. The real frame is kept
    // verbatim as `0227-real-banner-and-input-120x40`; the rest are synthetic,
    // derived from that frame's layout. The driver's prefix is the first 20
    // chars, exactly as `submit_prompt` computes it.

    const PROMPT_PREFIX: &str = "Count from 1 to 400,";

    #[test]
    fn v0229_real_banner_frame_confirms_the_prompt_in_the_input_box() {
        let rows = load_fixture("0227-real-banner-and-input-120x40");
        assert_eq!(rows.len(), 40);
        // The banner is present and must NOT be dismissed or suppressed.
        assert!(rows.iter().any(|r| r.contains("New model")));
        assert!(rows.iter().any(|r| r.contains("See plans  freebuff.com")));
        assert_eq!(classify(&rows), ScreenState::Idle);
        // The input box is the bottom-most rounded box, so its text — not the
        // banner's — is what the detector reads. This is the exact regression:
        // first-box-wins returned the banner.
        let box_text = input_box_text(&rows).expect("input box");
        assert!(
            box_text.contains("Count from 1 to 400"),
            "must read the input box, got: {box_text:?}"
        );
        assert!(
            !box_text.contains("See plans"),
            "must not read the promo banner: {box_text:?}"
        );
        assert!(input_box_shows(&rows, PROMPT_PREFIX));
        assert!(prompt_visible(&rows, PROMPT_PREFIX));
    }

    #[test]
    fn v0229_soft_wrapped_prompt_is_confirmed() {
        let rows = load_fixture("0227-synthetic-wrapped-input-120x40");
        assert!(rows.iter().any(|r| r.contains("one number per")));
        assert!(rows.iter().any(|r| r.contains("line, with a short")));
        // The driver's prefix lands on the first wrapped row.
        assert!(prompt_visible(&rows, PROMPT_PREFIX));
        // A needle spanning the wrap only matches once the rows are joined.
        assert!(input_box_shows(&rows, "one number per line"));
        assert!(input_box_text(&rows)
            .unwrap()
            .contains("Count from 1 to 400"));
    }

    #[test]
    fn v0229_wrapped_prompt_with_banner_is_confirmed() {
        let rows = load_fixture("0227-synthetic-wrapped-banner-120x40");
        assert!(rows.iter().any(|r| r.contains("See plans  freebuff.com")));
        assert!(prompt_visible(&rows, PROMPT_PREFIX));
        assert!(input_box_shows(&rows, "one number per line"));
        // The banner sits above the box and is neither dismissed nor read.
        assert!(!input_box_text(&rows).unwrap().contains("See plans"));
    }

    /// Regression guard: the pasted-text chip form still confirms a submit.
    #[test]
    fn v0229_pasted_chip_still_confirms_a_submit() {
        let rows = load_fixture("paste-chip-120x40");
        assert_eq!(pasted_chip_chars(&rows), Some(5001));
        // The box itself shows the placeholder; confirmation comes from the chip.
        assert!(!input_box_shows(&rows, PROMPT_PREFIX));
        assert!(prompt_visible(&rows, PROMPT_PREFIX));
    }

    /// Fail-loud: an empty input box must never look submitted.
    #[test]
    fn v0229_empty_input_box_is_not_confirmed() {
        let rows = load_fixture("0227-synthetic-empty-input-120x40");
        assert_eq!(input_box_text(&rows).as_deref(), Some(""));
        assert!(input_box_is_empty(&rows));
        assert!(!input_box_shows(&rows, PROMPT_PREFIX));
        assert!(!prompt_visible(&rows, PROMPT_PREFIX));
        // An empty needle matches nothing.
        assert!(!input_box_shows(&rows, ""));
    }

    /// Fail-loud: unrelated text in the box does not satisfy the prefix.
    #[test]
    fn v0229_unrelated_input_box_is_not_confirmed() {
        let rows = load_fixture("0227-synthetic-unrelated-input-120x40");
        assert!(input_box_text(&rows).unwrap().contains("python"));
        assert!(!input_box_shows(&rows, PROMPT_PREFIX));
        assert!(!prompt_visible(&rows, PROMPT_PREFIX));
    }

    /// The last rounded box wins, but its content is still returned when the
    /// box has no closing border (the bottom scrolled off).
    #[test]
    fn input_box_text_prefers_the_bottom_box_and_tolerates_no_close() {
        let rows = vec![
            "╭────╮".to_string(),
            "│ up │".to_string(),
            "╰────╯".to_string(),
            "╭────╮".to_string(),
            "│ low│".to_string(),
        ];
        assert_eq!(input_box_text(&rows).as_deref(), Some("low"));
    }

    // --- Real freebuff v0.2.29: no startup picker + the `/model` picker ------
    //
    // The 2026-10-17 live probe: freebuff 0.2.29 boots STRAIGHT to the idle
    // input box with a model already selected (NO startup picker); the model
    // picker is opened by the `/model` command, and it opens as a full
    // SCROLLABLE list (sections, no `See all`/`Show fewer` toggle). Both frames
    // below are VERBATIM from that probe.

    #[test]
    fn v0229_real_startup_frame_is_idle_not_a_picker() {
        let rows = load_fixture("0229-real-startup-no-picker-120x40");
        // The startup screen has no model picker: it is the idle/input state
        // whose status row carries `/model to change`.
        assert_eq!(classify(&rows), ScreenState::Idle);
        assert_eq!(
            active_model(&rows),
            Some(("Nemotron 3.5 Lightning".to_string(), None))
        );
        // There is genuinely no picker on this screen.
        assert_eq!(parse_catalog(&rows).entries.len(), 0);
        assert!(model_rows(&rows).is_empty());
        assert_eq!(advertised_model_count(&rows), None);
    }

    #[test]
    fn v0229_real_slash_model_picker_parses_sections_and_names() {
        let rows = load_fixture("0229-real-slash-model-picker-120x40");
        assert_eq!(classify(&rows), ScreenState::ModelList);
        let catalog = parse_catalog(&rows);
        assert_eq!(catalog.advertised, None, "the /model picker has no See-all");
        let got: Vec<(&str, ModelSection)> = catalog
            .entries
            .iter()
            .map(|e| (e.display_name.as_str(), e.section))
            .collect();
        // The ` • <effort>` suffix is stripped; section headers are sections.
        assert_eq!(
            got,
            vec![
                ("Nemotron 3.5 Lightning", ModelSection::Unlimited),
                ("Glyph Cluster", ModelSection::Unlimited),
                ("Solar Pro 4", ModelSection::Unlimited),
                ("Solar Mini 4", ModelSection::Unlimited),
                ("DeepSeek V4.1 Flash", ModelSection::Optimized),
            ]
        );
        // The `/model` picker prints no `Freebucks/hr` figure.
        assert!(catalog.entries.iter().all(|e| e.price_per_hour.is_none()));
        // The chrome (info box, `↑↓ choose model …` footer) is not a model.
        for e in &catalog.entries {
            assert!(!e.display_name.contains("Copy invite link"));
            assert!(!e.display_name.contains("choose model"));
        }
    }

    /// A scrolled picker can leave only a box's CENTERED continuation row
    /// visible (`3x · Recommended · …`), whose derived "name" is `3x`. That is
    /// not a model and must not be reported as one.
    #[test]
    fn v0229_scrolled_continuation_row_is_not_a_model() {
        let rows = vec![
            "│               3x · Recommended · May use data for AI training                │"
                .to_string(),
            "│                            Get 7x usage for $5 →                             │"
                .to_string(),
            "↓  See all 13 models".to_string(),
        ];
        assert!(parse_catalog(&rows).entries.is_empty());
        assert!(model_rows(&rows).is_empty());
    }

    /// BLOCKING regression (real frame): a model box's wrapped DESCRIPTION row
    /// is LEFT-aligned at one space and can contain the tagline separator, e.g.
    /// `detail. Claude Haiku 5.5 · 100k context is the same model at a lower
    /// hourly`. It was read as a model named `detail. Claude Haiku 5.5` and
    /// inflated `--json` with a junk entry.
    #[test]
    fn v0229_real_wrapped_description_row_is_not_a_model() {
        let rows = load_fixture("0229-real-picker-wrapped-description-120x40");
        // The junk row really is in the frame, or this test cannot bite.
        assert!(
            rows.iter()
                .any(|r| r.contains(" detail. Claude Haiku 5.5 · ")),
            "fixture must carry the wrapped description row"
        );
        let models = model_rows(&rows);
        assert!(
            models.iter().all(|m| !m.name.contains("detail")),
            "the description row must not be read as a model: {:?}",
            models.iter().map(|m| m.name.as_str()).collect::<Vec<_>>()
        );
        // Only real names survive, and no name is a sentence fragment.
        assert_eq!(
            models.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            vec![
                "Claude Haiku 5.5",
                "MiMo 2.6 Pro",
                "DeepSeek V4.1 Flash Fast",
                "Claude Haiku 5.5",
            ]
        );
        // This frame's POWERFUL header has scrolled off, so nothing on it can
        // know its section — the walk carries it across frames (see
        // `enumerate::collect_frame`).
        assert!(models.iter().all(|m| m.section == ModelSection::Unknown));
    }

    /// The frame that still has the `POWERFUL` header: its models take that
    /// section, which is what the walk then carries forward.
    #[test]
    fn v0229_real_powerful_section_frame_attributes_the_section() {
        let rows = load_fixture("0229-real-picker-powerful-section-120x40");
        let models = model_rows(&rows);
        // The header is the first content in this frame, so every model on it
        // belongs to POWERFUL.
        assert_eq!(
            models.first().map(|m| m.name.as_str()),
            Some("Gemini 4 Argon")
        );
        assert!(
            models.iter().all(|m| m.section == ModelSection::Powerful),
            "every model in this frame must be attributed: {:?}",
            models
                .iter()
                .map(|m| (m.name.as_str(), m.section))
                .collect::<Vec<_>>()
        );
        assert!(
            models.iter().any(|m| m.name == "DeepSeek V4.1 Flash Fast"),
            "the frame must carry the POWERFUL models"
        );
    }

    /// `  /model      Choose the model for your next message` / `  /model` — the
    /// strict box predicate and the popup marker, on a REAL 0.2.29 popup frame.
    ///
    /// `input_box_shows` (a substring match) accepts `foo/model`; submitting such
    /// a box sends a MESSAGE, which starts a billed session. The strict predicate
    /// must not.
    #[test]
    fn v0229_real_slash_popup_is_detected_and_the_box_holds_exactly_the_command() {
        let rows = load_fixture("0229-real-slash-popup-120x40");
        assert_eq!(classify(&rows), ScreenState::Idle);
        assert!(
            slash_popup_shows(&rows),
            "the real autocomplete popup must be detected"
        );
        assert!(
            input_box_is_exactly(&rows, "/model"),
            "the popup's chrome is not typed text: the box holds exactly /model"
        );
        assert!(!input_box_is_empty(&rows));
        // The old substring check would also have accepted these — the strict
        // one must not.
        for bad in ["/model/model", "foo/model", "/mod", "/model junk", ""] {
            assert!(
                !input_box_is_exactly(&idle_box(bad), "/model"),
                "box {bad:?} must NOT be accepted as exactly /model"
            );
        }
        assert!(input_box_is_exactly(&idle_box("/model"), "/model"));
        // A lone `/model` line is not a menu row, so a CLOSED popup is not
        // mistaken for an open one.
        assert!(!slash_popup_shows(&idle_box("/model")));
    }

    /// An idle screen whose input box holds `box_text`.
    fn idle_box(box_text: &str) -> Vec<String> {
        vec![
            " GLM 5.3 Flash · 59m left · 16.4K (2%)      ✕ End session".to_string(),
            "╭──────────────────────────────────────╮".to_string(),
            format!("│  ▍{box_text}  │"),
            "╰──────────────────────────────────────╯".to_string(),
        ]
    }
}
