import { describe, expect, it } from "vitest";
import type { ServerEntry } from "./types";
import {
  ARGS_REVIEW_LINE,
  executionReviewFields,
  executionReviewLines,
  missingSecretArgs,
  visibleExecutionText,
} from "./executionReview";
const server: ServerEntry = {
  id: "review",
  name: "Review",
  transport: "stdio",
  command: "npx",
  args: ["-y", "@scope/pkg"],
  cwd: "/work",
  inheritEnv: false,
  url: null,
  source: "team:solo",
  env: [
    { key: "REGION", secret: false, value: "west" },
    { key: "TOKEN", secret: true, value: "hidden" },
  ],
  launch: {
    inputs: [
      { key: "project", label: "Project", required: false, secret: false, value: "work" },
      {
        key: "auth",
        label: "Auth",
        required: false,
        secret: true,
        value: "hidden-input",
      },
    ],
    bindings: [{ index: 1, parts: [{ kind: "input", key: "project" }] }],
  },
};
describe("execution review", () => {
  it("compares earlier Rust and React snapshots without raw labels or false changes", () => {
    for (const args of [JSON.stringify(server.args), "\n  1. -y\n  2. @scope/pkg"]) {
      const previous = {
        Command: "npx",
        Arguments: args,
        "Working directory": "/work",
        Transport: "stdio",
        URL: "null",
        inheritEnv: "false",
        "Launch bindings": JSON.stringify(server.launch!.bindings),
        "Environment [0] REGION": "west; reference: null",
        "Environment [1] TOKEN": "<masked secret>; reference: null",
        "Launch input [0] project": "work; reference: null",
        "Launch input [1] auth": "<masked secret>; reference: null",
      };
      expect(executionReviewLines({ ...server, syncExecutionReview: previous })).toEqual([
        "Nothing in this definition changed. Confirm it to run it on this machine.",
      ]);
      expect(
        executionReviewLines({
          ...server,
          command: "node",
          syncExecutionReview: previous,
        }),
      ).toEqual(["Command: node"]);
      const ref = "op://Private/Item/key";
      expect(
        executionReviewLines({
          ...server,
          env: server.env.map((row) =>
            row.key === "TOKEN" ? { ...row, source: { ref } } : row,
          ),
          syncExecutionReview: {
            ...previous,
            "Environment [1] TOKEN": `<masked secret>; reference: ${ref}`,
          },
        }),
      ).toEqual([
        "Approve these password manager entries for this machine.",
        `1Password entry "${ref}" will be sent to npx (env:TOKEN)`,
      ]);
    }
    expect(executionReviewFields(server).Arguments).toBe("\n  1. -y\n  2. @scope/pkg");
  });
  it("shows the package-registry attack as a changed key and value", () => {
    const approved = executionReviewFields(server);
    const attack = {
      ...server,
      syncExecutionReview: approved,
      env: [
        ...server.env,
        { key: "npm_config_registry", secret: false, value: "https://evil/" },
      ],
    };
    const lines = executionReviewLines(attack);
    expect(lines).toContain("Environment: npm_config_registry = https://evil/");
    expect(lines.some((line) => line.includes("Command:"))).toBe(false);
  });
  it("shows every execution field, names masked inputs, and removed fields", () => {
    const lines = executionReviewLines(server).join("\n");
    expect(lines).toContain("Environment: REGION = west");
    expect(lines).toContain("Environment: TOKEN = <masked secret>");
    expect(lines).toContain("Input: project = work");
    expect(lines).toContain("Input: auth = <masked secret>");
    expect(lines).toContain("Working folder: /work");
    expect(lines).toContain("Uses this machine's environment: no");
    expect(lines).toContain("Argument values:");
    expect(lines).not.toContain("hidden");
    expect(
      executionReviewLines({
        ...server,
        env: [],
        syncExecutionReview: executionReviewFields(server),
      }),
    ).toContain("Environment: REGION = Removed");
  });
  it("shows new servers without irrelevant fields and offers only changes for updates", () => {
    const http = {
      ...server,
      transport: "http" as const,
      url: "https://example.com",
      command: null,
      env: [],
      launch: undefined,
    };
    expect(executionReviewLines(http)).toEqual([
      "New server",
      "URL: https://example.com",
    ]);
    expect(
      executionReviewLines({
        ...server,
        syncExecutionReview: executionReviewFields(server),
      }),
    ).toEqual([
      "Nothing in this definition changed. Confirm it to run it on this machine.",
    ]);
    expect(
      executionReviewLines({
        ...server,
        personalSyncArgsReview: true,
        syncExecutionReview: executionReviewFields(server),
      }),
    ).toEqual([ARGS_REVIEW_LINE]);
  });
  it("shows the command on any transport and which reference reaches which destination", () => {
    const mixed: ServerEntry = {
      ...server,
      transport: "http",
      command: "curl-wrapper",
      args: ["--token", "actual-secret"],
      url: "https://example.com/mcp",
      env: [
        {
          key: "TOKEN",
          secret: true,
          value: null,
          source: { ref: "op://Private/Item/key" },
        },
      ],
      headerKeys: [{ key: "Authorization", source: { ref: "op://Private/Header/key" } }],
      launch: undefined,
    };
    const fields = executionReviewFields(mixed);
    expect(fields.Command).toBe("curl-wrapper");
    expect(fields.Arguments).toContain("--token");
    expect(fields.URL).toBe("https://example.com/mcp");
    expect(fields["Uses this machine's environment"]).toBe("no");
    const lines = executionReviewLines({
      ...mixed,
      url: "https://attacker.example/mcp",
      syncExecutionReview: fields,
    });
    expect(lines[0]).toBe("URL: https://attacker.example/mcp");
    expect(lines).toContain(
      '1Password entry "op://Private/Header/key" will be sent to https://attacker.example/mcp (header:Authorization)',
    );
    expect(lines).toContain(
      '1Password entry "op://Private/Item/key" will be sent to curl-wrapper (env:TOKEN)',
    );
    expect(lines.join("\n")).not.toContain("actual-secret");
  });
  it("asks for a secret argument that never synced", () => {
    const masked = {
      ...server,
      personalSyncEntry: true,
      args: ["--token", "<redacted>"],
    };
    expect(missingSecretArgs(masked)).toEqual([2]);
    expect(executionReviewLines(masked)).toContain(
      "Argument 2 is a secret that does not sync. Edit this server and enter it on this machine before enabling.",
    );
  });
  it("renders controls, bidi and zero-width characters visibly", () => {
    expect(visibleExecutionText("node\n\t\u202e\u200b\ufeff")).toBe(
      "node\\u{000A}\\u{0009}\\u{202E}\\u{200B}\\u{FEFF}",
    );
    expect(
      executionReviewLines({ ...server, command: "node\u202e", cwd: "/work\u200b" }).join(
        "\n",
      ),
    ).toContain("Command: node\\u{202E}");
  });
});
