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
     `packaging/agent-plugin/toolport/.claude-plugin/plugin.json` (`version`);
     a vitest check (`src/test/agent-plugin.test.ts`) fails CI if these drift
     from `package.json`
   - `packaging/homebrew/toolport.rb` (`version`; update both dmg `sha256`s
     after publishing, in the Homebrew tap step below); a vitest check
     (`src/test/homebrew-cask.test.ts`) fails CI if the version drifts from
     `package.json`. This file is a snapshot; `brew install` reads the live
     tap, not this copy (see Homebrew tap below)
   - `packaging/linux/native/PKGBUILD` (`pkgver`; set its source `sha256sums`
     to `SKIP` until the tag archive exists, then pin the real digest before
     dispatching the Arch repository workflow below)
   - `CHANGELOG.md`; move `[Unreleased]` entries into a dated section
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
(GTK deb + rpm, plus the Tauri AppImage), each with the gateway bundled, plus
`toolport-agent-plugin.zip`,
and attaches them to a **draft** release titled `Toolport vX.Y.Z` whose body is the
changelog section. Review the draft, then click **Publish**.

## 2.0 staged rollout

Publish 2.0.0 for manual installation first. Hold the 1.x in-app update offer
and package-channel promotion for about two weeks while early users upgrade.
Keep 1.24.x available and provide security fixes for three months after 2.0.0.

This is release policy, not an automatic workflow delay. The Tauri endpoint is
`releases/latest/download/latest.json`; making 2.0.0 Latest with that asset
immediately offers it to Windows, macOS and AppImage users. Publishing also
triggers winget, the Homebrew tap follows Latest, and stable tags can update AUR.
Keep 2.0 out of Latest and hold those channel jobs during the manual-install
period; defer the Arch repository's publishing dispatch too. Do not publish a
normal Latest release and assume these channels will wait.

Write the notes under `## [2.0.0]` in `CHANGELOG.md`, add the release date when
cutting the tag, and keep the first paragraph to a short update summary. Both
release-body extraction steps read that exact section. The updater manifest
currently includes version, date and platform artifacts but **no `notes` field**;
the app displays `update.body` only when supplied. Before enabling the 1.x offer,
ensure `latest.json` carries the short summary as `notes` and links to the
[upgrade guide](upgrading-to-2.md). Writing the changelog alone does not populate
an in-app prompt. Check the published manifest and prompt before promotion.

## Package channels

The **Arch pacman repository** is a separate manual dispatch. The tag archive's
checksum cannot be pinned in the commit that creates the tag, because changing
that commit changes the archive. Once the tag exists, download its archive,
compute its SHA-256, replace `SKIP` in `packaging/linux/native/PKGBUILD`, and
commit that checksum to `main`. For example, for `vX.Y.Z`:

```bash
curl -fL -o /tmp/toolport-vX.Y.Z.tar.gz \
  https://github.com/btsouth/toolport/archive/refs/tags/vX.Y.Z.tar.gz
sha256sum /tmp/toolport-vX.Y.Z.tar.gz
```

Run a build-only check against the tag while the GitHub release is still a
draft, then publish the reviewed release and dispatch the real package update:

```bash
gh workflow run arch-repo.yml -f tag=vX.Y.Z -f dry_run=true
# After the release is published:
gh workflow run arch-repo.yml -f tag=vX.Y.Z -f dry_run=false
```

The workflow checks that `pkgver` matches the tag and refuses `SKIP`; it builds
from the tagged source before signing and uploading the package. Watch the run
through completion so a release does not leave pacman users on the old version.

Publishing is also what triggers **winget** (`winget.yml`): it submits a manifest
update to `microsoft/winget-pkgs` for the new version. It runs on publish rather
than on the tag because winget's validation downloads the installer URL, which 404s
while the release is still a draft. It no-ops with a warning unless the
`WINGET_TOKEN` secret (a PAT with `public_repo`) is set, so it can never fail a
release.

To retry a failed submission, run `winget.yml` manually with the same stable tag.
It downloads the existing installer, checks its SHA256 against the release asset
metadata, syncs the token owner's `winget-pkgs` fork from upstream `master`, and
reuses `toolport-<version>` after checking its saved manifest version, URL and hash. It never builds or
uploads release assets. An existing open or merged PR is reused; a closed,
unmerged PR or a retry manifest naming different artifacts fails for operator review. Releases without
an asset SHA256 digest fail validation rather than inventing a trusted checksum.
The fork must already exist. Its `master` is reset to upstream; other branches
are preserved. The checked-in files in `packaging/winget` are templates, with
version, installer URL/hash and release notes URL filled from the release.

Linux system packages now ship the GTK shell as `.deb` and `.rpm`, built once on
Ubuntu 24.04 and tested before upload to the draft. The Tauri AppImage remains the
fallback for Ubuntu 22.04, Debian 12 and older RPM distributions, and keeps its
in-app updater. GTK debs require Ubuntu 24.04+ or Debian 13+; RPMs require
GTK 4.14+, libadwaita 1.5+, GLib 2.80+ and glibc 2.39+. Stable releases also
update AUR `toolport-bin` by repackaging the GTK deb, so existing AUR users can
upgrade with their helper. The native pacman package remains an alternative. See
[`docs/linux-packages.md`](linux-packages.md) for build and upgrade checks.

Publishing is also when the **Homebrew tap** is bumped, and that is now
automatic. `brew install --cask btsouth/toolport/toolport` and
`brew upgrade --cask toolport` install the version + sha256 pinned in
[`btsouth/homebrew-toolport`](https://github.com/btsouth/homebrew-toolport)
`Casks/toolport.rb`. The `livecheck` / `github_latest` block in that cask only
feeds `brew livecheck`; it does not move the pin. The copy at
`packaging/homebrew/toolport.rb` in this repo is a snapshot `brew install` does
not read.

That tap's own `bump.yml` workflow tracks the latest published release, computes
both digests from the DMGs GitHub actually serves, and commits the result. It
runs every six hours, so it lands on its own; to have it immediately after a
release, dispatch it:

```bash
gh workflow run bump.yml --repo btsouth/homebrew-toolport
```

It is a no-op when the cask already matches, and it only ever tracks
`releases/latest`, which excludes drafts and prereleases. It lives in the tap
rather than beside `winget.yml` because that repo's own `GITHUB_TOKEN` can write
to it, where a workflow here would need a cross-repo token.

Do not hand-edit the tap's version or sha256. The url interpolates `version`, so
changing the version alone repoints every download at new artifacts while the
old digests stay behind, and brew then rejects the file it just downloaded. That
is the exact failure the workflow exists to prevent.

The snapshot at `packaging/homebrew/toolport.rb` is still updated by hand at
release time; `src/test/homebrew-cask.test.ts` fails CI if its version drifts
from `package.json`, so skipping it is loud. Its checksums are cosmetic (nothing
installs from it), but keep them honest by copying what the tap landed.

The **gateway container image** (`ghcr.io/btsouth/toolport-gateway`) publishes
separately on every push to `main` via `docker-publish.yml`; no tag required.

## After users upgrade

On each app launch Toolport **stops obsolete gateway processes** (older versioned
binaries and stale paths), keeping the current published/resolved gateway. Clients
that auto-respawn MCP pick up the new binary on the **next tool call** without a
full agent restart. Settings → Integrations → **Stop old gateways** runs the same
cleanup on demand. The Tauri in-app updater asks idle shared daemons to shut
down, then stops standalone gateway processes before installation. If a shared
daemon still has active MCP sessions, installation is refused and the app gives
recovery guidance. Retry after those sessions close; the updater never forces a
live shared daemon to exit.

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
