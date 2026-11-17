#!/usr/bin/env python3
"""Seed realistic macOS client configs, then check what Toolport wrote.

usage:
  clients.py seed <home> <manifest.json>
  clients.py check <home> <manifest.json> <entries-dir> [--not-connected] [--registry=<registry.json>]
                    [--previous=<tag>]
  clients.py restored <home> <manifest.json>
  clients.py unchanged <home> <before-manifest.json>

Results are printed as results.tsv rows ("<area>\\t<status>\\t<check>\\t<detail>")
so the shell steps can append them. Exit status is 1 when any row is FAIL.

Each client below lists the path and format its own documentation gives for
macOS, with the source, so a check compares Toolport against the client rather
than against Toolport's own idea of the client.
"""

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys

try:
    import tomllib
except ModuleNotFoundError:  # Python < 3.11
    tomllib = None

AS = "Library/Application Support"

# A server the user already had. Toolport must keep it byte-for-byte in meaning.
USER_SERVER = {
    "command": "npx",
    "args": ["-y", "@modelcontextprotocol/server-filesystem", "/Users/runner/Documents"],
}

CLIENTS = [
    {
        "id": "claude-desktop",
        "name": "Claude Desktop",
        "path": f"{AS}/Claude/claude_desktop_config.json",
        "format": "json-mcpServers",
        "doc": "https://modelcontextprotocol.io/quickstart/user (macOS: ~/Library/Application Support/Claude/claude_desktop_config.json, mcpServers)",
        "seed": {"mcpServers": {"filesystem": USER_SERVER}, "globalShortcut": "Alt+Space"},
    },
    {
        "id": "claude-code",
        "name": "Claude Code",
        "path": ".claude.json",
        "marker": ".claude",
        "format": "json-mcpServers",
        "doc": "https://docs.anthropic.com/en/docs/claude-code/mcp (user scope in ~/.claude.json, mcpServers)",
        "seed": {
            "numStartups": 12,
            "installMethod": "native",
            "autoUpdates": True,
            "projects": {"/Users/runner/src/app": {"allowedTools": [], "mcpServers": {}}},
            "mcpServers": {"filesystem": {"type": "stdio", **USER_SERVER, "env": {}}},
        },
    },
    {
        "id": "cursor",
        "name": "Cursor",
        "path": ".cursor/mcp.json",
        "format": "json-mcpServers",
        "doc": "https://docs.cursor.com/context/model-context-protocol (global ~/.cursor/mcp.json, mcpServers)",
        "seed": {"mcpServers": {"filesystem": USER_SERVER}},
    },
    {
        "id": "vscode",
        "name": "VS Code",
        "path": f"{AS}/Code/User/mcp.json",
        "format": "json-vscode",
        "doc": "https://code.visualstudio.com/docs/agents/reference/mcp-configuration (servers; stdio entries require type)",
        "seed": {"servers": {"filesystem": {"type": "stdio", **USER_SERVER}}, "inputs": []},
    },
    {
        "id": "codex",
        "name": "Codex",
        "path": ".codex/config.toml",
        "format": "toml-codex",
        "doc": "https://developers.openai.com/codex/mcp ([mcp_servers.<name>] command, args, env in ~/.codex/config.toml)",
        "seed_text": (
            'model = "gpt-5-codex"\n'
            'approval_policy = "on-request"\n\n'
            "# my servers\n"
            "[mcp_servers.filesystem]\n"
            'command = "npx"\n'
            'args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/runner/Documents"]\n'
        ),
    },
    {
        "id": "windsurf",
        "name": "Devin Desktop (Cascade)",
        # Cascade reads the Devin file; ~/.codeium/windsurf only feeds optional discovery.
        "path": ".config/devin/mcp_config.json",
        "format": "json-mcpServers",
        "doc": "https://docs.devin.ai/windsurf/plugins/cascade/mcp (~/.config/devin/mcp_config.json, mcpServers)",
        "seed": {"mcpServers": {"filesystem": USER_SERVER}},
    },
    {
        "id": "gemini-cli",
        "name": "Gemini CLI",
        "path": ".gemini/settings.json",
        "format": "json-mcpServers",
        "doc": "https://github.com/google-gemini/gemini-cli/blob/main/docs/tools/mcp-server.md (~/.gemini/settings.json, mcpServers)",
        "seed": {"theme": "GitHub", "mcpServers": {"filesystem": USER_SERVER}},
    },
    {
        "id": "github-copilot-cli",
        "name": "GitHub Copilot CLI",
        "path": ".copilot/mcp-config.json",
        "format": "json-copilot",
        "doc": "https://docs.github.com/en/copilot/how-tos/use-copilot-agents/use-copilot-cli (~/.copilot/mcp-config.json, mcpServers with tools)",
        "seed": {"mcpServers": {"filesystem": {"type": "local", **USER_SERVER, "tools": ["*"]}}},
    },
    {
        "id": "zed",
        "name": "Zed",
        "path": ".config/zed/settings.json",
        "format": "jsonc-zed",
        "doc": "https://zed.dev/docs/ai/mcp (~/.config/zed/settings.json, context_servers with command, args, env)",
        "seed_text": (
            "// Zed settings\n"
            "{\n"
            '  "theme": "One Dark",\n'
            '  "buffer_font_size": 14,\n'
            "  // existing MCP servers\n"
            '  "context_servers": {\n'
            '    "filesystem": {\n'
            '      "command": "npx",\n'
            '      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/Users/runner/Documents"],\n'
            '      "env": {}\n'
            "    }\n"
            "  }\n"
            "}\n"
        ),
    },
    {
        "id": "opencode",
        "name": "OpenCode",
        "path": ".config/opencode/opencode.json",
        "format": "json-opencode",
        "doc": "https://opencode.ai/docs/mcp-servers/ (~/.config/opencode/opencode.json, mcp.<name> type local, command array, environment)",
        "seed": {
            "$schema": "https://opencode.ai/config.json",
            "mcp": {
                "filesystem": {
                    "type": "local",
                    "command": ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/Users/runner/Documents"],
                    "enabled": True,
                }
            },
        },
    },
    {
        "id": "goose",
        "name": "Goose",
        "path": ".config/goose/config.yaml",
        "format": "yaml-goose",
        # Settled 2026-11-15 from Goose v1.59.1 source: crates/goose/src/config/paths.rs
        # uses etcetera::choose_app_strategy, which is Xdg on macOS (~/.config/goose),
        # never ~/Library/Application Support. Goose Desktop runs the same goosed config.
        "doc": "https://goose-docs.ai/docs/guides/config-files (macOS/Linux: ~/.config/goose/config.yaml, extensions with cmd, args, envs); source: https://github.com/aaif-goose/goose/blob/2365bc5e898a38133c373eef2d1357416acd66e2/crates/goose/src/config/paths.rs",
        # Where Toolport 2.3.x writes on macOS instead; Goose never reads it.
        "wrong_paths": ["Library/Application Support/Block/goose/config.yaml"],
        "seed_text": (
            "GOOSE_PROVIDER: openai\n"
            "GOOSE_MODEL: gpt-4o\n"
            "extensions:\n"
            "  developer:\n"
            "    enabled: true\n"
            "    name: developer\n"
            "    type: builtin\n"
            "    timeout: 300\n"
        ),
    },
    {
        "id": "kiro",
        "name": "Kiro",
        "path": ".kiro/settings/mcp.json",
        "marker": ".kiro",
        "format": "json-mcpServers",
        "doc": "https://kiro.dev/docs/mcp/configuration/ (~/.kiro/settings/mcp.json, mcpServers)",
        "seed": {"mcpServers": {"filesystem": {**USER_SERVER, "disabled": False}}},
    },
]

