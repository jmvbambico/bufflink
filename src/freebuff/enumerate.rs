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

use crate::freebuff::screen::{classify, model_rows, ScreenState};
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
    /// No mapping existed; the display name was used as a best-effort id.
    BestEffort,
}

/// One catalog entry, in picker order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogEntry {
    pub id: String,
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

/// Map a display name to a model id. Unknown names are emitted anyway with the
/// display name as a best-effort id (`IdSource::BestEffort`): silently dropping
/// a model is worse than an imperfect id because the lineup rotates.
pub fn map_display_name(name: &str) -> (String, IdSource) {
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
            return (id.to_string(), IdSource::Mapped);
        }
    }
    (name.to_string(), IdSource::BestEffort)
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
/// Show-fewer toggle row: no model row carries the `›` marker and a toggle
/// row is visible.
///
/// Pressing Enter on a model row starts a session and SPENDS Freebucks, so
/// this is a hard refusal point: Enter is refused unless this returns `true`.
pub fn focus_is_on_toggle(rows: &[String]) -> bool {
    let models = model_rows(rows);
    let no_model_focused = models.iter().all(|m| !m.focused);
    let toggle_visible = rows
        .iter()
        .any(|r| r.contains("See all") || r.contains("Show fewer"));
    no_model_focused && toggle_visible
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
            assert_eq!(got_id, id, "name: {name}");
            assert_eq!(got_source, source, "name: {name}");
        }
    }

    #[test]
    fn map_display_name_unmapped_uses_best_effort_id() {
        let (id, source) = map_display_name("Unknown New Model");
        assert_eq!(id, "Unknown New Model");
        assert_eq!(source, IdSource::BestEffort);
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
            id: "z-ai/glm-5.3-flash".to_string(),
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

    #[test]
    fn json_output_best_effort_entry_uses_display_name_as_id() {
        let (id, source) = map_display_name("Unknown New Model");
        assert_eq!(id, "Unknown New Model");
        assert_eq!(source, IdSource::BestEffort);
        let entry = CatalogEntry {
            id,
            display_name: "Unknown New Model".to_string(),
            section: None,
            price_per_hour: None,
            recommended: None,
            available: None,
            id_source: IdSource::BestEffort,
        };
        let s = serde_json::to_string(&entry).expect("serialise");
        assert!(s.contains("\"id\":\"Unknown New Model\""));
        assert!(s.contains("\"display_name\":\"Unknown New Model\""));
        assert!(!s.contains("price_per_hour"));
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
        let ids: Vec<&str> = catalog.iter().map(|e| e.id.as_str()).collect();
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
