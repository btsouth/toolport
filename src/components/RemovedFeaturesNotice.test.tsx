import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { Registry } from "@/lib/types";

const api = vi.hoisted(() => ({
  dismissRemovedFeaturesNotice: vi.fn(),
  openExportsDir: vi.fn(),
}));
const openExternal = vi.hoisted(() => vi.fn());
vi.mock("@/lib/api", () => api);
vi.mock("@/lib/openUrl", () => ({ openExternal }));
vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));

import { RemovedFeaturesNotice } from "./RemovedFeaturesNotice";

const base: Registry = { version: 3, servers: [], profiles: [], activeProfileId: null };

describe("RemovedFeaturesNotice", () => {
  beforeEach(() => vi.clearAllMocks());

  it("stays hidden for installs that used none of the removed features", () => {
    const { container } = render(
      <RemovedFeaturesNotice registry={base} onRegistryChange={vi.fn()} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("names the features, links an issue and dismisses", async () => {
    const user = userEvent.setup();
    const onRegistryChange = vi.fn();
    const dismissed = {
      ...base,
      removedFeaturesNotice: { features: ["routines"], dismissed: true },
    };
    api.dismissRemovedFeaturesNotice.mockResolvedValue(dismissed);
    api.openExportsDir.mockResolvedValue(undefined);
    render(
      <RemovedFeaturesNotice
        registry={{ ...base, removedFeaturesNotice: { features: ["routines"] } }}
        onRegistryChange={onRegistryChange}
      />,
    );
    expect(
      screen.getByText(/no longer includes Routines, which you used in 1.x/),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Open exports folder" }));
    expect(api.openExportsDir).toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "I need this" }));
    expect(openExternal).toHaveBeenCalledWith(
      expect.stringContaining("title=I%20need%20Routines"),
    );
    await user.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(onRegistryChange).toHaveBeenCalledWith(dismissed);
  });
});
