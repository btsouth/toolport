import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { DestructiveConfirmationNotice } from "./DestructiveConfirmationNotice";
import { dismissDestructiveConfirmationNotice } from "@/lib/api";
import { toastError } from "@/lib/toast";
import type { Registry } from "@/lib/types";

vi.mock("@/lib/api", () => ({ dismissDestructiveConfirmationNotice: vi.fn() }));
vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));

const existing: Registry = {
  version: 1,
  servers: [],
  profiles: [],
  confirmDestructive: false,
};

describe("destructive confirmation upgrade notice", () => {
  beforeEach(() => vi.clearAllMocks());

  it("offers Settings without silently changing the existing choice", async () => {
    const onSettings = vi.fn();
    const onRegistryChange = vi.fn();
    render(
      <DestructiveConfirmationNotice
        registry={existing}
        onSettings={onSettings}
        onRegistryChange={onRegistryChange}
      />,
    );
    expect(screen.getByRole("status")).toHaveTextContent("Your setting has not changed");
    await userEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(onSettings).toHaveBeenCalledOnce();
    expect(onRegistryChange).not.toHaveBeenCalled();
    expect(dismissDestructiveConfirmationNotice).not.toHaveBeenCalled();
  });

  it("persists dismissal and disappears on the acknowledged registry", async () => {
    const acknowledged = { ...existing, destructiveConfirmationNoticeSeen: true };
    vi.mocked(dismissDestructiveConfirmationNotice).mockResolvedValue(acknowledged);
    const onRegistryChange = vi.fn();
    const props = { onSettings: vi.fn(), onRegistryChange };
    const { rerender } = render(
      <DestructiveConfirmationNotice registry={existing} {...props} />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(onRegistryChange).toHaveBeenCalledWith(acknowledged);
    expect(acknowledged.confirmDestructive).toBe(false);
    rerender(<DestructiveConfirmationNotice registry={acknowledged} {...props} />);
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("keeps the offer visible when acknowledgement fails", async () => {
    vi.mocked(dismissDestructiveConfirmationNotice).mockRejectedValue("disk full");
    render(
      <DestructiveConfirmationNotice
        registry={existing}
        onSettings={vi.fn()}
        onRegistryChange={vi.fn()}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(toastError).toHaveBeenCalledWith("Couldn't dismiss the notice: disk full");
    expect(screen.getByRole("status")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Dismiss" })).toBeEnabled();
  });

  it("shows no upgrade offer before loading or on a new install", () => {
    const props = { onSettings: vi.fn(), onRegistryChange: vi.fn() };
    const { rerender } = render(
      <DestructiveConfirmationNotice registry={null} {...props} />,
    );
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
    rerender(
      <DestructiveConfirmationNotice
        registry={{
          ...existing,
          confirmDestructive: true,
          destructiveConfirmationNoticeSeen: true,
        }}
        {...props}
      />,
    );
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });
});
