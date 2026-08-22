# Arch Linux (AUR): `toolport-bin`

Arch and Arch-derived distros (Manjaro, EndeavourOS, **Omarchy**) running on
**Mesa** (AMD or Intel graphics) should install Toolport from the AUR, not from
the AppImage.

> **Not on the proprietary NVIDIA driver.** The EGL breakage this package exists
> to avoid is Mesa-specific. NVIDIA's EGL is a separate implementation that never
> hits it, and there this package is the one that fails: `conduit` exits at
> startup with `Gdk-Message: Error 71 (Protocol error) dispatching to Wayland
display`, and under `GDK_BACKEND=x11` it survives but cannot allocate buffers
> (`Failed to create GBM buffer of size 1240x820: Invalid argument`). Send NVIDIA
> users to the AppImage, which bundles its own GTK/WebKitGTK and renders fine.
> Verified on Omarchy/Hyprland, RTX 4070 SUPER, `nvidia-open-dkms` 610.57.04,
> system GTK 3.24.52 / WebKitGTK 2.52.6. Choose by **driver**, not by distro.

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
> git clone https://github.com/tsouth89/toolport && cd toolport
> scripts/render-aur.sh 1.15.0 ./aur     # use the released version
> cd aur && makepkg -si
> ```
>
> That produces the exact package the AUR would serve, from the same published
> `.deb` and the same checksums.

## Why a native package instead of the AppImage

The AppImage bundles Ubuntu 22.04's `libwebkit2gtk-4.1`, because that is what
`release.yml` builds against. It is old enough to have no `WebKitGPUProcess` at
all, and it cannot initialise EGL against a current Mesa. On Arch that shows up
as a window that opens **grey and empty**: the GTK shell runs while
`WebKitWebProcess` aborts on `EGL_BAD_PARAMETER` on every launch.

This is the bundle, not the machine. On the same failing session, `eglinfo -B -p
wayland|gbm|surfaceless` is healthy and a ten-line python-gobject `WebKit2 4.1`
WebView renders fine through the **system** WebKitGTK. None of
`WEBKIT_DISABLE_DMABUF_RENDERER`, `WEBKIT_DISABLE_COMPOSITING_MODE` or
`WEBKIT_FORCE_SANDBOX=0` avoids it, alone or combined. Displacing only the
bundled WebKit does not fix it either: each round surfaces the next ABI mismatch
as an `undefined symbol` (`gst_debug_log_id`, then `g_once_init_leave_pointer`),
and only displacing every shadowing library at once converges.

A bundled browser engine and a rolling-release GPU stack cannot be kept in
agreement, so this package does not try. It repackages the payload of the
official `.deb` and declares real Arch dependencies, so Toolport links the host's
WebKitGTK, exactly as the `.deb` does on Debian/Ubuntu.

The fat AppImage is unchanged and stays the right download for Ubuntu/Debian.

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
