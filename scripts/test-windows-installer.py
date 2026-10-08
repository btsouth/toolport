#!/usr/bin/env python3
"""Run the shipped NSIS bundle on a disposable Windows CI runner, never a host VM.

Windows known folders do not follow USERPROFILE/APPDATA environment overrides.
Keep all client/provenance fixtures in a temporary directory instead, using the
gateway's supported data-dir override and path-specific recovery records. NSIS
uses the disposable runner's own HKCU, shortcuts and bundle data directories.
"""
import hashlib
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
LOGS = ROOT / ".verify/windows-installer"


def wait_for(check, label, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.05)
    raise AssertionError(f"Timed out: {label}")


def app_pids(app):
    # Query the exact install path; never stop unrelated runner processes.
    escaped = str(app).replace("'", "''")
    result = subprocess.run(
        ["powershell", "-NoProfile", "-Command",
         f"Get-CimInstance Win32_Process -Filter \"Name='conduit.exe'\" | "
         f"Where-Object {{ $_.ExecutablePath -eq '{escaped}' }} | "
         "Select-Object -ExpandProperty ProcessId"],
        capture_output=True, text=True, timeout=15, check=True)
    return [int(pid) for pid in result.stdout.split()]


def stop_app(app):
    for pid in app_pids(app):
        subprocess.run(["taskkill", "/PID", str(pid), "/T", "/F"],
                       capture_output=True, timeout=15, check=True)
    wait_for(lambda: not app_pids(app), "app exit")


def installer_run(installer, args, label, expected=0):
    # NSIS requires /D to be last and unquoted, even with spaces in the path.
    command = f'"{installer}" {args}'
    with (LOGS / f"{label}.log").open("wb") as log:
        result = subprocess.run(command, stdout=log, stderr=log, timeout=120)
    assert result.returncode == expected, (label, result.returncode, expected)
    print(f"PASS: {label} (exit {result.returncode})", flush=True)


def start_gateway(gateway, args):
    return subprocess.Popen([str(gateway), *args], stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)


def start_client(gateway):
    client = start_gateway(gateway, ["fixture-direct-stdio"])
    client.stdin.write(b'{"jsonrpc":"2.0","id":1,"method":"initialize",'
                       b'"params":{"protocolVersion":"2024-11-05",'
                       b'"capabilities":{},"clientInfo":{"name":"installer-ci","version":"1"}}}\n')
    client.stdin.flush()
    lines = queue.Queue()
    threading.Thread(target=lambda: lines.put(client.stdout.readline()), daemon=True).start()
    response = json.loads(lines.get(timeout=30))
    assert response.get("id") == 1 and "result" in response, response
    return client


def seed_client(root, data, gateway, corrupt=False):
    path = root / "client/mcp.json"
    path.parent.mkdir(exist_ok=True)
    original = '{\r\n  "mcpServers" : {"native":{"command":"original"}},\r\n  "theme" : "user formatting"\r\n}\r\n'
    written = json.loads(original)
    written["mcpServers"]["toolport"] = {"command": str(gateway)}
    written = json.dumps(written, indent=2) + "\n"
    path.write_bytes(written.encode())
    sha = lambda text: hashlib.sha256(text.encode()).hexdigest()
    record = dict(version=1, format="JsonMcpServers", configPath=str(path),
                  original=original, originalHash=sha(original), baseline=json.loads(original),
                  capturedAt=1, toolportVersion="fixture", lastWritten=written,
                  lastWrittenHash=sha(written), createdParents=[], exactEligible=True,
                  preexistingGateways=[], disconnected=False, jsoncSettings=False,
                  disconnectBefore=None)
    backup = data / "backups/cursor"
    backup.mkdir(parents=True, exist_ok=True)
    (backup / f"original-{sha(str(path))}.json").write_text(
        "invalid provenance" if corrupt else json.dumps(record), encoding="utf-8")
    return path, original.encode(), written.encode()


