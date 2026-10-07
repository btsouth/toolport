# Arch Linux (AUR): `toolport-bin`

`toolport-bin` repackages the official GTK deb for Arch and Arch-derived distros,
including Omarchy. Existing AUR users keep the same package name and upgrade with
their AUR helper. The package uses host GTK4 and libadwaita, without WebKitGTK.

```bash
paru -S toolport-bin
# or
yay -S toolport-bin
```

The signed pacman repository's `toolport` package is an alternative. Both install
Toolport's binaries and launchers, so `toolport-bin` provides and conflicts with
`toolport`. Install one of them. The GTK deb layout is preserved, including
`toolport-gtk`, `toolport-gateway`, the `toolport` and `conduit` aliases, desktop
entry, icons, agent plugin and rollback helper.

## Rendering and publishing

`PKGBUILD` and `.SRCINFO` are generated, not checked in. Their checksums must
match the published `Toolport_<version>_amd64.deb` and tagged LICENSE:

```bash
scripts/render-aur.sh 2.0.0 ./aur
cd aur && makepkg -si
```

Use a published stable version. Prereleases are rejected because Arch's version
ordering cannot safely represent the release suffix. `.github/workflows/aur.yml`
runs on `release: released` only, validates the package in an Arch container,
compares the renderer's `.SRCINFO` with `makepkg --printsrcinfo`, and publishes it.
The container cannot modify the metadata that gets pushed.

For an unpublished build dry run, set `AUR_DEB_FILE` and `AUR_LICENSE_FILE` to
local files. These only change checksum inputs; the generated URLs still name a
stable release. Do not publish that metadata until the matching assets exist.

## Publisher setup and retries

1. Create an AUR account and add its SSH public key.
2. Store the matching private key in the repository secret `AUR_SSH_PRIVATE_KEY`.
3. Check the pinned ed25519 fingerprint in `aur.yml` against the SSH fingerprints
   at <https://aur.archlinux.org/>. A mismatch stops publishing.
4. Run `aur.yml` manually with a published stable tag and `dry_run` checked to
   validate without pushing. Uncheck it to publish.

Without the secret, the workflow validates and reports that nothing was pushed.
Manual runs reject drafts and prereleases. If the same version needs corrected
metadata, bump the workflow's `pkgrel` input or render with
`AUR_PKGREL=2 scripts/render-aur.sh 2.0.0 ./aur`. This lets existing installations
upgrade to the correction. The publish step refuses to overwrite a newer AUR
version or package revision with an older one.
