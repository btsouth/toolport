import { describe, expect, it } from "vitest";
import type { ServerEntry } from "./types";
import {
  executionReviewFields,
  executionReviewLines,
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
    expect(lines).toContain(
      "CHANGED: Environment [2] npm_config_registry: https://evil/; reference: null",
    );
    expect(lines.find((line) => line.includes("Command:"))).toBe("Command: npx");
  });
  it("shows every execution field, names masked inputs, and removed fields", () => {
    const lines = executionReviewLines(server).join("\n");
    expect(lines).toContain("Environment [0] REGION: west");
    expect(lines).toContain("Environment [1] TOKEN: <masked secret>");
    expect(lines).toContain("Launch input [0] project: work");
    expect(lines).toContain("Launch input [1] auth: <masked secret>");
    expect(lines).toContain("Working directory: /work");
    expect(lines).toContain("inheritEnv: false");
    expect(lines).toContain("Launch bindings:");
    expect(lines).not.toContain("hidden");
    expect(
      executionReviewLines({
        ...server,
        env: [],
        syncExecutionReview: executionReviewFields(server),
      }),
    ).toContain("CHANGED: Environment [0] REGION: removed");
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
