# Arch Linux (AUR): `toolport-bin`

A native package for Arch and Arch-derived distros (Manjaro, EndeavourOS,
**Omarchy**). It is an option, not a requirement: as of 1.16.0 the AppImage runs
on Arch too (see below), and the installer script now takes that path by default.
Choose this one if you would rather have a real package, so Toolport links the
host's WebKitGTK, picks up its security updates, and upgrades and removes
through pacman.

> **Not on the proprietary NVIDIA driver.** Linking the system GTK/WebKitGTK is
> exactly what breaks there: `conduit` exits at startup with `Gdk-Message: Error
71 (Protocol error) dispatching to Wayland display`, and under
> `GDK_BACKEND=x11` it survives but cannot allocate buffers (`Failed to create
GBM buffer of size 1240x820: Invalid argument`). Send NVIDIA users to the
> AppImage, which carries its own GTK/WebKitGTK and renders fine. Verified on
> Omarchy/Hyprland, RTX 4070 SUPER, `nvidia-open-dkms` 610.57.04, system GTK
> 3.24.52 / WebKitGTK 2.52.6.

```bash
# any AUR helper
paru -S toolport-bin
yay -S toolport-bin

# Omarchy
omarchy pkg aur add toolport-bin
```

> **Not on the AUR yet.** New AUR account registration is paused upstream while
> Arch deals with a wave of automated signups (the page returns HTTP 503). Until
> it reopens there is no account to push `toolport-bin` from, so the workflow
> below no-ops with a warning and the commands above will not find the package.
> Nothing to fix here, and do not script retries against the registration page.
> Arch announces the reopening on `aur-general` and the Arch news feed.
>
> Meanwhile the package builds fine from this repo, no AUR account involved:
>
> ```bash
> git clone https://github.com/btsouth/toolport && cd toolport
> scripts/render-aur.sh 1.15.0 ./aur     # use the released version
> cd aur && makepkg -si
> ```
>
> That produces the exact package the AUR would serve, from the same published
> `.deb` and the same checksums.

## Why this package was built, and what actually turned out to be wrong

It was built to route around a window that opened **grey and empty** on Arch: the
GTK shell ran while `WebKitWebProcess` aborted on `EGL_BAD_PARAMETER` every
launch. The bundled Ubuntu 22.04 `libwebkit2gtk-4.1` got the blame, on the theory
that it was too old to initialise EGL against a current Mesa.

That was wrong, and it is worth writing down because the wrong diagnosis was
convincing enough to ship a whole package around. The bundled WebKitGTK is 2.50.4, which is current. The AppImage
was bundling **wayland 1.20**, and `AppRun` puts the bundle on
`LD_LIBRARY_PATH` - which the loader then also applies to the host's GPU drivers,
since those are deliberately not bundled. So the host's Mesa resolved against it:

```
/usr/lib/libEGL_mesa.so.0: undefined symbol: wl_fixes_interface
```

`wl_fixes_interface` arrived in wayland 1.23, so `libEGL_mesa` never loaded and
`eglGetDisplay` returned `EGL_NO_DISPLAY`. It looked Mesa-specific because it
_is_: NVIDIA's EGL does not link `libwayland-client`.

That also explains why the earlier attempts all failed. No `WEBKIT_*` variable
avoids it, because WebKit is not the problem. Displacing bundled libraries one at
a time surfaced a chain of `undefined symbol` errors (`gst_debug_log_id`, then
`g_once_init_leave_pointer`) because each swap left the rest of the mismatched
set in place; only displacing all of them at once converged, which read as "the
bundle can never agree with the host" rather than "one library is doing this".

`scripts/patch-appimage.sh` now removes `libwayland-*` from the AppDir before the
image is packed, so the host's copies are used. The AppImage works on Mesa and
NVIDIA alike from 1.16.0.

## PKGBUILD is generated, not checked in

`PKGBUILD` and `.SRCINFO` carry a `sha256sum` of the `.deb` for one specific
release, so a checked-in copy would either be stale or be a checksum nobody
verified. They are rendered per release instead:

```bash
# after the GitHub release is PUBLISHED (draft assets 404)
scripts/render-aur.sh 1.15.0 ./aur
```

`.github/workflows/aur.yml` runs exactly that on `release: released`, builds the
package in an `archlinux:base-devel` container to prove the PKGBUILD works, and
pushes to the AUR. Package metadata (description, `depends`, `optdepends`) lives
in `scripts/render-aur.sh`; edit it there.

The container gets `aur/` **read-only** and writes its `makepkg --printsrcinfo`
output to a scratch mount, which the runner then diffs against the rendered
`.SRCINFO`. So the container proves the renderer agrees with makepkg without
being able to put a byte into what is published, and a moving
`archlinux:base-devel` tag can only fail the job. If that diff fails, the fix is
in `scripts/render-aur.sh`, not in the workflow.

## One-time setup before the first publish

The workflow no-ops with a warning until this is done, so it can never fail a
release.

1. Create an AUR account at <https://aur.archlinux.org/> and add an SSH public
   key to it. **Blocked right now**: registration is paused, see the note above.
2. Put the matching **private** key in the repo secret `AUR_SSH_PRIVATE_KEY`.
3. Confirm the ed25519 fingerprint pinned in `aur.yml`
   (`SHA256:RFzBCUItH9LZS0cKB5UE6ceAYhBD5C8GeOBip8Z11+4`) still matches the "SSH
   Fingerprints" published on <https://aur.archlinux.org/>. The workflow fetches
   the host key and refuses it unless the hash matches, so a rotated key fails
   the push closed instead of being trusted.
4. Set the `# Maintainer:` line in `scripts/render-aur.sh` to the AUR account's
   name and email if you want the conventional format.
5. Run the workflow once manually (`workflow_dispatch`) with the release tag and
   `dry_run` **checked**, to build and validate without pushing. Then re-run with
   `dry_run` unchecked; the first push creates the `toolport-bin` package.

## Re-publishing the same version with a fixed PKGBUILD

`pacman` compares `pkgver-pkgrel`. Pushing a corrected PKGBUILD at the same
`pkgrel` reads as "already installed" on every machine that took the broken one,
so only fresh installs get the fix. Bump the revision:

- workflow: run `aur.yml` with the same tag and `pkgrel` set to `2`, `3`, ...
- by hand: `AUR_PKGREL=2 scripts/render-aur.sh 1.15.0 ./aur`

The bump is deliberate, never automatic: silently re-rolling an already-published
AUR revision is worse than leaving it alone.

The publish step also refuses to move the AUR _backwards_. A catch-up dispatch
for an old tag after a newer release has shipped exits without pushing, since the
concurrency group serialises pushes but does not order them by version.

## Verifying a release by hand

```bash
scripts/render-aur.sh 1.15.0 ./aur
cd aur && makepkg -si
pgrep -x Xwayland   # still returns a PID after launching and quitting Toolport
```
