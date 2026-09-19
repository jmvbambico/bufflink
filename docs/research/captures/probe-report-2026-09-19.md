# freebuff TUI Probe Report — v0.0.172 (/opt/homebrew/bin/freebuff -> ../lib/node_modules/freebuff/index.js, macOS)

Probed interactively from a pseudo-terminal (tmux `probe`/`probe2` panes, 120x40 by
default, one run at 80x24). All runs under `script -q <log>` for raw byte capture.
Work dirs: `/tmp/bufflink-probe`, `/tmp/bufflink-probe2`. No git repo was modified.
Model in-use: `GLM 5.3 Flash` (z-ai/glm-5.3-flash), free tier.

> Scope note: the free-tier balance (25 Freebucks/day) was consumed mid-probe.
> From ~21:47 the app shows a `Not enough Freebucks` gate on the model-picker splash
> (see Q8 / Q9), so some tail-end interactions (multi-line submit, deny-flow re-checks)
> could not be exercised against a live input box. Everything up to Q6 used a working
> input box. Each tool decision was auto-approved in this build (see Q3).

---

## Q0 — Stale owner file (operator-requested extra)

- Operator left `~/.config/manicode/freebuff-instance-owner.json` with the **dead** pid
  `67436` (instance `ea1cf104-...`).
- **It did NOT block startup.** The app started normally (banner → model-picker splash).
- The owner file was **not replaced at process start**; it was still `{pid:67436}` at
  launch +2s and +6s while the splash was on screen.
- It was **replaced at the moment the splash was dismissed** (Enter accepted the
  highlighted model). At that point it became:
  ```json
  {"instanceId": "51719a96-9571-4205-b28a-5a58ebb2e232", "pid": 81348}
  ```
- Every subsequent fresh launch (post-splash) rewrote it with that run's pid.
- The file is **not removed on exit** (see Q5). At the end of the probe it still holds
  a dead pid (`88579`).

## Q1 — IDLE & BUSY UI (exact strings for a parser)

### First thing on screen (fresh launch, 120x40)
~1–2s after launch the **model-picker splash** shows (this is what the leftover
`probe2` was stuck on). Verbatim (colour glyphs shown as text):
```
                                                      ✕
   Start coding for free   2 day streak  ●●○○○○○

   ┌──────────────────────────────────────────────────────────────────────────────┐
   │ › GLM 5.3 Flash · Deep reasoning · Reasoning: max · Images · NEW             │
   │                               5 Freebucks/hr                                │
   └──────────────────────────────────────────────────────────────────────────────┘

   FREE · 20/25 Freebucks daily · resets in 2h 23m

   ↓  See all 4 models

   ✦ Refer friends → earn Freebucks:

   ⎘ Copy invite link  Open Earn ↵
```
- **Dismiss key: Enter** (accepts the highlighted `› GLM 5.3 Flash`). One Enter is
  enough on a normal start. (When Freebucks hit 0, Enter instead shows the gate line,
  see Q8.)
- Some banner glyphs are rendered from block chars; decorated with `─`/`┌`/`┐`
  box-drawing and 24-bit colour.
- After dismissal the idle screen:
```
  (FREEBUFF ASCII banner)
  Freebuff will run commands on your behalf to help you build.

  Directory /private/tmp/bufflink-probe

  GLM 5.3 Flash · 1h left                                              ✕ End session
  ╭──────────────────────────────────────────────────────────────────────╮
  │  ▍Enter a coding task or / for commands                            │   <- INPUT
  │                                                                    │
  ╰──────────────────────────────────────────────────────────────────────╯
```
- The input line is a bordered box. **Idle prompt text (exact):**
  `▍Enter a coding task or / for commands` (leading `▍` = blinking block cursor; dim
  placeholder when empty).
- **Top status bar:** `GLM 5.3 Flash · <n>m left` (left) and `✕ End session` (right).
  After use: `GLM 5.3 Flash · 59m left · 16.4K (2%)` (adds live token count + percent).
- Bottom hint row under each user message: `⎘  •  <seconds>s  •  △▽` (copy/duration/scroll).

