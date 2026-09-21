# bufflink Constitution

`bufflink` is a small Rust binary that makes the free `freebuff` CLI usable as
a coding agent inside omnigent by speaking the **Agent Client Protocol (ACP)**
on stdio while driving freebuff's interactive TUI in a pseudo-terminal behind
the scenes. This file is AUTHORITATIVE: where it and
`.agents/orchestration.yaml` disagree, this wins.

## Why it exists

freebuff (0.0.x) has no headless/JSON mode — see
https://github.com/CodebuffAI/freebuff/issues/947 — and omnigent has no
user-extensible native-TUI harness. Omnigent's generic `acp` harness, however,
runs any command that speaks ACP. bufflink is that command.

## Non-negotiables

- **Light.** One static binary, no runtime dependency on node, python or tmux.
  Keep the dependency tree small; justify every new crate in the PR body.
- **Drive the official CLI as a user would.** bufflink types into freebuff's
  TUI and reads what it shows. It never talks to freebuff's backend directly,
  never reuses its credentials, and never skips, hides or auto-dismisses ads
  or interstitials — it waits for them like a human would.
- **Never touch freebuff state you did not create.** `~/.config/manicode/**`
  is read-only to bufflink except through the freebuff process itself.
- **Fail loudly.** If the TUI shape changes and output cannot be parsed,
  surface a clear ACP error; never return an empty or fabricated reply.
- **Protocol fidelity over features.** A minimal, correct ACP agent
  (`initialize`, `session/new`, `session/prompt`, `session/cancel`,
  `session/update` chunks) beats a rich, flaky one.

## Development workflow

### Git flow

One long-lived branch, the same way `og` works:

```
feature/<slug> ──PR (gate green, cross-vendor review)──▶ main ──tag──▶ release
```

- **`main`** is the only long-lived branch and is protected: an agent never
  pushes to it or merges into it. Work reaches it only through a PR that the
  human merges. Every commit on `main` is releasable.
- **`feature/<slug>`** is one task, cut from `main`. **`integration/<goal>`**
  batches several tasks before one PR. Fixes found by review or by a live
  run go on the same feature branch before the PR merges.
- The deliverable of an orchestrated run is the PR into `main` with the full
  gate green and the reviewer's verdict in the PR body
  (`cross-vendor-review: passed` / `degraded-review`).

### Releases

Unlike `og`, whose version is `git describe`, `blink` reports the version in
`Cargo.toml` (`agentInfo.version` in `initialize`), so a release has a bump:

1. A `release/vX.Y.Z` branch bumps `Cargo.toml` + `Cargo.lock` and nothing
   else; it is a PR into `main` like any other (no review needed for the
   bump alone).
2. After the human merges it, the annotated tag `vX.Y.Z` is created on that
   `main` merge commit and pushed; the GitHub release is created from the
   tag with the release binary attached, built from that exact commit.

A tag that does not point at a commit on `main` is a mistake, not a release.
Agents may prepare the release branch and, once the bump is on `main`, tag,
build and publish; they never tag an unmerged commit.

### Branching

Task branches `feature/<slug>` are cut from `main`. The remote is GitHub
(`jmvbambico/bufflink`).

### Worktrees

One per task at `~/projects/.worktrees/bufflink/<slug>`; the integration
worktree at `~/projects/.worktrees/bufflink/integration-<goal>`.

### Gates

Workers run `fmt`, `clippy` and `unit` in their worktree. `e2e` (spawns the
real freebuff binary, which enforces a single running instance and shares
`~/.config/manicode`) runs once, on the integration branch only.

### Before hand-off

The integration branch has every task's commits, attributed; the full gate is
green; the reviewer (a different vendor from the implementer) has reported.
Say plainly in chat which worker implemented each task and what the reviewer
found.

## File placement

- `src/acp/` — ACP server: JSON-RPC framing, method dispatch, session state.
- `src/pty/` — spawning freebuff in a PTY, screen model, input injection.
- `src/freebuff/` — everything that knows what freebuff's TUI looks like
  (prompt detection, output extraction, approval prompts). Isolate the
  fragile knowledge here so a TUI change is a one-module fix.
- `tests/` — unit tests next to the module (`#[cfg(test)]`), fixture-driven
  parser tests in `tests/fixtures/`, real-freebuff smoke tests in
  `tests/e2e/` behind `#[ignore]` unless `BUFFLINK_E2E=1`.
- `docs/research/` — findings about freebuff's TUI and the ACP contract.
  Update when the TUI changes.

## Human checkpoints

Ask before any change to the ACP surface (method names, capability flags) or
to how freebuff is launched. Otherwise plan, dispatch and report.
