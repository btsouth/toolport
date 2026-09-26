# PR #960 release review

Reviewed 2026-09-25 against `origin/main` (`9c1256d`) and PR head
`31afa0f12c9d1d7e5457e0cc7826cb7d1ac7afaa`. The checkout was clean before review.
Read `AGENTS.md`, fetched main and the live PR details, and independently ran
verification. Existing CI results were for the original head, not the local fixes.
No installed Toolport registry or keychain was used. The initial review made no
commit, push, release, PR comment, or other publication. The user subsequently
approved proceeding with the reviewed fixes and fresh CI.

## Findings, highest severity first

Line references in this section refer to the original PR head unless stated
otherwise. The regression tests and fixes are in the working diff.

1. **P1: legacy team prefix matching transfers vault identity and consent.**
   `src-tauri/src/teams.rs:1797` accepted any old ID beginning with the new base
   plus a hyphen. With an enabled legacy `team_db-prod` or `team_db-2`, syncing
   original ID `db` with the same display name and command adopted that old ID
   and its standing consent. The regression failed on ID equality before the
   fix. Matching now accepts only the exact legacy base; ambiguous suffixes get
   a fresh identity and require setup. This defect was introduced by the PR.
   Reproduce with the `legacy_prefix_match_cannot_transfer_another_entries_vault_id_or_consent`
   unit test. Failure evidence: `.verify/release-review/legacy-prefix-before.log`.

2. **P1: leaving a team makes its retained vault namespace available to another
   team.** `src-tauri/src/teams.rs:1805` assigned the unsuffixed `team_shared`
   whenever there was no current row collision. Removing team A leaves its
   secrets in the vault; team B's original ID `shared` then received that same
   local ID. This behavior also existed on main. The new team identity hash is
   now used for every new allocation, while identified existing rows retain
   their IDs. Unit reproduction:
   `leaving_a_team_does_not_give_its_vault_namespace_to_the_next_team`.
   Failure evidence: `.verify/release-review/team-rejoin-before.log`. The new
   integration test also stores real disposable vault values and verifies the
   next team cannot resolve them.

3. **P1: changing a public team URL keeps the old token enabled at the new
   destination.** The `TeamClass::Ready` branch in `teams.rs` unconditionally
   appended an existing ID to `auto_enable`; the remote transport obtains its
   auth token by server ID (`remote.rs:1052`). Syncing `https://1.2.3.4/mcp` to
   `https://1.2.3.5/mcp` kept the same vaulted token and enablement. The test
   exercises state only and sends no request to those addresses. This behavior
   also existed on main. Existing remotes now use the same definition-based
   consent check as local commands. A second sync cannot re-enable them before
   consent. New allocations also include the initial definition so removing
   and re-adding a changed destination gets a different vault namespace.
   Reproduction: integration test
   `changed_team_remote_url_cannot_send_a_stored_token_without_new_consent`;
   `.verify/release-review/team-url-before.log` records the original failure.
   Follow-up hardening persists the review requirement so Enable all, ordinary
   enable requests and the playground cannot bypass it after reload or another
   sync. The expanded regression failed before this correction
   (`team-consent-gates-before.log`). OAuth client IDs, scopes and auth methods
   now participate in consent too, with coverage through actual team sync.
   React and GTK confirmation text describes remote addresses and saved auth.

4. **P2: sharing and team export corrupt bound arguments after secret flags.**
   `sharing_controller.rs:50` and `teams.rs:1663` replaced the marker in
   `--token <launch-input>` with `<redacted>`, leaving a binding that validation
   rejected on import/sync. Both exporters now retain the marker while still
   removing literal secret values. Reproduce with the two
   `*_preserves_bound_secret_flag_markers` tests. Both failed before the fix:
   `.verify/release-review/bound-export-before.log`.

