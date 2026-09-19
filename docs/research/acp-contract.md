# ACP contract — what bufflink must speak (researched 2026-09-19)

Sources: `agent-client-protocol` rust-sdk clone (Kilo explorer), omnigent
`inner/acp_executor.py` v-installed 2026-09-19 (Cursor explorer, every claim
`path:line`-cited; a first Kilo pass had errors and was re-verified), scratch
`cargo tree` measurements on this Mac.

## Crates

| crate | version | notes |
|---|---|---|
| `agent-client-protocol` | 2.2.0 | Apache-2.0, MSRV 1.88. Runtime-agnostic (`async-io`, `async-process`); examples use tokio. Builder API: `Agent.builder().on_receive_request(...).connect_to(Stdio::new())` (`src/role/acp.rs:286-339`, `examples/simple_agent.rs`). `ProtocolVersion::V1 = 1` stable; V2 behind `unstable_protocol_v2`. |
| `agent-client-protocol-schema` | 1.9.1 | published on crates.io; types only. |
| hello-world agent + tokio | — | `cargo tree --edges normal` = 197 lines, 2 dup crates (`syn` 2/3); release binary ≈ 421 KB. |

Alternative: hand-rolled newline-delimited JSON-RPC with `serde`+`serde_json`
(~200 lines). **Decision deferred to the plan** — the official crate buys
schema fidelity; the hand-rolled loop buys a ~10× smaller tree.

PTY / screen model (measured, tree lines incl. root):

| crate | ver | tree | verdict |
|---|---|---|---|
| `pty-process` | 0.5.3 | 7 (`rustix` only; **not** `portable-pty`) | **pick** — sized spawn, `resize`/`tcsetwinsize`, std/tokio `Child` for pid/wait, no `unsafe` on our side |
| `portable-pty` | 0.9.0 | 26 | only if Windows/conpty needed |
| `nix` (term,process) | 0.31.3 | 5 | `forkpty` is `unsafe fn`; DIY wait/resize |
| `rustix-openpty` | 0.2.0 | 9 | openpty only, no spawn |
| `vt100` | 0.16.2 | 7 (`vte`, `unicode-width`, `itoa`) | **pick** — `Parser::screen().contents()` cell grid + alternate-screen tracking |
| `vte` | 0.15.0 | 4 | parser only, no grid (Rust crate; no glib) |
| `alacritty_terminal` | 0.26.0 | 56 | heavy |
| `termwiz` | 0.23.3 | 155 | heavy |

## Protocol facts (schema 1.9.1)

- Framing: newline-delimited JSON-RPC 2.0 on stdio; **stderr is free** for logs.
- `initialize` → `{protocolVersion, agentCapabilities{loadSession, promptCapabilities{image,...}, mcp{...}}}`.
- `session/new` params `{cwd, mcpServers[], sessionId?, model?}` → `{sessionId}`.
- `session/prompt` params `{sessionId, prompt: ContentBlock[]}` → `{stopReason, usage?}`;
  `stopReason ∈ end_turn | max_tokens | max_turn_requests | refusal | cancelled`.
- `session/cancel` is a **notification**; the in-flight `session/prompt` must
  still be answered, with `stopReason: cancelled`.
- `session/update` notification, `sessionUpdate ∈ agent_message_chunk |
  agent_thought_chunk | tool_call | tool_call_update | plan | plan_update |
  session_info_update | current_mode_update | config_option_update |
  available_commands_update | usage_update | user_message_chunk`.
- `session/request_permission` (agent→client request) `{sessionId, toolCall,
  options[{optionId, name, kind ∈ allow_once|allow_always|reject_once|reject_always}]}`
  → `{outcome: {outcome: "selected", optionId} | {outcome: "cancelled"}}`.

## How omnigent drives an ACP agent (`inner/acp_executor.py`)