BY_ID = {c["id"]: c for c in CLIENTS}


def seed_text(client):
    if "seed_text" in client:
        return client["seed_text"]
    return json.dumps(client["seed"], indent=2) + "\n"


def sha256(path):
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


def row(area, status, check, detail=""):
    detail = str(detail).replace("\t", " ").replace("\n", " ")
    return f"{area}\t{status}\t{check}\t{detail}"


def seed(home, manifest_path):
    manifest = {}
    for c in CLIENTS:
        path = os.path.join(home, c["path"])
        os.makedirs(os.path.dirname(path), exist_ok=True)
        if c.get("marker"):
            os.makedirs(os.path.join(home, c["marker"]), exist_ok=True)
        with open(path, "w", encoding="utf-8") as f:
            f.write(seed_text(c))
        manifest[c["id"]] = {"path": path, "sha256": sha256(path), "name": c["name"]}
    # Keep the original bytes beside the manifest for byte-exact comparisons.
    originals = os.path.splitext(manifest_path)[0] + "-originals"
    os.makedirs(originals, exist_ok=True)
    for cid, item in manifest.items():
        shutil.copyfile(item["path"], os.path.join(originals, cid))
    with open(manifest_path, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2)
    return [row("clients", "INFO", f"seeded {c['name']}", c["path"]) for c in CLIENTS]


