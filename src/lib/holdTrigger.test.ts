import { describe, expect, it } from "vitest";
import { defaultConfig } from "../types/config";
import type { MacroRule } from "../types/config";
import { normalizeConfig } from "./tauri";
import {
  HOLD_TRIGGER_REPAIR_REASON,
  holdTriggerDraftError,
  holdTriggerOwner,
  repairLegacyHoldTrigger,
} from "./holdTrigger";

const fixture = (patch: Partial<MacroRule> = {}): MacroRule => ({
  id: "hold-test",
  name: "Hold test",
  enabled: true,
  triggerKeys: ["Ctrl", "F9"],
  mode: "hold",
  repeatCount: 1,
  speed: 1,
  recordMouseMove: true,
  recordMouseClicks: true,
  program: { kind: "macro", steps: [{ type: "delay", durationMs: 10 }] },
  ...patch,
});

describe("Hold trigger ownership", () => {
  it("accepts one owner with any supported modifiers in any order", () => {
    expect(holdTriggerOwner(["Ctrl", "Shift", "7"])).toBe("7");
    expect(holdTriggerOwner(["7", "Win", "Alt"])).toBe("7");
    expect(holdTriggerOwner(["F9"])).toBe("F9");
    expect(holdTriggerOwner(["Ctrl", "F01"])).toBe("F1");
  });

  it("rejects modifier-only, multiple-owner, and unknown-key sets", () => {
    for (const keys of [["Ctrl"], ["Ctrl", "A", "B"], ["Ctrl", "Unknown"]]) {
      expect(holdTriggerOwner(keys)).toBeNull();
      expect(holdTriggerDraftError(fixture({ triggerKeys: keys }))).toBe(
        HOLD_TRIGGER_REPAIR_REASON,
      );
    }
  });

  it("leaves disabled invalid drafts editable", () => {
    expect(
      holdTriggerDraftError(
        fixture({ enabled: false, triggerKeys: ["Ctrl", "A", "B"] }),
      ),
    ).toBeUndefined();
  });

  it("repairs legacy enabled rules without dropping payload or unrelated errors", () => {
    const original = fixture({
      triggerKeys: ["Ctrl"],
      importError: "原有错误",
    });
    const repaired = repairLegacyHoldTrigger(original);
    expect(repaired.enabled).toBe(false);
    expect(repaired.program).toEqual(original.program);
    expect(repaired.importError).toBe(
      `原有错误；${HOLD_TRIGGER_REPAIR_REASON}`,
    );
  });

  it("clears only the Hold repair reason after a valid edit", () => {
    const repaired = fixture({
      enabled: false,
      importError: `原有错误；${HOLD_TRIGGER_REPAIR_REASON}`,
    });
    expect(repairLegacyHoldTrigger(repaired).importError).toBe("原有错误");
    expect(
      repairLegacyHoldTrigger({
        ...repaired,
        importError: HOLD_TRIGGER_REPAIR_REASON,
      }).importError,
    ).toBeUndefined();
  });

  it("applies the same narrow repair during browser config normalization", () => {
    const payload = fixture({ triggerKeys: ["Shift"] });
    const normalized = normalizeConfig({
      ...defaultConfig,
      macros: [payload],
    });
    expect(normalized.macros[0]).toMatchObject({
      id: payload.id,
      enabled: false,
      triggerKeys: ["Shift"],
      importError: HOLD_TRIGGER_REPAIR_REASON,
      program: payload.program,
    });
  });
});
