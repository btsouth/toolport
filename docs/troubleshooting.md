# Troubleshooting

- **OAuth opens a blank page (macOS).** The OAuth flow redirects back to a local
  `http://127.0.0.1` callback. Safari can silently block that redirect, so the
  sign-in page renders blank. Set **Chrome or Brave** as your default browser (or
  paste an access token instead). Complete one attempt at a time, an abandoned
  attempt keeps the callback port reserved for a few minutes and can cause a
  "state mismatch" on the next try.
- **A client reports the gateway "was not found" (running from source).** Build
  the gateway binary once: `npm run build:gateway` (or
  `cargo build --no-default-features --bin toolport-gateway --manifest-path src-tauri/Cargo.toml`).
  `npm run tauri dev` builds the app but not this separate binary; packaged
  releases bundle it, so installed users never hit this.
- **An npx/uvx server shows "Error" then works on retry.** On a cold npm/PyPI cache
  the first connect can take up to ~2 minutes while the package downloads. v1.6.0+
  shows **"Installing…"** during that wait and pre-warms downloads when you add the
  server. If it still fails, check network access and try **Re-check** after a minute.
- **Repeated macOS keychain prompts / "could not read secret from the keychain"
  in dev.** An unsigned dev build gets an unstable code-signing identity, so the
  keychain re-prompts or denies reads. Signed release builds (v0.9.3+) don't: they
  store secrets in the macOS data-protection keychain under a shared access group,
  so the gateway reads them with no prompt. This is a dev-only artifact.
- **"could not read/store secret" on Linux.** Secret storage uses the freedesktop
  Secret Service (libsecret), provided by GNOME Keyring, KWallet, or similar. A
  headless box or a session without a running keyring daemon has nowhere to store
  secrets. Run Toolport in a desktop session, or install and unlock a keyring
  (e.g. `gnome-keyring`).
- **macOS keychain and the gateway (v0.9.3+).** The app and the separately-signed
  gateway share a team-scoped keychain access group, so the gateway reads the
  secrets the app saved with no prompt, even across app updates. (Earlier releases
  showed a one-time "Always Allow" prompt; on current signed builds it's gone.)
- **VS Code: the `toolport` server doesn't start automatically.** VS Code may require
  you to click **Start Server** on the `toolport` MCP entry the first time, that's VS
  Code's own MCP handling, not Toolport. After that it reconnects on its own.
- **Linux: the AppImage shows no window, or a grey empty one (`EGL_BAD_PARAMETER`).**
  Fixed in 1.16.0; update. On 1.15.0 and older the process would start, put a
  window on screen, and never paint it, with `WebKitWebProcess` dying at launch:

  ```
  Could not create default EGL display: EGL_BAD_PARAMETER. Aborting...
  ```

  The cause was the AppImage bundling wayland's client libraries. `AppRun` puts
  the bundle on `LD_LIBRARY_PATH`, which the loader then also applies to the
  host's GPU drivers, and those are deliberately _not_ bundled. So a current
  Mesa got resolved against Ubuntu 22.04's wayland 1.20 and could not load at
  all:

  ```
  /usr/lib/libEGL_mesa.so.0: undefined symbol: wl_fixes_interface
  ```

  `wl_fixes_interface` arrived in wayland 1.23. This read as an AMD-only bug for
  a long time, but it was never about the GPU: NVIDIA's proprietary EGL is a
  separate implementation that does not link `libwayland-client`, so it was the
  only stack that survived. Every Mesa driver hit it, on X11 as well as Wayland.
  1.16.0 stops bundling those four libraries, so the host's are used and both
  drivers work. It was not the bundled WebKitGTK, which is current.

  If a grey window survives the update, that is a different problem, and on a
  virtualized GPU it is usually EGL itself: try
  `EGL_PLATFORM=surfaceless ./Toolport_*.AppImage`, and turn on 3D acceleration
  if you are in a VM.

- **Arch + proprietary NVIDIA: `toolport-bin` exits at startup, but the AppImage
  works.** This one runs the other way round, and it is a system-stack problem,
  not a Toolport one: the native package links your system GTK/WebKitGTK, and on
  NVIDIA that combination exits immediately with

  ```
  Gdk-Message: Error 71 (Protocol error) dispatching to Wayland display.
  ```

  `GDK_BACKEND=x11` gets past that, but the window then cannot allocate buffers
  (`Failed to create GBM buffer of size 1240x820: Invalid argument`) and the app
  is unusable. The AppImage carries its own GTK and WebKitGTK and sidesteps both,
  which is why it is the default recommendation on Arch. Observed on Omarchy
  (Hyprland via uwsm), RTX 4070 SUPER, `nvidia-open-dkms` 610.57.04, against
  system GTK 3.24.52 / WebKitGTK 2.52.6.

- **Linux: the first launch killed Xwayland, and now nothing happens at all.**
  Fixed in 1.15.0. Older AppImages forced `GDK_BACKEND=x11` in a way nothing could
  override, so on a Wayland session with a fragile Xwayland (a VMware guest on the
  `vmwgfx` driver, for one) the first launch took Xwayland down session-wide, and
  every launch after that blocked forever on the orphaned X socket with no window
  and no error. Log out and back in to get Xwayland back, then use 1.15.0 or newer,
  where `GDK_BACKEND=wayland ./Toolport_*.AppImage` is honoured. Note the AppImage
  wrapper is not the app: the real process is `conduit`, and killing only the
  wrapper leaves it holding the single-instance lock so the next launch hangs the
  same way.

## Known issues

- **Linux only, glib `VariantStrIter` soundness ([RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429)).**
  Tauri's Linux webview stack pulls in `glib` 0.18 transitively (`wry → webkit2gtk →
gtk 0.18 → glib 0.18`). The fix only exists in `glib` 0.20+, and the gtk-0.18
  binding line, which is what Tauri 2 uses on Linux, hard-pins `glib = "^0.18"`, so
  the patched release cannot be selected without moving the whole webview stack. The
  bug is a soundness/null-deref crash (not remote code execution), is confined to the
  webview binding layer (Toolport never calls `VariantStrIter`), and does not affect
  the Windows or macOS builds. We are tracking the upstream move to a glib-0.20 stack
  and will apply a `[patch.crates-io]` backport if Linux crashes surface before then.
