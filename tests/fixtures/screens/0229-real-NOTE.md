# 0229-real fixtures — REAL captures, not synthetic

These two frames are VERBATIM captures from a live freebuff **v0.2.29** run on
2026-10-17 (real-derived), unlike the `0227-synthetic-*` set which was
hand-built from the 0.2.27 bundle.

- `0229-real-startup-no-picker-120x40.txt` — the startup screen. freebuff 0.2.29
  has **no startup model picker**: it boots straight to the idle input box with
  a model already selected and `/model to change` in the status row. This is the
  screen the old `--list-models` waited on and timed out against every time.

- `0229-real-slash-model-picker-120x40.txt` — the picker opened by submitting
  the `/model` command. It renders as a full **scrollable** list: UPPERCASE
  section labels (`UNLIMITED` / `OPTIMIZED` / `POWERFUL`), one model per square
  box with a ` • <effort>` suffix and a ` · ` tagline, and a
  `↑↓ choose model · Tab reasoning · Enter select · Esc cancel` footer. It has
  **no `See all` / `Show fewer` toggle**, and it prints **no `Freebucks/hr`
  price** — the `UNLIMITED` section is the free tier (`price_per_hour: 0`).

Both were captured through `blink`'s own PTY screen model, so they are exactly
what `screen::classify` / `parse_catalog` / `model_rows` see (trailing padding
trimmed).

## Three consecutive SCROLL frames of the same picker

The picker is a tall scrollable list, so these three frames were taken one Down
press apart (same 2026-10-17 v0.2.29 run) and are walked together by
`enumerate::collect_frame`'s tests:

- `0229-real-picker-powerful-section-120x40.txt` — the frame where the
  `POWERFUL` header is still on screen.
- `0229-real-picker-wrapped-description-120x40.txt` — the next frame: the
  header has scrolled OFF the top, and a model box's wrapped DESCRIPTION row
  (`detail. Claude Haiku 5.5 · 100k context …`) is left-aligned at one space.
- `0229-real-picker-tail-120x40.txt` — the tail, whose models
  (`Gemini 3.8 Flash`, `GPT-6.1 Sol`) never had a header on screen.

## The slash-command autocomplete popup

`0229-real-slash-popup-120x40.txt` — captured right after typing `/model` and
BEFORE any Enter. The popup renders INSIDE the input box, above the typed line,
as `/command   Description` rows; the first Enter lands on it instead of the
command. This frame is what the repeat-Enter guard checks for.
