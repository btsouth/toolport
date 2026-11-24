# @toolport/cli

**One setup for every coding agent.**

```sh
npx @toolport/cli          # check every agent on this machine
npx @toolport/cli usage    # see where your tokens go
npx @toolport/cli --fix    # preview and apply safe fixes
npx @toolport/cli init     # write a secret-free toolport.toml for this repo
npx @toolport/cli up       # set up every agent from toolport.toml
```

`toolport` finds Claude Code, Codex, Cursor, OpenCode and 40+ other agents, and shows
plaintext secrets in their configs, MCP servers that drift between them, broken skill
links, what each agent loads before the first prompt, and where tokens go in your real
sessions. It reads local files only and never prints a secret value.

Works with skills.sh: skills installed with `npx skills add` are part of every check.

This package holds a small launcher. npm installs the matching prebuilt binary for
macOS, Linux (glibc) or Windows from an optional dependency; there is no install script
and nothing is downloaded at run time. Set `TOOLPORT_CLI_BIN` to use a binary of your own.

Full guide: [docs/cli.md](https://github.com/btsouth/toolport/blob/main/docs/cli.md).
The free [Toolport desktop app](https://toolport.app/download) adds the MCP gateway,
sync across machines and the Skills page. MIT licensed.
