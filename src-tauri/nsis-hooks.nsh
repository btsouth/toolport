; NSIS installer hooks for Toolport.
;
; The app was renamed Conduit -> Toolport while deliberately keeping the bundle
; identifier (com.tsout.conduit) so in-place updates preserve each user's data
; directory and OS-keychain secrets. The side effect: the ORIGINAL Conduit
; installer created "Conduit" Start-menu / desktop shortcuts (with the old green
; icon) that an in-place update does not rename. Remove those stale shortcuts on
; (re)install so upgraders see "Toolport" with the porthole icon, not "Conduit".
;
; The standard install step creates the new "Toolport" shortcuts, so we only
; need to delete the leftover Conduit ones here. On a fresh install these
; Deletes are harmless no-ops.

; Extract the incoming helper: installed 1.x gateways do not know the preflight
; command. It runs as the current user, before any gateway files are replaced.
!define TOOLPORT_HOOK_DIR "${__FILEDIR__}"
!macro NSIS_HOOK_PREINSTALL
  InitPluginsDir
  !if "${ARCH}" == "x64"
    File /oname=$PLUGINSDIR\toolport-preflight.exe "${TOOLPORT_HOOK_DIR}\binaries\toolport-gateway-x86_64-pc-windows-msvc.exe"
  !else if "${ARCH}" == "arm64"
    File /oname=$PLUGINSDIR\toolport-preflight.exe "${TOOLPORT_HOOK_DIR}\binaries\toolport-gateway-aarch64-pc-windows-msvc.exe"
  !else
    File /oname=$PLUGINSDIR\toolport-preflight.exe "${TOOLPORT_HOOK_DIR}\binaries\toolport-gateway-i686-pc-windows-msvc.exe"
  !endif
  toolport_preflight_retry:
    nsExec::ExecToStack /TIMEOUT=60000 '"$PLUGINSDIR\toolport-preflight.exe" --installer-preflight "$INSTDIR"'
    Pop $0
    Pop $1
    ${If} $0 != 0
      ${If} $1 == ""
        StrCpy $1 "Toolport could not finish checking client sessions ($0). Installation has been deferred. Try again after closing affected clients."
      ${EndIf}
      DetailPrint "$1"
      ; The updater has already exited Toolport. Restore it on every deferred
      ; update/passive run, including silent mode, before leaving the installer.
      ${If} $UpdateMode = 1
      ${OrIf} $PassiveMode = 1
        Exec '"$INSTDIR\${MAINBINARYNAME}.exe"'
        IfSilent toolport_preflight_defer
        MessageBox MB_OK|MB_ICONEXCLAMATION "$1$\r$\nInstallation has been deferred and Toolport has been reopened."
        Goto toolport_preflight_defer
      ${EndIf}
      IfSilent toolport_preflight_defer
      MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "$1$\r$\nClose the affected clients and Retry, or Cancel to install later." IDRETRY toolport_preflight_retry
      toolport_preflight_defer:
        SetErrorLevel 1
        Quit
    ${EndIf}
!macroend

; Tauri invokes the uninstaller during /UPDATE too. Only real removal disconnects.
!macro NSIS_HOOK_PREUNINSTALL
  ; Setup passes _?= to run directly in the install directory. NSIS consumes
  ; that flag before $CMDLINE, so compare the executable and install directories.
  ; A normal removal runs NSIS's temporary uninstaller copy instead.
  GetFullPathName $0 "$EXEDIR"
  GetFullPathName $1 "$INSTDIR"
  ${If} $0 == $1
    Goto toolport_cleanup_done
  ${EndIf}
  ${If} $UpdateMode != 1
    ; The template checks again after this hook, but cancellation must precede
    ; disconnecting clients while the gateway binary still exists.
    !insertmacro CheckIfAppIsRunning "$INSTDIR\${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
    nsExec::ExecToLog /TIMEOUT=30000 '"$INSTDIR\toolport-gateway.exe" --disconnect-all'
    Pop $0
    ${If} $0 != 0
      DetailPrint "Toolport client cleanup failed ($0). Reinstall Toolport and run toolport-gateway.exe --disconnect-all as your user before deleting its data, or use Settings > Remove Toolport from all clients. Removal will continue."
      IfSilent toolport_cleanup_done
      ${If} $PassiveMode != 1
        MessageBox MB_OK|MB_ICONEXCLAMATION "Some client configs could not be restored. Removal will continue. Keep Toolport's data, reinstall Toolport, and use Settings > Remove Toolport from all clients or run toolport-gateway.exe --disconnect-all as your user."
      ${EndIf}
    ${EndIf}
  ${EndIf}
  toolport_cleanup_done:
!macroend

!macro NSIS_HOOK_POSTINSTALL
  Delete "$SMPROGRAMS\Conduit.lnk"
  Delete "$DESKTOP\Conduit.lnk"
!macroend
