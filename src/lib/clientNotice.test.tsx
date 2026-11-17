import { beforeEach, describe, expect, it } from "vitest";
import {
  clearListNotice,
  dismissClientNotice,
  resetClientNotices,
  setConnectNotice,
  setDetailNotice,
  useClientNotices,
} from "./clientNotice";
import { renderHook } from "@testing-library/react";

const reminder = (clientId: string) => ({
  clientId,
  title: "Not live yet",
  text: `Restart ${clientId} so it loads Toolport.`,
});
const error = (clientId: string) => ({ clientId, title: "refused", text: "" });
const notices = () => renderHook(() => useClientNotices()).result.current;

describe("client notices", () => {
  beforeEach(() => resetClientNotices());

  it("dismissing the reminder in one place clears it in both", () => {
    const shown = reminder("cursor");
    setConnectNotice(shown);
    dismissClientNotice(shown);
    expect(notices()).toEqual({ detail: null, list: null });
  });

  it("dismissing the list reminder keeps another client's details notice", () => {
    const shown = reminder("cursor");
    setConnectNotice(shown);
    const other = error("codex");
    setDetailNotice(other);
    expect(notices().list).toBe(shown);
    dismissClientNotice(shown);
    expect(notices()).toEqual({ detail: other, list: null });
  });

  it("dismissing another client's details notice keeps the list reminder", () => {
    const shown = reminder("cursor");
    setConnectNotice(shown);
    const other = error("codex");
    setDetailNotice(other);
    dismissClientNotice(other);
    expect(notices()).toEqual({ detail: null, list: shown });
  });

  it("later feedback for the same client replaces the list reminder too", () => {
    setConnectNotice(reminder("cursor"));
    const later = error("cursor");
    setDetailNotice(later);
    expect(notices()).toEqual({ detail: later, list: null });
  });

  it("a disconnect clears only that client's list reminder", () => {
    const shown = reminder("cursor");
    setConnectNotice(shown);
    clearListNotice("codex");
    expect(notices().list).toBe(shown);
    clearListNotice("cursor");
    expect(notices()).toEqual({ detail: null, list: null });
  });
});
