/* global process */
// A tiny stdio MCP fixture that refuses to start without its launch argument.
import readline from "node:readline";

if (process.argv[2] !== "account/key:secret") {
  process.stderr.write(
    `missing or invalid launch argument: ${process.argv[2] ?? "<none>"}\n`,
  );
  process.exit(1);
}

const input = readline.createInterface({ input: process.stdin });
for await (const line of input) {
  let request;
  try {
    request = JSON.parse(line);
  } catch {
    continue;
  }
  if (!Object.hasOwn(request, "id")) continue;
  let result = {};
  if (request.method === "initialize") {
    result = {
      protocolVersion: request.params?.protocolVersion ?? "2025-06-18",
      capabilities: { tools: {} },
      serverInfo: { name: "arg-fixture", version: "1" },
    };
  } else if (request.method === "tools/list") {
    result = {
      tools: [
        {
          name: "ready",
          description: "Reports ready",
          inputSchema: { type: "object", properties: {} },
        },
      ],
    };
  }
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: request.id, result }) + "\n");
}
