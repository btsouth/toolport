# Releasing

Releases are built by CI on a version tag (`.github/workflows/release.yml`).

1. Bump the version to match the tag in:
   - `src-tauri/tauri.conf.json` (`version`), drives the installer filename
   - `src-tauri/Cargo.toml` (`version`)
   - `package.json` (`version`)
   - `package-lock.json` (root `version` fields)
   - `src-tauri/Cargo.lock` (the Cargo package is still named `conduit` for history;
     update that package's `version` entry)
   - `packaging/agent-plugin/toolport/plugin.json` and
     `packaging/agent-plugin/toolport/.claude-plugin/plugin.json` (`version`) —
     a vitest check (`src/test/agent-plugin.test.ts`) fails CI if these drift
     from `package.json`
   - `CHANGELOG.md` — move `[Unreleased]` entries into a dated section
   - `server.json` only when publishing a matching standalone gateway package
   - `scripts/install.ps1` / `scripts/install.sh` only if you changed them, in which
     case also move `INSTALL_SCRIPTS_REF` in the site repo's `worker/index.js`, since
     `toolport.app/install.*` redirects to a pinned commit and will otherwise keep
     serving the old script
2. The `CHANGELOG.md` section from step 1 becomes the release body: CI extracts the
   lines under `## [X.Y.Z]` and falls back to generated notes if that heading is
   missing or empty. Write it there rather than anywhere else. (`docs/release-notes/`
   holds hand-written notes from before this was automated; nothing reads it.)
3. Commit the bump (e.g. `chore(release): 1.6.0`).
4. Merge to `main`, then tag and push:

   ```bash
   git checkout main && git pull
   git tag v1.6.0
   git push origin v1.6.0
   ```

CI builds installers for **Windows** (NSIS), **macOS** (dmg), and **Linux**
(deb + AppImage), each with the gateway bundled, plus `toolport-agent-plugin.zip`,
and attaches them to a **draft** release titled `Toolport vX.Y.Z` whose body is the
changelog section. Review the draft, then click **Publish**.

Publishing is also what triggers **winget** (`winget.yml`): it submits a manifest
update to `microsoft/winget-pkgs` for the new version. It runs on publish rather
than on the tag because winget's validation downloads the installer URL, which 404s
while the release is still a draft. It no-ops with a warning unless the
`WINGET_TOKEN` secret (a PAT with `public_repo`) is set, so it can never fail a
release.

To submit by hand instead, copy `packaging/winget` to a new
`manifests/t/Toolport/Toolport/<version>/` directory in a fork of
`microsoft/winget-pkgs` and update it for the release first: `PackageVersion`
in all three files, plus `InstallerUrl`, `InstallerSha256`, `ReleaseDate` and
`ReleaseNotesUrl`. The copy in this repo stays pinned to whatever version last
shipped, so submitting it unchanged re-submits that version's metadata and the new
release never reaches winget. Check it with `winget validate --manifest <dir>`
before opening the PR.

The **gateway container image** (`ghcr.io/tsouth89/toolport-gateway`) publishes
separately on every push to `main` via `docker-publish.yml` — no tag required.

## After users upgrade

On each app launch Toolport **stops obsolete gateway processes** (older versioned
binaries and stale paths), keeping the current published/resolved gateway. Clients
that auto-respawn MCP pick up the new binary on the **next tool call** without a
full agent restart. Settings → Integrations → **Stop old gateways** runs the same
cleanup on demand. The in-app updater still kills **all** gateway processes before
install so locked files can be replaced.

## Manual fallback

If you'd rather build locally:

```bash
npm run tauri:bundle
gh release create v1.6.0 \
  "src-tauri/target/release/bundle/nsis/Toolport_1.6.0_x64-setup.exe" \
  --title "Toolport v1.6.0" \
  --notes-file docs/release-notes/v1.6.0.md
```

## Signing

macOS installers are signed and notarized, and Windows installers are signed via
Azure Trusted Signing (when the `AZURE_*` secrets/variables are set; otherwise the
Windows build falls back to unsigned). Windows uses a standard certificate, so
SmartScreen reputation still accrues with downloads. See [SIGNING.md](SIGNING.md)
for details.
