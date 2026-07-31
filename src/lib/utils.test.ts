import { describe, it, expect } from "vitest";
import { fmtDollars, fmtMs, fmtPercent, fmtTokens, fmtTs, stableListKeys } from "./utils";

describe("stableListKeys", () => {
  it("uses the bare identity when there are no collisions", () => {
    expect(stableListKeys(["a", "b", "c"], (s) => s)).toEqual(["a", "b", "c"]);
  });

  it("disambiguates colliding identities deterministically", () => {
    const keys = stableListKeys(["a", "a", "b", "a"], (s) => s);
    expect(keys).toEqual(["a#2", "a#1", "b", "a"]);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it("keeps existing keys stable when new entries are prepended", () => {
    const before = stableListKeys(["x", "y", "x"], (s) => s);
    const after = stableListKeys(["new", "x", "y", "x"], (s) => s);
    expect(after.slice(1)).toEqual(before);
  });

  it("never collides a generated suffix with a real identity containing #", () => {
    const keys = stableListKeys(["a#1", "a", "a"], (s) => s);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it("keeps keys stable and unique when #-containing identities are prepended", () => {
    const before = stableListKeys(["a", "a"], (s) => s);
    const after = stableListKeys(["a#1", "a", "a"], (s) => s);
    expect(after.slice(1)).toEqual(before);
    expect(new Set(after).size).toBe(after.length);
  });
});

describe("fmtTokens", () => {
  it("renders small numbers as-is", () => {
    expect(fmtTokens(0)).toBe("0");
    expect(fmtTokens(1)).toBe("1");
    expect(fmtTokens(999)).toBe("999");
  });

  it("renders thousands with a k suffix", () => {
    expect(fmtTokens(1000)).toBe("1.0k");
    expect(fmtTokens(12345)).toBe("12.3k");
    expect(fmtTokens(999499)).toBe("999.5k");
  });

  it("renders millions with an M suffix", () => {
    expect(fmtTokens(1_000_000)).toBe("1.0M");
    expect(fmtTokens(1_234_567)).toBe("1.2M");
  });

  it("rolls over to B and T instead of thousands of M", () => {
    expect(fmtTokens(1_000_000_000)).toBe("1.0B");
    expect(fmtTokens(2_110_000_000)).toBe("2.1B");
    expect(fmtTokens(1_500_000_000_000)).toBe("1.5T");
  });

  it("handles the boundary at 999999 (rounds up to 1000.0k, not 1.0M)", () => {
    // 999999 is just below the 1_000_000 threshold, so it takes the "k" branch:
    // (999999 / 1000).toFixed(1) === "1000.0", giving "1000.0k" rather than "1.0M".
    expect(fmtTokens(999999)).toBe("1000.0k");
  });

  it("passes 0 and negative inputs through unformatted", () => {
    expect(fmtTokens(0)).toBe("0");
    expect(fmtTokens(-1)).toBe("-1");
    expect(fmtTokens(-12345)).toBe("-12345");
  });
});

describe("fmtMs", () => {
  it("renders a dash when the duration was not measured", () => {
    expect(fmtMs(null)).toBe("-");
  });

  it("renders sub-second durations in milliseconds", () => {
    expect(fmtMs(0)).toBe("0 ms");
    expect(fmtMs(180)).toBe("180 ms");
    expect(fmtMs(999)).toBe("999 ms");
  });

  it("switches to seconds at exactly 1000 ms", () => {
    expect(fmtMs(1000)).toBe("1.0 s");
    expect(fmtMs(1234)).toBe("1.2 s");
    expect(fmtMs(59_600)).toBe("59.6 s");
  });
});

describe("fmtDollars", () => {
  it("shows cents below ten dollars", () => {
    expect(fmtDollars(0)).toBe("$0.00");
    expect(fmtDollars(1.234)).toBe("$1.23");
    expect(fmtDollars(9.99)).toBe("$9.99");
  });

  it("drops cents at exactly ten dollars", () => {
    expect(fmtDollars(10)).toBe("$10");
    expect(fmtDollars(42.6)).toBe("$43");
  });

  it("handles the boundary at 999.5 (rounds up to $1000, not $1,000)", () => {
    // 999.5 is below the 1000 threshold, so it takes the toFixed(0) branch and
    // rounds to "1000" without a thousands separator.
    expect(fmtDollars(999.5)).toBe("$1000");
  });

  it("adds a thousands separator from 1000 up", () => {
    expect(fmtDollars(1000)).toBe(`$${(1000).toLocaleString()}`);
    expect(fmtDollars(1234.56)).toBe(`$${(1235).toLocaleString()}`);
  });
});

describe("fmtPercent", () => {
  it("always includes the percent suffix for zero values", () => {
    expect(fmtPercent(0)).toBe("0%");
  });

  it("uses adaptive precision below ten percent", () => {
    expect(fmtPercent(0.002)).toBe("0.2%");
    expect(fmtPercent(0.099)).toBe("9.9%");
    expect(fmtPercent(0.1)).toBe("10%");
  });

  it("floors tiny nonzero values instead of rounding them to zero", () => {
    expect(fmtPercent(0.0004, { floorNonZero: true })).toBe("<0.1%");
  });

  it("can use the nonzero floor when the count is nonzero but the rate rounded away", () => {
    expect(fmtPercent(0, { floorNonZero: true })).toBe("<0.1%");
  });
});

describe("fmtTs", () => {
  const timestamp = new Date("2026-07-22T19:30:00").getTime();

  it("formats timestamps with the default date and time format", () => {
    expect(fmtTs(timestamp)).toBe(
      new Date(timestamp).toLocaleString(undefined, {
        month: "short",
        day: "numeric",
        hour: "2-digit",
        minute: "2-digit",
      }),
    );
  });

  it("formats time-only timestamps", () => {
    expect(fmtTs(timestamp, "time")).toBe(new Date(timestamp).toLocaleTimeString());
  });

  it("formats date-only timestamps", () => {
    expect(fmtTs(timestamp, "date")).toBe(new Date(timestamp).toLocaleDateString());
  });

  it("formats month-and-day timestamps without a year", () => {
    expect(fmtTs(timestamp, "monthDay")).toBe(
      new Date(timestamp).toLocaleDateString(undefined, {
        month: "short",
        day: "numeric",
      }),
    );
  });
});
