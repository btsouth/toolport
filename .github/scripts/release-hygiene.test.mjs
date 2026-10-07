import { Buffer } from "node:buffer";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import test from "node:test";
import { releaseInstaller, manifests, submit } from "./winget.mjs";
import { assertContents } from "./package-contents.mjs";
import { parse } from "yaml";
const bytes = Buffer.from("immutable signed installer fixture");
const digest = `sha256:${createHash("sha256").update(bytes).digest("hex")}`;
const tag = "v2.0.0";
const url =
  "https://github.com/btsouth/toolport/releases/download/v2.0.0/Toolport_2.0.0_x64-setup.exe";
const release = {
  tag_name: tag,
  draft: false,
  prerelease: false,
  assets: [{ name: "Toolport_2.0.0_x64-setup.exe", browser_download_url: url, digest }],
};
const installer = releaseInstaller(release, tag, "btsouth/toolport");
const templates = [
  "Toolport.Toolport.yaml",
  "Toolport.Toolport.installer.yaml",
  "Toolport.Toolport.locale.en-US.yaml",
].map((name) => ({ name, content: readFileSync(`packaging/winget/${name}`, "utf8") }));

test("manifest versions, URLs and SHA256 come from the published release", () => {
  const files = manifests(templates, installer, bytes);
  for (const file of files) assert.equal(parse(file.content).PackageVersion, "2.0.0");
  assert.deepEqual(parse(files[1].content).Installers, [
    {
      Architecture: "x64",
      InstallerUrl: url,
      InstallerSha256: digest.slice(7).toUpperCase(),
    },
  ]);
  assert.throws(() => manifests(templates, installer, Buffer.from("changed")));
  for (const change of [
    { draft: true },
    { prerelease: true },
    { tag_name: "v2.0.1" },
    { assets: [] },
    { assets: [{ ...release.assets[0], digest: null }] },
    { assets: [{ ...release.assets[0], browser_download_url: url + "wrong" }] },
  ])
    assert.throws(() =>
      releaseInstaller({ ...release, ...change }, tag, "btsouth/toolport"),
    );
  assert.throws(() => releaseInstaller(release, "v2.0.0-preview.1", "btsouth/toolport"));
});

test("stale fork is synced before branching, failed PR creation retries the same branch", async () => {
  const calls = [];
  let entries;
  let attempts = 0;
  let pr;
  const api = {
    sync: async () => calls.push("sync"),
    branch: async () => entries,
    createBranch: async (_, branch, tree) => {
      assert.equal(calls.at(-1), "sync");
      calls.push("branch");
      entries = tree;
    },
    content: async (_, branch, path) =>
      entries.find((entry) => entry.path === path).content,
    pullRequests: async () => (pr ? [{ html_url: pr }] : []),
    createPullRequest: async () => {
      attempts++;
      if (attempts === 1) throw new Error("PR creation failed");
      pr = "fixture PR";
      return pr;
    },
  };
  const files = manifests(templates, installer, bytes);
  await assert.rejects(submit(api, "fixture/winget-pkgs", installer, files));
  assert.equal(await submit(api, "fixture/winget-pkgs", installer, files), "fixture PR");
  assert.equal(await submit(api, "fixture/winget-pkgs", installer, files), "fixture PR");
  assert.deepEqual(calls, ["sync", "branch", "sync", "sync"]);
  assert.equal(attempts, 2);
  await assert.rejects(
    submit(api, "fixture/winget-pkgs", installer, [{ ...files[0], content: "tampered" }]),
  );
});

test("payload assertions reject test helpers and unexpected binaries on every platform", () => {
  for (const root of [
    "./usr/bin/",
    "Toolport.app/Contents/MacOS/",
    "",
    "squashfs-root/usr/bin/",
  ]) {
    assertContents([root + "toolport-gateway", root + "conduit"]);
    assert.throws(() => assertContents([root + "mock-mcp-server"]));
    assert.throws(() => assertContents([root + "surprise.exe"]));
    if (root) assert.throws(() => assertContents([root + "surprise"]));
  }
  assertContents(["conduit.exe", "toolport-gateway.exe", "uninstall.exe"]);
});
