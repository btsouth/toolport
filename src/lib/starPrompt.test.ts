import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

// The module carries a "storage is broken" latch that must outlive a component,
// so each test gets a fresh copy of it rather than a leaked one.
let mod: typeof import("./starPrompt");

beforeEach(async () => {
  vi.resetModules();
  localStorage.clear();
  mod = await import("./starPrompt");
});

afterEach(() => vi.restoreAllMocks());

describe("readStarStage", () => {
  it("starts a brand-new install at the onboarding card", () => {
    expect(mod.readStarStage()).toBe("card");
  });

  it("starts an install that predates the prompt at the one-off card", () => {
    localStorage.setItem("toolport.onboarded", "1");
    expect(mod.readStarStage()).toBe("returning");
  });

  it("honours the pre-rename onboarding key", () => {
    localStorage.setItem("conduit.onboarded", "1");
    expect(mod.readStarStage()).toBe("returning");
  });

  it("prefers a recorded stage over the predates-the-prompt guess", () => {
    // Otherwise an existing user would get the one-off card on every launch.
    localStorage.setItem("toolport.onboarded", "1");
    mod.writeStarStage("done");
    expect(mod.readStarStage()).toBe("done");
  });

  it("round-trips the two stages that are written", () => {
    mod.writeStarStage("later");
    expect(mod.readStarStage()).toBe("later");
    mod.writeStarStage("done");
    expect(mod.readStarStage()).toBe("done");
  });

  it("ignores a garbage value", () => {
    localStorage.setItem(mod.STAR_PROMPT_KEY, "yes-please");
    expect(mod.readStarStage()).toBe("card");
  });
});

describe("done is terminal", () => {
  it("refuses to downgrade a finished ask back to later", () => {
    // The effect that spends an ask when it is shown can flush after a click on
    // Star has already finished it. Without this the chip would come back.
    mod.writeStarStage("done");
    mod.writeStarStage("later");
    expect(mod.readStarStage()).toBe("done");
  });

  it("still allows the ordinary later write", () => {
    mod.writeStarStage("later");
    expect(localStorage.getItem(mod.STAR_PROMPT_KEY)).toBe("later");
  });
});

describe("when storage is unusable", () => {
  it("stays silent when a read is refused", () => {
    // A spent ask that can't be recorded would mean asking on every launch.
    vi.spyOn(localStorage, "getItem").mockImplementation(() => {
      throw new Error("denied");
    });
    expect(mod.readStarStage()).toBe("done");
  });

  it("stays silent for the rest of the session after a write is refused", () => {
    const setItem = vi.spyOn(localStorage, "setItem").mockImplementation(() => {
      throw new Error("quota");
    });

    expect(() => mod.writeStarStage("done")).not.toThrow();

    // The write is what would have recorded the ask as spent. Since it did not
    // land, reading must not hand back a fresh ask to show again.
    setItem.mockRestore();
    expect(mod.readStarStage()).toBe("done");
  });

  it("stays silent across a relaunch while writes keep failing", async () => {
    // The module latch only lasts a session. A store that serves reads but
    // refuses writes — quota exceeded, a locked webview store — would otherwise
    // derive a fresh ask on every launch and show the card forever.
    vi.spyOn(localStorage, "setItem").mockImplementation(() => {
      throw new Error("quota");
    });
    expect(mod.readStarStage()).toBe("done");

    vi.resetModules();
    const relaunched = await import("./starPrompt");
    expect(relaunched.readStarStage()).toBe("done");
  });

  it("gives the ask back once writes work again", async () => {
    // A quota that was freed between launches is a working store, and pretending
    // otherwise would silently retire the prompt on a healthy install.
    const setItem = vi.spyOn(localStorage, "setItem").mockImplementation(() => {
      throw new Error("quota");
    });
    expect(mod.readStarStage()).toBe("done");

    setItem.mockRestore();
    vi.resetModules();
    const relaunched = await import("./starPrompt");
    expect(relaunched.readStarStage()).toBe("card");
  });

  it("leaves nothing behind after probing the write path", () => {
    expect(mod.readStarStage()).toBe("card");
    expect(localStorage.length).toBe(0);
  });

  it("does not write again once storage has failed", () => {
    const setItem = vi.spyOn(localStorage, "setItem").mockImplementationOnce(() => {
      throw new Error("quota");
    });
    mod.writeStarStage("later");

    setItem.mockClear();
    mod.writeStarStage("done");
    expect(setItem).not.toHaveBeenCalled();
  });
});

describe("value days", () => {
  it("counts distinct UTC dates", () => {
    expect(
      mod.toolCallDays([
        { ts: 86400000, ok: true },
        { ts: 86400001, ok: true },
      ]),
    ).toBe(1);
    expect(
      mod.toolCallDays([
        { ts: 86400000, ok: true },
        { ts: 172800000, ok: true },
      ]),
    ).toBe(2);
  });
  it("excludes failures, holds, governance and invalid dates", () => {
    expect(
      mod.toolCallDays([
        { ts: 86400000, ok: false },
        { ts: 172800000, ok: true, held: true },
        ...["approval", "routine", "advisor", "suggestion", "candidate"].map((kind) => ({
          ts: 259200000,
          ok: true,
          kind,
        })),
        { ts: NaN, ok: true },
        { ts: 0, ok: true },
      ]),
    ).toBe(0);
  });
});
