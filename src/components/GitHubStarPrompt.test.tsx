import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { GitHubStarPrompt } from "./GitHubStarPrompt";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { STAR_PROMPT_KEY, STAR_REPO_URL } from "@/lib/starPrompt";

let auditEntries = [
  { ts: 86400000, ok: true },
  { ts: 172800000, ok: true },
];
const getAuditLog = vi.fn(() => Promise.resolve(auditEntries));
vi.mock("@/lib/api", () => ({ getAuditLog: () => getAuditLog() }));
const openExternal = vi.fn();
vi.mock("@/lib/openUrl", () => ({
  openExternal: (...a: unknown[]) => openExternal(...a),
}));

// Toolport lives in the tray. The prompt must show nothing, and spend nothing,
// while the window is hidden; see windowVisible.test.ts for the signal itself.
let windowVisible = true;
// Only the native visibility signal is faked. useModalOpen stays real, so the
// dialog tests below run against the same body attribute a shipped dialog sets
// and would break loudly if that signal ever moved.
vi.mock("@/lib/windowVisible", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/windowVisible")>()),
  useWindowVisible: () => windowVisible,
}));

/** A real modal dialog, so the "covered by a dialog" tests exercise Radix's own
 *  overlay, focus trap and scroll lock rather than a hand-rolled stand-in. */
function CoveringDialog({ open }: { open: boolean }) {
  return (
    <Dialog open={open}>
      <DialogContent>
        <DialogTitle>Settings</DialogTitle>
      </DialogContent>
    </Dialog>
  );
}

/** Both cards are delayed: one waits out the closing wizard, the other leaves a
 *  returning user alone for a few seconds after launch. The second pass drains
 *  the beat the prompt waits before recording a surface as shown, which is only
 *  scheduled once the surface has actually rendered. */
async function flushDelays() {
  await act(async () => {
    vi.advanceTimersByTime(15000);
  });
  await act(async () => {
    vi.advanceTimersByTime(1);
  });
}

const onboardingCard = () => screen.queryByText(/you.re all set/i);
const returningCard = () => screen.queryByText(/enjoying toolport/i);
const chip = () => screen.queryByRole("button", { name: /star toolport on github/i });
const starButton = () => screen.getByRole("button", { name: /^star on github$/i });

/** Marks the install as one that onboarded before this prompt shipped. */
function existingInstall() {
  localStorage.setItem("toolport.onboarded", "1");
}

const user = () => userEvent.setup({ advanceTimers: vi.advanceTimersByTime });

beforeEach(() => {
  openExternal.mockReset();
  auditEntries = [
    { ts: 86400000, ok: true },
    { ts: 172800000, ok: true },
  ];
  getAuditLog.mockClear();
  localStorage.clear();
  windowVisible = true;
  vi.useFakeTimers({ shouldAdvanceTime: true });
});

describe("an install that predates the prompt", () => {
  it("gets one card a few seconds into the launch, not at second zero", async () => {
    existingInstall();
    render(<GitHubStarPrompt justOnboarded={false} enabledCount={4} />);
    expect(returningCard()).toBeNull();

    await flushDelays();
    expect(returningCard()).not.toBeNull();
    // Worded for a months-old install, not with onboarding wording.
    expect(onboardingCard()).toBeNull();
  });

  it("stays quiet on an install that was never set up", async () => {
    existingInstall();
    const { rerender } = render(
      <GitHubStarPrompt justOnboarded={false} enabledCount={0} />,
    );
    await flushDelays();
    expect(returningCard()).toBeNull();
    // Nothing was spent, so the ask is still owed once they enable a server.
    expect(localStorage.getItem(STAR_PROMPT_KEY)).toBeNull();

    rerender(<GitHubStarPrompt justOnboarded={false} enabledCount={3} />);
    await flushDelays();
    expect(returningCard()).not.toBeNull();
  });

  it("never follows the card with a chip", async () => {
    const u = user();
    existingInstall();
    const first = render(<GitHubStarPrompt justOnboarded={false} enabledCount={8} />);
    await flushDelays();
    await u.click(screen.getByRole("button", { name: /^no thanks$/i }));
    expect(localStorage.getItem(STAR_PROMPT_KEY)).toBe("done");
    first.unmount();

    render(<GitHubStarPrompt justOnboarded={false} enabledCount={8} />);
    await flushDelays();
    expect(chip()).toBeNull();
    expect(returningCard()).toBeNull();
  });

  it("opens the repo when starred", async () => {
    const u = user();
    existingInstall();
    render(<GitHubStarPrompt justOnboarded={false} enabledCount={3} />);
    await flushDelays();

    await u.click(starButton());

    expect(openExternal).toHaveBeenCalledWith(STAR_REPO_URL);
    expect(localStorage.getItem(STAR_PROMPT_KEY)).toBe("done");
    expect(returningCard()).toBeNull();
  });
});