def strip_jsonc(text):
    """Remove // and /* */ comments and trailing commas outside strings."""
    out, i, n, in_str = [], 0, len(text), False
    while i < n:
        ch = text[i]
        if in_str:
            out.append(ch)
            if ch == "\\" and i + 1 < n:
                out.append(text[i + 1])
                i += 2
                continue
            if ch == '"':
                in_str = False
        elif ch == '"':
            in_str = True
            out.append(ch)
        elif text.startswith("//", i):
            while i < n and text[i] != "\n":
                i += 1
            continue
        elif text.startswith("/*", i):
            end = text.find("*/", i + 2)
            i = n if end < 0 else end + 2
            continue
        else:
            out.append(ch)
        i += 1
    return re.sub(r",(\s*[}\]])", r"\1", "".join(out))


def load_yaml(path):
    try:
        import yaml  # type: ignore

        with open(path, encoding="utf-8") as f:
            return yaml.safe_load(f)
    except ModuleNotFoundError:
        out = subprocess.run(
            ["ruby", "-ryaml", "-rjson", "-e", "puts YAML.safe_load(File.read(ARGV[0])).to_json", path],
            capture_output=True,
            text=True,
            check=True,
        )
        return json.loads(out.stdout)


def load(client, path):
    fmt = client["format"]
    with open(path, encoding="utf-8") as f:
        text = f.read()
    if fmt.startswith("toml"):
        if tomllib is None:
            raise RuntimeError("tomllib needs Python 3.11+")
        return tomllib.loads(text)
    if fmt.startswith("yaml"):
        return load_yaml(path)
    if fmt.startswith("jsonc"):
        return json.loads(strip_jsonc(text))
    return json.loads(text)


