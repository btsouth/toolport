import { Buffer } from "node:buffer";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync, mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { releaseInstaller, manifests, submit, validateManifests } from "./winget.mjs";
import { assertContents, listContents, appImageOffset } from "./package-contents.mjs";
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
  validateManifests(files, installer);
  assert.equal(
    parse(files[2].content).ReleaseNotesUrl,
    "https://github.com/btsouth/toolport/releases/tag/v2.0.0",
  );
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
  let forkHead = "stale";
  let entries;
  let attempts = 0;
  let pr;
  const api = {
    sync: async () => {
      forkHead = "upstream";
      calls.push("sync");
    },
    branch: async () => entries,
    createBranch: async (_, branch, tree) => {
      assert.equal(calls.at(-1), "sync");
      assert.equal(forkHead, "upstream");
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
  const updatedTemplates = files.map((file) => ({
    ...file,
    content: file.content + "\n# template edited later\n",
  }));
  assert.equal(
    await submit(api, "fixture/winget-pkgs", installer, updatedTemplates),
    "fixture PR",
  );
  const storedInstaller = entries.find((entry) => entry.path.endsWith(".installer.yaml"));
  storedInstaller.content = storedInstaller.content.replace(
    installer.sha256,
    "0".repeat(64),
  );
  await assert.rejects(submit(api, "fixture/winget-pkgs", installer, files), /SHA256/);
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

test("archive inspection lists a real payload and rejects an added helper", () => {
  const root = mkdtempSync(join(tmpdir(), "toolport-payload-"));
  try {
    const bin = join(root, "Toolport.app/Contents/MacOS");
    mkdirSync(bin, { recursive: true });
    writeFileSync(join(bin, "conduit"), "app");
    writeFileSync(join(bin, "toolport-gateway"), "gateway");
    const archive = join(root, "app.tar.gz");
    const pack = () => execFileSync("tar", ["-czf", archive, "-C", root, "Toolport.app"]);
    pack();
    assertContents(listContents(archive));
    writeFileSync(join(bin, "mock-mcp-server"), "test helper");
    pack();
    assert.throws(() => assertContents(listContents(archive)), /Test artifact/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("AppImage offset skips runtime magic and rejects truncated filesystems", () => {
  const bytes = Buffer.alloc(512);
  bytes.write("hsqs", 16);
  const offset = 128;
  bytes.write("hsqs", offset);
  bytes.writeUInt32LE(131072, offset + 12);
  bytes.writeUInt16LE(4, offset + 28);
  bytes.writeBigUInt64LE(256n, offset + 40);
  assert.equal(appImageOffset(bytes), offset);
  bytes.writeBigUInt64LE(1024n, offset + 40);
  assert.equal(appImageOffset(bytes), -1);
});
