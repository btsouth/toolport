import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ImportReviewDialog, runsShell, isPrivateHostUrl } from "./ImportReviewDialog";
import type { ImportItem } from "@/lib/types";

function items(): ImportItem[] {
  return [
    {
      key: "a",
      name: "stripe",
      transport: "stdio",
      command: "npx",
      args: ["stripe-mcp"],
      url: null,
      isNew: true,
    },
    {
      key: "b",
      name: "linear",
      transport: "http",
      command: null,
      args: [],
      url: "https://mcp.linear.app/mcp",
      isNew: true,
    },
    {
      key: "c",
      name: "shellsrv",
      transport: "stdio",
      command: "bash",
      args: ["-c", "echo hi"],
      url: null,
      isNew: true,
    },
  ];
}

function renderDialog(overrides: Partial<Parameters<typeof ImportReviewDialog>[0]> = {}) {
  const onConfirm = vi.fn();
  const onOpenChange = vi.fn();
  render(
    <ImportReviewDialog
      open
      items={items()}
      onConfirm={onConfirm}
      onOpenChange={onOpenChange}
      {...overrides}
    />,
  );
  return { onConfirm, onOpenChange };
}

describe("ImportReviewDialog", () => {
  it("starts with every server selected and confirms them all", async () => {
    const { onConfirm } = renderDialog();
    // Button label reflects the full selection.
    await userEvent.click(screen.getByRole("button", { name: /import 3 servers/i }));
    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(new Set(onConfirm.mock.calls[0][0])).toEqual(new Set(["a", "b", "c"]));
  });

  it("exposes each server row as a pressed toggle", async () => {
    renderDialog();
    const stripe = screen.getByRole("button", { name: /stripe/i });

    expect(stripe).toHaveAttribute("aria-pressed", "true");
    await userEvent.click(stripe);
    expect(stripe).toHaveAttribute("aria-pressed", "false");
  });

  it("excludes a server from the confirm payload after it's deselected", async () => {
    const { onConfirm } = renderDialog();
    // Toggle "linear" off by clicking its row.
    await userEvent.click(screen.getByText("linear"));
    expect(screen.getByRole("button", { name: /import 2 servers/i })).toBeEnabled();
    await userEvent.click(screen.getByRole("button", { name: /import 2 servers/i }));
    expect(onConfirm).toHaveBeenCalledWith(expect.arrayContaining(["a", "c"]));
    expect(onConfirm.mock.calls[0][0]).not.toContain("b");
  });

  it("disables import when nothing is selected", async () => {
    renderDialog();
    for (const name of ["stripe", "linear", "shellsrv"]) {
      await userEvent.click(screen.getByText(name));
    }
    const cta = screen.getByRole("button", { name: /select a server/i });
    expect(cta).toBeDisabled();
  });

  it("cancel dismisses without confirming", async () => {
    const { onConfirm, onOpenChange } = renderDialog();
    await userEvent.click(screen.getByRole("button", { name: /cancel/i }));
    expect(onOpenChange).toHaveBeenCalledWith(false);
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it("flags a shell-command server so the user can't import it blindly", () => {
    renderDialog();
    expect(screen.getByText(/runs a shell command/i)).toBeInTheDocument();
  });

  it("renders nothing when closed", () => {
    const { container } = render(
      <ImportReviewDialog
        open={false}
        items={items()}
        onConfirm={vi.fn()}
        onOpenChange={vi.fn()}
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });
});

describe("runsShell", () => {
  it.each([
    // Bare shell names.
    ["sh", true],
    ["bash", true],
    ["zsh", true],
    ["cmd", true],
    ["powershell", true],
    ["pwsh", true],
    // Case-insensitive, and Windows .exe suffix is stripped.
    ["BASH", true],
    ["powershell.exe", true],
    // Full paths, both separators; backslashes are normalized.
    ["/bin/sh", true],
    ["/usr/bin/env", false],
    ["C:\\Windows\\System32\\cmd.exe", true],
    ["C:\\Program Files\\PowerShell\\7\\pwsh.exe", true],
    // Non-shell commands, including ones that merely contain a shell name.
    ["npx", false],
    ["node", false],
    ["bash-language-server", false],
    ["fish", false],
    // Only a trailing .exe is stripped.
    ["bash.exe.bak", false],
    // Missing command.
    [null, false],
    ["", false],
  ])("classifies %j as %s", (command, expected) => {
    expect(runsShell(command)).toBe(expected);
  });
});

describe("isPrivateHostUrl", () => {
  it.each([
    // Loopback names and addresses.
    ["http://localhost:3000/mcp", true],
    ["http://dev.localhost/mcp", true],
    ["http://127.0.0.1/mcp", true],
    ["http://[::1]:8080/mcp", true],
    // RFC1918 ranges.
    ["http://10.0.0.1/mcp", true],
    ["http://192.168.1.10:8080/mcp", true],
    ["http://172.16.0.1/mcp", true],
    ["http://172.31.255.255/mcp", true],
    // 172.x is only private for the second octet 16-31.
    ["http://172.15.0.1/mcp", false],
    ["http://172.32.0.1/mcp", false],
    // Link-local and "this network".
    ["http://169.254.169.254/latest/meta-data", true],
    ["http://0.0.0.0:9000/mcp", true],
    // Public hosts and addresses.
    ["https://mcp.linear.app/mcp", false],
    ["https://8.8.8.8/mcp", false],
    // Hostnames merely containing "localhost" are not loopback.
    ["http://localhost.example.com/mcp", false],
    ["http://notlocalhost/mcp", false],
    // Trailing dots (WHATWG keeps them on names only).
    ["http://localhost./mcp", true],
    ["http://127.0.0.1./mcp", true],
    // CGNAT 100.64.0.0/10
    ["http://100.64.0.1/mcp", true],
    ["http://100.127.255.255/mcp", true],
    ["http://100.63.0.1/mcp", false],
    ["http://100.128.0.1/mcp", false],
    // IPv6 link-local / ULA / unspecified / v4-mapped loopback
    ["http://[fe80::1]/", true],
    ["http://[fd00::1]/", true],
    ["http://[fc00::1]/", true],
    ["http://[::]/", true],
    ["http://[::ffff:127.0.0.1]/", true],
    ["http://[2001:db8::1]/", false],
    // Broadcast
    ["http://255.255.255.255/", true],
    // Obfuscated IPv4 encodings still warn via URL normalisation.
    ["http://2130706433/", true],
    ["http://0x7f000001/", true],
    ["http://127.1/", true],
    // userinfo: only the host after @ matters
    ["http://evil.example.com@localhost/", true],
    ["http://localhost@evil.example.com/", false],
    // Unparseable or missing URLs never warn.
    ["not a url", false],
    [null, false],
    [undefined, false],
    ["", false],
  ])("classifies %j as %s", (url, expected) => {
    expect(isPrivateHostUrl(url)).toBe(expected);
  });
});
