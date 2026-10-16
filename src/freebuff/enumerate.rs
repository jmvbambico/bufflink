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
    classify, model_rows, nav_ring, parse_catalog, toggle_row_has_focus_marker, ScreenState,
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

/// `true` only when the parsed screen proves focus is on the See-all /
/// Show-fewer toggle row.
///
/// Pressing Enter on a model row starts a session and SPENDS Freebucks, so this
/// is a hard refusal point: Enter is refused unless this returns `true`. It
/// FAILS CLOSED — the previous form inferred safety from the ABSENCE of a
/// focused model (`all()` over a model-row list is vacuously true when the list
/// is EMPTY), so a partially rendered / mid-clear / scrolled frame, or any
/// future row-shape change, could let Enter reach a focused model row.
///
/// Enter is allowed only when all of these hold:
/// (a) the toggle row is rendered AND is positively the focused stop — the
///     toggle line itself carries the `›` marker, or (the real 0.2.x renderer
///     marks a focused toggle by colour, which the PTY layer strips) the parse
///     proves it structurally below;
/// (b) no model row is focused, established from a NON-EMPTY parse;
/// (c) the screen parses coherently at all ([`nav_ring`] recognises the picker).
pub fn focus_is_on_toggle(rows: &[String]) -> bool {
    // (c) The screen must be recognisable as the 0.2.x picker: a model row or
    //     the toggle must be present. A blank/garbled/partial frame is None.
    let Some(ring) = nav_ring(rows) else {
        return false;
    };
    // (a) The toggle row must actually be rendered.
    if ring.toggle_index.is_none() {
        return false;
    }

    // (a) Explicit evidence: the toggle line itself carries the focus marker.
    let toggle_marked = rows.iter().any(|r| toggle_row_has_focus_marker(r));
    // (b) Independent "no model focused" reads, so a single parser missing a
    //     focus marker cannot alone fail open.
    let no_row_focused = model_rows(rows).iter().all(|m| !m.focused);
    let catalog = parse_catalog(rows);
    if catalog.entries.is_empty() {
        // No model row parsed at all. `all()` here would be vacuously true, so
        // this is exactly the empty-parse case that must NOT pass: only an
        // explicit toggle marker (and no focused row either parser can see)
        // proves focus.
        return toggle_marked && no_row_focused;
    }
    // (b) With a NON-EMPTY parse, `!focused` is meaningful: the picker is a
    //     single-focus ring, so no focused model row means the toggle holds it.
    let no_entry_focused = catalog.entries.iter().all(|e| !e.focused);
    no_entry_focused && no_row_focused
}