describe("an ask is spent when it is shown", () => {
  it("does not re-show a card that was ignored and then quit", async () => {
    existingInstall();
    const first = render(<GitHubStarPrompt justOnboarded={false} enabledCount={3} />);
    await flushDelays();
    expect(returningCard()).not.toBeNull();
    first.unmount(); // quit without answering

    render(<GitHubStarPrompt justOnboarded={false} enabledCount={3} />);
    await flushDelays();
    expect(returningCard()).toBeNull();
  });
});

describe("a server toggled off and back on", () => {
  it("does not make the chip flicker away and return", async () => {
    // The count is a gate for reaching the ask, not a condition to keep
    // meeting. The ask is already spent, so a second showing is pure flicker.
    localStorage.setItem(STAR_PROMPT_KEY, "later");
    const { rerender } = render(
      <GitHubStarPrompt justOnboarded={false} enabledCount={3} />,
    );
    await flushDelays();
    expect(chip()).not.toBeNull();

    rerender(<GitHubStarPrompt justOnboarded={false} enabledCount={2} />);
    await flushDelays();
    expect(chip()).not.toBeNull();

    rerender(<GitHubStarPrompt justOnboarded={false} enabledCount={3} />);
    expect(chip()).not.toBeNull();
  });

  it("does not make the existing-user card flicker either", async () => {
    existingInstall();
    const { rerender } = render(
      <GitHubStarPrompt justOnboarded={false} enabledCount={3} />,
    );
    await flushDelays();
    expect(returningCard()).not.toBeNull();

    rerender(<GitHubStarPrompt justOnboarded={false} enabledCount={0} />);
    expect(returningCard()).not.toBeNull();
  });

  it("still waits for the threshold the first time", () => {
    localStorage.setItem(STAR_PROMPT_KEY, "later");
    const { rerender } = render(
      <GitHubStarPrompt justOnboarded={false} enabledCount={2} />,
    );
    expect(chip()).toBeNull();
    // A dip before the threshold was ever met must not bank a high-water mark
    // the user never actually reached.
    rerender(<GitHubStarPrompt justOnboarded={false} enabledCount={1} />);
    expect(chip()).toBeNull();
  });

  it("reports one surface change, not a flicker, to the toast offset", async () => {
    const onVisibleChange = vi.fn();
    localStorage.setItem(STAR_PROMPT_KEY, "later");
    const { rerender } = render(
      <GitHubStarPrompt
        justOnboarded={false}
        enabledCount={4}
        onVisibleChange={onVisibleChange}
      />,
    );
    await flushDelays();
    onVisibleChange.mockClear();

    rerender(
      <GitHubStarPrompt
        justOnboarded={false}
        enabledCount={1}
        onVisibleChange={onVisibleChange}
      />,
    );
    rerender(
      <GitHubStarPrompt
        justOnboarded={false}
        enabledCount={4}
        onVisibleChange={onVisibleChange}
      />,
    );
    expect(onVisibleChange).not.toHaveBeenCalled();
  });
});