def entry_of(client, data):
    """Return (entry_for_mcp_call, problems) from a parsed config."""
    fmt = client["format"]
    problems = []
    if fmt in ("json-mcpServers", "json-copilot"):
        e = (data.get("mcpServers") or {}).get("toolport")
        if e is None:
            return None, ["no mcpServers.toolport entry"]
        if fmt == "json-copilot" and e.get("tools") != ["*"]:
            problems.append(f"tools is {e.get('tools')!r}, Copilot CLI needs a tools allowlist such as [\"*\"]")
        cmd, args, env = e.get("command"), e.get("args", []), e.get("env", {})
    elif fmt == "json-vscode":
        e = (data.get("servers") or {}).get("toolport")
        if e is None:
            return None, ["no servers.toolport entry"]
        if e.get("type") != "stdio":
            problems.append(f"type is {e.get('type')!r}; VS Code's reference lists type as required (\"stdio\")")
        cmd, args, env = e.get("command"), e.get("args", []), e.get("env", {})
    elif fmt == "toml-codex":
        e = (data.get("mcp_servers") or {}).get("toolport")
        if e is None:
            return None, ["no [mcp_servers.toolport] table"]
        cmd, args, env = e.get("command"), e.get("args", []), e.get("env", {})
    elif fmt == "jsonc-zed":
        e = (data.get("context_servers") or {}).get("toolport")
        if e is None:
            return None, ["no context_servers.toolport entry"]
        if isinstance(e.get("command"), dict):
            inner = e["command"]
            cmd, args, env = inner.get("path"), inner.get("args", []), inner.get("env", {})
        else:
            cmd, args, env = e.get("command"), e.get("args", []), e.get("env", {})
    elif fmt == "json-opencode":
        e = (data.get("mcp") or {}).get("toolport")
        if e is None:
            return None, ["no mcp.toolport entry"]
        if e.get("type") != "local":
            problems.append(f"type is {e.get('type')!r}, OpenCode needs \"local\"")
        command = e.get("command")
        if not isinstance(command, list) or not command:
            return None, problems + [f"command is {command!r}, OpenCode needs an array"]
        cmd, args, env = command[0], command[1:], e.get("environment", {})
        if e.get("enabled") is False:
            problems.append("entry is disabled")
    elif fmt == "yaml-goose":
        e = ((data or {}).get("extensions") or {}).get("toolport")
        if e is None:
            return None, ["no extensions.toolport entry"]
        if e.get("type") != "stdio":
            problems.append(f"type is {e.get('type')!r}, Goose needs stdio")
        if not e.get("enabled", False):
            problems.append("extension is not enabled")
        cmd, args, env = e.get("cmd"), e.get("args", []), e.get("envs", {})
    else:
        return None, [f"unknown format {fmt}"]
    if not isinstance(cmd, str) or not cmd:
        return None, problems + [f"command is {cmd!r}"]
    if "AppTranslocation" in cmd or cmd.startswith("/Volumes/") or cmd.startswith("/private/var/folders/"):
        problems.append(f"command {cmd} is a temporary location that disappears after a restart or eject")
    if not os.path.isabs(cmd):
        problems.append(f"command {cmd} is not an absolute path")
    elif not os.access(cmd, os.X_OK):
        problems.append(f"command {cmd} is not executable")
    if not isinstance(args, list):
        problems.append(f"args is {args!r}, not a list")
        args = []
    if not isinstance(env, dict):
        problems.append(f"env is {env!r}, not a table")
        env = {}
    if env.get("TOOLPORT_CLIENT_ID") != client["id"]:
        problems.append(f"TOOLPORT_CLIENT_ID is {env.get('TOOLPORT_CLIENT_ID')!r}, expected {client['id']!r}")
    return {"command": cmd, "args": [str(a) for a in args], "env": {k: str(v) for k, v in env.items()}}, problems


USER_TABLES = {
    "json-mcpServers": "mcpServers",
    "json-copilot": "mcpServers",
    "json-vscode": "servers",
    "toml-codex": "mcp_servers",
    "jsonc-zed": "context_servers",
    "json-opencode": "mcp",
}


def user_server(client, data):
    """The user's own pre-existing server object in a parsed config, or None."""
    if client["format"] == "yaml-goose":
        return ((data or {}).get("extensions") or {}).get("developer")
    return ((data or {}).get(USER_TABLES[client["format"]]) or {}).get("filesystem")


def seeded_user_server(client):
    """The same object as it was seeded, parsed with the client's own loader."""
    fmt = client["format"]
    text = seed_text(client)
    if fmt.startswith("toml"):
        data = tomllib.loads(text)
    elif fmt.startswith("yaml"):
        import tempfile

        with tempfile.NamedTemporaryFile("w", suffix=".yaml", delete=False) as f:
            f.write(text)
        try:
            data = load_yaml(f.name)
        finally:
            os.unlink(f.name)
    elif fmt.startswith("jsonc"):
        data = json.loads(strip_jsonc(text))
    else:
        data = json.loads(text)
    return user_server(client, data)


