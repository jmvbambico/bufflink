# 0227-synthetic-* screen fixtures

These fixtures are **SYNTHETIC** — hand-constructed from the render code inside
the installed freebuff 0.2.27 bundle, **not** captured from a live run. They
exist because real 0.2.27 screen captures were not yet available when the
v0.2.x picker classification was written.

They follow the bundle's own render rules:

- The toggle row is templated, not fixed:
  `↑  Show fewer` when expanded, otherwise `↓  See all <N> models`.
- The expanded list is SECTIONED; section labels render UPPERCASE as their own
  text rows, optionally followed by ` · <hint>`.
- Each model renders as its own single-border box: a focus marker (`›` when
  focused, a space otherwise) then the display name (BOLD when focused), an
  optional ` • <effort>` suffix, a tagline after padding or ` · `, and optional
  ` · NEW` / ` · TEST` badges. Price/status lines sit underneath, centre-padded,
  ` · ` separated (`N Freebucks/hr`, `Recommended`, `Price subject to change`, a
  locked label such as `Trial ended · Unlock to continue`, or an
  insufficient-balance warning such as `Not enough Freebucks`).
- The list sits in a scrollbox, so rows can be scrolled off-screen.

Files:

- `0227-synthetic-splash-collapsed-120x40.txt` — collapsed view, one box plus
  the counting toggle (`See all 12 models`).
- `0227-synthetic-splash-expanded-120x40.txt` — expanded view with the three
  base sections (UNLIMITED / OPTIMIZED / POWERFUL), a `Recommended` entry, and
  a model carrying an insufficient-balance warning.
- `0227-synthetic-splash-trial-120x40.txt` — a `LIMITED TRIAL` section with a
  locked model (`Trial ended · Unlock to continue`).
- `0227-synthetic-splash-scrolled-120x40.txt` — a mid-scroll frame: the
  sectioned list shows only 4 of the 13 advertised models while the counting
  toggle form is still rendered.
- `0227-synthetic-splash-focus-120x40.txt` — focus (`›`) on the second entry.
- `0227-synthetic-splash-narrow-80x24.txt` — 80x24 compacted rows (name and
  price on one row) plus a box cut off mid-render with no right border.

Replace these with real captures when a freebuff 0.2.x run is available.