/// Send `Enter` only when the guard confirms focus is on the toggle row.
async fn enter_toggle(pty: &Pty, rows: &[String]) -> Result<()> {
    if !focus_is_on_toggle(rows) {
        return Err(anyhow!(
            "refusing to send Enter: focus is not on the See-all/Show-fewer toggle row \
             (Enter on a model row starts a session and spends Freebucks)"
        ));
    }
    pty.key(Key::Enter).await
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

/// Enumerate the model catalog by driving `pty` through freebuff's picker.
///
/// Sequence: wait for the model picker, expand via the toggle row (Enter only
/// when the guard allows), read every row while scrolling until the advertised
/// total is seen or the walk converges, then return the catalog in picker
/// order. The PTY is left running; the caller must `kill` it on every exit
/// path.
pub async fn enumerate_catalog(pty: &Pty) -> Result<Vec<CatalogEntry>> {
    let deadline = Instant::now() + ENUMERATION_TIMEOUT;

    // 1. Wait for the model picker; fail fast on a terminal gate such as
    //    "already running" (single-instance) rather than spinning.
    let snap = pty
        .wait_for(
            |s| {
                let st = classify(&s.rows);
                matches!(st, ScreenState::ModelSplash | ScreenState::ModelList)
                    || st == ScreenState::AlreadyRunning
            },
            ENUMERATION_TIMEOUT,
        )
        .await?;
    let state = classify(&snap.rows);
    match state {
        ScreenState::AlreadyRunning => {
            return Err(anyhow!(
                "freebuff: already running — only one instance may run at a time; \
                 wait for it to end or stop the other instance"
            ));
        }
        ScreenState::ModelSplash | ScreenState::ModelList => {}
        st => {
            return Err(anyhow!(
                "freebuff: unexpected screen while enumerating models: {st:?}"
            ));
        }
    }

    // 2. Expand the picker if it is collapsed. Enter is only sent on the
    //    toggle row, and only after the guard confirms focus is there. The
    //    collapsed splash advertises the lineup size (`↓ See all N models`);
    //    record it so the walk below knows when every model has been read.
    let mut total: Option<usize> = None;
    let mut presses = 0u32;
    loop {
        let rows = pty.screen().rows.clone();
        let st = classify(&rows);
        if let Some(t) = advertised_total(&rows) {
            total = Some(t);
        }
        match st {
            ScreenState::ModelList => break, // already expanded
            ScreenState::ModelSplash => {
                if focus_is_on_toggle(&rows) {
                    enter_toggle(pty, &rows).await?;
                    tokio::time::sleep(SETTLE).await;
                } else if presses >= MAX_DOWN_PRESSES {
                    return Err(anyhow!(
                        "freebuff: could not reach the model-picker toggle row"
                    ));
                } else {
                    let before = pty.screen().text();
                    pty.key(Key::Down).await?;
                    presses += 1;
                    settle_after(pty, &before, deadline).await?;
                }
            }
            ScreenState::Booting => {
                // The TUI clears the screen (`ESC[2J`) before each repaint, so a
                // read taken in that window sees an empty grid and classifies as
                // Booting. It is a transient render state, not a different TUI
                // shape: let the paint settle and re-read.
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

    // 3. Walk the expanded list, collecting names and prices while scrolling
    //    until the advertised total is seen or the walk converges.
    let mut seen: Vec<(String, Option<u64>)> = Vec::new(); // (display_name, price)
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
        let mut newly_seen: Vec<(String, Option<u64>)> = Vec::new();
        for row in &models {
            if seen.iter().all(|(n, _)| *n != row.name) {
                let price = row
                    .cost_line
                    .as_deref()
                    .and_then(price_per_hour_from_cost_line);
                newly_seen.push((row.name.clone(), price));
            }
        }
        seen.extend(newly_seen);

        if let Some(total) = total {
            if seen.len() >= total {
                break;
            }
        }
        // In the expanded view the advertised total (`↓ See all N`) is no
        // longer shown; `↑ Show fewer` carries no number. When focus reaches
        // that toggle row we've scrolled past the last model, so the walk is
        // done. (`focus_is_on_toggle` requires no model focused, so it won't
        // fire right after expanding: focus=0 on the first model.)
        if focus_is_on_toggle(&rows) {
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

    let catalog: Vec<CatalogEntry> = seen
        .into_iter()
        .map(|(name, price)| {
            let (id, id_source) = map_display_name(&name);
            CatalogEntry {
                id,
                display_name: name,
                // `id` is `None` for an unmapped name; the entry is kept so the
                // rotation is visible in `--json` / the stderr warning.
                section: None,
                price_per_hour: price,
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

    #[test]
    fn focus_is_on_toggle_refuses_model_focus() {
        // Collapsed splash: focus is on the model row (`›`), not the toggle.
        assert!(!focus_is_on_toggle(&load_fixture("splash-120x40")));
        // Expanded list with a model focused.
        assert!(!focus_is_on_toggle(&load_fixture(
            "splash-list-focus4-120x40"
        )));
    }

    #[test]
    fn focus_is_on_toggle_allows_toggle_focus() {
        // `Show fewer` visible and no model row focused: focus is on the toggle.
        assert!(focus_is_on_toggle(&load_fixture(
            "splash-list-showfewer-120x40"
        )));
    }

    #[test]
    fn focus_is_on_toggle_rejects_no_toggle() {
        let rows = vec![
            "│   Unknown Model · something │".to_string(),
            "│   5 Freebucks/hr              │".to_string(),
        ];
        assert!(!focus_is_on_toggle(&rows));
    }

    #[test]
    fn focus_is_on_toggle_requires_no_model_focus() {
        // Toggle visible but a model is focused: must refuse.
        let rows = vec![
            "│ › Known Model · something │".to_string(),
            "│   5 Freebucks/hr              │".to_string(),
            "↓  See all 3 models".to_string(),
        ];
        assert!(!focus_is_on_toggle(&rows));
    }

    /// The money-safety regression: an EMPTY parse used to read as "nothing is
    /// focused" (`all()` is vacuously true), so this guard FAILED OPEN. Every
    /// unparseable / partial / blank frame must refuse Enter.
    #[test]
    fn focus_is_on_toggle_fails_closed_on_unparseable_screens() {
        // Empty and blank/whitespace-only frames.
        assert!(!focus_is_on_toggle(&[]));
        assert!(!focus_is_on_toggle(&["".to_string(), "   ".to_string()]));
        // Garbled / mid-clear frame: chrome but no recognisable picker shape.
        let garbled = vec![
            String::new(),
            "some half-rendered garbage".to_string(),
            "⎘ Copy invite link  Open Earn ↵".to_string(),
        ];
        assert!(!focus_is_on_toggle(&garbled));
        // A model box with no toggle row: not a toggle frame.
        let no_toggle = vec![
            "│   Unknown Model · something │".to_string(),
            "│   5 Freebucks/hr              │".to_string(),
        ];
        assert!(!focus_is_on_toggle(&no_toggle));
    }

    /// A frame where only the toggle parses (e.g. the model box is cut off mid
    /// render) has NO focus marker anywhere: with no model rows parsed, the
    /// "no model focused" read is vacuous, so it must refuse.
    #[test]
    fn focus_is_on_toggle_fails_closed_when_no_model_row_parses() {
        assert!(!focus_is_on_toggle(&["↓  See all 3 models".to_string()]));
        assert!(!focus_is_on_toggle(&["↑  Show fewer".to_string()]));
        // Even a model-shaped row that the parser cannot read (no ` · ` tagline
        // separator) leaves the parse empty -> refuse.
        let cut_off = vec![
            "│ › GLM 5.3 Flash".to_string(),
            "↓  See all 3 models".to_string(),
        ];
        assert!(!focus_is_on_toggle(&cut_off));
    }

    /// Positive evidence: the toggle line itself carries the `›` focus marker.
    /// (The real 0.2.x renderer marks a focused toggle only by colour, which the
    /// PTY layer strips, so this is a forward-compatible path, not the only one.)
    #[test]
    fn focus_is_on_toggle_accepts_an_explicitly_focused_toggle() {
        assert!(focus_is_on_toggle(&["›  ↓  See all 3 models".to_string()]));
        // Corroborated by an unfocused model box.
        assert!(focus_is_on_toggle(&[
            "│   MiMo 2.6 Pro · Balanced │".to_string(),
            "›  ↑  Show fewer".to_string(),
        ]));
        // A model row focused AND a marked toggle cannot co-occur; the focused
        // model wins and the guard refuses.
        assert!(!focus_is_on_toggle(&[
            "│ › MiMo 2.6 Pro · Balanced │".to_string(),
            "›  ↓  See all 3 models".to_string(),
        ]));
    }

    /// Documented deviation: the real 0.2.x picker draws the toggle with the
    /// focus colour/bold (stripped by the PTY layer) instead of `›`, so a frame
    /// with a parsed-but-unfocused model box plus the toggle IS the
    /// toggle-focused frame and must be accepted — otherwise the picker could
    /// never be expanded. Verified against the 0.2.x bundle renderer.
    #[test]
    fn focus_is_on_toggle_accepts_the_colour_only_toggle_frame() {
        let rows = vec![
            "│   GLM 5.3 Flash · Deep reasoning · Images │".to_string(),
            "│              5 Freebucks/hr               │".to_string(),
            "↓  See all 12 models".to_string(),
        ];
        assert!(focus_is_on_toggle(&rows));
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
}
