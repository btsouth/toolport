# Upgrading from Toolport 1.x to 2.0

Toolport 2.0 keeps what you set up in 1.x. Your servers, credentials, clients,
approvals and settings carry over, and Toolport keeps behaving the way you had it
configured. Where 2.0 has a different default, Settings offers it as a choice
instead of switching you over.

The first 2.0 start upgrades `registry.json` in the Toolport data folder:
`~/.config/Toolport` on Linux, `~/Library/Application Support/Toolport` on macOS
and `%APPDATA%\Toolport` on Windows. Before it changes anything it saves your 1.x
file next to it as `registry.json.v1-<time>.bak`.

## What stays the same

- **Safety.** If you had no approval settings on, Safety is Off and nothing
  starts asking for approval. If you used human approval or destructive-call
  confirmation, Safety is Ask. Anything else you had on in 1.x stays on: holding
  calls from shared or registry servers, hiding destructive tools, pausing tools
  whose definitions change and blocking prompt injection. Settings lists these
  under "Kept from 1.x". Picking a level there, or **Use standard**, replaces them
  with that level's protections.
- **Code Mode** keeps your 1.x setting. New 2.0 installs start with it off.
- **Discovery.** A discovery mode you chose for all clients still applies to every
  client you have not set individually.
- **Environment.** In 1.x every local server received your whole environment.
  Servers you added in 1.x keep that, shown as **Use my shell environment** in the
  server editor. Servers you add in 2.0 start with only PATH, HOME and other
  basics plus the variables you set, and you can switch it on per server.
- **Always allow.** Approvals you saved in 1.x keep working for tools whose
  definitions have not changed.

## What changed

- **Tool search** returns 10 candidates by default. An agent can ask for up to 50.
  1.x allowed up to 200.
- **Tool results** are passed on exactly as the server sent them. 1.x wrapped
  results it suspected of prompt injection; with blocking on, 2.0 blocks them
  instead.
- **Long tool names** are shortened to fit each client's limit.

## Removed features

2.0 removes agent rules, agent permissions and the guard hook, agent activity
hooks, routines and agent control. If you used any of them, Toolport shows a
notice naming them after the upgrade.

Your settings for them are copied to `exports/` in the data folder:

- agent rules as `rules-<date>.md`
- routines as `routines-<date>.json` (the original `routines.json` stays)
- agent permission rules as `agent-permissions-<date>.json`

Files Toolport wrote into your clients for these features are left as they were.

If you need one of them, choose **I need this** in the notice. It opens a GitHub
issue with the feature names filled in; nothing is sent until you submit it.

## Go back to 1.24

You can go back at any time.

1. Quit Toolport and the AI clients that use it.
2. In the data folder, move `registry.json` aside and copy the newest
   `registry.json.v1-<time>.bak` to `registry.json`. This is your 1.x setup
   exactly as it was before the upgrade, so changes made in 2.0 are not included.
3. Install the latest 1.24 release from the
   [releases page](https://github.com/btsouth/toolport/releases).

On Arch and Omarchy, `/usr/share/toolport/toolport-preview-rollback.sh` does all
three steps.
