# Toolport agent plugin

Connect any [Agent Plugins 1.0](https://agent-plugins.org) client (VS Code,
GitHub Copilot CLI, the Copilot app, and other conformant agents), or Claude
Code, to your local [Toolport](https://toolport.app) gateway with one install.

The plugin bundles:

- **An MCP server entry** (`mcp.json` / `.mcp.json`) that launches the
  `toolport-gateway` binary already installed on this machine, wherever the
  Toolport app put it (see `bin/launch-gateway.mjs` for the search order).
- **A skill** (`skills/toolport/`) teaching the agent Toolport's
  search → call workflow, code mode, shaped results, and the approval flow.

Both manifest layouts ship in this one folder (`plugin.json` + `mcp.json` at
the root for Agent Plugins 1.0, `.claude-plugin/plugin.json` + `.mcp.json` for
Claude Code), so the same directory installs into either ecosystem.

## Requirements

- The [Toolport desktop app](https://toolport.app) installed and opened at
  least once (it publishes the gateway binary the plugin launches).
- Node.js on PATH (the launcher is a small Node script; the clients this
  plugin targets already run on Node or assume a dev machine).

## Install

Use this directory straight from a checkout, or download
`toolport-agent-plugin.zip` and unzip it. That asset is attached to every
release tagged after the packaging job shipped, so on older releases the
checkout is the only source.

- **VS Code / Copilot CLI / Copilot app:** follow your client's "install a
  local agent plugin" flow and point it at the unzipped `toolport/` folder.
- **Claude Code:** `claude plugin install` from a marketplace listing this
  repo, or add the folder as a local plugin.

Every client you install the plugin into shares the same gateway, servers,
credentials, and profiles. That's the point of Toolport.

### Don't install it twice into one client

The Toolport app writes MCP config directly for most of the clients above, VS
Code, Claude Code, and GitHub Copilot CLI included. If you install the plugin
into a client the app already connected, that client gets two
`toolport` server entries, spawns two gateway processes, and shows every
meta-tool twice. Pick one: either disconnect the client in the Toolport app's
Clients view before installing the plugin, or skip the plugin for that client
and let the app keep managing it.

## Configuration

None required here. Servers, credentials, profiles, discovery mode, and
security settings are all managed in the Toolport app and apply to every
connected client. Advanced per-client overrides (`TOOLPORT_PROFILE`,
`TOOLPORT_DISCOVERY`, …) can be set as `env` on the server entry in
`mcp.json`; see the [Toolport README](https://github.com/tsouth89/toolport#configuration).

To point the plugin at a non-standard gateway location, set the
`TOOLPORT_GATEWAY` environment variable to the binary's absolute path.
