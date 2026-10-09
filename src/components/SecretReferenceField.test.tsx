import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { SecretReferenceField } from "./SecretReferenceField";
const testReference = vi.hoisted(() => vi.fn());
vi.mock("@/lib/api", () => ({ testSecretReference: testReference }));
describe("SecretReferenceField", () => {
  it("tests one reference and shows success without a resolved value", async () => {
    testReference.mockResolvedValueOnce(undefined);
    render(<SecretReferenceField serverId="docs" value="op://Engineering/Docs/key" onChange={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Test" }));
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("This machine can read the key"));
    expect(testReference).toHaveBeenCalledWith("docs", "op://Engineering/Docs/key");
    expect(testReference).toHaveBeenCalledTimes(1);
  });
  it("shows the resolver's actionable state", async () => {
    testReference.mockRejectedValueOnce({ state: "notInstalled", message: "1Password: Install the official op CLI on this machine." });
    render(<SecretReferenceField serverId="docs" value="op://Engineering/Docs/key" onChange={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Test" }));
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("Install the official op CLI"));
  });
  it("selects the provider's reference format", async () => {
    const change = vi.fn();
    render(<SecretReferenceField serverId="" value="op://Engineering/Docs/key" onChange={change} />);
    await userEvent.selectOptions(screen.getByLabelText("Password manager provider"), "keeper://");
    expect(change).toHaveBeenCalledWith("keeper://8f8I-OqPV58o2r91wVgZ_A/field/password");
  });
});
