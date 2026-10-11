import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { SecretReferenceField } from "./SecretReferenceField";
const testReference = vi.hoisted(() => vi.fn());
vi.mock("@/lib/api", () => ({ testSecretReference: testReference }));
describe("SecretReferenceField", () => {
  it("tests one reference and shows success without a resolved value", async () => {
    testReference.mockResolvedValueOnce(undefined);
    render(
      <SecretReferenceField
        serverId="docs"
        value="op://Engineering/Docs/key"
        onChange={vi.fn()}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Test" }));
    await waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent(
        "Success in the desktop app environment",
      ),
    );
    expect(testReference).toHaveBeenCalledWith("docs", "op://Engineering/Docs/key");
    expect(testReference).toHaveBeenCalledTimes(1);
  });
  it("shows the resolver's actionable state", async () => {
    testReference.mockRejectedValueOnce({
      state: "notInstalled",
      message: "1Password: Install the official op CLI on this machine.",
    });
    render(
      <SecretReferenceField
        serverId="docs"
        value="op://Engineering/Docs/key"
        onChange={vi.fn()}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Test" }));
    await waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent("Install the official op CLI"),
    );
  });
  it("starts a new provider's reference from its prefix only", async () => {
    const change = vi.fn();
    render(
      <SecretReferenceField
        serverId=""
        value="op://Engineering/Docs/key"
        onChange={change}
      />,
    );
    await userEvent.selectOptions(
      screen.getByLabelText("Password manager provider"),
      "keeper://",
    );
    // Only the prefix: an example reference could be saved by mistake.
    expect(change).toHaveBeenCalledWith("keeper://");
  });
});