def user_server_kept(client, data):
    """True only when the user's server is still there with every field unchanged."""
    return user_server(client, data) == seeded_user_server(client)


def imported_into(registry):
    """True when Toolport's registry holds the seeded filesystem server unchanged:
    same command, same arguments, stdio, and no environment added."""
    if not registry or not os.path.exists(registry):
        return False
    with open(registry, encoding="utf-8") as f:
        reg = json.load(f)
    for s in reg.get("servers", []):
        if (
            s.get("command") == USER_SERVER["command"]
            and s.get("args") == USER_SERVER["args"]
            and s.get("transport", "stdio") == "stdio"
            and not s.get("env")
            and not s.get("url")
        ):
            return True
    return False


def check(home, manifest_path, entries_dir, expect_connected=True, registry=None):
    with open(manifest_path, encoding="utf-8") as f:
        manifest = json.load(f)
    os.makedirs(entries_dir, exist_ok=True)
    rows = []
    for cid, item in manifest.items():
        client = BY_ID[cid]
        area = "clients"
        path = item["path"]
        if not os.path.exists(path):
            rows.append(row(area, "FAIL", f"{client['name']} config exists", f"{path} is missing"))
            continue
        try:
            data = load(client, path)
        except Exception as e:  # noqa: BLE001 - report any parse failure
            rows.append(row(area, "FAIL", f"{client['name']} config parses", f"{path}: {e}"))
            continue
        try:
            entry, problems = entry_of(client, data)
            kept = user_server_kept(client, data)
            found = user_server(client, data)
            imported = found is None and imported_into(registry)
        except Exception as e:  # noqa: BLE001 - a malformed config is a FAIL, not a crash
            rows.append(row(area, "FAIL", f"{client['name']} config has the client's documented shape", f"{path}: {type(e).__name__}: {e}"))
            continue
        if entry is None:
            status = "FAIL" if expect_connected else "INFO"
            detail = f"{'; '.join(problems)} ({path})"
            for wrong in client.get("wrong_paths", []):
                wp = os.path.join(home, wrong)
                if os.path.exists(wp):
                    detail += f"; Toolport wrote {wp} instead, which {client['name']} never reads. Doc: {client['doc']}"
            rows.append(row(area, status, f"{client['name']} has a Toolport entry where it reads its config", detail))
            continue
        with open(os.path.join(entries_dir, f"{cid}.json"), "w", encoding="utf-8") as f:
            json.dump(entry, f, indent=2)
        if problems:
            rows.append(row(area, "FAIL", f"{client['name']} entry matches the client's documented format", f"{'; '.join(problems)}. Doc: {client['doc']}"))
        else:
            rows.append(row(area, "PASS", f"{client['name']} entry matches the client's documented format", f"{entry['command']} at {path}"))
        if kept:
            rows.append(row(area, "PASS", f"{client['name']} keeps the user's existing server"))
        elif imported:
            rows.append(row(area, "PASS", f"{client['name']} keeps the user's existing server", "moved unchanged into Toolport's registry by the connect review"))
        else:
            why = "is gone" if found is None else f"was changed to {json.dumps(found)[:200]}"
            rows.append(row(area, "FAIL", f"{client['name']} keeps the user's existing server", f"the seeded server {why} in {path}, and Toolport's registry has no unchanged copy"))
    return rows


