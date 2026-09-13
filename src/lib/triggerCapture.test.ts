import { describe, expect, it } from "vitest";
import { nextCapturedKeys } from "./triggerCapture";

describe("macro trigger capture", () => {
  it("replaces a previous single key in the same capture session", () => {
    expect(nextCapturedKeys(["F9"], "F8")).toEqual(["F8"]);
  });

  it("keeps modifiers while replacing the final key", () => {
    expect(nextCapturedKeys(["Ctrl", "F9"], "F8")).toEqual(["Ctrl", "F8"]);
  });

  it("builds a modifier combination", () => {
    expect(nextCapturedKeys(["Ctrl"], "F8")).toEqual(["Ctrl", "F8"]);
  });
});
