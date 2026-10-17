//! `src/freebuff/enumerate.rs` — enumerate freebuff's model catalog from its TUI.
//!
//! Drives a freebuff PTY session through the model picker: expands it via the
//! See-all/Show-fewer toggle row only (Enter on a model row calls
//! `startSession` and SPENDS Freebucks), collects every display name while
//! scrolling, and maps them to model ids.
//!
//! The freebuff binary is injected through `PtyConfig.program` (default
//! `freebuff`, overridable with `BLINK_FREEBUFF_BIN`). No freebuff backend is
//! called and no credentials are reused.
//!
//! `--list-models` prints one model id per line and og consumes that list as
//! pinnable ids, so a display name must never be emitted as one: an unmapped
//! name is dropped from the plain output (and flagged on stderr / in `--json`).

use crate::freebuff::screen::{
    classify, input_box_is_empty, input_box_is_exactly, model_rows, nav_ring,
    toggle_row_has_focus_marker, ModelRow, ModelSection, ScreenState, TAKE_OVER_ERROR,
};
use crate::pty::{Key, Pty};
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::time::{Duration, Instant};

/// Total time allowed for the whole enumeration so it cannot spin forever.
const ENUMERATION_TIMEOUT: Duration = Duration::from_secs(30);
/// Settle after a keypress before re-reading the screen.
const SETTLE: Duration = Duration::from_millis(300);
/// Max Down presses before giving up (prevents an infinite scroll walk).
const MAX_DOWN_PRESSES: u32 = 100;
/// Max times `/model` is typed before giving up. freebuff flushes its input
/// queue when it finishes initialising, so the first attempt can vanish; a
/// single re-type covers it, and the bound keeps a wedged TUI from looping.
const MAX_TYPE_ATTEMPTS: u32 = 4;
/// Max Backspaces used to clear a NON-EMPTY input box before typing. Bounded,
/// and the box is re-read afterwards and must be empty: a counted clear against
/// arbitrary text cannot be trusted, so an unclearable box aborts instead.
const MAX_CLEAR_BACKSPACES: u32 = 64;
/// How many unreadable frames to tolerate before refusing to press Enter. A
/// half-painted frame can classify as the splash before its model row is drawn;
/// re-reading a few times is safe because no Enter is sent while unverified.
const MAX_UNREADABLE_FRAMES: u32 = 5;

/// Source of a catalog entry's id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdSource {
    /// The display name matched a known entry in the lineup map.
    Mapped,
    /// No mapping existed; the entry carries no id (`id: null`).
    Unmapped,
}

/// One catalog entry, in picker order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogEntry {
    /// Canonical freebuff model id, or `None` when the display name had no
    /// known mapping (the lineup rotated). `None` serialises as `"id":null` so
    /// the entry stays visible in `--json` without a name-shaped fake id.
    pub id: Option<String>,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// Cost of an hour on this model, in the picker's own unit for the figure it
    /// prints next to a model (`N Freebucks/hr` on older builds). The name is
    /// kept for wire compatibility — og consumes this shape.
    ///
    /// The unit is deliberately NOT renamed: freebuff 0.2.x bills a session in
    /// HOURS (`an hour on it takes N hours`), and the 0.2.29 `/model` picker
    /// prints no `Freebucks/hr` figure at all, so this is `0` for the free
    /// `unlimited` section and `null` (omitted) for every model whose price the
    /// picker does not state. It is a probe result, never a computed one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_per_hour: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available: Option<bool>,
    #[serde(skip)]
    pub id_source: IdSource,
}

/// Map a display name to a model id. A name with no known mapping yields
/// `(None, IdSource::Unmapped)`: the lineup rotates, but a display name is NOT
/// a model id, and emitting one would break og (it would offer the name as a
/// pinnable id that fails at dispatch). The unmapped entry is still reported in
/// `--json` and named on stderr by [`unmapped_warning`].
pub fn map_display_name(name: &str) -> (Option<String>, IdSource) {
    const LINEUP: &[(&str, &str)] = &[
        ("GLM 5.3 Flash", "z-ai/glm-5.3-flash"),
        ("MiMo 2.6 Pro", "mimo/mimo-v2.6-pro"),
        ("MiMo 2.6 Flash", "mimo/mimo-v2.5"),
        ("GPT-6 Luna", "openai/gpt-6-luna"),
        ("GPT-6.1 Sol", "openai/gpt-6.1-sol"),
        ("DeepSeek V4.1 Flash", "deepseek/deepseek-v4-flash"),
        ("Solar Pro 4", "upstage/solar-pro4"),
        ("Solar Mini 4", "upstage/solar-mini4"),
        ("Muse Spark 1.3", "meta/muse-spark-1.3-contributor"),
        ("Gemini 3.8 Flash", "google/gemini-3.8-flash"),
    ];
    for &(display, id) in LINEUP {
        if display == name {
            return (Some(id.to_string()), IdSource::Mapped);
        }
    }
    (None, IdSource::Unmapped)
}

/// Whether `id` is a well-formed freebuff model id: lowercase ASCII letters and
/// digits with `.`, `_`, `-`, `/` separators, starting alphanumeric, 2..=128
/// bytes. This is the shape freebuff itself accepts — a provider-prefixed id
/// (`z-ai/glm-5.3-flash`), a bare path (`mimo/mimo-v2.6-pro`) or an opaque
/// picker key (`m-79293c5b7d`) all pass; a display name such as
/// `Claude Fable 5.1` (spaces, uppercase) never does.
pub fn is_well_formed_model_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() < 2 || bytes.len() > 128 {
        return false;
    }
    let first = bytes[0];
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    bytes.iter().all(|b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(*b, b'.' | b'_' | b'-' | b'/')
    })
}

/// The ids `--list-models` prints, one per line: only entries with a mapped,
/// well-formed id. An unmapped (or malformed) entry is omitted so og is never
/// offered an unusable id; it is still named by [`unmapped_warning`] and kept
/// in the `--json` catalog.
pub fn plain_model_ids(catalog: &[CatalogEntry]) -> Vec<&str> {
    catalog
        .iter()
        .filter_map(|e| e.id.as_deref())
        .filter(|id| is_well_formed_model_id(id))
        .collect()
}

/// Display names with no known id mapping, in picker order.
pub fn unmapped_display_names(catalog: &[CatalogEntry]) -> Vec<&str> {
    catalog
        .iter()
        .filter(|e| e.id.is_none())
        .map(|e| e.display_name.as_str())
        .collect()
}

/// A single stderr line naming every unmapped display name, or `None` when all
/// entries mapped. Pure so `main` and the tests share the exact wording: a
/// rotation is visible in og's setup output without corrupting stdout.
pub fn unmapped_warning(catalog: &[CatalogEntry]) -> Option<String> {
    let names = unmapped_display_names(catalog);
    if names.is_empty() {
        return None;
    }
    Some(format!(
        "blink: warning: no model id known for display name(s): {} — omitted from --list-models so og is never offered an unusable id; see --list-models --json (\"id\":null) for the full lineup",
        names.join(", ")
    ))
}

/// Parse the price (Freebucks/hr) from a cost line such as `5 Freebucks/hr`
/// or `15 Freebucks/hr · Off-peak 10/hr`. Returns `None` when no price is
/// present rather than inventing one.
pub fn price_per_hour_from_cost_line(line: &str) -> Option<u64> {
    let mut digits = String::new();
    let mut started = false;
    for ch in line.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            started = true;
        } else if started {
            break;
        }
    }
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

/// Positive focus evidence for the model rows on one screen.
///
/// Enter on a model row starts a session and SPENDS Freebucks, so focus is
/// never inferred from ABSENCE: an empty parse is [`ModelFocus::NoRows`], never
/// "nothing is focused".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelFocus {
    /// Exactly one model row positively carries the focus marker, at this index
    /// within the parsed model rows.
    Single(usize),
    /// Model rows parsed, but none carries the focus marker.
    NoneMarked,
    /// No model row parsed at all — must never be read as "nothing focused".
    NoRows,
    /// Two or more model rows carry the marker: not a single-focus ring.
    Multiple,
}

/// Read positive focus evidence from a screen. [`model_rows`] and the
/// screen-catalog parser read the SAME rows and miss the SAME glyph, so they
/// are not independent evidence; a row counts as focused only when the marker
/// is positively present. A row drawn with a changed glyph (e.g. `❯`) parses as
/// an unfocused model row, which is exactly the frame that must not be mistaken
/// for "the toggle is focused".
fn model_focus(rows: &[String]) -> ModelFocus {
    let models = model_rows(rows);
    if models.is_empty() {
        return ModelFocus::NoRows;
    }
    let mut hit = None;
    let mut marked = 0usize;
    for (i, m) in models.iter().enumerate() {
        if m.focused {
            marked += 1;
            hit = Some(i);
        }
    }
    match marked {
        0 => ModelFocus::NoneMarked,
        1 => ModelFocus::Single(hit.expect("marked == 1")),
        _ => ModelFocus::Multiple,
    }
}

/// State of the validated walk that reaches the picker's See-all/Show-fewer
/// toggle row.
///
/// Focus is tracked POSITIVELY: the walk starts only from a row whose focus
/// marker is detected, and every Down press is validated against the row the
/// rendered ring says focus must move to. It never concludes "the toggle is
/// focused" from the absence of a marker on any model row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToggleWalk {
    /// Focused model-row index the walk verified before its first press.
    start: Option<usize>,
    /// Presses the verified start proved separate the focused row from the
    /// toggle row.
    planned: Option<u32>,
    /// Presses taken so far.
    presses: u32,
}

impl ToggleWalk {
    /// A walk that has not yet established its starting position.
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget the verified start (used when a repaint leaves the picker).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Record that a Down press was just sent.
    pub fn record_down(&mut self) {
        self.presses = self.presses.saturating_add(1);
    }

    /// The number of Down presses taken so far.
    pub fn presses(&self) -> u32 {
        self.presses
    }
}

/// The next action of the validated toggle walk. A value, so every abort path
/// is assertable without a live PTY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToggleStep {
    /// Press Down and re-read the screen.
    Down,
    /// Focus is positively on the toggle: Enter is safe.
    Enter,
    /// Focus could not be verified: send NO Enter. Carries the reason.
    Abort(String),
}

