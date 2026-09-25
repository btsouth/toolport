# Catalog curation review for the next minor release

Reviewed 2026-09-25. The curated catalog now has **55** entries: 19 fixed
hosted endpoints, 34 local stdio packages, and 2 self-hosted URL templates.
The [official MCP Registry](https://registry.modelcontextprotocol.io/) remains
available in Toolport for searching beyond this set. Registry publication and
GitHub stars alone do not establish that a server is maintained, safe to launch,
or useful without extra setup. There is no need to force the catalog to exactly
50 entries.

## Inclusion bar

A curated entry needs a publisher or well-established project, a primary source
that documents the exact endpoint or launch command, a working setup path in
Toolport, and a distinct useful purpose. A package must be available in its
registry; a deprecated package needs an explicit reason to retain it. Every
required argument and credential must be declared, and a credential-bearing
argument must use a vaulted launch input. A hosted server should use the
publisher's documented endpoint and authentication method. These checks verify
the preset shape, not live access to a user's provider account.

The [stdio audit](catalog-launch-setup.md) records the package, required inputs,
and status for all 34 local presets. Three existing presets are known legacy
exceptions: PostgreSQL and Slack use unsupported reference packages;
Elasticsearch's npm package is deprecated. Their documented tool sets have no
confirmed equivalent drop-in replacement, so this release preserves existing
saved entries and calls out the package status instead of silently substituting
different functionality. Browserbase's current package is published, but its
publisher's repository is archived. AWS's current API package has an announced
2027 retirement path. Revisit these before later releases.

## Additions from this review

| Entry                                                        | Verified setup                                                                                                                                                                                | Why it belongs                                                                                                      |
| ------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| [Postman](https://github.com/postmanlabs/postman-mcp-server) | Publisher's `https://mcp.postman.com/minimal` remote endpoint with OAuth. Minimal mode covers core workspace, collection, and environment work; users can edit the URL for Code or Full mode. | A common API workflow missing from the catalog, with a direct hosted setup that avoids a local package and API key. |
| [Redis](https://github.com/redis/mcp-redis)                  | Publisher's `uvx --from redis-mcp-server@latest redis-mcp-server --url <URL>` command; PyPI version 0.5.1. Toolport vaults the required URL because it can contain a password.                | A common database missing from the catalog, with a documented single-argument local setup.                          |

The hosted review found a **confirmed endpoint defect**: [Atlassian's current
setup guide](https://support.atlassian.com/atlassian-ai-gateway/docs/get-started-with-the-atlassian-remote-mcp-server/)
specifies v2, while the preset still used v1. The catalog now uses
`https://mcp.atlassian.com/v2/mcp?tools=all`, the publisher's documented
gateway option for exposing all tools in a flat `tools/list`. Only untouched
saved v1 catalog entries migrate; a customized URL or tool selection stays in
place. OAuth may ask the user to sign in again. The broken Atlassian and
Parallel Search documentation links were also updated to current publisher
pages. An unauthenticated HTTP request can establish that a hosted route
exists, but 401, 403, and 405 responses do not prove OAuth or tool calls work
with a real account. Stripe's documented root URL, for example, must not be
changed to `/mcp` just because a HEAD request to the root returns 404.
Provider-authenticated probes remain a release acceptance task when
credentials are available.

## Candidates held back

| Candidate                                                                        | Current upstream finding                                                                                                                                           | What would make it curatable                                                                      |
| -------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------- |
| [GitLab](https://docs.gitlab.com/user/model_context_protocol/mcp_server/)        | Official server is beta and disabled by default. Instance URL and administrator setup vary.                                                                        | Stable availability and a setup flow that can distinguish GitLab.com from self-managed instances. |
| [Azure MCP](https://github.com/microsoft/mcp/tree/main/servers/Azure.Mcp.Server) | Official `@azure/mcp` package is still a beta release; authentication relies on Azure CLI or another configured identity.                                          | Stable package and an explicit credential discovery/setup flow.                                   |
| [Serena](https://github.com/oraios/serena)                                       | Popular coding server, but upstream asks users to install and initialize its tool before client configuration and warns that copied marketplace commands go stale. | A reproducible installation and initialization path in Toolport.                                  |
| [Codebase Memory](https://github.com/DeusData/codebase-memory-mcp)               | Popular coding server, but distributed as a local binary/installer rather than a verified `npx` or `uvx` preset.                                                   | A trusted cross-platform executable path and versioned installation flow.                         |
| [Grafana](https://github.com/grafana/mcp-grafana)                                | Official server needs a Grafana URL and service-account token, and documents binary/Docker deployment.                                                             | A clear cross-platform launcher or self-hosted MCP URL template with tested authentication.       |

This is a candidate list, not a popularity ranking. Popular projects may be
applications, gateways, or installers instead of servers that Toolport can
honestly present as one-click presets. Before a minor release, run Toolport's
full verification and desktop Rust checks, test at least one vaulted argument
server and one OAuth remote with real provider accounts, and review the remaining
legacy package exceptions. Windows and Linux-native release builds need their
own platform validation; a Linux test run cannot establish Windows behavior.
