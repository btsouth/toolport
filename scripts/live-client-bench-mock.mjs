import { appendFileSync, readFileSync } from "node:fs";
import { createInterface } from "node:readline";

// A downstream process for one public catalog namespace. No external calls.
const [catalogPath, namespace, trace, delayText = "0", notifyText = "0"] =
  process.argv.slice(2);
const delay = Number(delayText);
const notify = Number(notifyText);
const catalog = JSON.parse(readFileSync(catalogPath, "utf8"));
const tools = catalog
  .filter((t) => namespace === "catalog" || t.name.startsWith(`${namespace}__`))
  .map((t) => ({
    ...t,
    name: namespace === "catalog" ? t.name : t.name.slice(namespace.length + 2),
  }));
const log = (event) =>
  appendFileSync(trace, JSON.stringify({ at: Date.now(), namespace, ...event }) + "\n");
const send = (message) =>
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...message }) + "\n");
let firstList = true;
let changed = false;
const input = createInterface({ input: process.stdin });
input.on("line", async (line) => {
  const message = JSON.parse(line);
  log({
    method: message.method,
    ...(message.method === "tools/call" ? { params: message.params } : {}),
  });
  let result;
  switch (message.method) {
    case "initialize":
      result = {
        protocolVersion: message.params.protocolVersion,
        capabilities: { tools: { listChanged: true } },
        serverInfo: { name: "public-catalog-fixture", version: "1" },
      };
      break;
    case "tools/list":
      if (firstList && delay) await new Promise((resolve) => setTimeout(resolve, delay));
      firstList = false;
      result = {
        tools: changed
          ? [
              ...tools,
              {
                name: "bench_added_after_notification",
                description: "Return the notification acceptance marker.",
                inputSchema: { type: "object", properties: {} },
              },
            ]
          : tools,
      };
      break;
    case "tools/call": {
      const known =
        tools.some((t) => t.name === message.params.name) ||
        (changed && message.params.name === "bench_added_after_notification");
      result = {
        ...(known ? {} : { isError: true }),
        content: [
          {
            type: "text",
            text: JSON.stringify({
              ok: known,
              name: `${namespace}__${message.params.name}`,
              value: 1,
              url: "https://example.test/issues/42",
              rows: [],
              marker: "fixture-complete",
            }),
          },
        ],
      };
      // A call barrier makes the list-change occur during an active model turn.
      if (notify && namespace === "slack" && !changed) {
        changed = true;
        setTimeout(() => {
          log({ method: "sent/tools/list_changed" });
          send({ method: "notifications/tools/list_changed" });
        }, notify);
      }
      break;
    }
    case "resources/list":
      result = { resources: [] };
      break;
    case "resources/templates/list":
      result = { resourceTemplates: [] };
      break;
    case "prompts/list":
      result = { prompts: [] };
      break;
    case "ping":
      result = {};
      break;
    default:
      if (message.id !== undefined)
        send({
          id: message.id,
          error: { code: -32601, message: "Unknown fixture method" },
        });
      return;
  }
  if (message.id !== undefined) send({ id: message.id, result });
});
