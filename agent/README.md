# RamWarden for AI agents

An [Agent Skill](https://agentskills.io) that lets a user's AI agent install
RamWarden, explain and free memory through the local daemon, read ThreatWarden,
and help the user buy a pass. The skill is `skills/ramwarden/SKILL.md`; the
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

RamWarden Pro is $4 for 2 months and ThreatWarden $6 for 2 months, in USDC on
Base or Solana. The agent never spends on its own: the user asks, the agent
shows the exact plan and price, and the user approves every signature and
payment in their own wallet. See `skills/ramwarden/references/buying.md`.
