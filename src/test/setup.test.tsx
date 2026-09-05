import { afterAll, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";

const restoredFocus = vi.fn();

afterAll(() => {
  // The shared afterEach must finish Radix's delayed unmount before this file's
  // jsdom realm disappears. Otherwise it can dispatch a Node Event on old DOM.
  expect(restoredFocus).toHaveBeenCalledOnce();
});

it("finishes dialog focus restoration during shared cleanup", () => {
  render(
    <Dialog open>
      <DialogContent onCloseAutoFocus={restoredFocus} aria-describedby={undefined}>
        <DialogTitle>Cleanup fixture</DialogTitle>
      </DialogContent>
    </Dialog>,
  );
  expect(screen.getByRole("dialog")).toBeVisible();
});