# The repairs an upgrade to 2.3.7 makes to Toolport's own entry on its first start.
# Each is allowed only when the new file shows it, and nothing else may change:
# - the `type` these clients document as required, added when it is missing (#1340);
# - the gateway path written in canonical form (#1340);
# - Codex only: the gateway settings Toolport writes when the user has not set them
#   or an alias for them (`CODEX_GATEWAY_SETTINGS` in clients.rs, #1336), with
#   exactly the values Toolport writes;
# - `TOOLPORT_ATTRIBUTION_ID` set to the client's own id, added to the entry's env
#   when Toolport rewrites the entry (#1104).
REPAIRED_TYPES = {"cursor": "stdio", "vscode": "stdio", "github-copilot-cli": "local"}
ATTRIBUTION_ID = "TOOLPORT_ATTRIBUTION_ID"
CODEX_SETTINGS = [
    ("supports_parallel_tool_calls", (), True),
    ("startup_timeout_sec", ("startup_timeout_ms",), 75),
    ("tool_timeout_sec", (), 600),
]


def comments(fmt, text):
    """The comments in a TOML or JSONC file, in order, which parsing drops."""
    if fmt.startswith("toml"):
        marks = ("#",)
    elif fmt.startswith("jsonc"):
        marks = ("//", "/*")
    else:
        return []
    found = []
    for line in text.splitlines():
        quoted = escaped = False
        for i, ch in enumerate(line):
            if escaped:
                escaped = False
            elif ch == "\\" and quoted:
                escaped = True
            elif ch == '"':
                quoted = not quoted
            elif not quoted and line.startswith(marks, i):
                found.append(line[i:].strip())
                break
    return found


def upgrade_repairs(cid, before_path, after_path):
    """The repairs that turn the config at before_path into the one at after_path,
    or None when anything else changed (or either file cannot be read)."""
    import copy

    client = BY_ID.get(cid)
    if client is None or client["format"] not in USER_TABLES:
        return None
    try:
        before, after = load(client, before_path), load(client, after_path)
        with open(before_path, encoding="utf-8") as f, open(after_path, encoding="utf-8") as g:
            # Parsing drops comments; the user's must all still be there.
            if comments(client["format"], f.read()) != comments(client["format"], g.read()):
                return None
    except Exception:
        return None
    table = USER_TABLES[client["format"]]
    expected = copy.deepcopy(before)
    entry = ((expected or {}).get(table) or {}).get("toolport")
    new = ((after or {}).get(table) or {}).get("toolport") if isinstance(after, dict) else None
    if not isinstance(entry, dict) or not isinstance(new, dict):
        return None
    repairs = []
    kind = REPAIRED_TYPES.get(cid)
    if kind and "type" not in entry and new.get("type") == kind:
        entry["type"] = kind
        repairs.append(f'added type "{kind}"')
    command = entry.get("command")
    if (
        isinstance(command, str)
        and os.path.isabs(command)
        and os.path.normpath(command) != command
        and new.get("command") == os.path.normpath(command)
    ):
        entry["command"] = os.path.normpath(command)
        repairs.append(f"gateway path made canonical ({entry['command']})")
    if client["format"] == "toml-codex":
        for key, aliases, value in CODEX_SETTINGS:
            if any(k in entry for k in (key, *aliases)) or key not in new:
                continue
            if new[key] == value and type(new[key]) is type(value):
                entry[key] = value
                repairs.append(f"{key} = {str(value).lower()} added")
    env, new_env = entry.get("env"), new.get("env")
    if (
        isinstance(new_env, dict)
        and new_env.get(ATTRIBUTION_ID) == cid
        and (env is None or (isinstance(env, dict) and ATTRIBUTION_ID not in env))
    ):
        entry.setdefault("env", {})[ATTRIBUTION_ID] = cid
        repairs.append(f"{ATTRIBUTION_ID} added")
    if not repairs or expected != after:
        return None
    return repairs


