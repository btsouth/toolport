/* global process */
// Contract fixture only. It never connects to a provider or proves provider auth.
import readline from "node:readline";
import { readFileSync } from "node:fs";
import { URL } from "node:url";
import http from "node:http";

const pins = JSON.parse(
  readFileSync(new URL("../src-tauri/catalog-pins.json", import.meta.url), "utf8"),
);
const spec = (runner, name, separator = "@") =>
  `${name}${separator}${pins[`${runner}:${name}`].version}`;

const args = process.argv.slice(2);
const contracts = [
  [
    "-y",
    spec("npx", "@twilio-alpha/mcp"),
    "ACreview/SKreview:review secret:$literal & value",
  ],
  [
    "-y",
    spec("npx", "@modelcontextprotocol/server-postgres"),
    "postgresql://user:review%40secret@localhost/test",
  ],
  [
    "-y",
    spec("npx", "@modelcontextprotocol/server-filesystem"),
    "/fixture/directory with spaces",
  ],
  [
    "--from",
    spec("uvx", "redis-mcp-server", "=="),
    "redis-mcp-server",
    "--url",
    "redis://user:review%40secret@localhost:6379/0",
  ],
  [spec("uvx", "awslabs.aws-api-mcp-server")],
  [spec("uvx", "mcp-server-qdrant")],
];
if (!contracts.some((expected) => JSON.stringify(expected) === JSON.stringify(args))) {
  // Deliberately echo escaped and truncated credentials to exercise redaction.
  process.stderr.write(`invalid arguments: ${JSON.stringify(args).slice(-100)}\n`);
  process.exit(1);
}
if (
  args[0] === spec("uvx", "awslabs.aws-api-mcp-server") &&
  (process.env.AWS_ACCESS_KEY_ID || process.env.AWS_SECRET_ACCESS_KEY)
) {
  process.exit(2);
}
if (
  args[0] === spec("uvx", "mcp-server-qdrant") &&
  (process.env.QDRANT_URL !== "http://127.0.0.1:6333" ||
    process.env.QDRANT_API_KEY ||
    process.env.COLLECTION_NAME)
) {
  process.exit(3);
}

for await (const line of readline.createInterface({ input: process.stdin })) {
  const request = JSON.parse(line);
  if (!Object.hasOwn(request, "id")) continue;
  let result = {};
  if (request.method === "initialize") {
    // The gateway fixture can hold one handshake until a partial catalog was
    // observed. Release comes from the test, not an elapsed-time assumption.
    if (process.env.TOOLPORT_FIXTURE_CATALOG_GATE) {
      await new Promise((resolve, reject) => {
        http
          .get(process.env.TOOLPORT_FIXTURE_CATALOG_GATE, (response) => {
            response.resume();
            response.on("end", resolve);
          })
          .on("error", reject);
      });
    }
    result = {
      protocolVersion: request.params.protocolVersion,
      capabilities: { tools: {} },
      serverInfo: { name: "catalog-contract", version: "1" },
    };
  } else if (request.method === "tools/list") {
    result = {
      tools: [
        {
          name: "ready",
          description: "Read fixture readiness",
          inputSchema: { type: "object", properties: {} },
        },
      ],
    };
  } else if (request.method === "tools/call") {
    result = { content: [{ type: "text", text: "fixture ready" }] };
  }
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: request.id, result }) + "\n");
}
