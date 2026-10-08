#!/usr/bin/env bash
# Compile each hook branch with NSIS on Linux; this does not run Windows code.
set -euo pipefail
cd "$(dirname "$0")/.."
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cp src-tauri/nsis-hooks.nsh "$tmp/hooks.nsh"
mkdir "$tmp/binaries"
for target in x86_64 aarch64 i686; do
  printf 'compile-only sidecar fixture' > "$tmp/binaries/toolport-gateway-$target-pc-windows-msvc.exe"
done
for arch in x64 arm64 x86; do
  cat > "$tmp/test.nsi" <<NSIS
Unicode true
!include LogicLib.nsh
!include FileFunc.nsh
!include "$tmp/hooks.nsh"
!define ARCH "$arch"
Name "Toolport hook compile test"
OutFile "$tmp/$arch.exe"
InstallDir "\$LOCALAPPDATA\\ToolportHookTest"
RequestExecutionLevel user
Var PassiveMode
Var UpdateMode
Section
  !insertmacro NSIS_HOOK_PREINSTALL
  !insertmacro NSIS_HOOK_POSTINSTALL
  WriteUninstaller "\$INSTDIR\\uninstall.exe"
SectionEnd
Section Uninstall
  !insertmacro NSIS_HOOK_PREUNINSTALL
SectionEnd
NSIS
  makensis -V2 "$tmp/test.nsi"
  test -s "$tmp/$arch.exe"
  echo "PASS: NSIS hooks compile for $arch"
done