5. **P2: GTK Test Connection uses a cleared field's previous value.**
   `linux_native/mod.rs:10426` applied an edited launch value only when nonempty.
   Clearing a saved directory therefore tested the old directory, although
   saving would clear it. The probe now clears nonsecret values and retains the
   documented blank-secret vault fallback. Reproduction:
   `cleared_launch_field_is_missing_in_native_probe_but_blank_secret_keeps_vault`;
   `.verify/release-review/gtk-clear-before.log`.

6. **P2: two curated remote setups do not match publisher requirements.**
   `catalog.rs:286` offered Asana's old URL. Its current
   [integration guide](https://developers.asana.com/docs/integrating-with-asanas-mcp-server)
   requires a preregistered OAuth authorization-code client and documents
   `/v2/mcp`. Live Asana authorization metadata advertises neither CIMD nor
   DCR; Toolport's `oauth.rs:402` supports those two registration paths only.
   Client-credentials auth is a different grant and does not bridge the gap.
   Asana is held out of new curated additions, with saved entries preserved.
   `catalog.rs:314` supplied Langfuse's incorrect `/mcp` example; the publisher
   documents [`/api/public/mcp` with Basic auth](https://langfuse.com/docs/api-and-data-platform/features/mcp-server).
   Corrected the hint, description, setup guidance and documentation link.
   A local fixture verifies Toolport preserves Basic headers; it does not prove
   a connection to a real Langfuse instance. These preset defects predate the PR.

7. **P3: stale publisher links and an unverified Postiz host.** Postiz now uses
   the [documented bearer endpoint](https://docs.postiz.com/mcp/setup),
   `https://mcp.postiz.com/mcp`. Both it and the old `api.postiz.com/mcp` return
   unauthenticated 401s; that does not prove the old route is broken or that the
   routes are equivalent with credentials. No saved URL is migrated across
   hosts. OpenRouter's 404 documentation link now points to its
   [publisher announcement](https://openrouter.ai/blog/announcements/openrouter-mcp-server/).

## End-to-end evidence

`src-tauri/tests/catalog_launch.rs` and
`fixtures/catalog-launch-server.mjs` exercise the actual catalog definitions,
controllers, encrypted vault, registry reload, transport, sharing/import, team
sync, enablement, and HTTP gateway. The fake stdio servers reject incorrect
Twilio compound credentials, PostgreSQL URLs, Filesystem directories and Redis
argv. Whitespace, punctuation and percent-encoding remain literal, without
shell or ambient-variable substitution. AWS and Qdrant fixtures reject unwanted
optional credentials. The fixture is explicitly not a real provider.

The tests cover incomplete enablement, required environment values, a corrupt
vault, conflicting ambient secrets, secret-free registry/backups/exports,
redaction of a failing child's escaped/truncated output, prewarm argument
resolution and child initialization, gateway discovery and tool calls, colliding
team IDs in reversed order, definition changes requiring consent, and legacy
Twilio migration with existing vaulted keys and backup idempotence. Customized
legacy entries remain untouched. A loopback HTTP fixture checks Basic auth and
OAuth refresh, resource binding, refresh-token rotation and reuse by the gateway.
It seeds OAuth state and does not simulate a provider's browser consent screen.

React launch/setup tests run in the full frontend suite. The GTK regression
tests the same value merge helper called by its Test Connection callback.
Desktop prewarming remains guarded against team entries needing consent and
uses the shared resolver. Resolving valid arguments and handshaking a fixture
does not prove every package can be downloaded on every platform.

All 34 local preset package records were fetched from npm/PyPI and compared with
publisher instructions, executables and environment variables. The existing
[stdio audit](catalog-launch-setup.md) records the names and sources. Published
source was also inspected for the Cloudflare wrapper's `CLOUDFLARE_API_TOKEN`
and Exa's `EXA_API_KEY`. Evidence snapshots are under
`.verify/release-review/packages.json` and adjacent publisher text files.
PostgreSQL and Slack remain unsupported reference packages; Elasticsearch is
deprecated; Browserbase's repository is archived. These are existing exceptions,
not evidence of authenticated operation or suitable drop-in replacements.

The official registry's
[stable API](https://github.com/modelcontextprotocol/registry/blob/main/docs/reference/api/official-registry-api.md)
supports the PR's `/v0.1/servers?limit=50&version=latest&search=github` request.
Live raw responses contained 50/50 latest entries with that parameter, compared
with 16/50 latest entries without it. Toolport mapped 47 launchable entries.
Snapshots: `registry-latest.json` and `registry-all.json` in the review evidence
directory. The query change is correct.

Live credential-free Toolport probes initialized and listed tools from
Microsoft Learn (3), DeepWiki (3), Cloudflare Docs (2), Parallel Search (2), and
Context7 (2). No provider tool was invoked. A separate opt-in test downloaded
and launched the real `@modelcontextprotocol/server-filesystem` package, with
isolated npm configuration/cache and a disposable allowed directory: 14 tools.
These live results prove initialization and discovery at review time only.

Hosted endpoint sources checked during review:

| Preset          | Publisher source                                                                                                              | Evidence boundary                                                     |
| --------------- | ----------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| Stripe          | [MCP guide](https://docs.stripe.com/mcp)                                                                                      | Documented root endpoint; real auth untested                          |
| GitHub          | [Publisher repository](https://github.com/github/github-mcp-server)                                                           | Authenticated initialize/tools/list: 45 tools; no provider operations |
| Vercel          | [Vercel MCP](https://vercel.com/docs/mcp/vercel-mcp)                                                                          | Documented hosted URL; real auth untested                             |
| Sentry          | [Hosted server](https://mcp.sentry.dev/)                                                                                      | Documented `/mcp`; real auth untested                                 |
| Cloudflare Docs | [Cloudflare servers](https://developers.cloudflare.com/agents/model-context-protocol/mcp-servers-for-cloudflare/)             | Live anonymous initialize and tools/list                              |
| Supabase        | [MCP guide](https://supabase.com/docs/guides/getting-started/mcp)                                                             | Documented hosted URL; real auth untested                             |
| Neon            | [MCP guide](https://neon.tech/docs/ai/neon-mcp-server)                                                                        | Documented hosted URL; real auth untested                             |
| Notion          | [Setup guide](https://developers.notion.com/guides/mcp/get-started-with-mcp)                                                  | Documented hosted URL; real auth untested                             |
| Postman         | [Publisher repository](https://github.com/postmanlabs/postman-mcp-server)                                                     | Documented `/minimal`; real auth untested                             |
| Composio        | [Connect guide](https://docs.composio.dev/docs/composio-connect)                                                              | Documented hosted URL; real auth untested                             |
| Linear          | [MCP guide](https://linear.app/docs/mcp)                                                                                      | Public documentation only; no private Linear access                   |
| Atlassian       | [Remote MCP guide](https://support.atlassian.com/atlassian-ai-gateway/docs/get-started-with-the-atlassian-remote-mcp-server/) | Documented v2 gateway URL; real auth untested                         |
| Postiz          | [Setup guide](https://docs.postiz.com/mcp/setup)                                                                              | Documented bearer route; real key untested                            |
| Context7        | [Publisher repository](https://github.com/upstash/context7)                                                                   | Live anonymous initialize and tools/list                              |
| DeepWiki        | [MCP guide](https://docs.devin.ai/work-with-devin/deepwiki-mcp)                                                               | Live anonymous initialize and tools/list                              |
| Microsoft Learn | [MCP guide](https://learn.microsoft.com/en-us/training/support/mcp)                                                           | Live anonymous initialize and tools/list                              |
| Hugging Face    | [MCP guide](https://huggingface.co/docs/hub/en/hf-mcp-server)                                                                 | Documented hosted URL; configured account access untested             |
| OpenRouter      | [Publisher announcement](https://openrouter.ai/blog/announcements/openrouter-mcp-server/)                                     | Documented hosted URL; real auth untested                             |
| Parallel Search | [Search MCP guide](https://docs.parallel.ai/integrations/mcp/search-mcp)                                                      | Live anonymous initialize and tools/list                              |
| n8n             | [Client examples](https://docs.n8n.io/connect/connect-to-n8n-mcp-server/mcp-client-examples/)                                 | Documented self-hosted path; real instance untested                   |
| Langfuse        | [MCP guide](https://langfuse.com/docs/api-and-data-platform/features/mcp-server)                                              | Documented self-hosted path and Basic header; local auth fixture only |

## Verification record

The original head passed `npm run doctor` and all nine stages of `npm run verify`
before changes. Its full summary is
`.verify/run-1790386607477-1239380/summary.json`.

The deliberately failing regression runs above established defects independently
of old green CI. Two review-harness problems were corrected: directly decoding
an export as a Registry instead of importing it, and assigning the same npmrc
file to user and global config. One interim full run stopped at lint because
downloaded publisher JavaScript in `.verify/` was linted; those evidence files
are now stored as text. Another stopped at the newly added team URL regression
before its fix. Neither partial run is a full pass.

Verification results for the final release-candidate code:

| Exact check                                                                                                                    | Result and evidence                                                                                                                                                                                                                                                                                                         |
| ------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `npm run doctor`                                                                                                               | PASS; `.verify/release-review/doctor.log`                                                                                                                                                                                                                                                                                   |
| `npm run verify`                                                                                                               | PASS, all nine stages; `.verify/run-1790389415503-1355804/summary.json`. Frontend: 60 files, 678 tests. Headless library: 1,442 passed, one native-keychain test ignored. Gateway unit tests: 376 passed. All integration targets and both smoke checks passed. The two opt-in live tests were separately run successfully. |
| `npm run test:rust`                                                                                                            | PASS with default desktop features, library/binaries/integration targets; `.verify/release-review/desktop-ready-final.log`                                                                                                                                                                                                  |
| `cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --features gtk-desktop --bin toolport-gtk`             | PASS; `.verify/release-review/gtk-ready-check.log`                                                                                                                                                                                                                                                                          |
| `cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features gtk-desktop --lib linux_native::`            | PASS, 81 tests; `.verify/release-review/gtk-ready-tests.log`                                                                                                                                                                                                                                                                |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --no-default-features --lib --bins`                                         | PASS with existing warnings; `.verify/release-review/clippy-ready.log`. No claim of warning-free compilation.                                                                                                                                                                                                               |
| `cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --test catalog_launch -- --include-ignored --nocapture` | PASS, all seven tests, including both live tests; `.verify/release-review/catalog-ready-final.log`. This final run also strips all inherited Toolport/Conduit environment overrides and restores them afterward.                                                                                                            |
| `npm run audit:prod`                                                                                                           | PASS, zero vulnerabilities; `.verify/release-review/audit-prod.log`                                                                                                                                                                                                                                                         |
| `bash scripts/install.Tests.bash`                                                                                              | PASS, 34 tests; `.verify/release-review/install-bash.log`                                                                                                                                                                                                                                                                   |
| `cargo check --manifest-path src-tauri/Cargo.toml --target x86_64-pc-windows-msvc --no-default-features --lib --bins`          | BLOCKED/FAIL (exit 101), `ring` build needs MSVC `lib.exe`, absent on this Linux host; `.verify/release-review/windows-cross-check.log`. This is an environment limit, not a successful Windows check or a demonstrated Toolport source error.                                                                              |

Production startup bundle: 557,226 raw JS bytes and 172,596 gzip bytes, within
the existing budgets. This is a static bundle measurement, not native desktop
performance evidence. No performance claim is inferred from browser fixtures.
GTK 4.14.5, libadwaita 1.5.0 and WebKitGTK 2.52.6 were available locally.
Windows Pester tests could not run because PowerShell is unavailable; macOS
execution and signed installers are unavailable here. The existing native
keychain integration test remains ignored on this headless Linux environment.

After the follow-up consent fixes, both full verification and desktop Rust
tests were rerun successfully. The intermediate runs caught an existing error
message assertion; the final message preserves its Teams-after-review guidance.
GTK and Clippy also passed with the persisted review requirement. Only report
and changelog text changed after the successful full runs.

## Release boundary

Local release artifacts were built without installation or publication:

- `npm exec tauri build -- --ci --no-sign --bundles deb --config src-tauri/tauri.bundle.conf.json --config '{"bundle":{"createUpdaterArtifacts":false}}'`
  passed. The final `.deb` reports version 1.22.0, includes the correct launcher,
  and contains a gateway byte-identical to the staged headless sidecar.
  Build log: `.verify/release-review/linux-release-final.log`.
- `cargo build --release --locked --manifest-path src-tauri/Cargo.toml --no-default-features --features gtk-desktop --bin toolport-gtk --bin toolport-gateway`
  passed. The actual native PKGBUILD `package()` function was run under a
  temporary staging prefix, validating both binaries, desktop metadata, icons,
  license and agent-plugin manifests at 1.22.0. This is not an Arch `makepkg`
  validation. Logs: `native-release-build.log` and `native-package.log`.
- Both extracted gateways passed `npm run smoke:headless` with
  `TOOLPORT_GATEWAY_BIN` pointing at the extracted binary, disposable data and
  an explicit disposable file-vault key. Logs: `deb-gateway-smoke-final.log`
  and `native-gateway-smoke.log` in the review evidence directory.
- The native packaged gateway connected to `https://api.githubcopilot.com/mcp/`
  using the existing `gh` sign-in credential, initialized as Toolport 1.22.0,
  and listed **45 GitHub tools**. No provider tool was invoked. The credential
  stayed in memory/child environment and was not printed or written into the
  evidence. Registry and process logs were temporary and removed. This proves
  real token-authenticated initialization/discovery, not browser OAuth or
  account operations. Evidence: `github-authenticated-probe.log`.

The local validation artifacts are in `.verify/release-review/`:

| Artifact                                                       | SHA-256                                                            |
| -------------------------------------------------------------- | ------------------------------------------------------------------ |
| `Toolport_1.22.0_amd64.deb`                                    | `76aeeb678310ff21edab7c7f04e0f1ad6168b3c7ec8caa103589e3ee639b7223` |
| `toolport-native-1.22.0-linux-x86_64.tar.gz` (staging archive) | `46263cc2fc7cd4325cc813c0d77f1dad94c03cae262012b439500b61391e5605` |

These are unsigned local Linux builds. Updater artifact signing was deliberately
disabled for this validation; official release artifacts must still be built
and signed by the release workflow after approval. They were not installed.

Version fields were compared across npm/lockfile, Cargo/lockfile, Tauri,
agent-plugin manifests and packaging. They prepare 1.22.0 consistently. The
separately published MCP server version is independent. As documented in
`docs/RELEASING.md`, the repository Homebrew file is a snapshot until release
assets exist, and Arch's source checksum must be pinned after tagging; placeholder
checksums are not publishable artifacts.

Twilio and other provider credentials, hosted browser OAuth consent and account
scopes, service-side API operations, and native OS vault UX remain unproven.
GitHub token authentication was verified separately as described above.
The file-vault fixture does not establish Windows Credential Manager or macOS
Keychain behavior. Signed installers, updater verification, notarization,
Windows/macOS builds of the local fixes, and GUI behavior on clean installations
still need release-artifact validation. Local unsigned Linux package construction
and extracted-gateway smoke checks passed; those are not signing or clean-install
evidence. Existing CI for `31afa0f` does not cover this
working diff.

**The unchanged original PR head remains a no-go.** The reviewed fixes passed
local release-candidate validation and need fresh cross-platform CI before
merge. Publishing still requires the real-provider and signed installer
acceptance checks from the curation/release docs.