/// Decide the next step of the walk that reaches the picker's toggle row.
///
/// The only paths to [`ToggleStep::Enter`] are:
/// * the toggle line itself positively carries the focus marker and no model
///   row is marked (a marked toggle cannot co-exist with a focused model in a
///   single-focus ring); or
/// * a walk that started from a single positively-marked model row, whose
///   length was fixed by the rendered ring, and whose every press was validated
///   to land on the row the ring predicted, ending on the toggle.
///
/// Anything that cannot be verified FAILS CLOSED with [`ToggleStep::Abort`],
/// because Enter on a model row starts a billed session.
///
/// NB: on the collapsed splash only the recommended model row and the toggle are
/// focus stops (the rest of the lineup hides behind `See all N models`), so the
/// ring this screen proves is the RENDERED rows plus the toggle. The advertised
/// count describes the expanded list; it is only cross-checked here for
/// consistency, never used to count presses.
pub fn toggle_walk_next(rows: &[String], walk: &mut ToggleWalk) -> ToggleStep {
    let Some(ring) = nav_ring(rows) else {
        return ToggleStep::Abort("the screen is not the freebuff model picker".to_string());
    };
    if ring.toggle_index.is_none() {
        return ToggleStep::Abort("the See-all/Show-fewer toggle row is not rendered".to_string());
    }

    // Positive evidence straight from the toggle line. The real 0.2.x renderer
    // marks a focused toggle by colour only (which the PTY layer strips), but
    // when a marker IS emitted it is unambiguous.
    if rows.iter().any(|r| toggle_row_has_focus_marker(r)) {
        return match model_focus(rows) {
            ModelFocus::NoneMarked | ModelFocus::NoRows => ToggleStep::Enter,
            ModelFocus::Single(i) => ToggleStep::Abort(format!(
                "the toggle line carries a focus marker but model row {i} is also focused"
            )),
            ModelFocus::Multiple => ToggleStep::Abort(
                "the toggle line carries a focus marker but several model rows are focused"
                    .to_string(),
            ),
        };
    }

    // Establish the verified starting position once.
    if walk.start.is_none() {
        let start = match model_focus(rows) {
            ModelFocus::Single(i) => i,
            ModelFocus::NoneMarked => {
                return ToggleStep::Abort(
                    "no model row positively carries the focus marker: focus cannot be \
                     tracked from a verified start"
                        .to_string(),
                )
            }
            ModelFocus::NoRows => return ToggleStep::Abort("no model row is rendered".to_string()),
            ModelFocus::Multiple => {
                return ToggleStep::Abort(
                    "several model rows carry the focus marker: not a single-focus ring"
                        .to_string(),
                )
            }
        };
        // Count the rendered rows with the SAME parser that established focus
        // (`model_rows`), and cross-check the advertised count against it. The
        // two row parsers read one screen and can disagree, and a toggle that
        // advertises fewer models than are rendered is self-contradictory: both
        // are unverifiable, so fail closed rather than guess a walk length.
        let rendered = model_rows(rows).len();
        if start >= rendered {
            return ToggleStep::Abort(format!(
                "the focused model row {start} is outside the {rendered} rendered row(s)"
            ));
        }
        if let Some(advertised) = ring.advertised_models {
            if advertised < rendered {
                return ToggleStep::Abort(format!(
                    "the toggle advertises {advertised} model(s) but {rendered} are rendered"
                ));
            }
        }
        // The toggle sits immediately after the last RENDERED model row, so the
        // walk length is that many model stops plus the toggle hop.
        walk.start = Some(start);
        walk.planned = Some((rendered - start) as u32);
        walk.presses = 0;
        return ToggleStep::Down;
    }

    let start = walk.start.expect("start set together with planned");
    let planned = walk.planned.expect("planned set together with start");

    if walk.presses >= planned {
        // The ring's worth of presses have been taken: the next stop must be
        // the toggle, so no model row may be positively focused.
        return match model_focus(rows) {
            ModelFocus::NoneMarked => ToggleStep::Enter,
            ModelFocus::Single(i) => ToggleStep::Abort(format!(
                "expected the toggle after {planned} Down press(es) but model row {i} is still focused"
            )),
            ModelFocus::NoRows => ToggleStep::Abort(
                "the screen stopped parsing before the toggle was reached".to_string(),
            ),
            ModelFocus::Multiple => ToggleStep::Abort(
                "several model rows are focused before the toggle was reached".to_string(),
            ),
        };
    }

    // Mid-walk: focus must have advanced one row, to `start + presses`.
    let expected = start + walk.presses as usize;
    match model_focus(rows) {
        ModelFocus::Single(i) if i == expected => ToggleStep::Down,
        ModelFocus::Single(i) => ToggleStep::Abort(format!(
            "focus jumped to model row {i} but the walk expected row {expected}"
        )),
        ModelFocus::NoneMarked => ToggleStep::Abort(
            "focus left the model rows before the walk reached the toggle".to_string(),
        ),
        ModelFocus::NoRows | ModelFocus::Multiple => {
            ToggleStep::Abort("the screen stopped parsing mid-walk".to_string())
        }
    }
}

/// Whether the expanded-list walk has scrolled past the last model (the toggle
/// row holds focus). This is a TERMINATION signal only — it never authorises
/// Enter. It requires a NON-EMPTY model parse so an empty or partial frame is
/// never read as "nothing is focused" (the vacuous-`all()` fix).
fn walk_past_last_model(rows: &[String]) -> bool {
    match nav_ring(rows) {
        Some(ring) if ring.toggle_index.is_some() => {
            matches!(model_focus(rows), ModelFocus::NoneMarked)
        }
        _ => false,
    }
}

/// Wait for the screen to change after a keypress, then let the repaint finish.
///
/// The TUI clears the grid (`ESC[2J`) as the first step of every repaint, so
/// `wait_for` unblocks on that clear — often before the new content is written.
/// Sleeping one `SETTLE` afterwards makes the next `screen()` read a complete
/// frame instead of a half-cleared one.
async fn settle_after(pty: &Pty, before: &str, deadline: Instant) -> Result<()> {
    let wait = Duration::from_secs(2).min(deadline.saturating_duration_since(Instant::now()));
    if wait.is_zero() {
        return Err(anyhow!("freebuff enumeration deadline expired"));
    }
    pty.wait_for(|s| s.text() != before, wait).await?;
    tokio::time::sleep(SETTLE).await;
    Ok(())
}

/// Where the model picker is, given the first stable screen.
///
/// freebuff 0.2.29 has NO startup picker: it boots to the idle input box with a
/// model already selected (`/model to change` in the status row), so the picker
/// must be opened by submitting the `/model` command. Older builds (and the
/// scripted fake) render the picker directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickerEntry {
    /// A picker is already on screen.
    Visible,
    /// The idle input box is on screen: the picker must be opened with `/model`.
    Open,
}

/// Classify the entry path from a stable screen, or `None` when the screen is
/// neither a picker nor the idle input box.
fn picker_entry(state: &ScreenState) -> Option<PickerEntry> {
    match state {
        ScreenState::ModelSplash | ScreenState::ModelList => Some(PickerEntry::Visible),
        ScreenState::Idle => Some(PickerEntry::Open),
        _ => None,
    }
}

/// Whether the screen is a terminal gate: a hard precondition that can never
/// resolve by waiting (so neither the entry wait nor the `/model` submit may
/// spin on it). Mirrors the driver's `is_terminal_gate`; kept local so a gate
/// error here is always specific.
fn is_terminal_state(state: &ScreenState) -> bool {
    matches!(
        state,
        ScreenState::AlreadyRunning
            | ScreenState::HoursGate { .. }
            | ScreenState::FreebucksGate { .. }
            | ScreenState::TakeOver
            | ScreenState::Login
            | ScreenState::KickedOut
            | ScreenState::SessionEnded
    )
}

/// The terminal gate on screen as an operator error, or `None`. Enumeration is
/// a read-only probe: a gate (no hours/Freebucks, login, already running, a
/// resume prompt) is surfaced, never dismissed (AGENTS.md).
fn gate_error(state: &ScreenState) -> Option<anyhow::Error> {
    match state {
        ScreenState::AlreadyRunning => Some(anyhow!(
            "freebuff: already running — only one instance may run at a time; \
             wait for it to end or stop the other instance"
        )),
        ScreenState::HoursGate { message, .. } => {
            Some(anyhow!("freebuff: not enough hours — {message}"))
        }
        ScreenState::FreebucksGate { message } => {
            Some(anyhow!("freebuff: not enough Freebucks — {message}"))
        }
        ScreenState::TakeOver => Some(anyhow!("{}", TAKE_OVER_ERROR)),
        ScreenState::Login => Some(anyhow!(
            "freebuff needs an interactive login: run `freebuff` once in a terminal and log in, then retry"
        )),
        ScreenState::KickedOut => Some(anyhow!(
            "freebuff: another instance took over this account"
        )),
        ScreenState::SessionEnded => Some(anyhow!(
            "freebuff: session-ended screen — refusing to resume a session to enumerate models"
        )),
        _ => None,
    }
}

/// Whether `row` carries freebuff's MODAL AFFORDANCE: an `Enter: <action>` and
/// an `Esc: <action>` on the SAME row.
///
/// This is the idiom BOTH live modal interstitials use — the hours gate
/// (`Enter: retry  Esc: back to draft`) and the free-plan take-over modal
/// (`Enter: take over  Esc: back to draft`). It is STRUCTURAL: it catches either
/// modal regardless of wording, and it does NOT match ordinary transcript prose
/// such as `Enter to retry`, the idle placeholder `Enter a coding task or / for
/// commands`, or the info line `Your first message starts the session.` — none
/// of which carries an `Esc:` affordance. It replaces an earlier spend-KEYWORD
/// list, which missed `take over` (no spend word) and false-matched transcript
/// `retry`.
///
/// Matched PER ROW, not screen-wide. Both observed modals render the two
/// affordances on one physical row and they are short (32 / 36 chars), so they
/// do not wrap even at 80 columns; per-row matching is far less prone to false
/// hits than a screen-wide `Enter:`-anywhere + `Esc:`-anywhere check, which would
/// flag any screen carrying the two in unrelated places. The all-fixtures scan
/// (`no_healthy_fixture_reads_as_a_confirmation`) proves no healthy frame is
/// flagged by it.
///
/// NOTE: this is a BACKSTOP, not the primary guard. What actually protects the
/// user is the SUBMIT ALLOWLIST in [`submit_step`]: Enter is sent ONLY when the
/// state is [`ScreenState::Idle`] AND the bottom-most rounded box holds EXACTLY
/// `/model`. Every interstitial captured so far REPLACES the input box (a
/// square-bordered modal, with no rounded box at all), so [`input_box_is_exactly`]
/// already fails closed on them. Do not weaken the allowlist believing this
/// detector covers it.
fn is_modal_affordance_row(row: &str) -> bool {
    has_affordance(row, "Enter") && has_affordance(row, "Esc")
}

/// Whether `row` renders a `<key>: <action>` affordance: the key, a colon, then a
/// non-empty action. `key` is matched case-sensitively as freebuff renders it
/// (`Enter` / `Esc`), so a lowercase token inside another word is not read as one.
fn has_affordance(row: &str, key: &str) -> bool {
    let marker = format!("{key}:");
    match row.find(&marker) {
        Some(i) => !row[i + marker.len()..].trim_start().is_empty(),
        None => false,
    }
}

/// A modal interstitial that would commit if Enter were pressed (take over, or
/// the hours gate's retry), or `None`: the first row carrying freebuff's modal
/// affordance (see [`is_modal_affordance_row`]). A fail-closed BACKSTOP — see the
/// note there.
fn confirmation_text(rows: &[String]) -> Option<String> {
    rows.iter()
        .find(|r| is_modal_affordance_row(r))
        .map(|r| r.trim().to_string())
}

/// One catalog row collected from a picker frame, before id mapping. Carries
/// everything `--json` reports so the mapping step is pure.
#[derive(Debug, Clone)]
struct SeenEntry {
    display_name: String,
    section: Option<String>,
    price: Option<u64>,
}

impl SeenEntry {
    fn from_row(r: &ModelRow, section: Option<String>) -> Self {
        let explicit = r
            .cost_line
            .as_deref()
            .and_then(price_per_hour_from_cost_line);
        Self {
            display_name: r.name.clone(),
            price: explicit.or_else(|| free_price(section.as_deref())),
            section,
        }
    }
}

/// Fold one picker frame's rows into `seen`, carrying the section forward.
///
/// The picker is walked DOWNWARD and a section's header scrolls off the top
/// before its last models do, so a frame can show a section's models with no
/// header at all — freebuff 0.2.29's `Gemini 3.8 Flash` / `GPT-6.1 Sol` arrived
/// that way and came back section-less. Because the list is one ordered walk,
/// the last section actually seen is the section of the rows that follow it, so
/// it is carried forward; a row that carries its own header always wins, and a
/// later frame may fill in a section earlier left unset.
fn collect_frame(
    seen: &mut Vec<SeenEntry>,
    models: &[ModelRow],
    last_section: &mut Option<String>,
) {
    for r in models {
        let own = section_name(r.section);
        if own.is_some() {
            *last_section = own.clone();
        }
        let section = own.or_else(|| last_section.clone());
        match seen.iter_mut().find(|s| s.display_name == r.name) {
            Some(existing) => {
                if existing.section.is_none() {
                    existing.section = section;
                }
            }
            None => seen.push(SeenEntry::from_row(r, section)),
        }
    }
}