### Busy (generating a reply)
- Status bar switches to a live spinner. Verbatim (reconstructed from raw bytes):
  `thinking... <Ns>  ■ Esc` (letters of "thinking..." re-coloured as they stream; elapsed
  seconds increment; `■ Esc` = cancel hint, a filled square).
- Content shows a `• Thinking` (or `▸ Thinking` when collapsed) block with italic dim
  reasoning, then tool blocks, then the reply, then `⎘ • <Ns> • △▽`.
- **Idle vs busy:** busy iff status bar shows `thinking... <N>s  ■ Esc` (or `working...`);
  idle iff `GLM 5.3 Flash · …left · …K (%)` plus `✕ End session`. Input box shows
## Q2 — Raw terminal protocol (from `/tmp/bufflink-probe/raw1.log`, 609639 bytes, full first working session; python byte-counts)

| Feature | Sequence | Count |
|---:|---:|---:|
| Alternate screen ON  | `ESC[?1049h` | 1 |
| Alternate screen OFF | `ESC[?1049l` | 3 (incl. exit teardowns) |
| Bracketed paste ON   | `ESC[?2004h` | 5 |
| Bracketed paste OFF  | `ESC[?2004l` | 4 |
| **Cursor-position query `ESC[6n`** | 3 |
| **XTVERSION `ESC[>0q`** | 1 |
| **Kitty keyboard `ESC[?u`** | 1 |
| **BG colour query `ESC]11;?` BEL** | 2 |
| **FG colour query `ESC]10;?` BEL** | 2 |
| **Window-px query `ESC[14t`** | 1 |
| Mouse `ESC[?1000h` / `?1002h` / `?1003h` / `?1006h` | 1 / 1 / 1 / 1 |
| Synced output ON  `ESC[?2026h` | 929 |
| Synced output OFF `ESC[?2026l` | 929 |
| Cursor hide/show `ESC[?25l`/`?25h` | 930 / 2 |
| modifyOtherKeys `ESC[>4;1m` | 1 |
| tmux user-define caps `ESC[?<n>$p` | 6 (`?1016$p,?2027$p,?2031$p,?1004$p,?2004$p,?2026$p`) |
| Kitty notif `ESC]99;i=...` + ST | 1 |
| iTerm2 `ESC]1337;Capabilities` + ST | 1 |

**Verdict:**
- **(a) alt screen**: YES (`ESC[?1049h` at startup, `?1049l` on teardown).
- **(b) bracketed paste**: YES (`ESC[?2004h`).
- **(c) device queries the DRIVER MUST answer**: YES — **`ESC[6n`** (×3), plus `ESC[>0q`,
  `ESC[?u`, `ESC]11;?`, `ESC]10;?`, `ESC[14t`, and the tmux `?…$p` probes. Confirmed:
  **zero `CSI <row>;<col>R` replies are present** — I did not answer them and the app kept
  running, but a robust bridge should answer `ESC[6n`, `ESC[?u`, and the OSC colour queries.
- **(d) mouse tracking**: YES — all four (`?1000h`,`1002h`,`1003h`,`1006h`) enabled at once.
- **(e) synchronized output**: YES — `ESC[?2026h/l` used 929× each (per-frame wraps).

**Env-var reactions** (each run separately after the main instance exited; only startup/splash
reachable because Freebucks were gone; none refused to start, none changed colour):
- `TERM=dumb` → still emits 24-bit colour (`ESC[38;2;255;255;255m`) and starts fine.
- `NO_COLOR=1` → still emits colour, starts fine.
- `CI=1` → still emits colour, starts fine.
So freebuff does its own colour detection and ignores these for palette decisions.

### Enter / multi-line behaviour observed (feeding an input box)
When text was injected with a trailing `Enter` (`tmux send-keys -t probe '...' Enter`),
freebuff kept the text **in the input box** (saw `…text + ▍`), and that trailing Enter was
effectively ignored; **a second, standalone `Enter` submitted the prompt.** A parser should
not assume one Enter submits; send Enter again if the text is still present with a cursor.
## Q3 — Approval prompts

