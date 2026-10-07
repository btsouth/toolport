import { expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { AccessUpgradeNotice } from "./AccessUpgradeNotice";
import { dismissAccessUpgradeNotice, stopStaleGateways } from "@/lib/api";
import type { Registry } from "@/lib/types";
vi.mock("@/lib/api", () => ({
  dismissAccessUpgradeNotice: vi.fn(),
  stopStaleGateways: vi.fn(),
}));
vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));

it("shows the v3 upgrade notice, reuses cleanup, and hides after persisted dismissal", async () => {
  const registry: Registry = {
    version: 3,
    servers: [],
    profiles: [],
    activeProfileId: null,
    accessUpgradeNoticePending: true,
  };
  const dismissed = { ...registry, accessUpgradeNoticePending: false };
  vi.mocked(dismissAccessUpgradeNotice).mockResolvedValue(dismissed);
  vi.mocked(stopStaleGateways).mockResolvedValue({
    killed: ["old"],
    failed: [],
    needsRestart: [],
  });
  const onChange = vi.fn();
  const { rerender } = render(
    <AccessUpgradeNotice registry={registry} onRegistryChange={onChange} />,
  );
  expect(screen.getByRole("status")).toHaveTextContent("from before the upgrade");
  await userEvent.click(screen.getByRole("button", { name: "Stop old gateways" }));
  await waitFor(() => expect(stopStaleGateways).toHaveBeenCalledOnce());
  expect(screen.getByRole("status")).toHaveTextContent("Stopped 1 old gateways.");
  await userEvent.click(screen.getByRole("button", { name: "Dismiss" }));
  await waitFor(() => expect(onChange).toHaveBeenCalledWith(dismissed));
  rerender(<AccessUpgradeNotice registry={dismissed} onRegistryChange={onChange} />);
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
});