/// The `--json` section label for the free tier. freebuff 0.2.29 renders it as
/// the `UNLIMITED` header; those models carry no `Freebucks/hr` figure because
/// they cost nothing.
const UNLIMITED_LABEL: &str = "unlimited";

/// The `--json` section label for a parsed picker section. `Unknown` (the
/// flat, un-sectioned pickers of older builds) is omitted rather than invented.
fn section_name(section: ModelSection) -> Option<String> {
    match section {
        ModelSection::Unlimited => Some(UNLIMITED_LABEL.to_string()),
        ModelSection::Optimized => Some("optimized".to_string()),
        ModelSection::Powerful => Some("powerful".to_string()),
        ModelSection::LimitedTrial => Some("limited-trial".to_string()),
        ModelSection::Unknown => None,
    }
}

/// The price of an entry whose picker shows no `Freebucks/hr` figure. The
/// v0.2.29 `/model` picker prints NO price at all: its `unlimited` section is
/// the free tier, so those are `0`; every other section leaves the price
/// unknown rather than guessing.
fn free_price(section: Option<&str>) -> Option<u64> {
    (section == Some(UNLIMITED_LABEL)).then_some(0)
}

/// Clear the input box if it is not already empty (the placeholder state).
///
/// Returns `Ok(true)` only when a FRESH read shows the box empty. Returns
/// `Ok(false)` when it could not be cleared. NEVER presses Enter, so it cannot
/// submit anything however the clear goes.
async fn ensure_input_box_empty(pty: &Pty, deadline: Instant) -> Result<bool> {
    if input_box_is_empty(&pty.screen().rows) {
        return Ok(true);
    }
    for _ in 0..MAX_CLEAR_BACKSPACES {
        if Instant::now() >= deadline {
            return Ok(false);
        }
        pty.key(Key::Backspace).await?;
    }
    tokio::time::sleep(SETTLE).await;
    // Re-verify from a fresh read: a counted clear is a guess, and guessing
    // against unknown text is how a stray MESSAGE gets submitted.
    Ok(input_box_is_empty(&pty.screen().rows))
}

/// Get the input box to hold EXACTLY `cmd`, or fail loudly without pressing
/// Enter.
///
/// Two rules, both because a stray MESSAGE starts a billed session:
///
/// * The box must be EMPTY before we type. If it is not, it is cleared with a
///   bounded number of Backspaces and RE-VERIFIED empty; an unclearable box
///   aborts rather than being typed into blind.
/// * After typing, the box must hold EXACTLY `cmd`
///   ([`input_box_is_exactly`], not the substring [`input_box_shows`]), because
///   a box like `foo/model` or `/model/model` would otherwise pass and be
///   submitted as a message. A retry clears deterministically again first, so
///   leftovers can never accumulate.
///
/// Shared with `FreebuffBackend::shutdown`, which types `/exit` through it for
/// the same reason `/model` is typed through it: freebuff flushes its input
/// queue, so a command typed once can vanish, and Enter must never be pressed
/// onto a box that does not positively hold it.
pub(crate) async fn type_and_confirm_command(
    pty: &Pty,
    cmd: &str,
    deadline: Instant,
) -> Result<()> {
    for _ in 0..MAX_TYPE_ATTEMPTS {
        if Instant::now() >= deadline {
            return Err(anyhow!("freebuff enumeration deadline expired"));
        }
        if !ensure_input_box_empty(pty, deadline).await? {
            let _ = pty.key(Key::Escape).await;
            return Err(anyhow!(
                "freebuff: the input box is not empty and could not be cleared, so `{cmd}` was \
                 NOT typed into it (typing into unknown content could submit a MESSAGE, which \
                 starts a billed session)"
            ));
        }
        pty.type_text(cmd).await?;
        let window = Duration::from_secs(2).min(deadline.saturating_duration_since(Instant::now()));
        if window.is_zero() {
            return Err(anyhow!("freebuff enumeration deadline expired"));
        }
        if pty
            .wait_for(|s| input_box_is_exactly(&s.rows, cmd), window)
            .await
            .is_ok()
        {
            return Ok(());
        }
    }
    let _ = pty.key(Key::Escape).await;
    Err(anyhow!(
        "freebuff: the input box never held exactly `{cmd}`, so Enter was NOT sent \
         (a box with leftover text would submit a MESSAGE, which starts a billed session)"
    ))
}

/// The action the `/model` submit may take. A value, so every path is
/// assertable without a live PTY — the same shape as [`ToggleStep`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubmitStep {
    /// A picker is already on screen: stop WITHOUT pressing anything.
    Opened,
    /// Positively the idle input box showing the command: Enter is safe.
    Enter,
}

/// Decide whether the ONE `/model` submit may be sent, from a FRESHLY read
/// screen.
///
/// `Err` means "press nothing": the caller Esc's and aborts with this error.
///
/// Two hazards, both money-safety:
///
/// * A blind Enter. A picker that has rendered receives it on its focused MODEL
///   row — where Enter STARTS A BILLED SESSION, before [`toggle_walk_next`]
///   governs anything.
/// * A SUBSTRING match. `input_box_shows` is a contains match, so a box holding
///   `foo/model` would be "confirmed" and submitted as a MESSAGE, which starts a
///   billed session. The box must hold EXACTLY the command
///   ([`input_box_is_exactly`]).
///
/// The screen must classify STRICTLY as [`ScreenState::Idle`]: the classifier
/// recognises the typed-in idle box itself (see `classify`), and the picker
/// states are matched BEFORE Idle, so a mixed repaint that paints any picker
/// rows classifies as a picker and presses nothing. Nothing is widened here.
///
/// Exactly ONE Enter is ever sent. `pty.key` only WRITES the key — it does not
/// wait for freebuff to consume it — so a repeat guarded on a screen snapshot
/// could still be delivered AFTER the first Enter opened the picker, landing on
/// a focused model row and starting a billed session. There is deliberately no
/// repeat path here (see [`open_picker_via_slash_model`]); anything that cannot
/// be verified fails closed.
fn submit_step(state: &ScreenState, rows: &[String], cmd: &str) -> Result<SubmitStep> {
    if matches!(state, ScreenState::ModelSplash | ScreenState::ModelList) {
        return Ok(SubmitStep::Opened);
    }
    if let Some(err) = gate_error(state) {
        return Err(err);
    }
    if let Some(line) = confirmation_text(rows) {
        return Err(anyhow!(
            "freebuff: refusing to confirm a screen that would start a session: {line}"
        ));
    }
    if !matches!(state, ScreenState::Idle) {
        return Err(anyhow!(
            "freebuff: the screen is not the idle input box ({state:?}); Enter was NOT sent \
             — Enter on an unrecognised screen could start a billed session"
        ));
    }
    if !input_box_is_exactly(rows, cmd) {
        return Err(anyhow!(
            "freebuff: the input box does not hold exactly `{cmd}`, so Enter was NOT sent \
             (leftover text would submit a MESSAGE, which starts a billed session)"
        ));
    }
    Ok(SubmitStep::Enter)
}

/// Type `/model` into the idle input box and submit it with a SINGLE Enter,
/// opening the picker.
///
/// Hazards, all handled explicitly because this can SPEND the user's money:
///
/// * A malformed submission would send a plain MESSAGE, and freebuff's first
///   message STARTS A BILLED SESSION. The box must be EMPTY before typing and
///   hold EXACTLY `/model` before submitting ([`input_box_is_exactly`], never the
///   substring [`input_box_shows`]) — re-confirmed by [`submit_step`].
/// * freebuff renders its idle screen a beat before it starts reading input and
///   FLUSHES the input queue when it finishes initialising, so a burst typed the
///   instant Idle appears is silently lost (measured live against 0.2.29: the
///   box still showed the placeholder after typing). The command is therefore
///   re-typed, bounded, and the box is cleared deterministically and
///   re-verified empty before each retry ([`type_and_confirm_command`]).
/// * A REPEAT Enter is NEVER sent. `pty.key` only WRITES the key — it does not
///   wait for freebuff to consume it — so a repeat guarded on a screen snapshot
///   can be delivered AFTER the first Enter opened the picker, where it lands on
///   a focused MODEL row and starts a billed session. Measured live against
///   0.2.29 (4/4 attempts, one per round of a single session): the picker opens
///   on the FIRST Enter — the autocomplete popup does NOT swallow it — so no
///   repeat is needed. If the picker does not open, this fails LOUDLY instead of
///   nudging again.
///
/// No model row is ever Entered here, so nothing is selected or billed.
async fn open_picker_via_slash_model(pty: &Pty, deadline: Instant) -> Result<()> {
    const MODEL_COMMAND: &str = "/model";

    // Only submit when the box holds EXACTLY the command — a stray message
    // would start a session.
    type_and_confirm_command(pty, MODEL_COMMAND, deadline).await?;

    // Re-read and classify IMMEDIATELY before pressing Enter. The picker may
    // have rendered since we typed; a blind Enter would land on its focused
    // MODEL row and start a billed session.
    let snap = pty.screen();
    match submit_step(&classify(&snap.rows), &snap.rows, MODEL_COMMAND) {
        Ok(SubmitStep::Opened) => return Ok(()),
        Ok(SubmitStep::Enter) => {}
        Err(err) => {
            let _ = pty.key(Key::Escape).await;
            return Err(err);
        }
    }

    // Exactly ONE Enter — never a repeat.
    pty.key(Key::Enter).await?;

    // Give the picker a bounded moment to render. A timeout is a LOUD failure,
    // NOT a reason to send another Enter: a queued second Enter could be
    // delivered after this one opened the picker and land on a model row.
    let open_window =
        Duration::from_secs(3).min(deadline.saturating_duration_since(Instant::now()));
    if open_window.is_zero() {
        return Err(anyhow!("freebuff enumeration deadline expired"));
    }
    let opened = pty
        .wait_for(
            |s| {
                matches!(
                    classify(&s.rows),
                    ScreenState::ModelSplash | ScreenState::ModelList
                )
            },
            open_window,
        )
        .await;
    if opened.is_err() {
        let _ = pty.key(Key::Escape).await;
        return Err(anyhow!(
            "freebuff: the model picker did not open after submitting /model once; no repeat \
             Enter is sent, because a queued second Enter could land on a model row and start a \
             billed session — see docs/research/freebuff-tui.md"
        ));
    }
    tokio::time::sleep(SETTLE).await;
    Ok(())
}

