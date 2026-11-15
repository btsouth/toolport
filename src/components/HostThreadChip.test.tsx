import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { toast } from "sonner";
import { HostThreadChip } from "./HostThreadChip";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }),
}));
afterEach(() => vi.clearAllMocks());

const thread = "0f8e2c1a-9b7d-4e3f-a1b2-c3d4e5f60718";

function setup() {
  const user = userEvent.setup();
  const writeText = vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue();
  const rowClick = vi.fn();
  const rowKey = vi.fn();
  render(
    <div role="button" tabIndex={0} onClick={rowClick} onKeyDown={rowKey}>
      <HostThreadChip row={{ host: "t3-code", hostThread: thread }} />
    </div>,
  );
  return { user, writeText, rowClick, rowKey };
}

it("copies the full thread id on click without toggling its row", async () => {
  const { user, writeText, rowClick } = setup();
  const chip = screen.getByRole("button", { name: `Copy thread id ${thread}` });
  expect(chip).toHaveTextContent("Thread 0f8e2c1a");
  expect(chip).toHaveAttribute("title", `T3 Code thread ${thread}`);
  await user.click(chip);
  expect(writeText).toHaveBeenCalledWith(thread);
  expect(toast.success).toHaveBeenCalledWith("Copied thread id");
  expect(rowClick).not.toHaveBeenCalled();
});

it("is reachable with Tab and copies with Enter or Space", async () => {
  const { user, writeText, rowKey } = setup();
  await user.tab();
  await user.tab();
  const chip = screen.getByRole("button", { name: `Copy thread id ${thread}` });
  expect(chip).toHaveFocus();
  rowKey.mockClear();
  await user.keyboard("{Enter}");
  await user.keyboard(" ");
  expect(writeText).toHaveBeenCalledTimes(2);
  expect(rowKey).not.toHaveBeenCalled();
});

it("says so when the clipboard refuses", async () => {
  const { user, writeText } = setup();
  writeText.mockRejectedValue(new Error("denied"));
  await user.click(screen.getByRole("button", { name: `Copy thread id ${thread}` }));
  expect(toast.error).toHaveBeenCalledWith(
    "Couldn't copy to clipboard",
    expect.anything(),
  );
  expect(toast.success).not.toHaveBeenCalled();
});
