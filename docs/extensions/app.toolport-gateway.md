# `app.toolport/gateway` MCP extension

Version: `1.0.0`

Toolport advertises this third-party extension in the `extensions` object of its
modern `server/discover` capability response. The vendor prefix is the reverse of
the Toolport-owned `toolport.app` domain.

The extension introduces no new protocol methods. It describes Toolport features
that continue to use ordinary MCP tools, so clients that do not understand the
extension retain the same behavior.

## Server settings

```json
{
  "version": "1.0.0",
  "discoveryMode": "lazy",
  "codeMode": true,
  "agentControl": false,
  "destructiveConfirmation": false,
  "humanApproval": true
}
```

- `version` is the independent version of this extension settings schema.
- `discoveryMode` is `lazy`, `grouped`, or `full` for the requesting client.
- `codeMode` reports whether `toolport_run_script` is available.
- `agentControl` reports whether the server enable/disable tools are available.
- `destructiveConfirmation` reports whether destructive calls use the
  agent-facing `toolport_confirm` flow. It is `false` when human approval
  supersedes that flow.
- `humanApproval` reports whether the effective human approval gate is active,
  including a Teams-enforced gate.

Servers implementing this extension MUST return all six fields. Clients MUST NOT
treat any `true` value as permission to bypass Toolport policy; the ordinary tool
call remains the enforcement point. Clients SHOULD treat an unknown `version` as
unsupported and continue through core MCP discovery and tool calls.

## Graceful degradation

Toolport MUST keep its core meta-tools available according to the active settings
whether or not a client declares this extension. A client that ignores
`app.toolport/gateway` can therefore continue using `tools/list` and `tools/call`
without behavior changes.
