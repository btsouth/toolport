import { afterEach, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import { setLanguage } from "@/lib/i18n";
import { ImportReviewDialog } from "./ImportReviewDialog";

vi.mock("@/lib/api", () => ({
  getUiLanguage: vi.fn().mockResolvedValue("en"),
  setUiLanguage: vi.fn().mockResolvedValue(undefined),
}));

afterEach(async () => {
  await act(() => setLanguage("en"));
});

it("translates the missing credential reason with the key the GTK review uses", async () => {
  await act(() => setLanguage("de"));
  render(
    <ImportReviewDialog
      open
      requireCredentials
      items={[
        {
          key: "calendar",
          name: "Calendar",
          transport: "stdio",
          command: "calendar-mcp",
          args: [],
          url: null,
          isNew: true,
          credentials: [{ key: "PAT", secret: true, present: false, required: true }],
        },
      ]}
      onConfirm={vi.fn()}
      onOpenChange={vi.fn()}
    />,
  );
  expect(
    screen.getByText("Geben Sie PAT ein oder deaktivieren Sie Calendar"),
  ).toBeVisible();
});