/// Enumerate the model catalog by driving `pty` through freebuff's picker.
///
/// Sequence: wait for the model picker, expand via the toggle row (Enter only
/// when the guard allows), read every row while scrolling until the advertised
/// total is seen or the walk converges, then return the catalog in picker
/// order. The PTY is left running; the caller must `kill` it on every exit
/// path.
pub async fn enumerate_catalog(pty: &Pty) -> Result<Vec<CatalogEntry>> {
    let deadline = Instant::now() + ENUMERATION_TIMEOUT;

    // 1. Wait for a stable screen from which the picker can be reached, or a
    //    terminal gate to fail fast on (never spin on "already running",
    //    hours/freebucks/login — AGENTS.md: never dismiss an interstitial).
    //
    //    freebuff 0.2.29 has NO startup model picker: it boots STRAIGHT to the
    //    idle input box with a model already selected (`/model to change` in the
    //    status row), which is why waiting for ModelSplash/ModelList timed out
    //    against the real CLI while passing against the scripted fake. Older
    //    builds (and the fake) DO render the picker directly, so both are
    //    accepted here and the idle case is opened by command below.
    let snap = pty
        .wait_for(
            |s| {
                let st = classify(&s.rows);
                picker_entry(&st).is_some() || is_terminal_state(&st)
            },
            ENUMERATION_TIMEOUT,
        )
        .await?;
    let state = classify(&snap.rows);
    if let Some(err) = gate_error(&state) {
        return Err(err);
    }

    // 2. If freebuff did not already show the picker, open it by COMMAND: type
    //    `/model` into the idle input box and submit it. Submitting a command is
    //    safe; a stray MESSAGE would start a billed session (see
    //    `open_picker_via_slash_model`, which verifies the box first).
    match picker_entry(&state) {
        Some(PickerEntry::Visible) => {}
        Some(PickerEntry::Open) => open_picker_via_slash_model(pty, deadline).await?,
        None => {
            return Err(anyhow!(
                "freebuff: unexpected screen while enumerating models: {state:?}"
            ));
        }
    }

    // 3. Expand the picker if it is collapsed. Enter is sent ONLY when the
    //    validated walk (see `toggle_walk_next`) proves focus has arrived at the
    //    toggle row; any unverifiable frame aborts without pressing Enter. The
    //    collapsed splash advertises the lineup size (`↓ See all N models`);
    //    record it so the walk below knows when every model has been read.
    let mut total: Option<usize> = None;
    let mut presses = 0u32;
    let mut unreadable_frames = 0u32;
    let mut walk = ToggleWalk::new();
    loop {
        let rows = pty.screen().rows.clone();
        let st = classify(&rows);
        if let Some(t) = advertised_total(&rows) {
            total = Some(t);
        }
        match st {
            ScreenState::ModelList => break, // already expanded
            ScreenState::ModelSplash => match toggle_walk_next(&rows, &mut walk) {
                ToggleStep::Enter => {
                    pty.key(Key::Enter).await?;
                    tokio::time::sleep(SETTLE).await;
                }
                ToggleStep::Down => {
                    if presses >= MAX_DOWN_PRESSES {
                        return Err(anyhow!(
                            "freebuff: could not reach the model-picker toggle row"
                        ));
                    }
                    let before = pty.screen().text();
                    pty.key(Key::Down).await?;
                    walk.record_down();
                    presses += 1;
                    settle_after(pty, &before, deadline).await?;
                }
                ToggleStep::Abort(reason) => {
                    // A frame caught mid-repaint can classify as the splash
                    // before its model row is drawn. Re-read a few times before
                    // giving up; no Enter is ever sent while focus is unverified.
                    if model_rows(&rows).is_empty() && unreadable_frames < MAX_UNREADABLE_FRAMES {
                        unreadable_frames += 1;
                        tokio::time::sleep(SETTLE).await;
                    } else {
                        return Err(anyhow!(
                            "freebuff: refusing to send Enter on the model picker — {reason} \
                             (Enter on a model row starts a session and spends Freebucks)"
                        ));
                    }
                }
            },
            ScreenState::Booting => {
                // The TUI clears the screen (`ESC[2J`) before each repaint, so a
                // read taken in that window sees an empty grid and classifies as
                // Booting. It is a transient render state, not a different TUI
                // shape: let the paint settle and re-read.
                walk.reset();
                tokio::time::sleep(SETTLE).await;
            }
            st => {
                return Err(anyhow!(
                    "freebuff: unexpected screen while expanding the picker: {st:?}"
                ));
            }
        }
        if Instant::now() >= deadline {
            return Err(anyhow!("freebuff enumeration deadline expired"));
        }
    }

    // 4. Walk the expanded list, collecting rows while scrolling until the
    //    advertised total is seen or the walk converges. `model_rows` strips
    //    freebuff's ` • <effort>` suffix, carries each row's section, and picks
    //    up any `Freebucks/hr` cost line, and its section is carried across
    //    frames (see `collect_frame`). The v0.2.29 `/model` picker prints no
    //    price at all; its `unlimited` section is the free tier, which
    //    `free_price` reports as 0.
    let mut seen: Vec<SeenEntry> = Vec::new();
    // Last section actually seen on screen, carried across frames (see
    // `collect_frame`).
    let mut last_section: Option<String> = None;
    let mut presses = 0u32;
    let mut consecutive_empty = 0u32;

    loop {
        if Instant::now() >= deadline {
            return Err(anyhow!("freebuff enumeration deadline expired"));
        }
        let rows = pty.screen().rows.clone();
        let state = classify(&rows);
        if state == ScreenState::AlreadyRunning {
            return Err(anyhow!(
                "freebuff: another instance started while enumerating models"
            ));
        }
        if state != ScreenState::ModelList {
            tokio::time::sleep(SETTLE).await;
            continue;
        }

        let models = model_rows(&rows);
        let before_count = seen.len();
        collect_frame(&mut seen, &models, &mut last_section);

        if let Some(total) = total {
            if seen.len() >= total {
                break;
            }
        }
        // In the expanded view the advertised total (`↓ See all N`) is no
        // longer shown; `↑ Show fewer` carries no number. When focus reaches
        // that toggle row we've scrolled past the last model, so the walk is
        // done. This is a TERMINATION signal only — it never authorises Enter —
        // and it requires a non-empty model parse so an empty frame is not read
        // as "nothing is focused".
        if walk_past_last_model(&rows) {
            break;
        }

        if seen.len() == before_count {
            consecutive_empty += 1;
            // This picker is focus-first: Down moves focus and the window only
            // scrolls once focus crosses its bottom edge, so up to `visible`
            // presses can legitimately yield no new row before one scrolls in.
            // Only call it converged well past that many.
            let visible = models.len().max(1) as u32;
            if consecutive_empty >= visible + 2 {
                break; // converged: scrolling yields nothing new
            }
        } else {
            consecutive_empty = 0;
        }

        if presses >= MAX_DOWN_PRESSES {
            return Err(anyhow!("freebuff: model list walk did not converge"));
        }
        let before = pty.screen().text();
        pty.key(Key::Down).await?;
        presses += 1;
        settle_after(pty, &before, deadline).await?;
    }

    // Leave the picker WITHOUT selecting anything: Esc dismisses it. Every Enter
    // inside the picker went through `toggle_walk_next` and landed on the toggle
    // row only, so no model row was ever Entered and nothing is selected or
    // billed. (The caller still kills the PTY on every exit path, so no orphan
    // freebuff is left behind.)
    let _ = pty.key(Key::Escape).await;
    tokio::time::sleep(SETTLE).await;

    let catalog: Vec<CatalogEntry> = seen
        .into_iter()
        .map(|s| {
            let (id, id_source) = map_display_name(&s.display_name);
            CatalogEntry {
                id,
                display_name: s.display_name,
                // `id` is `None` for an unmapped name; the entry is kept so the
                // rotation is visible in `--json` / the stderr warning.
                section: s.section,
                price_per_hour: s.price,
                recommended: None,
                available: None,
                id_source,
            }
        })
        .collect();

    if catalog.is_empty() {
        return Err(anyhow!(
            "freebuff: could not read any model rows from the picker"
        ));
    }

    Ok(catalog)
}

