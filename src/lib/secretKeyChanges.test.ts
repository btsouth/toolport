import { describe, expect, it } from "vitest";
import {
  KEEP_IN_KEYCHAIN,
  NAME_THIS_VARIABLE,
  NAME_USED_BY_ARGUMENT,
  rowProblem,
  secretKeyChanges,
} from "./secretKeyChanges";

const stored = { storedAs: "GITHUB_TOKN", key: "GITHUB_TOKN", value: "", secret: true };

describe("secretKeyChanges", () => {
  it("keeps an unchanged keychain variable", () => {
    expect(secretKeyChanges(["GITHUB_TOKN"], [stored])).toEqual({
      removed: [],
      renamed: [],
      blocked: [],
    });
  });

  it("moves the stored value of a renamed variable with no new value", () => {
    const row = { ...stored, key: "GITHUB_TOKEN" };
    expect(secretKeyChanges(["GITHUB_TOKN"], [row])).toEqual({
      removed: [],
      renamed: [["GITHUB_TOKN", "GITHUB_TOKEN"]],
      blocked: [],
    });
  });

  it("replaces the stored value when a renamed variable gets a new one", () => {
    const row = { ...stored, key: "GITHUB_TOKEN", value: "new" };
    expect(secretKeyChanges(["GITHUB_TOKN"], [row]).removed).toEqual(["GITHUB_TOKN"]);
  });

  it("blocks a keychain variable made plain with no value", () => {
    const row = { ...stored, secret: false };
    expect(rowProblem(row)).toBe(KEEP_IN_KEYCHAIN);
    expect(secretKeyChanges(["GITHUB_TOKN"], [row])).toEqual({
      removed: [],
      renamed: [],
      blocked: [["GITHUB_TOKN", KEEP_IN_KEYCHAIN]],
    });
  });

  it("deletes the stored value once a plain value or a reference replaces it", () => {
    const plain = { ...stored, secret: false, value: "visible" };
    expect(rowProblem(plain)).toBeNull();
    expect(secretKeyChanges(["GITHUB_TOKN"], [plain]).removed).toEqual(["GITHUB_TOKN"]);
    const reference = { ...stored, source: { ref: "op://Vault/Item/field" } };
    expect(secretKeyChanges(["GITHUB_TOKN"], [reference]).removed).toEqual([
      "GITHUB_TOKN",
    ]);
  });

  it("deletes the stored value of a removed row only", () => {
    const other = { key: "NEW", value: "", secret: true };
    expect(secretKeyChanges(["GITHUB_TOKN"], [other])).toEqual({
      removed: ["GITHUB_TOKN"],
      renamed: [],
      blocked: [],
    });
  });

  it("blocks a stored variable whose name was cleared", () => {
    const row = { ...stored, key: " " };
    expect(secretKeyChanges(["GITHUB_TOKN"], [row]).blocked).toEqual([
      ["GITHUB_TOKN", NAME_THIS_VARIABLE],
    ]);
  });

  it("blocks a rename onto a secret argument's name", () => {
    const row = { ...stored, key: "IMPORTED_ARG_0" };
    expect(secretKeyChanges(["GITHUB_TOKN"], [row], ["IMPORTED_ARG_0"]).blocked).toEqual([
      ["GITHUB_TOKN", NAME_USED_BY_ARGUMENT],
    ]);
  });
});