**Finding: this build auto-approves every tool; NO approval prompt ever appeared.** I sent the
three exact requested prompts and watched closely:

1. `Run the shell command echo hello-from-probe and tell me its output.`
   - No approval UI. The model then ran it:
     ```
     $ echo hello-from-probe
     hello-from-probe
     ```
     Model reasoning: *"...completely harmless, read-only command with no side effects, so
     there's no need to ask for permission. I'll execute it directly."*
   - chat-messages.json block:
     ```json
     {
       "type": "tool",
       "toolCallId": "uhswuTmKRds",
       "toolName": "run_terminal_command",
       "input": {"command": "echo hello-from-probe", "process_type": "SYNC", "timeout_seconds": 30},
       "agentId": "main-agent",
       "includeToolCall": true,
       "output": "hello-from-probe\n",
       "isCollapsed": true
     }
     ```
2. `Create a file named probe.txt containing the single line hi`
   - No approval UI. UI showed a `• Create probe.txt` activity line, reply:
     `Created  probe.txt  containing the single line  hi .` (`probe.txt` = `hi\n`).
   - chat-messages.json block:
     ```json
     {
       "type": "tool",
       "toolCallId": "uhvZtQde16g",
       "toolName": "write_file",
       "input": {"path": "probe.txt", "instructions": "Create probe.txt containing a single line \"hi\"", "content": "hi\n"},
       "agentId": "main-agent",
       "includeToolCall": true,
       "output": "file: probe.txt\nmessage: Created file successfully."
     }
     ```
3. `Run the shell command ls -la` (the DENY test)
   - **No approval prompt appeared here either**; it ran `ls -la` directly and printed the
     listing. **There was nothing to deny** in this configuration, so I could not exercise a
     deny action (no y/n, no menu, no 1/2/3, no arrows, no "always allow" — no permission
     gate exists in this build).

**Denied-case JSON:** not obtainable — no deny pipeline exists. Full block-type inventory in the
working session's chat-messages.json: `mode-divider:1, text:9, tool:3`. No `approval`/`prompt`/
`command` block type in this version; `log.jsonl` had no `permission`/`approv`/`always allow`
markers either.

## Q4 — Cancel

### Esc
Sent `Count from 1 to 400, one number per line, with a short comment after each.`, waited ~5s
(statusbar `thinking... 4s  ■ Esc`), then `tmux send-keys -t probe Escape`.
- Generation **stopped**. Partial thinking ended with literal `[...] [response interrupted]`;
  input returned to `▍Enter a coding task…`.
- chat-messages.json: last AI message holds the **partial** chain truncated mid-word
  (*"Could be a test of whether I'll b…"*), **no** `isComplete`/`cancelled` marker.
- log.jsonl has the abort signature:
  ```
  <ts> | msg=Agent execution failed | {'error': {'name': 'Error', 'message': 'user-interrupt'}}
  <ts> | msg=Main prompt finished    | {'outputType': 'error'}
  ```
  So a `Main prompt finished` line still appears after Esc, but with `outputType=error`
  (vs `lastMessage` on success). A parser should key on `outputType == "error"` or the
  `user-interrupt` error line.

### Ctrl-C
Repeated the counting prompt, waited ~5s, then `tmux send-keys -t probe C-c`.
- Same as Esc: stopped, `[response interrupted]`, returned to idle, same `Agent execution
  failed`(`user-interrupt`) + `Main prompt finished`(`outputType=error`) pair.
- **A single Ctrl-C did NOT exit the process.** All freebuff/node/script PIDs stayed alive.
## Q5 — Exit

| Method | Behaviour |
|---|---|
| `/exit` | **Exits.** Prints `To continue this session later, run:` then `freebuff --continue 2026-09-19T14-37-47.705Z`, then teardown (`ESC[?1049l` alt-off, mouse `?1000l/1002l/1003l/1006l` off, `?2004l` paste-off, `?25h`, OSC `ESC]0;`, `ESC]12;default`, `ESC]112`). |
| `/quit` | **Exits** (typed `/quit` autocompletes, Enter exits as `/exit`). |
| Ctrl-D (empty input) | No-op, repeated; no exit. |
| Ctrl-C ×2 within 2s (idle input) | No exit (idle box unchanged; all PIDs alive). |
| Splash `✕` / `Open Earn ↵` | Not tested (would need paywall interaction). |

