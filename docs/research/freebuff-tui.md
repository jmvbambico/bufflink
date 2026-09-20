# freebuff — what the bridge can rely on (v0.0.172, probed 2026-09-19)

Two probes: a Codex explorer (cancelled early; on-disk artefacts read
afterwards) and a full Cline explorer run in tmux under `script` (raw bytes).
Full probe report: `captures/probe-report-2026-09-19.md`; screen captures
usable as parser fixtures in `captures/`.

## Binary and options

- Launcher `/opt/homebrew/bin/freebuff` (npm, MIT) downloads and execs the
  real compiled Bun binary `~/.config/manicode/freebuff` (~90 MB).
- Options: positional `[prompt...]`, `--agent <id>`, `--cwd <dir>`,
  `--continue [conversation-id]`, `--lite` (`--free` deprecated), `--max`,
  `--plan`, `--clear-logs`, `login`.
- **No headless / JSON / print mode** (CodebuffAI/freebuff#947, open).
- Ignores `TERM=dumb`, `NO_COLOR=1`, `CI=1`: still starts, still emits 24-bit
  colour.

## Economy — read this before running anything

- Free tier: **25 Freebucks/day**, the default model `GLM 5.3 Flash` costs
  **5 Freebucks per hour of session** ("1h left" in the status bar).
- Accepting the model splash starts the hour. The probe's five launches
  (main run, three env-var checks, second instance) drained the day's 25 in
  ten minutes; from then on the splash showed
  `Not enough Freebucks — 5 Freebucks/hr against 0 left. Enter opens plans.`
  with `FREE · 0/25 Freebucks daily · resets in 2h 12m`.
- Consequence: **one freebuff process per bufflink process, kept alive across
  prompts**; the e2e gate launches freebuff **once** and sends every prompt to
  that instance; never launch freebuff for a smoke check.

## Lifecycle (verbatim strings a parser can key on)

| phase | screen | how the bridge proceeds |
|---|---|---|
| 0 launch | FREEBUFF block banner animates in | wait |
| 1 model splash (1–6 s) | `Start coding for free`, box with `› GLM 5.3 Flash · Deep reasoning · Reasoning: max · Images · NEW` / `5 Freebucks/hr`, `FREE · N/25 Freebucks daily · resets in …`, `↓  See all 4 models`, `⌘ Copy invite link  Open Earn ↵` | **Enter** accepts the highlighted (default) model — the same key a human presses. Waits indefinitely otherwise. |
| 1b Freebucks gate | inside the model box: `Not enough Freebucks — … Enter opens plans.` | **stop**: surface an ACP error; never press Enter (opens purchase). |
| 1c already running | `Freebuff is already running` / `Only one freebuff instance is allowed at a time.` / buttons `Take over` (default, highlighted) `Exit` | **stop**: choose `Exit` (→ arrow then Enter, or just report and kill) and surface an ACP error naming the owner pid from `freebuff-instance-owner.json`. Never take over. |
| 2 idle | `Freebuff will run commands on your behalf to help you build.`, `Directory <cwd>`, status `GLM 5.3 Flash · 59m left · 16.4K (2%)` + `✕ End session` on the right, bordered input box with `▍Enter a coding task or / for commands` | ready |
| 3 busy | status bar becomes `thinking... <N>s  ■ Esc` (or `working... <N>s  ■ Esc`); content shows `• Thinking` (or `▸ Thinking` collapsed), tool lines like `$ echo hello` / `• Create probe.txt`, then the reply, then `⌘ • <N>s • △▽`. The elapsed counter is absent in the first instant (` working...   ■ Esc`); after a large paste it can take >5 s to appear. | poll transcript on disk |
| 4 kicked out | `Another freebuff instance took over this account.` / `Only one CLI per account can be active at a time.` / `Close the other instance, then restart freebuff here.` / `Press Ctrl+C to exit.` | surface ACP error; Ctrl-C exits it |

Idle vs busy: busy **iff** the status bar matches `(thinking|working)\.\.\. \d+s`;
idle iff it matches `· \d+[hm] left` and the input placeholder is visible.

## Input handling

- Sent text echoes as `[HH:MM PM]` + prompt + `⌘`.
- **A trailing Enter inside the same burst as the text did not submit**; the
  text sat in the box with the cursor and a second, standalone Enter
  submitted. Bracketed paste is on (`ESC[?2004h`), so the bridge should:
  wrap the prompt in `ESC[200~ … ESC[201~`, wait for the box to show it,
  send CR separately, then verify the status bar went busy (re-send CR once
  if not). Newlines inside a paste are literal (multi-line prompt), not
  submit — not exercised live (Freebucks ran out), inferred from the paste
  mode.
- When a pasted prompt is longer than ~1 000 chars freebuff does **not** echo it
  inline in the input box; instead it shows a chip above the box reading
  `📋 Pasted text (N chars)` (N with thousands separators, e.g. `5,001`) and
  re-displays the box with its placeholder. Enter submits it — expected, to be
  confirmed live. (Observed at 5 001 chars; inline echo at ≤ 74.)
- Slash menu on `/`: `/help /diagnostics /interview /plan /review /queue /new
  /history /copy /export /feedback /bash /theme:toggle /byok /reasoning`;
  `/exit` and `/quit` work but are not listed. Escape does not close the
  menu; Backspace does.

## Cancel

- **Esc** while busy stops generation: screen shows `[response interrupted]`,
  box returns to idle. Ctrl-C does the same and does **not** exit the process.
- `log.jsonl` then has `Agent execution failed` with
  `{'error': {'name': 'Error', 'message': 'user-interrupt'}}` followed by
  `Main prompt finished` with `outputType: 'error'` (success turns end with
  `outputType: 'lastMessage'`).
- `chat-messages.json` keeps the partial AI message, truncated mid-word, with
  **no** `isComplete`/cancelled marker.

## Exit

- `/exit` (or `/quit`) + Enter: prints `To continue this session later, run:`
  / `freebuff --continue <chat-id>`, tears down (`ESC[?1049l`, mouse off,
  paste off, `ESC[?25h`, OSC 0/12/112 resets) and the process exits.
- Ctrl-D: no-op. Ctrl-C ×2 while idle: no exit. Single Ctrl-C: no exit.
- `freebuff-instance-owner.json` (`{instanceId, pid}`) is written when the
  splash is accepted, **never removed**; a stale pid does not block the next
  start. On SIGTERM the bridge should type `/exit` and give it ~3 s before
  killing the child (omnigent's SIGTERM→SIGKILL budget is 5 s).

## Approvals — there are none in this build

Three prompts (`echo`, `write_file`, `ls -la`) all executed **without any
approval UI**; the model's own reasoning said it saw no need to ask. No
`approval`/`permission` block or log marker exists. The bridge therefore
cannot gate tools; it reports them (below). `session/request_permission`
is out of scope until freebuff grows a permission gate.

## Structured transcript on disk — the output channel

Per project (keyed by cwd basename) and per chat:
`~/.config/manicode/projects/<cwd-basename>/chats/<ISO-timestamp>/`.
A new chat dir is created per launch (bare `--continue` too — it does **not**
resume; only `--continue <id>` does). Snapshot `chats/` before launch and
watch the newest dir created after it. The dir appears at the first prompt,
not at launch.

| file | content |
|---|---|
| `chat-messages.json` | JSON array (~1 MB: includes tool definitions). `variant:"user"` `{content}`; `variant:"ai"` `{blocks[], isComplete, completionTime, credits, metadata}`. Blocks: `{type:"text", textType:"reasoning"\|"text", content}` and `{type:"tool", toolCallId, toolName, input{…}, output:"…", agentId:"main-agent", includeToolCall, isCollapsed?}`. Observed toolNames: `run_terminal_command` (`input{command, process_type:"SYNC", timeout_seconds}`), `write_file` (`input{path, instructions, content}`). A leading `{type:"mode-divider", mode:"LITE"}` ai message opens the chat. |
| `chat-meta.json` | `{messageCount, firstPrompt, messagesSize, messagesMtimeMs}` — cheap change detector. |
| `run-state.json` | `{sessionState, traceSessionId, output, inference}` |
| `log.jsonl` | pino lines `{level, timestamp, pid, hostname, msg, data?}`. Turn: `[send-message] Sending message with sdk run config` → `Start agent <model> step N` → `End agent … step N` → **`Main prompt finished`** `{outputType: 'lastMessage' \| 'error'}`. Also `[ads] Web API returned error` noise. |

Also in `~/.config/manicode/`: `message-history.json`, `freebuff-instance-owner.json`,
`credentials.json` (never read by bufflink).

## Terminal protocol (from 610 KB of raw bytes)

- Alternate screen (`ESC[?1049h`), bracketed paste, mouse tracking
  `?1000h ?1002h ?1003h ?1006h`, synchronized output `?2026h/l` around every
  frame (929×), cursor hidden (`?25l` 930×), `ESC[>4;1m` modifyOtherKeys.
- **Queries sent** (kept running when unanswered, but a faithful driver should
  reply): `ESC[6n` cursor position (×3), `ESC[?u` kitty keyboard, `ESC[>0q`
  XTVERSION, `ESC]10;?` / `ESC]11;?` fg/bg colour, `ESC[14t` pixel size,
  `ESC[?<n>$p` mode reports (1004 1016 2004 2026 2027 2031), OSC 99 (kitty
  notification), OSC 1337 (iTerm2 capabilities).
- Minimal replies: `ESC[<row>;<col>R` for `6n`; `ESC[?0u` for `?u`;
  `ESC]11;rgb:0000/0000/0000 ESC\` and `ESC]10;rgb:ffff/ffff/ffff ESC\`;
  ignore the rest. Size 120×40 renders everything cleanly; 80×24 works too.

## Ads

One inline sponsored card (`│ Eon   Ad │ … eon.io ↗ │`) rendered between
thinking and reply on the third turn; needed no key and scrolled away. The
ads endpoint mostly errors (`[ads] Web API returned error`, 400). Never
dismiss ads except by waiting; ad text is not in `chat-messages.json`, so it
never reaches the ACP client.

## Timing

Launch → splash 1–6 s; Enter → idle < 2 s. Trivial turn (Enter →
`Main prompt finished`) 8–14 s. Cancel takes effect < 2 s.

## Probe protocol (reuse — costs 5 Freebucks per launch)

```
mkdir -p /tmp/bufflink-probe && cd /tmp/bufflink-probe && git init
tmux new-session -d -s probe -x 120 -y 40 'script -q /tmp/bufflink-probe/raw.log freebuff --cwd /tmp/bufflink-probe'
sleep 6; tmux send-keys -t probe Enter                      # accept model splash
tmux send-keys -t probe -l 'Reply with exactly the single word PONG.'; sleep 1; tmux send-keys -t probe Enter
tmux capture-pane -t probe -p -e -S -200
tail -f ~/.config/manicode/projects/bufflink-probe/chats/*/log.jsonl
tmux send-keys -t probe -l '/exit'; tmux send-keys -t probe Enter
```
Never dismiss ads except by waiting; never modify `~/.config/manicode`;
never choose `Take over`.

## Learned from live bridge runs (2026-09-20, three launches, blink e2e)

- `chat-messages.json` is written **once, at the end of the turn** (~970 KB
  with tool definitions), ~13 ms **after** `Main prompt finished` lands in
  `log.jsonl`. The bridge waits for the last AI message to show
  `isComplete: true` before its final flush (`BLINK_SETTLE_TIMEOUT_S`, 5 s).
  Consequence: nothing streams while freebuff is thinking; reasoning + reply
  arrive together at completion. Long turns therefore sit silent against
  omnigent's 300 s idle timer (`HARNESS_ACP_PROMPT_TIMEOUT_S`).
- The chat directory is created at **launch** (splash accept), before the
  first prompt; `chat-messages.json` appears only after the first turn.
- Typing `/exit` + Enter from the bridge did **not** exit freebuff within 3 s
  in any of the three runs (it did in the manual tmux probe). Suspect the
  slash-autocomplete popup swallows the first Enter. The bridge falls back to
  SIGTERM/SIGKILL on the child's **process group**: the npm launcher forwards
  no signals, and the real Bun binary survives SIGHUP from the closing PTY.
- Splash → Idle takes ~2.3 s; a trivial turn 3–7 s; Esc cancel is honoured
  in < 0.4 s and the next prompt works in the same session.
