#!/usr/bin/env python3
"""Synthetic first-write provenance for the packaged disconnect-all round trip."""
import hashlib
import json
from pathlib import Path

for name in ("toolport-remove-a", "toolport-remove-b"):
    home = Path("/home") / name
    path = home / ".cursor/mcp.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    original = '{\r\n  "mcpServers" : {"native":{"command":"original"}},\r\n  "theme" : "user formatting"\r\n}\r\n'
    written = json.loads(original)
    written["mcpServers"]["toolport"] = {"command": "/usr/bin/toolport-gateway"}
    written = json.dumps(written, indent=2) + "\n"
    path.write_bytes(written.encode())
    Path(f"/tmp/{name}-original").write_bytes(original.encode())
    Path(f"/tmp/{name}-connected").write_bytes(written.encode())
    sha = lambda text: hashlib.sha256(text.encode()).hexdigest()
    # This is the same versioned provenance schema as clients/restore.rs.
    record = dict(version=1, format="JsonMcpServers", configPath=str(path),
                  original=original, originalHash=sha(original), baseline=json.loads(original),
                  capturedAt=1, toolportVersion="fixture", lastWritten=written,
                  lastWrittenHash=sha(written), createdParents=[], exactEligible=True,
                  preexistingGateways=[], disconnected=False, jsoncSettings=False,
                  disconnectBefore=None)
    data = home / ".config/Toolport/backups/cursor"
    data.mkdir(parents=True, exist_ok=True)
    (data / f"original-{sha(str(path))}.json").write_text(json.dumps(record))
