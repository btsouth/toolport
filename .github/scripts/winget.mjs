import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import process from "node:process";
import console from "node:console";
import { pathToFileURL } from "node:url";
import { parse, stringify } from "yaml";

export function releaseInstaller(release, tag, repo) {
  if (
    !/^v\d+\.\d+\.\d+$/.test(tag) ||
    release.tag_name !== tag ||
    release.draft ||
    release.prerelease
  )
    throw new Error("Only a published stable release can go to winget");
  const version = tag.slice(1);
  const name = `Toolport_${version}_x64-setup.exe`;
  const url = `https://github.com/${repo}/releases/download/${tag}/${name}`;
  const assets = release.assets.filter((asset) => asset.name === name);
  if (
    assets.length !== 1 ||
    assets[0].browser_download_url !== url ||
    !/^sha256:[a-f0-9]{64}$/i.test(assets[0].digest ?? "")
  )
    throw new Error("Release installer URL or SHA256 is missing or ambiguous");
  return { version, url, sha256: assets[0].digest.slice(7).toUpperCase() };
}

export function manifests(templates, installer, bytes) {
  if (createHash("sha256").update(bytes).digest("hex").toUpperCase() !== installer.sha256)
    throw new Error("Downloaded installer does not match release SHA256");
  return templates.map(({ name, content }) => {
    const doc = parse(content);
    if (doc.PackageIdentifier !== "Toolport.Toolport")
      throw new Error("Wrong package identifier");
    doc.PackageVersion = installer.version;
    if (doc.ManifestType === "installer") {
      delete doc.ReleaseDate;
      doc.Installers = [
        {
          Architecture: "x64",
          InstallerUrl: installer.url,
          InstallerSha256: installer.sha256,
        },
      ];
    }
    if (doc.ManifestType === "defaultLocale")
      doc.ReleaseNotesUrl = installer.url.replace(/\/download\/([^/]+)\/.*$/, "/tag/$1");
    return { name, content: stringify(doc) };
  });
}

// The adapter is also used by the fixture. It has no release-write operation.
export async function submit(api, fork, installer, files) {
  await api.sync(fork);
  const branch = `toolport-${installer.version}`;
  const prefix = `manifests/t/Toolport/Toolport/${installer.version}/`;
  const entries = files.map(({ name, content }) => ({
    path: prefix + name,
    mode: "100644",
    type: "blob",
    content,
  }));
  const existing = await api.branch(fork, branch);
  if (existing) {
    for (const entry of entries) {
      if ((await api.content(fork, branch, entry.path)) !== entry.content)
        throw new Error("Retry branch differs from immutable release manifest");
    }
  } else {
    await api.createBranch(fork, branch, entries);
  }
  const prs = await api.pullRequests(fork, branch);
  if (prs.length) {
    if (prs[0].state === "closed" && !prs[0].merged_at)
      throw new Error(
        `Previous winget PR was closed without merging: ${prs[0].html_url}`,
      );
    return prs[0].html_url;
  }
  return api.createPullRequest(fork, branch, installer.version);
}

function gh(args, body) {
  return execFileSync("gh", args, {
    encoding: "utf8",
    input: body && JSON.stringify(body),
    timeout: 120000,
  });
}
function request(path, body) {
  return JSON.parse(
    gh(["api", path, ...(body ? ["--method", "POST", "--input", "-"] : [])], body),
  );
}
function optional(path) {
  try {
    return request(path);
  } catch (error) {
    if (error.stderr?.toString().includes("HTTP 404")) return null;
    throw error;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const repo = process.env.GITHUB_REPOSITORY;
  const tag = process.env.TAG;
  const installer = releaseInstaller(
    request(`repos/${repo}/releases/tags/${tag}`),
    tag,
    repo,
  );
  const response = await fetch(installer.url, { signal: AbortSignal.timeout(120000) });
  if (!response.ok) throw new Error(`Installer download failed: ${response.status}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  const files = manifests(
    [
      "Toolport.Toolport.yaml",
      "Toolport.Toolport.installer.yaml",
      "Toolport.Toolport.locale.en-US.yaml",
    ].map((name) => ({
      name,
      content: readFileSync(`packaging/winget/${name}`, "utf8"),
    })),
    installer,
    bytes,
  );
  const owner = request("user").login;
  const fork = `${owner}/winget-pkgs`;
  const api = {
    sync: () =>
      gh([
        "repo",
        "sync",
        fork,
        "--source",
        "microsoft/winget-pkgs",
        "--branch",
        "master",
        "--force",
      ]),
    branch: (_, branch) => optional(`repos/${fork}/git/ref/heads/${branch}`),
    content: (_, branch, path) =>
      Buffer.from(
        request(`repos/${fork}/contents/${path}?ref=${branch}`).content,
        "base64",
      ).toString("utf8"),
    createBranch: (_, branch, tree) => {
      const parent = request(`repos/${fork}/git/ref/heads/master`).object.sha;
      const base = request(`repos/${fork}/git/commits/${parent}`).tree.sha;
      const created = request(`repos/${fork}/git/trees`, { base_tree: base, tree });
      const commit = request(`repos/${fork}/git/commits`, {
        message: `Update Toolport to ${installer.version}`,
        tree: created.sha,
        parents: [parent],
      });
      request(`repos/${fork}/git/refs`, { ref: `refs/heads/${branch}`, sha: commit.sha });
    },
    pullRequests: (_, branch) =>
      request(
        `repos/microsoft/winget-pkgs/pulls?head=${owner}:${branch}&state=all&per_page=100`,
      ),
    createPullRequest: (_, branch, version) =>
      request("repos/microsoft/winget-pkgs/pulls", {
        title: `New version: Toolport.Toolport version ${version}`,
        head: `${owner}:${branch}`,
        base: "master",
        body: `Update Toolport to ${version} using the published release installer.`,
      }).html_url,
  };
  console.log(await submit(api, fork, installer, files));
}
