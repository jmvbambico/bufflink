# bufflink

An [Agent Client Protocol](https://agentclientprotocol.com) bridge for the
free [freebuff](https://freebuff.com/cli) coding agent, so omnigent (or any
ACP client) can drive it.

Status: **design / research**. See `AGENTS.md` and `docs/research/`.

Planned omnigent config:

```yaml
acp:
  agents:
    - { name: Freebuff, command: bufflink }
```
