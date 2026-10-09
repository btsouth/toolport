#!/usr/bin/env node
// Offline payload audit. The Rust harness exercises gateway dispatch with fake data.
import { spawnSync } from "node:child_process";
import { mkdirSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const output = resolve(root, ".verify", "token-budget.json");
mkdirSync(resolve(root, ".verify"), { recursive: true });
const result = spawnSync(
  "cargo",
  [
    "test",
    "--locked",
    "--manifest-path",
    "src-tauri/Cargo.toml",
    "--no-default-features",
    "--features",
    "test-support",
    "--bin",
    "toolport-gateway",
    "tests::token_budget_audit",
    "--",
    "--ignored",
    "--exact",
    "--nocapture",
  ],
  {
    cwd: root,
    env: { ...process.env, TOOLPORT_TOKEN_AUDIT_OUTPUT: output },
    stdio: "inherit",
    timeout: 3_000_000,
  },
);
if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status || 1);
const { measurements } = JSON.parse(readFileSync(output, "utf8"));
console.log("Payload costs, excluding vendor framing and prompt cache effects:");
for (const [name, count] of Object.entries(measurements)) {
  console.log(
    `${name}: ${count.o200k_base} o200k_base tokens; ~${count.claude_approx_chars_div_4} Claude chars/4 tokens; ${count.bytes} bytes`,
  );
}
console.log(`Payloads and measurements: ${output}`);