describe("while the app sits in the tray", () => {
  it("shows nothing and spends nothing", async () => {
    windowVisible = false;
    existingInstall();
    render(<GitHubStarPrompt justOnboarded={false} enabledCount={4} />);
    await flushDelays();

    expect(returningCard()).toBeNull();
    // The single ask is still owed, not burned on an empty screen.
    expect(localStorage.getItem(STAR_PROMPT_KEY)).toBeNull();
  });

  it("does not let a backgrounded server toggle burn the chip", async () => {
    // The gateway can enable servers while the window is hidden.
    windowVisible = false;
    localStorage.setItem(STAR_PROMPT_KEY, "later");
    render(<GitHubStarPrompt justOnboarded={false} enabledCount={5} />);
    await flushDelays();

    expect(chip()).toBeNull();
    expect(localStorage.getItem(STAR_PROMPT_KEY)).toBe("later");
  });

  it("starts the wait over when the window is opened", async () => {
    windowVisible = false;
    existingInstall();
    const { rerender } = render(
      <GitHubStarPrompt justOnboarded={false} enabledCount={4} />,
    );
    await flushDelays();
    expect(returningCard()).toBeNull();

    windowVisible = true;
    rerender(<GitHubStarPrompt justOnboarded={false} enabledCount={4} />);
    expect(returningCard()).toBeNull(); // the delay runs from the open, not the launch

    await flushDelays();
    expect(returningCard()).not.toBeNull();
  });
});

describe("while a modal dialog covers the app", () => {
  it("shows nothing and spends nothing", async () => {
    // The dialog paints over the corner, traps focus and aria-hides everything
    // under it. Rendering the card there would burn the one ask on something
    // nobody can see, click or reach with a screen reader.
    existingInstall();
    render(
      <>
        <CoveringDialog open />
        <GitHubStarPrompt justOnboarded={false} enabledCount={4} />
      </>,
    );
    await flushDelays();

    expect(returningCard()).toBeNull();
    expect(localStorage.getItem(STAR_PROMPT_KEY)).toBeNull();
  });

  it("asks once the dialog is closed", async () => {
    existingInstall();
    const { rerender } = render(
      <>
        <CoveringDialog open />
        <GitHubStarPrompt justOnboarded={false} enabledCount={4} />
      </>,
    );
    await flushDelays();
    expect(returningCard()).toBeNull();

    rerender(
      <>
        <CoveringDialog open={false} />
        <GitHubStarPrompt justOnboarded={false} enabledCount={4} />
      </>,
    );
    await flushDelays();

    expect(returningCard()).not.toBeNull();
    expect(localStorage.getItem(STAR_PROMPT_KEY)).toBe("done");
  });

  it("does not let a dialog burn the chip either", async () => {
    localStorage.setItem(STAR_PROMPT_KEY, "later");
    render(
      <>
        <CoveringDialog open />
        <GitHubStarPrompt justOnboarded={false} enabledCount={5} />
      </>,
    );
    await flushDelays();

    expect(chip()).toBeNull();
    expect(localStorage.getItem(STAR_PROMPT_KEY)).toBe("later");
  });
});

describe("a launch that ends before a delay does", () => {
  it("drops the card timer when the prompt unmounts mid-delay", async () => {
    const { unmount } = render(
      <GitHubStarPrompt justOnboarded={true} enabledCount={0} />,
    );
    act(() => {
      vi.advanceTimersByTime(300); // inside the 700ms wait
    });
    expect(onboardingCard()).toBeNull();

    unmount();
    expect(vi.getTimerCount()).toBe(0);
    await flushDelays();
    expect(onboardingCard()).toBeNull();
  });

  it("drops the returning timer when the prompt unmounts mid-delay", async () => {
    existingInstall();
    const { unmount } = render(
      <GitHubStarPrompt justOnboarded={false} enabledCount={4} />,
    );
    act(() => {
      vi.advanceTimersByTime(3000); // inside the 8s wait
    });

    unmount();
    expect(vi.getTimerCount()).toBe(0);
    await flushDelays();
    // The card never appeared, so the single ask is still owed next launch.
    expect(localStorage.getItem(STAR_PROMPT_KEY)).toBeNull();
  });

  it("books the chip when onboarding finishes but the window hides first", async () => {
    // finishOnboarding writes the onboarding flag straight away, while the card
    // waits out the closing wizard. A hide in between used to record nothing, so
    // the next launch read an onboarded install with no star record and handed a
    // day-old install the months-old wording, with no second chance.
    windowVisible = false;
    const first = render(<GitHubStarPrompt justOnboarded={true} enabledCount={4} />);
    await flushDelays();
    expect(onboardingCard()).toBeNull();
    expect(localStorage.getItem(STAR_PROMPT_KEY)).toBe("later");
    first.unmount();

    windowVisible = true;
    existingInstall(); // the flag finishOnboarding wrote for real
    render(<GitHubStarPrompt justOnboarded={false} enabledCount={4} />);
    await flushDelays();

    expect(returningCard()).toBeNull();
    expect(chip()).not.toBeNull();
  });
});