- Slash menu (typing `/`): `/help /diagnostics /interview /plan /review /queue /new /history
  /copy /export /feedback /bash /theme:toggle /byok /reasoning`. **Neither `/exit` nor
  `/quit` is listed**, but both autocomplete and work (`/exit` shows `Quit the CLI`).
- Escape does **not** close the slash menu; Backspace (clearing `/`) returns to the placeholder.
- **Exit status**: the pane process exited so the whole tmux session died
  (`no server running`), so `$?` could not be read from a wrapper; the clean teardown bytes
  are the evidence it exited normally.
- **Owner file after exit: left behind, not removed** (e.g. `{"instanceId":"f6d854ff-…",
  "pid":87545}` remained with a dead pid).

## Q6 — Second instance (single-instance enforcement)

Setup: `probe` (cwd /tmp/bufflink-probe) idle, owner=`6fe975f1-…`, pid 88366. Started `probe2`
(cwd /tmp/bufflink-probe2).

- **probe2 @5s / @15s / @30s**: it did **not** refuse outright and did **not** wait/bounce — it
  showed a **choice dialog** that persists until a key is pressed:
  ```
  Freebuff is already running
  Only one freebuff instance is allowed at a time.

              ┌───────────┐  ┌──────┐
              │ Take over │  │ Exit │
              └───────────┘  └──────┘
  ```
  Raw: `Freebuff is already running` (bright), `Only one freebuff instance is allowed at a
  time.`, two buttons: `Take over` (green, **highlighted = default**) and `Exit` (grey).
- **probe (first) while idle**: stayed idle (`▍Enter a coding task…`), not immediately kicked;
  the takeover is only triggered by choosing.
- **On Enter (Take over)**: probe2 entered full idle (`Directory /private/tmp/bufflink-probe2`,
  `▍Enter a coding task…`) and the owner file flipped to probe2
  (`{"instanceId":"152d3890-…","pid":88579}`).
- **probe (first) after takeover**: its live screen (upper rows) became:
  ```
  Another freebuff instance took over this account.
  Only one CLI per account can be active at a time.
  Close the other instance, then restart freebuff here.
  Press Ctrl+C to exit.
  ```
  (Bottom rows still showed stale idle-box scrollback, but the app was dead.)
- **Ctrl+C on the kicked-out instance exited its process** (node/freebuff died; only the tmux
  pane shell lingered, which I then cleaned up).
- `~/.config/manicode/projects/bufflink-probe2` had no chat dir until its first real prompt.
## Q7 — `--continue`

