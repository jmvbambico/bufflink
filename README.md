# bufflink

An [Agent Client Protocol](https://agentclientprotocol.com) bridge for the
free [freebuff](https://freebuff.com/cli) coding agent, so omnigent (or any
ACP client) can drive it.

Status: **v1 working end to end** (one freebuff per `blink` process; prompt, cancel, clean exit). See `AGENTS.md` and `docs/research/`.

Planned omnigent config:

```yaml
acp:
  agents:
    - { name: Freebuff, command: blink, omnigent_mcp: false, inject_system_prompt: false }
```