def main():
    assert os.name == "nt" and os.environ.get("GITHUB_ACTIONS") == "true", "Disposable Windows CI only"
    LOGS.mkdir(parents=True, exist_ok=True)
    installers = list((ROOT / "src-tauri/target/release/bundle/nsis").glob("*-setup.exe"))
    assert len(installers) == 1, installers
    installer = installers[0]
    with tempfile.TemporaryDirectory(prefix="toolport nsis ") as temp:
        root = Path(temp)
        install = root / "installed"
        install.mkdir()
        gateway = install / "toolport-gateway.exe"
        app = install / "conduit.exe"
        data = root / "data"
        os.environ["TOOLPORT_DATA_DIR"] = str(data)
        os.environ["TOOLPORT_NO_KEYRING"] = "1"
        children = []
        try:
            # A fresh passive deferral has a client but no application to reopen.
            shutil.copy2(ROOT / "src-tauri/binaries/toolport-gateway-x86_64-pc-windows-msvc.exe", gateway)
            client = start_client(gateway)
            children.append(client)
            installer_run(installer, f"/S /P /D={install}", "fresh-passive-busy", 1)
            assert client.poll() is None and not app.exists()
            assert not app_pids(app)
            client.kill()
            client.wait(timeout=10)
            gateway.unlink()

            installer_run(installer, f"/S /D={install}", "fresh-silent-install")
            assert all((install / name).is_file() for name in
                       ("conduit.exe", "toolport-gateway.exe", "uninstall.exe"))
            assert not app_pids(app), "Fresh silent install must not reopen"
            path, original, connected = seed_client(root, data, gateway)
            client = start_client(gateway)
            children.append(client)
            before = hashlib.sha256(app.read_bytes()).hexdigest()
            installer_run(installer, f"/S /UPDATE /D={install}", "busy-update-defers", 1)
            assert client.poll() is None, "Installer killed an active MCP session"
            assert hashlib.sha256(app.read_bytes()).hexdigest() == before
            assert path.read_bytes() == connected
            wait_for(lambda: app_pids(app), "deferred update reopens app")
            print("PASS: busy update preserves client session/config and reopens existing app", flush=True)
            stop_app(app)
            client.kill()
            client.wait(timeout=10)

            daemon = start_gateway(gateway, ["--daemon"])
            children.append(daemon)
            wait_for(lambda: list(data.glob("daemon-*.json")), "idle daemon descriptor")
            installer_run(installer, f"/S /UPDATE /D={install}", "idle-update")
            assert daemon.wait(timeout=10) == 0, "Idle daemon was not stopped gracefully"
            assert path.read_bytes() == connected, "Update disconnected client config"
            assert not app_pids(app), "Successful silent update without /R must not reopen"
            installer_run(installer, f"/S /UPDATE /R /D={install}", "idle-update-reopen")
            wait_for(lambda: app_pids(app), "successful update /R reopens app")
            stop_app(app)

            # Silent mode has no delete-data choice: the template defaults to keep.
            # Check both NSIS-managed bundle data and gateway recovery data.
            sentinels = [Path(os.environ[var]) / "com.tsout.conduit/installer-ci-sentinel"
                         for var in ("APPDATA", "LOCALAPPDATA")]
            for sentinel in sentinels:
                sentinel.parent.mkdir(parents=True, exist_ok=True)
                sentinel.write_bytes(b"keep app data\n")
            installer_run(install / "uninstall.exe", "/S", "silent-uninstall")
            wait_for(lambda: not gateway.exists() and not app.exists(), "uninstaller completion")
            assert path.read_bytes() == original, "Uninstall did not restore byte-exact client config"
            assert (data / "backups/cursor").is_dir()
            assert all(s.read_bytes() == b"keep app data\n" for s in sentinels)
            print("PASS: silent uninstall restores exact CRLF bytes and keeps app/recovery data", flush=True)

            installer_run(installer, f"/S /D={install}", "reinstall-for-cleanup-failure")
            path, _, connected = seed_client(root, data, gateway, corrupt=True)
            installer_run(install / "uninstall.exe", "/S", "failed-cleanup-silent-uninstall")
            wait_for(lambda: not gateway.exists(), "failed-cleanup uninstaller completion")
            assert path.read_bytes() == connected
            assert any((data / "backups/cursor").glob("original-*.json"))
            assert all(s.exists() for s in sentinels)
            for sentinel in sentinels:
                sentinel.unlink()
            print("PASS: cleanup failure keeps client bytes and recovery data", flush=True)
        finally:
            stop_app(app)
            for child in children:
                if child.poll() is None:
                    child.kill()
                child.wait(timeout=10)
    print("PASS: real Windows NSIS installer round trip", flush=True)


if __name__ == "__main__":
    main()