`freebuff --cwd /tmp/bufflink-probe --continue` (no id) in tmux `probe`:
- It did **NOT** resume the newest chat. It created a **brand-new chat dir** and landed on the
  **model-picker splash** (same as a fresh start).
  - Created `2026-09-19T14-47-02.931Z/` contained only a log.jsonl:
    ```
    [chat-runtime] Freebuff session over; holding queued messages until rejoin
    Could not read run state; restoring transcript without agent context      (ENOENT run-state.json)
    Could not read chat messages; restoring agent context without transcript (ENOENT chat-messages.json)
    No readable state files in chat directory
    ```
  - So bare `--continue` behaves like `--new`: fresh empty session, previous messages are
    **not** shown.
  - (The `--continue 2026-09-19T14-37-47.705Z` form printed on `/exit` is the intended way to
    resume a specific chat; bare `--continue` didn't pick the newest.)
- When the Freebucks gate later blocked the splash, `/exit`'s Enter was not processed, so I
  killed the tmux session I had created (no other instance to disturb).

## Q8 — Small terminal (80x24) & multi-line

- At 80x24 the model-picker splash renders with box-drawing narrowed to 80 columns (same layout).
- **Freebucks gate (paywall interstitial)** appeared here: with a 0/25 balance, Enter on the
  splash produced, inside the model box:
  ```
  │ Not enough Freebucks — 5 Freebucks/hr against 0 left. Enter opens plans. │
  ```
  with `FREE · 0/25 Freebucks daily · resets in 2h 12m` below. This is a **hours-long** gate
  (not a short countdown), so I did not bypass it and could not reach the input box to test
  multi-line submission live; I recorded it rather than dismissing it.
- Multi-line / bracketed-paste behaviour (inferred from Q2 raw logs + input handling): the
  device enables `ESC[?2004h`, and a trailing newline on injected input does **not** submit
  (text stays in the box with the cursor); an explicit second Enter submits. A bridge should
  treat pasted newlines as literal input (bracketed paste) and require an explicit Enter.

## Q9 — Ads / interstitial

- **One ad appeared once** (Q3, during the `ls -la` response, the 3rd tool turn). A rendered
  **sponsored card** between thinking and reply:
  ```
  ╭─────────────────────────────────────────────────────────────────────────────╮
  │ Eon                                   Ad                                   │
  │ Optimize your cloud infrastructure with Eon's cost-optimized storage layer and multi-region rate man… │
  │                                              eon.io ↗                       │
  ╰─────────────────────────────────────────────────────────────────────────────╯
  ```
- It included the literal word **`Ad`** in the header, required **no key** to dismiss, and
  scrolled with the response.
- The ads endpoint errored in logs (`[ads] Web API returned error`, gravity 400 "Invalid option:
  expected one of ios|freebuff_web_chat|chat_assistant|…"), which explains the single render.
- **Interstitial:** the only "waiting" surface is the Freebucks gate (Q8, hours-long) plus the
  one-shot model-picker splash at startup (Q1).

## Q10 — Timing

- **Startup → idle prompt**: ~1–6s to the model-picker splash; the splash waits **indefinitely
  for Enter**, after which idle is reached in <2s → ~3–8s total with a prompt Enter.
- **Enter → `Main prompt finished`** (trivial prompts, log timestamps):
  - `echo hello-from-probe` (4 steps): ~11s wall (11s badge).
  - file-create: ~8s wall.  `ls -la`: ~14s wall.
  - counting prompt (cancelled): <7s until `Agent execution failed`.
  Typical trivial turn: **~8–14s**.

## Cleanup performed
Killed only the sessions I created (`probe`, `probe2`) via tmux `kill-session` after clean
exits or when the Freebucks gate blocked `/exit`. End state: **no** freebuff process, **no**
tmux server. The stale owner file was left in place (untouched, per rule). `/tmp/bufflink-probe`
and `/tmp/bufflink-probe2` were not deleted.

## Files produced (in /tmp/bufflink-probe)
- Raw logs: `raw1.log` (609639 B, first working session), `raw-exit-tests.log`, `raw-q6.log`,
  `raw-q7.log`, `raw-dumb.log`, `raw-nocolor.log`, `raw-ci.log`.
- Captures: `launch-2s.txt, launch-6s.txt, launch-after-enter.txt, q3-approval-shell-6s.txt,
  q3-approval-shell-8s.txt, q3-file-10s.txt, q3-file-approval-20s.txt, q3-ls-2s.txt,
  q3-ls-3s.txt, q3-shell-done.txt, q3-ls-done.txt, q3-ls-now.txt, q4-count-5s-pre-esc.txt,
  q4-after-esc.txt, q4-after-ctrlc.txt, q5-slash-menu.txt, q6-5s.txt(probe2),
  q6-takeover.txt(probe2), q6-probe-after-takeover.txt, q7-8s.txt, q7-after-enter.txt,
  q7-final.txt, q8-idle-80x24.txt, q8-freebucks-gate.txt, q2-dumb-screen.txt`.
- Pre-existing from the earlier (halted) run: `leftover/probe-pane.txt`, `leftover/probe2-pane.txt`.
  `▍Enter a coding task…` whenever idle.