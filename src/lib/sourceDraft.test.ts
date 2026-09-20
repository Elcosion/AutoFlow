import { describe, expect, it } from "vitest";
import { shouldHydrateSourceDraft } from "./sourceDraft";

describe("Rhai source draft hydration", () => {
  it("preserves an edited source draft when the same macro is refreshed", () => {
    expect(shouldHydrateSourceDraft("macro-1", "macro-1", true)).toBe(false);
  });

  it("hydrates clean drafts and newly selected macros", () => {
    expect(shouldHydrateSourceDraft("macro-1", "macro-1", false)).toBe(true);
    expect(shouldHydrateSourceDraft("macro-1", "macro-2", true)).toBe(true);
  });
});