| aspect | behaviour | where |
|---|---|---|
| spawn | `shlex.split(command)` → `create_subprocess_exec`, never a shell; `cwd` = workspace | :177-179, :417-428 |
| env | deny-by-default allowlist: `HOME PATH TERM TMPDIR SSH_AUTH_SOCK OMNIGENT`, prefixes `HTTP_ HTTPS_ NO_PROXY SSL_ XDG_ LANG LC_`, plus `env_passthrough` names. `NO_COLOR` **not** forwarded. | `agent_env.py:36-108` |
| initialize | `protocolVersion: 1`, `clientCapabilities{fs{readTextFile:true,writeTextFile:true}, terminal:false}`; 30 s timeout. Reads only `agentCapabilities.promptCapabilities.image`; never checks the agent's version; `loadSession` ignored. | :661-688 |
| session/new | `{cwd, mcpServers}` — `[]` when `omnigent_mcp: false`; `model` only if `send_model: true`. `session/load` never sent. | :701-712 |
| model | `send_model` → `session/new.model`; else `session/set_config_option {configId:"model"}` **only if** the agent advertised it via `config_option_update`. No env var. | :711, :1373-1406 |
| prompt | `{sessionId, prompt:[{type:"text",text}]}`; `inject_system_prompt: true` (default) prepends omnigent's system prompt into the text → **prompts are long and multi-line**. | :1518-1544 |
| turn end | the JSON-RPC **response** to `session/prompt`; `stopReason` never read. `usage{inputTokens,outputTokens,totalTokens,cachedReadTokens}` mapped if present. | :1556-1575, :1183-1212 |
| idle timeout | 300 s (`HARNESS_ACP_PROMPT_TIMEOUT_S`), **reset by every inbound message** — stream something during long thinking. | :145-157, :1547 |
| updates | `agent_message_chunk`→text, `agent_thought_chunk`→reasoning, `tool_call` (`title`,`kind`,`rawInput`,`toolCallId`), `tool_call_update` (`status` completed/failed, `content`/`rawOutput`), `usage_update.size`, `config_option_update`. `plan`, `available_commands_update`, unknown kinds silently ignored. Only `content.text` is read. | :1247-1341 |
| permission | policy → elicitation; picks `allow_once` over `allow_always`, `reject_once` over `reject_always`; falls back to `{outcome:"cancelled"}` if no matching option. Names the tool from `toolCall.title` → `kind`; shows `rawInput`. | :974-1074 |
| fs/terminal | serves `fs/read_text_file`, `fs/write_text_file`; `terminal/*` → `-32601`. | :789-865 |
| cancel | sends `session/cancel` and returns — **no wait, no signal**. | :1601-1621 |
| close | close stdin → `terminate_tree` (SIGTERM, 5 s) → `kill_tree` (SIGKILL). stderr drained at debug level, last 20 lines kept. | :1626-1658 |
| lifetime | **one agent process and one session reused across turns** for the conversation. | :1458-1459, :698 |
| config | `acp.agents[]`: `name`, `command` (required), `model`, `session_id_mode` (server/client), `send_model` (false), `omnigent_mcp` (true), `inject_system_prompt` (true), `env_passthrough` ([]). No `env`/`cwd`/`args` keys. | `onboarding/acp_auth.py:127-184` |

### Consequences for bufflink

1. One bufflink process ⇒ one freebuff process ⇒ one chat; keep it alive
   across prompts. `session/new` is called once; a second `session/new` in the
   same process is unexpected but must not spawn a second freebuff.
2. Must exit freebuff cleanly on **stdin EOF and SIGTERM** (5 s budget) — a
   leftover freebuff holds the single-instance lock for every later run.
3. Answer the pending prompt with `cancelled` after `session/cancel`; nothing
   else is waited for, so also stop the TUI generation (Esc — see the TUI doc).
4. Emit `agent_thought_chunk`s from `chat-messages.json` reasoning blocks as
   they appear so the 300 s idle timer keeps resetting.
5. Approval prompts in the TUI → `session/request_permission` with
   `allow_once` + `reject_once` options only (never `allow_always`: the
   constitution forbids auto-approval). Fill `title` with the command / path.
6. Advertise `promptCapabilities.image: false`, `loadSession: false`, no
   `config_option_update` (model is chosen by freebuff's own splash /
   `--lite`; bufflink does not switch it).
7. Recommended `~/.omnigent/config.yaml` entry:
   `{name: Freebuff, command: blink, omnigent_mcp: false, inject_system_prompt: false}`
   — whether the injected system prompt survives freebuff's input box is a TUI
   question (multi-line paste, Q8 of the probe).
8. `TERM` is forwarded; bufflink sets its own env for the PTY child
   (`TERM=xterm-256color`, size 120×40) regardless.

## Precedent

- Zed's Claude adapter now lives at
  `github.com/agentclientprotocol/claude-agent-acp` (old URL 301s there); TS.
- gemini-cli ACP mode: TypeScript, `packages/cli/src/acp/*.ts`.
- Both answer `session/cancel` with `stopReason: cancelled` and forward tool
  approvals as `session/request_permission`.
