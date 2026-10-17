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
