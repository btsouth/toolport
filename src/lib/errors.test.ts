import { describe, it, expect } from "vitest";
import { errorHeadline, truncateMiddle, shortenUrls } from "./errors";

describe("errorHeadline", () => {
  it("surfaces EADDRINUSE with the address, past a stack trace", () => {
    const raw = [
      "Error: listen EADDRINUSE: address already in use 127.0.0.1:39541",
      "    at Server.setupListenHandle [as _listen2] (node:net:1817:16)",
      "    at listenInCluster (node:net:1865:12)",
    ].join("\n");
    expect(errorHeadline(raw)).toBe("Port already in use (127.0.0.1:39541)");
  });

  it("recognizes ENOENT, ECONNREFUSED, timeout, 401, 403", () => {
    expect(errorHeadline("spawn npx ENOENT")).toBe("Command or file not found (ENOENT)");
    expect(errorHeadline("connect ECONNREFUSED 127.0.0.1:8080")).toBe(
      "Connection refused",
    );
    expect(errorHeadline("timed out waiting for initialize")).toBe(
      "Timed out waiting for the server",
    );
    expect(errorHeadline("Request failed: 401 Unauthorized")).toBe(
      "Authentication required (401)",
    );
    expect(errorHeadline("HTTP 403 Forbidden")).toBe("Access forbidden (403)");
  });

  it("recognizes DNS, reset, permission, rate limit, 5xx, and TLS failures", () => {
    expect(errorHeadline("getaddrinfo ENOTFOUND api.example.com")).toBe(
      "Host not found (ENOTFOUND)",
    );
    expect(errorHeadline("getaddrinfo EAI_AGAIN mcp.example.com")).toBe(
      "Temporary DNS failure (EAI_AGAIN)",
    );
    expect(errorHeadline("read ECONNRESET")).toBe("Connection reset");
    expect(errorHeadline("spawn EACCES")).toBe("Permission denied (EACCES)");
    expect(errorHeadline("bind EPERM")).toBe("Permission denied (EPERM)");
    expect(errorHeadline("upstream returned 429 Too Many Requests")).toBe(
      "Rate limited (429)",
    );
    expect(errorHeadline("service unavailable HTTP 503")).toBe("Server error (503)");
    expect(errorHeadline("UNABLE_TO_VERIFY_LEAF_SIGNATURE")).toBe(
      "TLS certificate not trusted",
    );
    expect(errorHeadline("DEPTH_ZERO_SELF_SIGNED_CERT")).toBe(
      "TLS certificate not trusted",
    );
    expect(errorHeadline("SELF_SIGNED_CERT_IN_CHAIN")).toBe(
      "TLS certificate not trusted",
    );
  });

  it("does not treat a high port number as a 5xx server error", () => {
    expect(errorHeadline("listen EADDRINUSE 127.0.0.1:50001")).toBe(
      "Port already in use (127.0.0.1:50001)",
    );
    expect(errorHeadline("listen EADDRINUSE 127.0.0.1:503")).toBe(
      "Port already in use (127.0.0.1:503)",
    );
  });

  it("does not treat a bare numeric port as an HTTP failure", () => {
    expect(errorHeadline("connect failed on port 503")).toBe(
      "connect failed on port 503",
    );
    expect(errorHeadline("retrying after 429 ms")).toBe("retrying after 429 ms");
  });

  it("does not treat a giant OAuth authorize URL as an auth headline by itself", () => {
    // A ~2KB authorize URL with no 401/unauthorized text should fall to the last
    // line, with the URL middle-truncated, not blow up the message.
    const url = "https://auth.example.com/authorize?" + "scope=".repeat(300) + "x";
    const raw = `Failed to start mcp-remote\nOpen this URL to continue: ${url}`;
    const head = errorHeadline(raw);
    expect(head).not.toContain("Authentication required");
    expect(head).toContain("…");
    expect(head.length).toBeLessThanOrEqual(160);
  });

  it("falls back to exit status, then the last meaningful line", () => {
    expect(errorHeadline("child process exited with status 1")).toBe(
      "Exited with status 1",
    );
    expect(errorHeadline("line one\n\n   the real problem   \n")).toBe(
      "the real problem",
    );
  });

  it("never throws on empty/nullish input", () => {
    expect(errorHeadline("")).toBe("Unknown error");
    expect(errorHeadline(null)).toBe("Unknown error");
    expect(errorHeadline(undefined)).toBe("Unknown error");
  });
});

describe("truncateMiddle / shortenUrls", () => {
  it("middle-truncates only past the cap", () => {
    expect(truncateMiddle("short", 100)).toBe("short");
    const out = truncateMiddle("a".repeat(200), 21);
    expect(out).toHaveLength(21);
    expect(out).toContain("…");
  });

  it("shortens long URLs but leaves short ones", () => {
    expect(shortenUrls("see http://x.io/ok", 80)).toBe("see http://x.io/ok");
    expect(shortenUrls("go " + "http://x.io/" + "q".repeat(200), 40)).toContain("…");
  });
});
