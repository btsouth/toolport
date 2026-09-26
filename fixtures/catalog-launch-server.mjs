/* global process */
// Contract fixture only. It never connects to a provider or proves provider auth.
import readline from "node:readline";

const args = process.argv.slice(2);
const contracts = [
  ["-y", "@twilio-alpha/mcp", "ACreview/SKreview:review secret:$literal & value"],
  [
    "-y",
    "@modelcontextprotocol/server-postgres",
    "postgresql://user:review%40secret@localhost/test",
  ],
  ["-y", "@modelcontextprotocol/server-filesystem", "/fixture/directory with spaces"],
  [
    "--from",
    "redis-mcp-server@latest",
    "redis-mcp-server",
    "--url",
    "redis://user:review%40secret@localhost:6379/0",
  ],
  ["awslabs.aws-api-mcp-server@latest"],
  ["mcp-server-qdrant"],
];
if (!contracts.some((expected) => JSON.stringify(expected) === JSON.stringify(args))) {
  // Deliberately echo escaped and truncated credentials to exercise redaction.
  process.stderr.write(`invalid arguments: ${JSON.stringify(args).slice(-100)}\n`);
  process.exit(1);
}
if (
  args[0] === "awslabs.aws-api-mcp-server@latest" &&
  (process.env.AWS_ACCESS_KEY_ID || process.env.AWS_SECRET_ACCESS_KEY)
) {
  process.exit(2);
}
if (
  args[0] === "mcp-server-qdrant" &&
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