def compare(home, manifest_path, label, allow_upgrade_repairs=False):
    with open(manifest_path, encoding="utf-8") as f:
        manifest = json.load(f)
    originals = os.path.splitext(manifest_path)[0] + "-originals"
    rows = []
    for cid, item in manifest.items():
        name = item["name"]
        path = item["path"]
        if not os.path.exists(path):
            rows.append(row("clients", "FAIL", f"{name} {label}", f"{path} is missing"))
            continue
        try:
            actual = sha256(path)
        except OSError as e:
            rows.append(row("clients", "FAIL", f"{name} {label}", f"{path} unreadable: {e}"))
            continue
        if actual == item["sha256"]:
            rows.append(row("clients", "PASS", f"{name} {label}", f"sha256 {actual[:16]}"))
        else:
            try:
                with open(os.path.join(originals, cid), encoding="utf-8", errors="replace") as f:
                    before = f.read()
                with open(path, encoding="utf-8", errors="replace") as f:
                    after = f.read()
            except OSError as e:
                rows.append(row("clients", "FAIL", f"{name} {label}", f"{path} differs; no diff: {e}"))
                continue
            repairs = upgrade_repairs(cid, os.path.join(originals, cid), path) if allow_upgrade_repairs else None
            if repairs:
                rows.append(row("clients", "PASS", f"{name} {label}", "only Toolport's entry was repaired: " + "; ".join(repairs)))
                continue
            import difflib

            diff = "".join(difflib.unified_diff(before.splitlines(True), after.splitlines(True), "before", "after", n=0))
            # The detail is cut short; keep the whole changed file with the evidence.
            kept = os.path.splitext(manifest_path)[0] + "-changed"
            os.makedirs(kept, exist_ok=True)
            shutil.copyfile(path, os.path.join(kept, cid))
            detail = f"{path} differs (whole file: {os.path.basename(kept)}/{cid}): {diff[:300]}"
            rows.append(row("clients", "FAIL", f"{name} {label}", detail))
    return rows


def snapshot(manifest_path, out_path):
    """Record the current hashes of the files in a manifest (for upgrade checks)."""
    with open(manifest_path, encoding="utf-8") as f:
        manifest = json.load(f)
    for item in manifest.values():
        item["sha256"] = sha256(item["path"]) if os.path.exists(item["path"]) else None
    originals = os.path.splitext(out_path)[0] + "-originals"
    os.makedirs(originals, exist_ok=True)
    for cid, item in manifest.items():
        if item["sha256"]:
            shutil.copyfile(item["path"], os.path.join(originals, cid))
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2)
    return []


def as_previous_release(rows, tag):
    """The previous release only sets up an upgrade and is not under test: what
    it gets wrong is noted as UNTESTED, never a FAIL. Tool calls through the
    entries it wrote are checked separately and still fail."""
    out = []
    for r in rows:
        area, status, check_name, detail = (r.split("\t", 3) + ["", "", "", ""])[:4]
        if status == "FAIL":
            r = row(area, "UNTESTED", check_name, f"the previous release ({tag}) is not under test: {detail}")
        out.append(r)
    return out


def main(argv):
    if len(argv) < 4:
        print(__doc__, file=sys.stderr)
        return 2
    cmd, home, manifest = argv[1], argv[2], argv[3]
    if cmd == "seed":
        rows = seed(home, manifest)
    elif cmd == "check":
        if len(argv) < 5:
            print(__doc__, file=sys.stderr)
            return 2
        reg = next((a.split("=", 1)[1] for a in argv if a.startswith("--registry=")), None)
        rows = check(home, manifest, argv[4], "--not-connected" not in argv, reg)
        previous = next((a.split("=", 1)[1] for a in argv if a.startswith("--previous=")), None)
        if previous:
            rows = as_previous_release(rows, previous)
    elif cmd == "restored":
        rows = compare(home, manifest, "config restored byte-exact")
    elif cmd == "unchanged":
        rows = compare(home, manifest, "config untouched by the upgrade", allow_upgrade_repairs=True)
    elif cmd == "updated":
        rows = compare(home, manifest, "config untouched by the in-app update", allow_upgrade_repairs=True)
    elif cmd == "snapshot":
        rows = snapshot(manifest, argv[4] if len(argv) > 4 else manifest)
    else:
        print(__doc__, file=sys.stderr)
        return 2
    for r in rows:
        print(r)
    return 1 if any("\tFAIL\t" in r for r in rows) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