/// Parse the advertised total from the collapsed splash's `↓ See all N models`
/// row. The row may be wrapped in a box border (`│ … │`), so the marker is
/// located anywhere in the row rather than anchored at the start. `↑ Show
/// fewer` carries no number, so `None` is returned when no `See all N` row is
/// present and the caller keeps its last known total.
fn advertised_total(rows: &[String]) -> Option<usize> {
    const MARKER: &str = "See all ";
    for row in rows {
        if let Some(idx) = row.find(MARKER) {
            let n: usize = row[idx + MARKER.len()..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .ok()?;
            return Some(n);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::PtyConfig;
    use std::env;
    use std::time::Duration;

    /// Serialises the PTY end-to-end tests: they set process-global env vars
    /// (`env::set_var`), so running them concurrently would race. A tokio lock
    /// (not `std`) is held across awaits.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    macro_rules! fixture {
        ($name:expr) => {
            include_str!(concat!("../../tests/fixtures/screens/", $name, ".txt"))
        };
    }

    fn load_fixture(name: &str) -> Vec<String> {
        let content = match name {
            "splash-120x40" => fixture!("splash-120x40"),
            "splash-list-focus4-120x40" => fixture!("splash-list-focus4-120x40"),
            "splash-list-showfewer-120x40" => fixture!("splash-list-showfewer-120x40"),
            "0227-synthetic-splash-collapsed-120x40" => {
                fixture!("0227-synthetic-splash-collapsed-120x40")
            }
            // VERBATIM 120x40 frames captured from the 2026-10-17 live probe of
            // freebuff v0.2.29 (real-derived, not synthetic).
            "0229-real-startup-no-picker-120x40" => fixture!("0229-real-startup-no-picker-120x40"),
            "0229-real-slash-model-picker-120x40" => {
                fixture!("0229-real-slash-model-picker-120x40")
            }
            "0229-real-picker-powerful-section-120x40" => {
                fixture!("0229-real-picker-powerful-section-120x40")
            }
            "0229-real-picker-wrapped-description-120x40" => {
                fixture!("0229-real-picker-wrapped-description-120x40")
            }
            "0229-real-picker-tail-120x40" => fixture!("0229-real-picker-tail-120x40"),
            "0229-real-slash-popup-120x40" => fixture!("0229-real-slash-popup-120x40"),
            "0229-real-take-over-modal-120x40" => fixture!("0229-real-take-over-modal-120x40"),
            _ => panic!("unknown fixture: {name}"),
        };
        content.lines().map(|s| s.to_string()).collect()
    }

    /// Build a catalog entry exactly the way `enumerate_catalog` does.
    fn entry(display_name: &str, price: u64) -> CatalogEntry {
        let (id, id_source) = map_display_name(display_name);
        CatalogEntry {
            id,
            display_name: display_name.to_string(),
            section: None,
            price_per_hour: Some(price),
            recommended: None,
            available: None,
            id_source,
        }
    }

    #[test]
    fn map_display_name_covers_the_lineup() {
        let cases = [
            ("GLM 5.3 Flash", ("z-ai/glm-5.3-flash", IdSource::Mapped)),
            ("MiMo 2.6 Pro", ("mimo/mimo-v2.6-pro", IdSource::Mapped)),
            ("MiMo 2.6 Flash", ("mimo/mimo-v2.5", IdSource::Mapped)),
            ("GPT-6 Luna", ("openai/gpt-6-luna", IdSource::Mapped)),
            ("GPT-6.1 Sol", ("openai/gpt-6.1-sol", IdSource::Mapped)),
            (
                "DeepSeek V4.1 Flash",
                ("deepseek/deepseek-v4-flash", IdSource::Mapped),
            ),
            ("Solar Pro 4", ("upstage/solar-pro4", IdSource::Mapped)),
            ("Solar Mini 4", ("upstage/solar-mini4", IdSource::Mapped)),
            (
                "Muse Spark 1.3",
                ("meta/muse-spark-1.3-contributor", IdSource::Mapped),
            ),
            (
                "Gemini 3.8 Flash",
                ("google/gemini-3.8-flash", IdSource::Mapped),
            ),
        ];
        for (name, (id, source)) in cases {
            let (got_id, got_source) = map_display_name(name);
            assert_eq!(got_id.as_deref(), Some(id), "name: {name}");
            assert_eq!(got_source, source, "name: {name}");
        }
    }

    #[test]
    fn map_display_name_unmapped_has_no_id() {
        // `Claude Fable 5.1` is in freebuff's lineup (0.2.27) but has no entry in
        // this module's LINEUP map: it must yield NO id, never the name itself.
        for name in ["Unknown New Model", "Claude Fable 5.1"] {
            let (id, source) = map_display_name(name);
            assert_eq!(id, None, "name: {name}");
            assert_eq!(source, IdSource::Unmapped, "name: {name}");
        }
    }

    #[test]
    fn is_well_formed_model_id_table() {
        // Real 0.2.x ids: provider-prefixed, bare, opaque key, dotted segment.
        for ok in [
            "z-ai/glm-5.3-flash",
            "mimo/mimo-v2.6-pro",
            "openai/gpt-6-luna",
            "m-79293c5b7d",
            "ab",
        ] {
            assert!(is_well_formed_model_id(ok), "should accept {ok}");
        }
        // Display names (spaces, uppercase, too short) are never ids.
        for bad in [
            "Claude Fable 5.1",
            "GLM 5.3 Flash",
            "a",
            "",
            "-glm",
            "glm 5",
        ] {
            assert!(!is_well_formed_model_id(bad), "should reject {bad:?}");
        }
        assert!(!is_well_formed_model_id(&"a".repeat(129)));
    }

    #[test]
    fn price_per_hour_from_cost_line_parses_leading_digits() {
        assert_eq!(price_per_hour_from_cost_line("5 Freebucks/hr"), Some(5));
        assert_eq!(
            price_per_hour_from_cost_line("15 Freebucks/hr · Off-peak 10/hr"),
            Some(15)
        );
        assert_eq!(price_per_hour_from_cost_line("Deep reasoning"), None);
        assert_eq!(price_per_hour_from_cost_line(""), None);
    }

    /// Assert the walk never returns Enter and never walks for a frame from
    /// which focus cannot be positively established.
    fn assert_no_enter(rows: &[String], walk: &mut ToggleWalk) {
        for _ in 0..4 {
            match toggle_walk_next(rows, walk) {
                ToggleStep::Enter => panic!("must never Enter on {rows:?}"),
                ToggleStep::Down => panic!("must never walk on {rows:?}"),
                ToggleStep::Abort(_) => {}
            }
        }
    }

    /// `n` rendered model boxes with the marker on `focused`, plus the toggle.
    fn three_models_focused(focused: usize) -> Vec<String> {
        let names = [
            "GLM 5.3 Flash · Deep reasoning",
            "MiMo 2.6 Pro · Balanced",
            "GPT-6 Luna · Balanced",
        ];
        let mut rows = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let marker = if i == focused { "›" } else { " " };
            rows.push(format!("│ {marker} {name} │"));
        }
        rows.push("↓  See all 3 models".to_string());
        rows
    }

    /// A model row focused (`›`) never authorises Enter: the walk starts by
    /// pressing Down, never by concluding the toggle holds focus.
    #[test]
    fn toggle_walk_does_not_enter_while_a_model_is_focused() {
        for f in 0..3 {
            let mut walk = ToggleWalk::new();
            assert_eq!(
                toggle_walk_next(&three_models_focused(f), &mut walk),
                ToggleStep::Down
            );
        }
        // The collapsed splash (recommended row focused) likewise walks.
        let mut walk = ToggleWalk::new();
        assert_eq!(
            toggle_walk_next(&load_fixture("splash-120x40"), &mut walk),
            ToggleStep::Down
        );
    }

    /// Focus is NEVER inferred from absence: without a positively-marked model
    /// row, the walk aborts and no Enter is sent.
    #[test]
    fn toggle_walk_aborts_when_initial_focus_is_not_positively_identified() {
        // Model box parses but carries no marker.
        assert_no_enter(
            &[
                "│   GLM 5.3 Flash · Deep reasoning · Images │".to_string(),
                "│                              5 Freebucks/hr │".to_string(),
                "↓  See all 12 models".to_string(),
            ],
            &mut ToggleWalk::new(),
        );
        // Two rows carry the marker: not a single-focus ring.
        assert_no_enter(
            &[
                "│ › GLM 5.3 Flash · Deep reasoning │".to_string(),
                "│ › MiMo 2.6 Pro · Balanced │".to_string(),
                "↓  See all 2 models".to_string(),
            ],
            &mut ToggleWalk::new(),
        );
        // Empty parses: model_rows recognises nothing, so "nothing is focused"
        // must never be concluded.
        assert_no_enter(&["↓  See all 3 models".to_string()], &mut ToggleWalk::new());
        assert_no_enter(&["↑  Show fewer".to_string()], &mut ToggleWalk::new());
        assert_no_enter(&[], &mut ToggleWalk::new());
        assert_no_enter(&["".to_string(), "   ".to_string()], &mut ToggleWalk::new());
        // A model-shaped row the parser cannot read (no ` · ` separator) leaves
        // the parse empty -> abort.
        assert_no_enter(
            &[
                "│ › GLM 5.3 Flash".to_string(),
                "↓  See all 3 models".to_string(),
            ],
            &mut ToggleWalk::new(),
        );
        // A model box with no toggle row is not the picker -> abort.
        assert_no_enter(
            &[
                "│   Unknown Model · something │".to_string(),
                "│   5 Freebucks/hr              │".to_string(),
            ],
            &mut ToggleWalk::new(),
        );
    }

    /// The reviewer's exact scenario: a FOCUSED model row rendered with a
    /// DIFFERENT glyph (`❯` instead of `›`) parses as an (unfocused) model row.
    /// Both the old absence-based guard and the two row parsers saw no focus and
    /// pressed Enter — onto a focused model row, starting a billed session. The
    /// walk must ABORT: never walk, never conclude the toggle is focused.
    #[test]
    fn toggle_walk_aborts_on_a_focused_row_with_a_changed_glyph() {
        let rows = vec![
            "│ ❯ GLM 5.3 Flash · Deep reasoning · Reasoning: max · Images │".to_string(),
            "│                             5 Freebucks/hr · Recommended │".to_string(),
            "↓  See all 12 models".to_string(),
        ];
        let mut walk = ToggleWalk::new();
        match toggle_walk_next(&rows, &mut walk) {
            ToggleStep::Abort(_) => {}
            other => panic!("expected Abort, got {other:?}"),
        }
        assert_eq!(walk.presses(), 0, "the walk never took a press");
        // Re-reading the same frame must keep refusing, never Enter.
        assert_no_enter(&rows, &mut walk);
    }

    /// A Down press that does not move focus must abort, not be read as arrival
    /// at the toggle.
    #[test]
    fn toggle_walk_aborts_when_a_press_does_not_move_focus() {
        let start = load_fixture("0227-synthetic-splash-collapsed-120x40");
        let mut walk = ToggleWalk::new();
        assert_eq!(toggle_walk_next(&start, &mut walk), ToggleStep::Down);
        walk.record_down();
        // The screen is unchanged: focus did not move off the model row.
        assert!(matches!(
            toggle_walk_next(&start, &mut walk),
            ToggleStep::Abort(_)
        ));
    }

    /// Focus landing on a row other than the one the ring predicted.
    #[test]
    fn toggle_walk_aborts_when_focus_jumps_to_an_unexpected_row() {
        let mut walk = ToggleWalk::new();
        assert_eq!(
            toggle_walk_next(&three_models_focused(0), &mut walk),
            ToggleStep::Down
        );
        walk.record_down();
        // After one press focus must be on row 1, not row 2.
        assert!(matches!(
            toggle_walk_next(&three_models_focused(2), &mut walk),
            ToggleStep::Abort(_)
        ));
        // Focus leaving the models entirely before the planned end also aborts.
        let mut walk = ToggleWalk::new();
        let _ = toggle_walk_next(&three_models_focused(0), &mut walk);
        walk.record_down();
        assert!(matches!(
            toggle_walk_next(
                &[
                    "│   GLM 5.3 Flash · Deep reasoning │".to_string(),
                    "│   MiMo 2.6 Pro · Balanced │".to_string(),
                    "│   GPT-6 Luna · Balanced │".to_string(),
                    "↓  See all 3 models".to_string(),
                ],
                &mut walk
            ),
            ToggleStep::Abort(_)
        ));
    }

    /// The frame stops parsing mid-walk.
    #[test]
    fn toggle_walk_aborts_when_the_screen_stops_parsing_mid_walk() {
        let mut walk = ToggleWalk::new();
        assert_eq!(
            toggle_walk_next(&three_models_focused(0), &mut walk),
            ToggleStep::Down
        );
        walk.record_down();
        let garbled = vec![
            String::new(),
            "some half-rendered garbage".to_string(),
            "⎘ Copy invite link  Open Earn ↵".to_string(),
        ];
        assert!(matches!(
            toggle_walk_next(&garbled, &mut walk),
            ToggleStep::Abort(_)
        ));
    }

    /// The toggle's advertised count cannot disagree with the rendered ring.
    #[test]
    fn toggle_walk_aborts_when_the_advertised_count_disagrees_with_the_ring() {
        let rows = vec![
            "│ › GLM 5.3 Flash · Deep reasoning │".to_string(),
            "│   MiMo 2.6 Pro · Balanced │".to_string(),
            "↓  See all 1 models".to_string(),
        ];
        let mut walk = ToggleWalk::new();
        match toggle_walk_next(&rows, &mut walk) {
            ToggleStep::Abort(msg) => assert!(msg.contains("advertises"), "unexpected: {msg}"),
            other => panic!("expected Abort, got {other:?}"),
        }
    }

    /// The happy path: a verified start plus validated Down presses arrive at
    /// the toggle, and only THEN is Enter returned.
    #[test]
    fn toggle_walk_enters_only_after_the_validated_walk_reaches_the_toggle() {
        let start = load_fixture("0227-synthetic-splash-collapsed-120x40");
        let mut walk = ToggleWalk::new();
        assert_eq!(toggle_walk_next(&start, &mut walk), ToggleStep::Down);
        walk.record_down();
        // After the one validated Down press, the model row is unfocused and
        // the toggle is the next stop.
        let after = vec![
            "│   GLM 5.3 Flash · Deep reasoning · Reasoning: max · Images │".to_string(),
            "│                             5 Freebucks/hr · Recommended │".to_string(),
            "↓  See all 12 models".to_string(),
        ];
        assert_eq!(toggle_walk_next(&after, &mut walk), ToggleStep::Enter);
        assert_eq!(walk.presses(), 1, "exactly one validated Down press");
    }

    /// The expansion walk terminates on the toggle only with a non-empty model
    /// parse: an empty frame is never read as "nothing is focused".
    #[test]
    fn walk_past_last_model_requires_a_non_empty_parse() {
        assert!(!walk_past_last_model(&["↓  See all 3 models".to_string()]));
        assert!(!walk_past_last_model(&["↑  Show fewer".to_string()]));
        assert!(!walk_past_last_model(&[]));
        // A focused model plus the toggle: not past the last model.
        assert!(!walk_past_last_model(&[
            "│ › MiMo 2.6 Pro · Balanced │".to_string(),
            "↓  See all 3 models".to_string(),
        ]));
        // Unfocused model rows plus the toggle: the walk has scrolled past.
        assert!(walk_past_last_model(&load_fixture(
            "splash-list-showfewer-120x40"
        )));
    }

    /// Positive evidence: the toggle line itself carries the `›` focus marker.
    /// (The real 0.2.x renderer marks a focused toggle only by colour, which the
    /// PTY layer strips, so this is a forward-compatible path, not the only one.)
    #[test]
    fn toggle_walk_enters_on_an_explicitly_focused_toggle() {
        assert_eq!(
            toggle_walk_next(
                &["›  ↓  See all 3 models".to_string()],
                &mut ToggleWalk::new()
            ),
            ToggleStep::Enter
        );
        assert_eq!(
            toggle_walk_next(
                &[
                    "│   MiMo 2.6 Pro · Balanced │".to_string(),
                    "›  ↑  Show fewer".to_string(),
                ],
                &mut ToggleWalk::new()
            ),
            ToggleStep::Enter
        );
        // A model row focused AND a marked toggle cannot co-occur: refuse.
        assert!(matches!(
            toggle_walk_next(
                &[
                    "│ › MiMo 2.6 Pro · Balanced │".to_string(),
                    "›  ↓  See all 3 models".to_string(),
                ],
                &mut ToggleWalk::new()
            ),
            ToggleStep::Abort(_)
        ));
    }

    #[test]
    fn plain_model_ids_omits_unmapped_and_keeps_mapped() {
        let catalog = vec![
            entry("GLM 5.3 Flash", 5),
            entry("Claude Fable 5.1", 15),
            entry("MiMo 2.6 Pro", 10),
        ];
        assert_eq!(
            plain_model_ids(&catalog),
            vec!["z-ai/glm-5.3-flash", "mimo/mimo-v2.6-pro"],
            "only well-formed mapped ids, one per line"
        );
    }

    /// The reviewer's scenario: the fixture lineup includes `Claude Fable 5.1`,
    /// which this module cannot map. It must be absent from the plain
    /// one-id-per-line output, marked (`"id":null`) in JSON, and warned on
    /// stderr — a display name is never emitted as an id.
    #[test]
    fn unmapped_display_name_is_dropped_flagged_and_warned() {
        let catalog = vec![entry("GLM 5.3 Flash", 5), entry("Claude Fable 5.1", 15)];

        // Plain output: the unmapped entry is gone; the name is NOT an id line.
        let plain = plain_model_ids(&catalog);
        assert_eq!(plain, vec!["z-ai/glm-5.3-flash"]);
        assert!(!plain.contains(&"Claude Fable 5.1"));

        // JSON: present but marked, with the display name preserved.
        #[derive(serde::Serialize)]
        struct Payload {
            schema: u32,
            models: Vec<CatalogEntry>,
        }
        let json = serde_json::to_string(&Payload {
            schema: 1u32,
            models: catalog.clone(),
        })
        .expect("serialise");
        assert!(json.contains("\"id\":null"));
        assert!(json.contains("\"display_name\":\"Claude Fable 5.1\""));
        assert!(!json.contains("\"id\":\"Claude Fable 5.1\""));

        // stderr: a one-line warning naming the rotation.
        let warning = unmapped_warning(&catalog).expect("warning");
        assert!(warning.contains("Claude Fable 5.1"));
        assert!(!warning.contains("GLM 5.3 Flash"));
        assert!(unmapped_warning(&[entry("GLM 5.3 Flash", 5)]).is_none());
    }

    #[test]
    fn advertised_total_parses_bare_and_bordered_rows() {
        // The fake emits the toggle line bare; the real splash wraps it in a
        // box border. Both must yield the advertised count.
        assert_eq!(
            advertised_total(&["↓  See all 10 models".to_string()]),
            Some(10)
        );
        assert_eq!(
            advertised_total(&["│  ↓  See all 4 models                    │".to_string()]),
            Some(4)
        );
        // `↑ Show fewer` carries no number: no total.
        assert_eq!(advertised_total(&["↑  Show fewer".to_string()]), None);
        assert_eq!(
            advertised_total(&["Start coding for free".to_string()]),
            None
        );
        assert_eq!(advertised_total(&[]), None);
    }

    #[test]
    fn json_output_omits_unknown_fields() {
        let entry = CatalogEntry {
            id: Some("z-ai/glm-5.3-flash".to_string()),
            display_name: "GLM 5.3 Flash".to_string(),
            section: None,
            price_per_hour: Some(5),
            recommended: None,
            available: None,
            id_source: IdSource::Mapped,
        };
        #[derive(serde::Serialize)]
        struct Payload {
            schema: u32,
            models: Vec<CatalogEntry>,
        }
        let s = serde_json::to_string(&Payload {
            schema: 1u32,
            models: vec![entry],
        })
        .expect("serialise");
        assert!(s.contains("\"schema\":1"));
        assert!(s.contains("\"id\":\"z-ai/glm-5.3-flash\""));
        assert!(s.contains("\"display_name\":\"GLM 5.3 Flash\""));
        assert!(s.contains("\"price_per_hour\":5"));
        // Unknown fields are omitted, never invented.
        assert!(!s.contains("section"));
        assert!(!s.contains("recommended"));
        assert!(!s.contains("available"));
        assert!(!s.contains("id_source"));
    }

    /// End-to-end enumeration against the scripted fake freebuff (a real PTY
    /// session). No real freebuff is involved; a freebuff hour is currently
    /// running on this machine so the live binary cannot be used here.
    #[tokio::test]
    async fn enumerate_catalog_end_to_end_against_fake() -> anyhow::Result<()> {
        let _env_guard = ENV_LOCK.lock().await;
        let fake = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fake-freebuff/fake-freebuff.sh");
        let manicode =
            std::env::temp_dir().join(format!("blink-enumerate-test-{}", std::process::id()));
        std::fs::create_dir_all(&manicode)?;

        let prev_manicode = env::var("BLINK_MANICODE_DIR").ok();
        let prev_lines = env::var("LINES").ok();
        let prev_mode = env::var("FAKE_FREEBUFF_MODE").ok();
        env::set_var("BLINK_MANICODE_DIR", &manicode);
        env::set_var("LINES", "40"); // wide enough that every model is visible at once
        env::set_var("FAKE_FREEBUFF_MODE", "list");

        let result = (async {
            let pty = Pty::spawn(PtyConfig {
                program: fake.to_string_lossy().to_string(),
                args: vec![],
                cwd: Some(manicode.clone()),
                env: vec![],
                cols: 120,
                rows: 40,
            })
            .await
            .map_err(|e| anyhow!("spawn fake: {e:#}"))?;
            let catalog = tokio::time::timeout(Duration::from_secs(20), enumerate_catalog(&pty))
                .await
                .map_err(|_| anyhow!("enumeration timed out"))??;
            let _ = pty.kill().await;
            Ok::<_, anyhow::Error>(catalog)
        })
        .await;

        // Restore the process environment even on failure.
        match prev_manicode {
            Some(v) => env::set_var("BLINK_MANICODE_DIR", v),
            None => env::remove_var("BLINK_MANICODE_DIR"),
        }
        match prev_lines {
            Some(v) => env::set_var("LINES", v),
            None => env::remove_var("LINES"),
        }
        match prev_mode {
            Some(v) => env::set_var("FAKE_FREEBUFF_MODE", v),
            None => env::remove_var("FAKE_FREEBUFF_MODE"),
        }
        let _ = std::fs::remove_dir_all(&manicode);

        let catalog = result?;
        assert_eq!(catalog.len(), 10);
        let ids: Vec<&str> = catalog
            .iter()
            .map(|e| e.id.as_deref().expect("lineup id mapped"))
            .collect();
        assert_eq!(
            ids,
            [
                "z-ai/glm-5.3-flash",
                "mimo/mimo-v2.6-pro",
                "mimo/mimo-v2.5",
                "openai/gpt-6-luna",
                "openai/gpt-6.1-sol",
                "deepseek/deepseek-v4-flash",
                "upstage/solar-pro4",
                "upstage/solar-mini4",
                "meta/muse-spark-1.3-contributor",
                "google/gemini-3.8-flash",
            ]
        );
        let names: Vec<&str> = catalog.iter().map(|e| e.display_name.as_str()).collect();
        assert_eq!(
            names,
            [
                "GLM 5.3 Flash",
                "MiMo 2.6 Pro",
                "MiMo 2.6 Flash",
                "GPT-6 Luna",
                "GPT-6.1 Sol",
                "DeepSeek V4.1 Flash",
                "Solar Pro 4",
                "Solar Mini 4",
                "Muse Spark 1.3",
                "Gemini 3.8 Flash",
            ]
        );
        // Prices are parsed from the cost lines, not invented.
        assert_eq!(catalog[0].price_per_hour, Some(5));
        assert_eq!(catalog[1].price_per_hour, Some(10));
        assert_eq!(catalog[5].price_per_hour, Some(15));
        // Every lineup name is mapped; none silently dropped.
        assert!(catalog
            .iter()
            .all(|e| matches!(e.id_source, IdSource::Mapped)));
        Ok(())
    }

    /// Enumeration scrolls the picker until the advertised total is seen.
    #[tokio::test]
    async fn enumerate_catalog_scrolls_until_total_seen() -> anyhow::Result<()> {
        let _env_guard = ENV_LOCK.lock().await;
        let fake = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fake-freebuff/fake-freebuff.sh");
        let manicode =
            std::env::temp_dir().join(format!("blink-enumerate-scroll-{}", std::process::id()));
        std::fs::create_dir_all(&manicode)?;

        let prev_manicode = env::var("BLINK_MANICODE_DIR").ok();
        let prev_lines = env::var("LINES").ok();
        let prev_mode = env::var("FAKE_FREEBUFF_MODE").ok();
        env::set_var("BLINK_MANICODE_DIR", &manicode);
        // Only 3 model rows fit in the viewport, so the walk must scroll.
        env::set_var("LINES", "14");
        env::set_var("FAKE_FREEBUFF_MODE", "list");

        let result = (async {
            let pty = Pty::spawn(PtyConfig {
                program: fake.to_string_lossy().to_string(),
                args: vec![],
                cwd: Some(manicode.clone()),
                env: vec![],
                cols: 120,
                rows: 40,
            })
            .await
            .map_err(|e| anyhow!("spawn fake: {e:#}"))?;
            let catalog = tokio::time::timeout(Duration::from_secs(20), enumerate_catalog(&pty))
                .await
                .map_err(|_| anyhow!("enumeration timed out"))??;
            let _ = pty.kill().await;
            Ok::<_, anyhow::Error>(catalog)
        })
        .await;

        match prev_manicode {
            Some(v) => env::set_var("BLINK_MANICODE_DIR", v),
            None => env::remove_var("BLINK_MANICODE_DIR"),
        }
        match prev_lines {
            Some(v) => env::set_var("LINES", v),
            None => env::remove_var("LINES"),
        }
        match prev_mode {
            Some(v) => env::set_var("FAKE_FREEBUFF_MODE", v),
            None => env::remove_var("FAKE_FREEBUFF_MODE"),
        }
        let _ = std::fs::remove_dir_all(&manicode);

        let catalog = result?;
        assert_eq!(catalog.len(), 10, "all models seen after scrolling");
        assert!(
            catalog
                .iter()
                .all(|e| matches!(e.id_source, IdSource::Mapped)),
            "every scrolled-in model mapped"
        );
        Ok(())
    }

    // --- Real freebuff v0.2.29: no startup picker, open it with `/model` -----
    //
    // The 2026-10-17 live probe showed freebuff 0.2.29 boots STRAIGHT to the
    // idle input box with a model already selected and `/model to change` in the
    // status row — there is NO startup picker. `--list-models` therefore has to
    // open the picker by submitting `/model`. The startup frame below is
    // VERBATIM from that probe.

    /// The real startup screen is recognised as the idle/input state that needs
    /// `/model` — NOT a picker, NOT a terminal gate, NOT an error.
    #[test]
    fn v0229_real_startup_without_a_picker_opens_via_slash_model() {
        let rows = load_fixture("0229-real-startup-no-picker-120x40");
        let state = classify(&rows);
        assert_eq!(
            state,
            ScreenState::Idle,
            "real startup must classify as Idle"
        );
        assert_eq!(
            picker_entry(&state),
            Some(PickerEntry::Open),
            "the picker must be opened by command, not read off the startup screen"
        );
        assert!(
            !matches!(state, ScreenState::ModelSplash | ScreenState::ModelList),
            "there is no startup picker to read"
        );
        assert!(gate_error(&state).is_none(), "startup is not an error");
        // The status row really does carry the model and the `/model` hint.
        assert_eq!(
            crate::freebuff::screen::active_model(&rows),
            Some(("Nemotron 3.5 Lightning".to_string(), None))
        );
    }

    /// The real `/model` picker frame is read by the existing parsers: it is a
    /// picker, and every rendered model row is collected. This is the parser
    /// tested against a REAL picker instead of a synthetic frame.
    #[test]
    fn v0229_real_slash_model_picker_is_a_picker_with_rows() {
        let rows = load_fixture("0229-real-slash-model-picker-120x40");
        let state = classify(&rows);
        assert!(
            matches!(state, ScreenState::ModelSplash | ScreenState::ModelList),
            "the real /model picker must classify as a picker, got {state:?}"
        );
        assert_eq!(picker_entry(&state), Some(PickerEntry::Visible));
        assert!(gate_error(&state).is_none());
        // `model_rows` — the parser enumeration reads — must read the real
        // picker with the ` • <effort>` suffix stripped and the section set.
        let rows_parsed = model_rows(&rows);
        assert!(
            rows_parsed
                .iter()
                .any(|r| r.name == "Nemotron 3.5 Lightning"
                    && r.section == ModelSection::Unlimited),
            "the real picker must yield clean names + sections: {:?}",
            rows_parsed
                .iter()
                .map(|r| (r.name.as_str(), r.section))
                .collect::<Vec<_>>()
        );
        assert!(
            rows_parsed.iter().all(|r| !r.name.contains('•')),
            "the effort suffix must not leak into display names"
        );
        // The scrolled-continuation artifact (`3x · Recommended`) is not a name.
        assert!(
            rows_parsed.iter().all(|r| !r.name.starts_with('3')),
            "a multiplier row must not be read as a model"
        );
    }

    /// `confirmation_text` fires on freebuff's modal affordance — an
    /// `Enter: <action>` and an `Esc: <action>` on one row — and is deliberately
    /// blind to the idle info line (`Your first message starts the session.`)
    /// that is present on the REAL startup screen: otherwise every enumeration
    /// would abort before it began.
    #[test]
    fn confirmation_text_detects_modal_affordance_but_not_the_idle_info_line() {
        assert_eq!(
            confirmation_text(&["Enter: retry  Esc: back to draft".to_string()]).as_deref(),
            Some("Enter: retry  Esc: back to draft")
        );
        let startup = load_fixture("0229-real-startup-no-picker-120x40");
        // The idle info box literally says "starts the session".
        assert!(
            startup.iter().any(|r| r.contains("starts the session")),
            "fixture must carry the info line for this guard to bite"
        );
        assert_eq!(
            confirmation_text(&startup),
            None,
            "the idle info line must never read as a confirmation"
        );
    }

    /// The detector matches by freebuff's AFFORDANCE IDIOM — an `Enter: <action>`
    /// AND an `Esc: <action>` on one row — so BOTH live modals fire (the hours
    /// gate and the free-plan take-over modal) regardless of wording, while
    /// ordinary transcript prose and the healthy idle lines do not. It replaced a
    /// spend-KEYWORD list that missed `take over` and false-matched `retry`.
    #[test]
    fn confirmation_detector_matches_modal_affordances_not_transcript_prose() {
        for row in [
            // The two REAL modal affordances captured live (2026-10-16/17).
            "│ Enter: retry  Esc: back to draft                     │",
            "│ Enter: take over  Esc: back to draft                 │",
        ] {
            assert!(
                is_modal_affordance_row(row),
                "must be read as a modal affordance: {row:?}"
            );
        }
        // Fail-closed in BOTH directions: an `Enter:` with no `Esc:`, an `Esc:`
        // with no `Enter:`, and ordinary prose with no colon affordance must
        // never fire — including the transcript-style `Enter to retry` the old
        // keyword detector false-matched on `retry`.
        for row in [
            "Enter to retry",
            "│  ▍Enter a coding task or / for commands  │",
            "│ Your first message starts the session. │",
            "Press Enter to continue in a new session",
            "Not enough Freebucks — 5 Freebucks/hr against 0 left. Enter opens plans.",
            "↑↓ choose model · Tab reasoning · Enter select · Esc cancel",
            "Enter: retry",
            "Esc: back to draft",
            "│ 25/25 Freebucks remaining │",
            "│ 3/5 hours remaining │",
            "📋 Pasted text (5,001 chars)",
        ] {
            assert!(!is_modal_affordance_row(row), "must NOT match: {row:?}");
        }
    }

    /// Guard against a false positive that would break enumeration OUTRIGHT:
    /// across EVERY screen fixture, only the frames carrying freebuff's modal
    /// affordance may read as a confirmation. With the per-row affordance
    /// detector that is EXACTLY the two real modal interstitials — the hours
    /// gate and the free-plan take-over modal — both of which classify as
    /// terminal gates first (so this is only a fail-closed backstop). The
    /// idle/picker/transcript frames — including the real idle screens whose info
    /// box carries `Freebucks`/`hours` copy and the resume/gate screens whose
    /// `Enter ...` has no `Esc:` affordance — must not.
    #[test]
    fn no_healthy_fixture_reads_as_a_confirmation() {
        // Fixtures that render freebuff's `Enter: x  Esc: y` modal affordance.
        // Both are terminal gates (the hours gate and the take-over modal), so
        // this list is a fail-closed backstop, not the thing that protects money.
        const EXPECTED: &[&str] = &[
            "0229-real-hours-gate-120x40.txt",
            "0229-real-take-over-modal-120x40.txt",
        ];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/screens");
        let mut scanned = 0usize;
        for entry in std::fs::read_dir(&dir).expect("fixtures dir") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("txt") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let content = std::fs::read_to_string(&path).expect("read fixture");
            let rows: Vec<String> = content.lines().map(|s| s.to_string()).collect();
            let flagged = confirmation_text(&rows);
            if EXPECTED.contains(&name.as_str()) {
                assert!(flagged.is_some(), "{name} must read as a confirmation");
            } else {
                assert!(
                    flagged.is_none(),
                    "healthy fixture {name} must not be flagged: {flagged:?}"
                );
            }
            scanned += 1;
        }
        assert!(
            scanned >= 30,
            "expected to scan every screen fixture, saw {scanned}"
        );
    }

    /// BLOCKING regression, driven by three CONSECUTIVE real frames of the live
    /// `/model` scroll: a wrapped description row must not become a model, and a
    /// section whose header has scrolled off must still be attributed. The tail
    /// frame alone cannot know its section, so this walks the frames the way
    /// `enumerate_catalog` does.
    #[test]
    fn v0229_real_frames_drop_the_junk_row_and_keep_every_section() {
        // The tail frame genuinely has no header on screen.
        let tail = load_fixture("0229-real-picker-tail-120x40");
        assert!(
            model_rows(&tail)
                .iter()
                .all(|r| r.section == ModelSection::Unknown),
            "the tail fixture must have no section header, or this test cannot bite"
        );

        let mut seen: Vec<SeenEntry> = Vec::new();
        let mut last_section: Option<String> = None;
        for name in [
            // The real walk starts at the top of the list (headers on screen),
            // then scrolls down until the headers have scrolled off.
            "0229-real-slash-model-picker-120x40",
            "0229-real-picker-powerful-section-120x40",
            "0229-real-picker-wrapped-description-120x40",
            "0229-real-picker-tail-120x40",
        ] {
            collect_frame(
                &mut seen,
                &model_rows(&load_fixture(name)),
                &mut last_section,
            );
        }

        // (a) the wrapped description row is gone.
        assert!(
            seen.iter().all(|s| !s.display_name.contains("detail")),
            "the wrapped description row must not be a model: {:?}",
            seen.iter()
                .map(|s| s.display_name.as_str())
                .collect::<Vec<_>>()
        );
        // (b) every real model landed in a real section, including the two that
        //     only ever appeared after their header had scrolled off.
        assert!(
            seen.iter().all(|s| s.section.is_some()),
            "every model must have a section: {:?}",
            seen.iter()
                .map(|s| (s.display_name.as_str(), s.section.as_deref()))
                .collect::<Vec<_>>()
        );
        let section_of = |name: &str| {
            seen.iter()
                .find(|s| s.display_name == name)
                .unwrap_or_else(|| panic!("missing {name}"))
                .section
                .clone()
        };
        assert_eq!(section_of("Solar Pro 4").as_deref(), Some("unlimited"));
        assert_eq!(
            section_of("DeepSeek V4.1 Flash").as_deref(),
            Some("optimized")
        );
        for name in [
            "Gemini 3.8 Flash",
            "GPT-6.1 Sol",
            "DeepSeek V4.1 Flash Fast",
        ] {
            assert_eq!(
                section_of(name).as_deref(),
                Some("powerful"),
                "{name} must inherit the section carried across frames"
            );
        }
    }

    // --- `/model` submit safety: exactly one Enter, no blind nudge ----------

    /// A screen that is POSITIVELY the idle input box holding `box_text`.
    fn idle_rows(box_text: &str) -> Vec<String> {
        vec![
            " GLM 5.3 Flash · 59m left · 16.4K (2%)      ✕ End session".to_string(),
            "╭──────────────────────────────────────╮".to_string(),
            format!("│  ▍{box_text}  │"),
            "╰──────────────────────────────────────╯".to_string(),
        ]
    }

    /// The real 0.2.29 idle-with-typed-text layout (verbatim shape from the
    /// 2026-10-17 live probe): FREEBUFF banner + info box + input box + a status
    /// row that carries `/model to change` and NO `✕ End session`.
    fn real_typed_idle_rows(box_text: &str) -> Vec<String> {
        vec![
            " ███████████                        ██                 ▄████  ▄██".to_string(),
            " ┌──────────────────────────────────────────────────────────┐".to_string(),
            " │ Your first message starts the session.                   │".to_string(),
            " │ Some models aren't available on this connection          │".to_string(),
            " └──────────────────────────────────────────────────────────┘".to_string(),
            "╭──────────────────────────────────────────────────────────╮".to_string(),
            "│  /model      Choose the model for your next message      │".to_string(),
            format!("│  ▍{box_text}                                              │"),
            "╰──────────────────────────────────────────────────────────╯".to_string(),
            " GLM 5.3 Flash • high · /private/tmp · /model to change · Chat: New chat".to_string(),
            " ← for history · ? for help".to_string(),
        ]
    }

    /// The single Enter is returned only for a confirmed idle screen whose box
    /// holds EXACTLY the command. Leftover text that merely CONTAINS the command
    /// must never authorise a keystroke — that is how a stray MESSAGE, which
    /// starts a billed session, gets submitted.
    #[test]
    fn submit_step_enters_only_on_a_confirmed_idle_box_holding_exactly_the_command() {
        let rows = idle_rows("/model");
        assert_eq!(classify(&rows), ScreenState::Idle);
        assert!(matches!(
            submit_step(&classify(&rows), &rows, "/model"),
            Ok(SubmitStep::Enter)
        ));

        // Every one of these SATISFIES the substring helper `input_box_shows`
        // and yet would be submitted as a MESSAGE (or a different command).
        for box_text in [
            "/model/model",
            "foo/model",
            "/mod",
            "/model junk",
            "Enter a coding task or / for commands",
            "",
        ] {
            let rows = idle_rows(box_text);
            assert!(matches!(classify(&rows), ScreenState::Idle), "{box_text:?}");
            assert!(
                submit_step(&classify(&rows), &rows, "/model").is_err(),
                "box {box_text:?} must NOT authorise Enter"
            );
        }
    }

    /// The real 0.2.29 idle-with-typed-text screen IS the idle input box: a live
    /// session status row plus a rendered box holding exactly the command.
    /// Typing into the box REPLACED the placeholder, so it must classify as
    /// `Idle` — the classifier recognises the typed-in box itself rather than
    /// reporting `Booting`, which is why nothing needs to widen the
    /// authorisation. It authorises the single Enter; a bare `Booting` frame
    /// with no status row and no box still fails closed.
    #[test]
    fn submit_step_accepts_the_real_typed_idle_screen_but_not_a_bare_booting_frame() {
        let rows = real_typed_idle_rows("/model");
        assert_eq!(
            classify(&rows),
            ScreenState::Idle,
            "a typed idle box must classify as Idle, not Booting"
        );
        assert!(
            crate::freebuff::screen::active_model(&rows).is_some(),
            "a live status row is present"
        );
        assert!(input_box_is_exactly(&rows, "/model"));
        assert!(matches!(
            submit_step(&classify(&rows), &rows, "/model"),
            Ok(SubmitStep::Enter)
        ));

        // A Booting frame (no status row, no box) authorises nothing.
        let bare = vec![" ██████".to_string()];
        assert_eq!(classify(&bare), ScreenState::Booting);
        assert!(
            submit_step(&classify(&bare), &bare, "/model").is_err(),
            "a bare Booting frame must not authorise Enter"
        );
    }

    /// The mixed repaint the reviewer flagged: a model picker APPEARING while a
    /// stale idle status row and the `/model` input box are still painted. The
    /// classifier checks the picker states BEFORE Idle, so the frame must
    /// classify as a PICKER and the submit must press NOTHING — a blind Enter
    /// would land on the picker's focused MODEL row and start a billed session.
    #[test]
    fn submit_step_treats_a_mixed_picker_and_input_frame_as_a_picker() {
        let rows: Vec<String> = vec![
            // A live status row, so the frame is not dismissed as Booting.
            " GLM 5.3 Flash • high · /tmp · /model to change · Chat: New chat".to_string(),
            // Picker rows already painted over it.
            "│ › GLM 5.3 Flash · Deep reasoning · Images │".to_string(),
            "│   MiMo 2.6 Pro · Strong reasoning · Images │".to_string(),
            "↓  See all 12 models".to_string(),
            // The stale input box, still holding exactly `/model`.
            "╭──────────────────────────────────────╮".to_string(),
            "│  ▍/model  │".to_string(),
            "╰──────────────────────────────────────╯".to_string(),
        ];
        // The box really does hold the command, so only the CLASSIFICATION
        // stands between this frame and a blind Enter.
        assert!(input_box_is_exactly(&rows, "/model"));
        let state = classify(&rows);
        assert!(
            matches!(state, ScreenState::ModelSplash | ScreenState::ModelList),
            "a frame painting picker rows must classify as a picker, got {state:?}"
        );
        assert!(
            matches!(submit_step(&state, &rows, "/model"), Ok(SubmitStep::Opened)),
            "a mixed picker frame must press NOTHING"
        );
    }

    /// The picker rendered before the read: the step must be `Opened`, i.e. the
    /// caller returns WITHOUT pressing anything — a keypress here would land on
    /// the picker's focused MODEL row (Enter on a model row starts a session and
    /// spends Freebucks).
    #[test]
    fn submit_step_stops_without_pressing_when_a_picker_is_already_up() {
        for name in [
            "0229-real-slash-model-picker-120x40",
            "0227-synthetic-splash-collapsed-120x40",
        ] {
            let rows = load_fixture(name);
            let state = classify(&rows);
            assert!(
                matches!(state, ScreenState::ModelSplash | ScreenState::ModelList),
                "{name}: {state:?}"
            );
            assert!(
                matches!(submit_step(&state, &rows, "/model"), Ok(SubmitStep::Opened)),
                "{name}: a picker already on screen must never authorise a keypress"
            );
        }
    }

    /// A gate or a start/spend confirmation aborts (Esc + a loud error) instead
    /// of being confirmed.
    #[test]
    fn submit_step_aborts_on_a_gate_or_a_confirmation() {
        let gate =
            vec!["│ Not enough hours for MiMo Fast: an hour on it takes 2 hours. │".to_string()];
        assert!(matches!(classify(&gate), ScreenState::HoursGate { .. }));
        let err = submit_step(&classify(&gate), &gate, "/model")
            .expect_err("a gate must not authorise Enter")
            .to_string();
        assert!(err.contains("not enough hours"), "{err}");

        let confirm = vec!["Enter: take over  Esc: back to draft".to_string()];
        let err = submit_step(&classify(&confirm), &confirm, "/model")
            .expect_err("a modal affordance must not authorise Enter")
            .to_string();
        assert!(err.contains("take over"), "{err}");
    }

    /// BLOCKING: the free-plan take-over modal (a session is held ELSEWHERE; the
    /// free plan shares one session slot across CLI and Desktop) is a terminal
    /// gate. Enumeration must FAIL FAST with the actionable error and press
    /// NOTHING on it — Enter would take over the other session, which AGENTS.md
    /// forbids. The modal has NO rounded input box, so the allowlist already
    /// fails closed; the gate turns the silent timeout into an actionable error.
    #[test]
    fn submit_step_refuses_on_the_take_over_modal() {
        let rows = load_fixture("0229-real-take-over-modal-120x40");
        let state = classify(&rows);
        assert_eq!(
            state,
            ScreenState::TakeOver,
            "the free-plan take-over modal must classify as its gate"
        );
        // Keyed on the modal's distinctive copy, not a bare `Take over`.
        assert!(gate_error(&state).is_some(), "TakeOver is a terminal gate");
        // No bottom-most ROUNDED box: the allowlist alone already fails closed.
        assert_eq!(crate::freebuff::screen::input_box_text(&rows), None);
        assert!(!input_box_is_exactly(&rows, "/model"));
        let err = submit_step(&state, &rows, "/model")
            .expect_err("the take-over modal must not authorise Enter")
            .to_string();
        assert!(err.contains("active elsewhere"), "{err}");
        assert!(err.contains("will not choose `Take over`"), "{err}");
    }

    /// Fail closed: anything that is not POSITIVELY the idle screen — a repaint
    /// with an empty grid, or unrecognised prose — authorises nothing.
    #[test]
    fn submit_step_fails_closed_on_an_unrecognised_screen() {
        let empty: Vec<String> = Vec::new();
        assert_eq!(classify(&empty), ScreenState::Booting);
        assert!(
            submit_step(&classify(&empty), &empty, "/model").is_err(),
            "an empty/repainting screen must never authorise Enter"
        );

        let unknown = vec!["some prose".to_string()];
        assert_eq!(classify(&unknown), ScreenState::Unknown);
        assert!(
            submit_step(&classify(&unknown), &unknown, "/model").is_err(),
            "an unrecognised screen must never authorise Enter"
        );
    }

    /// Structural end-to-end proof: the fake opens the picker on the FIRST
    /// Enter, so any further nudge lands on its focused MODEL row — where the
    /// fake exits 1 (exactly what it does for a model-row Enter). The catalog
    /// must still come back complete, proving the picker-entry path pressed
    /// nothing after the picker opened. Guards against a nudge being
    /// reintroduced.
    #[tokio::test]
    async fn enumerate_never_nudges_enter_once_the_picker_is_open() -> anyhow::Result<()> {
        let _env_guard = ENV_LOCK.lock().await;
        let fake = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fake-freebuff/fake-freebuff.sh");
        let manicode =
            std::env::temp_dir().join(format!("blink-enumerate-noswallow-{}", std::process::id()));
        std::fs::create_dir_all(&manicode)?;

        let prev_manicode = env::var("BLINK_MANICODE_DIR").ok();
        let prev_lines = env::var("LINES").ok();
        let prev_mode = env::var("FAKE_FREEBUFF_MODE").ok();
        env::set_var("BLINK_MANICODE_DIR", &manicode);
        env::set_var("LINES", "40");
        env::set_var("FAKE_FREEBUFF_MODE", "list");

        let result = (async {
            let pty = Pty::spawn(PtyConfig {
                program: fake.to_string_lossy().to_string(),
                args: vec![],
                cwd: Some(manicode.clone()),
                env: vec![],
                cols: 120,
                rows: 40,
            })
            .await
            .map_err(|e| anyhow!("spawn fake: {e:#}"))?;
            let catalog = tokio::time::timeout(Duration::from_secs(20), enumerate_catalog(&pty))
                .await
                .map_err(|_| anyhow!("enumeration timed out"))??;
            let _ = pty.kill().await;
            Ok::<_, anyhow::Error>(catalog)
        })
        .await;

        match prev_manicode {
            Some(v) => env::set_var("BLINK_MANICODE_DIR", v),
            None => env::remove_var("BLINK_MANICODE_DIR"),
        }
        match prev_lines {
            Some(v) => env::set_var("LINES", v),
            None => env::remove_var("LINES"),
        }
        match prev_mode {
            Some(v) => env::set_var("FAKE_FREEBUFF_MODE", v),
            None => env::remove_var("FAKE_FREEBUFF_MODE"),
        }
        let _ = std::fs::remove_dir_all(&manicode);

        let catalog = result?;
        assert_eq!(
            catalog.len(),
            10,
            "every model must still be read, with no stray nudge after the picker opened"
        );
        Ok(())
    }

    /// STRUCTURAL guard: the picker-entry path writes EXACTLY ONE Enter.
    ///
    /// `pty.key(Enter)` only WRITES the key; it does not wait for freebuff to
    /// consume it. A repeat guarded on a screen snapshot could therefore still be
    /// delivered AFTER the previous Enter opened the picker — landing on a
    /// focused MODEL row and starting a billed session. The fake records every
    /// Enter it receives while idle, i.e. by the picker-entry path (in-picker
    /// Enters from the toggle walk are governed separately and are not logged);
    /// the recorded sequence must contain exactly one. Structural, not
    /// timing-based: it asserts the recorded key sequence.
    #[tokio::test]
    async fn picker_entry_path_writes_exactly_one_enter() -> anyhow::Result<()> {
        let _env_guard = ENV_LOCK.lock().await;
        let fake = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fake-freebuff/fake-freebuff.sh");
        let base =
            std::env::temp_dir().join(format!("blink-enumerate-oneenter-{}", std::process::id()));
        let manicode = base.join("manicode");
        let keylog = base.join("keys.log");
        std::fs::create_dir_all(&manicode)?;

        let prev_manicode = env::var("BLINK_MANICODE_DIR").ok();
        let prev_lines = env::var("LINES").ok();
        let prev_mode = env::var("FAKE_FREEBUFF_MODE").ok();
        let prev_keylog = env::var("FAKE_FREEBUFF_KEYLOG").ok();
        env::set_var("BLINK_MANICODE_DIR", &manicode);
        env::set_var("LINES", "40");
        env::set_var("FAKE_FREEBUFF_MODE", "list");
        env::set_var("FAKE_FREEBUFF_KEYLOG", &keylog);

        let result = (async {
            let pty = Pty::spawn(PtyConfig {
                program: fake.to_string_lossy().to_string(),
                args: vec![],
                cwd: Some(manicode.clone()),
                env: vec![],
                cols: 120,
                rows: 40,
            })
            .await
            .map_err(|e| anyhow!("spawn fake: {e:#}"))?;
            let catalog = tokio::time::timeout(Duration::from_secs(20), enumerate_catalog(&pty))
                .await
                .map_err(|_| anyhow!("enumeration timed out"))??;
            let _ = pty.kill().await;
            Ok::<_, anyhow::Error>(catalog)
        })
        .await;

        match prev_manicode {
            Some(v) => env::set_var("BLINK_MANICODE_DIR", v),
            None => env::remove_var("BLINK_MANICODE_DIR"),
        }
        match prev_lines {
            Some(v) => env::set_var("LINES", v),
            None => env::remove_var("LINES"),
        }
        match prev_mode {
            Some(v) => env::set_var("FAKE_FREEBUFF_MODE", v),
            None => env::remove_var("FAKE_FREEBUFF_MODE"),
        }
        match prev_keylog {
            Some(v) => env::set_var("FAKE_FREEBUFF_KEYLOG", v),
            None => env::remove_var("FAKE_FREEBUFF_KEYLOG"),
        }

        let catalog = result?;
        assert_eq!(catalog.len(), 10, "the catalog must still be read");

        let log = std::fs::read_to_string(&keylog).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&base);
        let enters = log.lines().filter(|l| *l == "idle-enter").count();
        assert_eq!(
            enters, 1,
            "the picker-entry path must write EXACTLY ONE Enter; recorded key log:\n{log}"
        );
        Ok(())
    }
}
