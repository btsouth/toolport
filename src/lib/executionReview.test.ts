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
    ).toEqual([]);
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
