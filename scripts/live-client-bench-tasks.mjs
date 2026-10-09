// All requests derive from the public search evaluation development split.
// Values are fictional and results are served by the local mock only.
export const specifications = [
  [
    "r4-001",
    "Refund charge ch_fixture because it was a duplicate.",
    { body: { charge: "ch_fixture", reason: "duplicate" } },
  ],
  [
    "r4-002",
    "Use email alex@example.test and name Alex.",
    { body: { email: "alex@example.test", name: "Alex" } },
  ],
  ["r4-009", "The charge ID is ch_fixture.", { charge: "ch_fixture" }],
  ["r4-010", "Capture charge ch_fixture.", { charge: "ch_fixture" }],
  ["r4-067", "The deployment ID is dpl_fixture.", { idOrUrl: "dpl_fixture" }],
  [
    "r4-077",
    "Project demo, key API_URL, value https://api.example.test, type plain, target production.",
    {
      idOrName: "demo",
      body: {
        key: "API_URL",
        value: "https://api.example.test",
        type: "plain",
        target: ["production"],
      },
    },
  ],
  [
    "r4-105",
    "Zone ID 023e105f4ecef8ad9ca31a8372d0c353.",
    { zone_id: "023e105f4ecef8ad9ca31a8372d0c353" },
  ],
  [
    "r4-111",
    "Zone ID 023e105f4ecef8ad9ca31a8372d0c353, purge everything.",
    { zone_id: "023e105f4ecef8ad9ca31a8372d0c353", body: { purge_everything: true } },
  ],
  [
    "r4-136",
    "Owner fixture, repo demo, title Login failure.",
    { owner: "fixture", repo: "demo", title: "Login failure" },
  ],
  [
    "r4-138",
    "Search query repo:fixture/demo authentication.",
    { q: "repo:fixture/demo authentication" },
  ],
  [
    "r4-139",
    "Owner fixture, repo demo, issue number 42.",
    { owner: "fixture", repo: "demo", issue_number: 42 },
  ],
  [
    "r4-140",
    "Owner fixture, repo demo, issue 42, title Login fixed, labels [bug].",
    {
      owner: "fixture",
      repo: "demo",
      issue_number: 42,
      title: "Login fixed",
      labels: ["bug"],
    },
  ],
  [
    "r4-149",
    "Search query repo:fixture/demo authenticate.",
    { q: "repo:fixture/demo authenticate" },
  ],
  [
    "r4-157",
    "Project fixture/demo, title Login failure.",
    { projectPath: "fixture/demo", title: "Login failure" },
  ],
  ["r4-167", "Use the mock's configured login.", {}],
  ["r4-172", "Query login failures.", { query: "login failures" }],
  ["r4-173", "Identifier ENG-123.", { identifier: "ENG-123" }],
  ["r4-174", "Identifiers [ENG-123].", { identifiers: ["ENG-123"] }],
  ["r4-196", "Return at most 5 channels.", { limit: 5 }],
  [
    "r4-201",
    "Channel C_FIXTURE, parent timestamp 1234567890.123456.",
    { channel_id: "C_FIXTURE", thread_ts: "1234567890.123456" },
  ],
  [
    "r4-267",
    "Calendar primary, at most 5 events.",
    { calendarId: "primary", maxResults: 5 },
  ],
  [
    "r4-280",
    "Project fixture, SQL SELECT 1 AS value.",
    { project_id: "fixture", query: "SELECT 1 AS value" },
  ],
  ["r4-304", "Path /fixture/config.txt.", { path: "/fixture/config.txt" }],
  [
    "r4-393",
    "Rename /fixture/old.txt to /fixture/new.txt.",
    { source: "/fixture/old.txt", destination: "/fixture/new.txt" },
  ],
  [
    "r4-299",
    "Owner fixture, repo demo, title Login failure. Send its returned URL to Slack channel C_FIXTURE.",
    { owner: "fixture", repo: "demo", title: "Login failure" },
    "slack__slack_post_message",
    { channel_id: "C_FIXTURE", text: "https://example.test/issues/42" },
  ],
  [
    "r4-392",
    "Project fixture, migration name create_audit, SQL CREATE TABLE audit(id integer); then list tables in schema public with verbose false in the same project.",
    {
      project_id: "fixture",
      name: "create_audit",
      query: "CREATE TABLE audit(id integer);",
    },
    "supabase__list_tables",
    { project_id: "fixture", schemas: ["public"], verbose: false },
  ],
  [
    "r4-301",
    "Project fixture. First run SELECT 1 AS value, then list tables in schema public with verbose false in the same project.",
    { project_id: "fixture", query: "SELECT 1 AS value" },
    "supabase__list_tables",
    { project_id: "fixture", schemas: ["public"], verbose: false },
  ],
  ["r4-339", "Only use a matching MCP capability; if absent, say unavailable.", null],
  ["r4-341", "Only use a matching MCP capability; if absent, say unavailable.", null],
  ["r4-408", "Only use a matching MCP capability; if absent, say unavailable.", null],
];

export function tasksFrom(dev) {
  return specifications.map(([id, detail, args, second, secondArgs]) => {
    const intent = dev.find((x) => x.id === id);
    if (!intent || intent.split !== "dev") throw new Error(`Missing dev intent ${id}`);
    const expected =
      args === null ? [] : [{ name: intent.primary.replaceAll("-", "_"), args }];
    if (id === "r4-001")
      expected[0].alternatives = intent.acceptable_alternatives.map((name) => ({
        name,
        args: { charge: "ch_fixture", body: { reason: "duplicate" } },
      }));
    if (second) expected.push({ name: second, args: secondArgs });
    return {
      id,
      category: intent.category,
      prompt: `${intent.query}. ${detail}`,
      expected,
    };
  });
}
