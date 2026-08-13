---
name: toolport
description: Use when the user asks for any external action or data: email, payments, deployments, databases, repos, issues, files, web search, messaging, or any connected service. Toolport is the front door to every MCP server on this machine; search it before concluding a capability is unavailable.
---

# Working through Toolport

Toolport is a local gateway that aggregates every MCP server the user has set
up. Instead of hundreds of tool definitions, you see a few meta-tools and
discover the real tools on demand.

## Core workflow

1. **Search first.** For any external action, call `toolport_search_tools` with
   keywords describing the capability (`"list emails"`, `"create payment"`,
   `"recent deployments"`). If the service is connected, its tool is here, so do
   not tell the user a capability is unavailable until you have searched.
2. **Call it.** The first matching result includes its exact name and full input
   schema and is ready to use: call `toolport_call_tool` with that `name` and
   all parameters inside the `arguments` object. Don't keep searching for a
   better match, and never invent identifiers (teamId, projectId, and so on). Fetch
   them with a list/get tool on the same server first.
3. **Orient when needed.** `toolport_status` lists every connected server, its
   tool count, and the tokens Toolport has saved. Pass `server` to
   `toolport_search_tools` (with an empty `query`) to list one server's full
   tool set.

## Search tips

- Tool names are namespaced per server: `stripe__create_refund`.
- If the result says more tools matched than were shown, narrow with `server`
  or raise `limit` before concluding anything is missing.
- Many servers expose a generic API bridge (one write/create tool), so search
  by capability, not exact operation names.

## Multi-step work: `toolport_run_script`

When you already know the steps, run ONE JavaScript orchestration script
server-side instead of many round-trips: `servers.stripe.create_refund({...})`
(sync) or `.async({...})` with `Promise.all` to fan out. Intermediate results
stay full-sized inside the script; only your returned value is shaped for
context. Pass `validate: true` for a dry run that compiles the script and
returns the plan without executing. If a script fails partway,
`structuredContent.toolportScript.progress` lists which calls already ran
(their side effects are committed), so resume by index, not tool name.

## Results, approvals, and errors

- **Truncated results:** a `[Toolport shaped this result]` marker means the
  result was cut for context, not lost. Page the rest with
  `toolport_fetch_result` using the marker's `cursor`/`offset`, or pass
  `projection` (a dot path like `data.items.0.name`) to pull one field.
- **Destructive calls:** Toolport may intercept a destructive call and return a
  preview with a `token`. Confirm with `toolport_confirm` within 60 seconds to
  execute it unchanged, or a human approves it in the Toolport app. A denied
  call is a decision, not an error, so don't retry it verbatim.
- **Server management:** when the user has allowed agent control,
  `toolport_enable_server` / `toolport_disable_server` turn servers on or off
  by id or name (see `toolport_status` for the list).
- **Gateway not found:** if the Toolport server itself fails to start, the
  desktop app isn't installed. The user can get it at https://toolport.app.
