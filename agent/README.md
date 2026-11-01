# RamWarden for AI agents

An [Agent Skill](https://agentskills.io) that lets a user's AI agent install
RamWarden, explain and free memory through the local daemon, read ThreatWarden,
and help the user subscribe or cancel. The skill is `skills/ramwarden/SKILL.md`; the
behaviour it promises is specified in
[`docs/specs/agent-skill.md`](../docs/specs/agent-skill.md).

## Install

Claude Code:

```
/plugin marketplace add ethanrxla/ramwarden
/plugin install ramwarden@ramwarden
```

Any agent that reads Agent Skills (Codex, Cursor, Gemini CLI, OpenCode and
others):

```
npx skills add ethanrxla/ramwarden
```

or copy `skills/ramwarden/` into the agent's skills folder (for Claude Code,
`~/.claude/skills/ramwarden/`).

The skill installs nothing by itself. It tells the agent how to install the
RamWarden app (with the user's yes, and after checking the download's
checksum), and how to talk to it on `127.0.0.1:7823`.

## Paying

RamWarden Lite is free (one device, fixed rules). RamWarden Plus is
$4/month or $20/year with a 14-day free trial, and RamWarden Pro (adds
ThreatWarden) $5/month or $24/year, charged when it starts. Both renew until
cancelled, and Plus upgrades to Pro in place. Payment is USDC on Base
(MetaMask, or Phantom's EVM account), or a card when offered. The agent never spends on its own: the user asks, the agent
says exactly what they are agreeing to (price, renewal, the one-year USDC
approval), and the user approves every signature and payment in their own
wallet. Cancelling is always one request away. See `skills/ramwarden/references/buying.md`.
