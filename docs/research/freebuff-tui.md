# freebuff — what the bridge can rely on (v0.0.172, probed 2026-09-19)

Partial: a Codex explorer probed freebuff in tmux (120x40) before its run was
cancelled; Hivemind read the on-disk artefacts afterwards. Items marked TODO
were not observed yet.

## Binary and options

- Launcher `/opt/homebrew/bin/freebuff` (npm, MIT) downloads and execs the
  real compiled Bun binary `~/.config/manicode/freebuff` (~90 MB).
- Options (from binary strings + `--help`): positional `[prompt...]` "Initial
  prompt to send to the agent", `--agent <id>`, `--cwd <dir>`,
  `--continue [conversation-id]`, `--lite` (`--free` deprecated alias),
  `--max`, `--plan`, `--clear-logs`, `login`.
- **No headless / JSON / print mode.** `--headless` and `--json` strings in
  the binary belong to the bundled browser agent and to `bun`, not to the CLI.
  This is issue CodebuffAI/freebuff#947, still open.

## Structured transcript on disk — the output channel

Per project (keyed by the cwd basename) and per chat, freebuff writes
`~/.config/manicode/projects/<cwd-basename>/chats/<ISO-timestamp>/`:

| file | content |
|---|---|
| `chat-messages.json` | JSON array of messages. `variant: "user"` with `content`; `variant: "ai"` with `blocks[]` — `{type:"text", textType:"reasoning"|"text", content}` (tool blocks TODO: not yet observed) — plus `isComplete: true`, `completionTime`, `credits`, `metadata`. A leading `{type:"mode-divider", mode:"LITE"}` ai message opens the chat. |
| `chat-meta.json` | `{messageCount, firstPrompt, messagesSize, messagesMtimeMs}` |
| `run-state.json` | `{sessionState, traceSessionId, output, inference}` |
| `log.jsonl` | pino-style lines `{level, timestamp, pid, hostname, msg, data?}`. Turn lifecycle: `[send-message] Sending message with sdk run config` → `Start agent <model> step N` → `End agent ... step N` → **`Main prompt finished`**. |

Observed reply for the probe prompt "Reply with exactly the single word
PONG": one reasoning block, one text block `PONG`, `isComplete: true`.
Model shown in the TUI header: `GLM 5.3 Flash · 1h left` (LITE mode).

**Consequence for bufflink:** the PTY is needed only to *drive* the TUI
(launch, type the prompt, answer approval prompts, cancel). Reply text,
reasoning and completion come from `chat-messages.json` + the
`Main prompt finished` log line — no screen scraping of assistant output.
A new chat directory appears per freebuff process (and on `--continue`?
TODO), so the bridge should snapshot the `chats/` listing before launch and
watch the newest directory created after it.

Also present: `~/.config/manicode/message-history.json` (prompt history),
`freebuff-instance-owner.json` (single-instance lock — behaviour with a second
process TODO), `credentials.json` (never read by bufflink).

## TUI shape (from tmux captures)

- Startup: large "FREEBUFF" block banner, then
  `Freebuff will run commands on your behalf to help you build.`,
  `Directory <cwd>`, a status line `<model> · <time> left`, then the input.
- Sent prompts echo as `[HH:MM PM]` + the prompt text followed by `⌘`.
- TODO (not captured before cancellation): exact idle-prompt glyph, approval
  prompt text and keys, ad/waiting-room rendering, Esc vs Ctrl-C cancel
  behaviour, clean exit command, 80x24 behaviour, multi-line paste.

## Probe protocol (reuse)

```
mkdir -p /tmp/bufflink-probe && cd /tmp/bufflink-probe && git init
tmux new-session -d -s probe -x 120 -y 40 'freebuff --cwd /tmp/bufflink-probe'
tmux send-keys -t probe 'Reply with exactly the single word PONG and nothing else.' Enter
tmux capture-pane -t probe -p -e -S -200      # screen with escapes
tail -f ~/.config/manicode/projects/bufflink-probe/chats/*/log.jsonl
tmux kill-session -t probe
```
Never dismiss ads except by waiting; never modify `~/.config/manicode`.
