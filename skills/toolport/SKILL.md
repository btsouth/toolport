---
name: toolport
description: Check coding agent configs, plaintext secrets, MCP drift, broken skill links, turn-0 token budgets, and offline session token usage with @toolport/cli, then preview and apply safe fixes or bootstrap a repo from toolport.toml.
---

# Toolport agent checkup and setup

Use `@toolport/cli` (or `toolport` if already installed on `PATH`) to audit and fix coding agent setups across Claude Code, Codex, Cursor, OpenCode, and other MCP clients. Everything runs locally and never prints secret values.

## Choosing the binary

Prefer `toolport` if `command -v toolport` points to the CLI or Linux GTK build (`toolport --version --json` succeeds). Otherwise run `npx --yes @toolport/cli`.

## Common workflows

### 1. Audit this machine and project

```sh
npx --yes @toolport/cli scan --json
```

Summarize the findings concisely for the user:

- Plaintext secrets on disk (setting name, server, agent, file path; never inspect or print secret values)
- Conflicting or duplicated MCP server definitions across agents
- Broken skill links or missing global rules links
- Turn-0 context budget (rules and skill listings loaded before the first prompt)

### 2. Preview and apply safe fixes

Always preview before modifying files:

```sh
npx --yes @toolport/cli fix --dry-run
```

Show the planned changes to the user and confirm which ones they want applied before running:

```sh
npx --yes @toolport/cli fix --yes
```

Use `--only skills`, `--only rules`, or `--only secrets` to scope the repair. Add `--connect` only when the user wants agents rewired to the Toolport gateway and plaintext copies removed from agent config files.

### 3. Profile local token and cache usage

```sh
npx --yes @toolport/cli usage --days 14 --json
```

Highlight what stays in context (startup floor, bash output, file reads, lingering screenshots, MCP calls), cache breaks, unused MCP servers, and bursty servers that fit Code Mode.

### 4. Bootstrap or share a repo setup (`toolport.toml`)

- Create a secret-free manifest from the current repo: `npx --yes @toolport/cli init`
- Preview setting up this repo from `toolport.toml`: `npx --yes @toolport/cli up --dry-run`
- Apply after confirmation: `npx --yes @toolport/cli up --yes`
