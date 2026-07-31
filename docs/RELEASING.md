# Releasing

Releases are built by CI on a version tag (`.github/workflows/release.yml`).

1. Bump the version to match the tag in:
   - `src-tauri/tauri.conf.json` (`version`), drives the installer filename
   - `src-tauri/Cargo.toml` (`version`)
   - `package.json` (`version`)
   - `package-lock.json` (root `version` fields)
   - `src-tauri/Cargo.lock` (the Cargo package is still named `conduit` for history;
     update that package's `version` entry)
   - `CHANGELOG.md` — move `[Unreleased]` entries into a dated section
   - `server.json` only when publishing a matching standalone gateway package
2. Draft user-facing notes in `docs/release-notes/vX.Y.Z.md` (paste into the GitHub
   release body when publishing the draft CI creates).
3. Commit the bump (e.g. `chore(release): 1.6.0`).
4. Merge to `main`, then tag and push:

   ```bash
   git checkout main && git pull
   git tag v1.6.0
   git push origin v1.6.0
   ```

CI builds installers for **Windows** (NSIS), **macOS** (dmg), and **Linux**
(deb + AppImage), each with the gateway bundled, and attaches them to a **draft**
release with auto-generated notes. Replace or augment the body with
`docs/release-notes/vX.Y.Z.md`, then click **Publish**.

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