describe("the toast-offset callback", () => {
  it("reports the existing-user card too", async () => {
    const onVisibleChange = vi.fn();
    existingInstall();
    render(
      <GitHubStarPrompt
        justOnboarded={false}
        enabledCount={3}
        onVisibleChange={onVisibleChange}
      />,
    );
    await flushDelays();
    expect(onVisibleChange).toHaveBeenLastCalledWith("returning");
  });
});

describe("the value gate", () => {
  it.each([false, true])(
    "never asks during onboarding (returning=%s)",
    async (returning) => {
      if (returning) existingInstall();
      const { rerender } = render(
        <GitHubStarPrompt justOnboarded={false} onboardingOpen enabledCount={9} />,
      );
      await flushDelays();
      expect(screen.queryByRole("status")).toBeNull();
      rerender(<GitHubStarPrompt justOnboarded enabledCount={9} />);
      await flushDelays();
      expect(screen.queryByRole("status")).toBeNull();
    },
  );
  it.each(["later", "returning"])(
    "waits for real calls on two dates (%s)",
    async (stage) => {
      if (stage === "later") localStorage.setItem(STAR_PROMPT_KEY, stage);
      else existingInstall();
      auditEntries = [
        { ts: 86400000, ok: true },
        { ts: 86400001, ok: true },
      ];
      const { rerender } = render(
        <GitHubStarPrompt justOnboarded={false} enabledCount={3} />,
      );
      await flushDelays();
      expect(screen.queryByRole("status")).toBeNull();
      auditEntries.push({ ts: 172800000, ok: true });
      rerender(
        <GitHubStarPrompt justOnboarded={false} enabledCount={3} refreshKey={1} />,
      );
      await flushDelays();
      expect(screen.queryByRole("status")).not.toBeNull();
      expect(localStorage.getItem(STAR_PROMPT_KEY)).toBe("done");
    },
  );
  it.each(["later", "returning"])(
    "requires three enabled servers (%s)",
    async (stage) => {
      if (stage === "later") localStorage.setItem(STAR_PROMPT_KEY, stage);
      else existingInstall();
      const { rerender } = render(
        <GitHubStarPrompt justOnboarded={false} enabledCount={2} />,
      );
      await flushDelays();
      expect(screen.queryByRole("status")).toBeNull();
      rerender(<GitHubStarPrompt justOnboarded={false} enabledCount={3} />);
      await flushDelays();
      expect(screen.queryByRole("status")).not.toBeNull();
    },
  );
  it("does not re-show an ignored chip", async () => {
    localStorage.setItem(STAR_PROMPT_KEY, "later");
    const first = render(<GitHubStarPrompt justOnboarded={false} enabledCount={3} />);
    await flushDelays();
    expect(chip()).not.toBeNull();
    first.unmount();
    render(<GitHubStarPrompt justOnboarded={false} enabledCount={3} />);
    await flushDelays();
    expect(chip()).toBeNull();
  });
  it("opens the repo from the chip", async () => {
    localStorage.setItem(STAR_PROMPT_KEY, "later");
    render(<GitHubStarPrompt justOnboarded={false} enabledCount={3} />);
    await flushDelays();
    await user().click(chip()!);
    expect(openExternal).toHaveBeenCalledWith(STAR_REPO_URL);
    expect(chip()).toBeNull();
  });
  it("fails closed on unreadable activity", async () => {
    existingInstall();
    getAuditLog.mockRejectedValueOnce(new Error("unreadable"));
    render(<GitHubStarPrompt justOnboarded={false} enabledCount={3} />);
    await flushDelays();
    expect(returningCard()).toBeNull();
    expect(localStorage.getItem(STAR_PROMPT_KEY)).toBeNull();
  });
});
