// Product payloads are explicit. AppImage may also contain its system runtime.
const native = new Set([
  "usr/bin/toolport-gtk",
  "usr/bin/toolport-gateway",
  "usr/bin/toolport",
  "usr/bin/conduit",
  "usr/share/applications/com.tsout.Toolport.desktop",
  "usr/share/metainfo/com.tsout.Toolport.metainfo.xml",
  "usr/share/icons/hicolor/32x32/apps/toolport.png",
  "usr/share/icons/hicolor/128x128/apps/toolport.png",
  "usr/share/icons/hicolor/256x256/apps/toolport.png",
  "usr/share/toolport/agent-plugin/toolport-agent-plugin.zip",
  "usr/share/toolport/toolport-preview-rollback.sh",
  "usr/share/toolport/disconnect-users.sh",
  "usr/share/licenses/toolport/LICENSE",
  "usr/share/licenses/toolport-bin/LICENSE",
  "usr/share/doc/toolport/copyright",
]);
const nsis = new Set([
  "conduit.exe",
  "toolport-gateway.exe",
  "uninstall.exe",
  "$PLUGINSDIR/toolport-preflight.exe",
  ...[
    "System",
    "nsExec",
    "nsis_tauri_utils",
    "inetc",
    "ApplicationID",
    "UserInfo",
    "LangDLL",
    "StartMenu",
    "WebView2Loader",
  ].map((name) => `$PLUGINSDIR/${name}.dll`),
]);
const mac = new Set([
  "Contents/Info.plist",
  "Contents/PkgInfo",
  "Contents/MacOS/conduit",
  "Contents/MacOS/toolport-gateway",
  "Contents/MacOS/conduit-gateway",
  "Contents/embedded.provisionprofile",
  ...[
    "Info.plist",
    "PkgInfo",
    "MacOS/toolport-gateway",
    "embedded.provisionprofile",
    "_CodeSignature/CodeResources",
  ].map((path) => `Contents/Helpers/ToolportGateway.app/Contents/${path}`),
  "Contents/Resources/icon.icns",
  "Contents/_CodeSignature/CodeResources",
]);
function normalize(path) {
  return path
    .replaceAll("\\", "/")
    .replace(/^\.\//, "")
    .replace(/^\/+/, "")
    .replace(/^squashfs-root\//, "")
    .replace(/\/$/, "");
}
function parentOfAllowed(path, allowed) {
  return [...allowed].some((file) => file.startsWith(`${path}/`));
}
export function assertManifest(paths, kind) {
  const normalized = paths
    .filter(Boolean)
    .map(normalize)
    .filter((path) => path && path !== ".");
  const shell =
    kind === "native" || kind === "pacman"
      ? "usr/bin/toolport-gtk"
      : kind === "nsis" || kind === "msi"
        ? "conduit.exe"
        : kind === "appimage" || kind === "tauri-deb"
          ? "usr/bin/conduit"
          : "Contents/MacOS/conduit";
  const gateway =
    kind === "native" || kind === "pacman" || kind === "appimage" || kind === "tauri-deb"
      ? "usr/bin/toolport-gateway"
      : kind === "nsis" || kind === "msi"
        ? "toolport-gateway.exe"
        : "Contents/MacOS/toolport-gateway";
  const files = normalized
    .map((path) => {
      if (kind === "msi") {
        if (!/^(?:ProgramFiles(?:64)?Folder|LocalAppDataFolder)\/Toolport\//.test(path))
          throw new Error(`Unexpected msi installation path: ${path}`);
        return path.replace(
          /^(?:ProgramFiles(?:64)?Folder|LocalAppDataFolder)\/Toolport\//,
          "",
        );
      }
      if (kind === "mac") {
        if (/\.app(?:\/|$)/.test(path))
          return path
            .split(/\.app\/?/)
            .slice(1)
            .join(".app/")
            .replace(/\/$/, "");
        return path;
      }
      return path;
    })
    .filter(Boolean);
  if (!files.includes(shell) || !files.includes(gateway))
    throw new Error(`Missing intended shell or gateway in ${kind} package`);
  const unexpected = [];
  for (const path of files) {
    if (
      /(?:^|\/)(?:mock-mcp-server|search_eval_scale|toolport-live-client-task|deps|examples)(?:[./-]|$)/i.test(
        path,
      )
    )
      throw new Error(`Test artifact shipped: ${path}`);
    let allowed;
    if (kind === "native" || kind === "pacman") {
      allowed =
        native.has(path) ||
        parentOfAllowed(path, native) ||
        (kind === "pacman" && /^\.(?:PKGINFO|BUILDINFO|MTREE|INSTALL)$/.test(path));
    } else if (kind === "nsis") {
      allowed = nsis.has(path) || parentOfAllowed(path, nsis);
    } else if (kind === "msi") {
      allowed = /^(?:conduit|toolport-gateway)\.exe$/.test(path);
    } else if (kind === "mac") {
      allowed =
        mac.has(path) ||
        parentOfAllowed(path, mac) ||
        /^(?:Applications|\.DS_Store|\.VolumeIcon\.icns|\.background(?:\/background\.(?:png|tiff))?)$/.test(
          path,
        );
    } else if (kind === "appimage") {
      allowed =
        /^(?:AppRun(?:\.wrapped)?|\.DirIcon|apprun-hooks\/linuxdeploy-plugin-gtk\.sh|(?:[Tt]oolport|conduit)\.(?:desktop|png)|usr\/bin\/(?:conduit|toolport-gateway|xdg-mime))$/.test(
          path,
        ) ||
        /^(?:usr|usr\/bin|usr\/lib|usr\/lib64|usr\/share)$/.test(path) ||
        /^(?:usr\/)?lib(?:64)?\/(?:[^/]+\/)*(?:lib[^/]+\.so(?:\.[0-9]+)*|ld-linux[^/]+\.so(?:\.[0-9]+)*|WebKit(?:Web|Network|GPU)Process)$/.test(
          path,
        ) ||
        /^usr\/share\/(?:applications\/(?:[Tt]oolport|conduit)\.desktop|icons\/hicolor\/\d+x\d+\/apps\/(?:[Tt]oolport|conduit)\.png)$/.test(
          path,
        ) ||
        /^usr\/lib(?:64)?\/(?:gio|gdk-pixbuf-2\.0|gtk-3\.0|gtk-4\.0|webkit2gtk-4\.1|webkitgtk-6\.0)\//.test(
          path,
        ) ||
        /^usr\/lib\/girepository-1\.0\/[A-Za-z0-9]+-[0-9]+(?:\.[0-9]+)?\.typelib$/.test(
          path,
        ) ||
        /^usr\/lib\/im-(?:am-et|broadway|cedilla|cyrillic-translit|inuktitut|ipa|multipress|thai|ti-er|ti-et|viqr|wayland|xim)\.so$/.test(
          path,
        ) ||
        /^usr\/share\/doc\/lib[A-Za-z0-9+.-]+\/copyright$/.test(path) ||
        /^usr\/share\/(?:glib-2\.0\/schemas|mime|themes|icons|locale)\//.test(path);
      // Runtime directories themselves appear in the archive listing too.
      if (!allowed && files.some((file) => file.startsWith(`${path}/`))) allowed = true;
    } else if (kind === "tauri-deb") {
      allowed =
        /^(?:usr\/bin\/(?:conduit|toolport|toolport-gateway)|usr\/share\/applications\/Toolport\.desktop|usr\/share\/icons\/hicolor\/\d+x\d+\/apps\/(?:Toolport|conduit)\.png)$/.test(
          path,
        ) || files.some((file) => file.startsWith(`${path}/`));
    } else throw new Error(`Unknown package kind: ${kind}`);
    if (!allowed) unexpected.push(path);
  }
  if (unexpected.length)
    throw new Error(`Unexpected ${kind} payload:\n${unexpected.join("\n")}`);
  return files;
}
