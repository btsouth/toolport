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
   - `packaging/homebrew/toolport.rb` (`version`; update both dmg `sha256`s
     after publishing, in the Homebrew tap step below) — a vitest check
     (`src/test/homebrew-cask.test.ts`) fails CI if the version drifts from
     `package.json`. This file is a snapshot; `brew install` reads the live
     tap, not this copy (see Homebrew tap below)
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

Publishing is also when the **Homebrew tap** must be bumped. There is no
workflow for this next to `winget.yml` (SBS-260 is the backlog for automating
it). `brew install --cask tsouth89/toolport/toolport` and
`brew upgrade --cask toolport` install the version + sha256 pinned in
[`tsouth89/homebrew-toolport`](https://github.com/tsouth89/homebrew-toolport)
`Casks/toolport.rb`. The `livecheck` / `github_latest` block in that cask only
feeds `brew livecheck`; it does not move the pin. The copy at
`packaging/homebrew/toolport.rb` in this repo is a snapshot `brew install` does
not read.

After the GitHub release is **published** (the `.dmg` URLs 404 while the
release is still a draft, same reason `winget.yml` waits for `released`):

```bash
# Hashes must come from the published assets. Do not reuse the previous
# release's sha256, and do not invent one. Verified for v1.14.0: GitHub's
# asset digest matched shasum of the downloaded dmgs. --clobber prevents
# versionless files left by an earlier run from being reused.
gh release download vX.Y.Z --repo tsouth89/toolport --pattern 'Toolport_*apple-darwin.dmg' --clobber
shasum -a 256 Toolport_aarch64-apple-darwin.dmg Toolport_x86_64-apple-darwin.dmg
```

Then in `tsouth89/homebrew-toolport` `Casks/toolport.rb` (and the snapshot
here):

1. Set `version` to the tag without the `v`.
2. Set the `on_arm` sha256 to the `Toolport_aarch64-apple-darwin.dmg` digest.
3. Set the `on_intel` sha256 to the `Toolport_x86_64-apple-darwin.dmg` digest.
4. Keep both zap roots: `~/Library/Application Support/Conduit` (legacy
   installs that have not migrated) and `~/Library/Application Support/Toolport`
   (current `data_dir_leaf_name` in `src-tauri/src/brand.rs`). Cache/pref zap
   paths stay `com.tsout.conduit` because the bundle id is intentionally
   unchanged.
5. Open a PR on the tap. `brew install` keeps serving the old pin until that
   change is on the tap's default branch.

`src/test/homebrew-cask.test.ts` fails CI if the in-repo snapshot version
drifts from `package.json`, so a release bump that skips this file is loud.
It cannot see the live tap; that is this checklist.

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
