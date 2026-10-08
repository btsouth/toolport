import { describe, expect, it } from "vitest";
import { serverNameOrder } from "./serverOrder";

describe("stable server order", () => {
  it("ignores toggles and health and breaks equal-name ties by ID", () => {
    const rows = [
      { id: "b", name: "same", enabled: false, ok: false },
      { id: "a", name: "Same", enabled: true, ok: true },
      { id: "c", name: "Alpha", enabled: false, ok: false },
    ];
    const before = [...rows].sort(serverNameOrder).map((row) => row.id);
    rows.forEach((row) => {
      row.enabled = !row.enabled;
      row.ok = !row.ok;
    });
    expect([...rows].sort(serverNameOrder).map((row) => row.id)).toEqual(before);
    expect(before).toEqual(["c", "a", "b"]);
  });
});
