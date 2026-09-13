import { describe, expect, it } from "vitest";
import {
  classifyMacroSource,
  macroToSource,
} from "./macroSource";
import { normalizeConfig } from "./tauri";
import {
  RHAI_API_NAMES,
  RHAI_API_SIGNATURES,
} from "../components/RhaiEditor";
import { isSupportedAssetFileName } from "../components/AssetManager";
import type { MacroRule, MacroStep } from "../types/config";

const rhaiMacro: MacroRule = {
  id: "vision-macro",
  name: "视觉宏",
  enabled: false,
  triggerKeys: ["F8"],
  mode: "once",
  repeatCount: 1,
  speed: 1,
  recordMouseMove: true,
  recordMouseClicks: true,
  program: {
    kind: "rhai",
    apiVersion: 1,
    source: 'let title = active_window_title();\nwait_image("button", 0, 0, 400, 300, 0.9, 1000, 100);',
  },
};

describe("vision automation frontend contracts", () => {
  it("classifies vision calls as advanced and preserves their source", () => {
    expect(classifyMacroSource('window_exists("Editor");')).toBe("advanced");
    if (rhaiMacro.program.kind !== "rhai") throw new Error("expected Rhai macro");
    expect(macroToSource(rhaiMacro)).toBe(rhaiMacro.program.source);
  });

  it("exposes all vision APIs with signatures for completion and help", () => {
    for (const name of [
      "active_window_title",
      "window_exists",
      "window_rect",
      "wait_window",
      "pixel_matches",
      "wait_pixel",
      "find_image",
      "wait_image",
    ]) {
      expect(RHAI_API_NAMES).toContain(name);
      expect(RHAI_API_SIGNATURES[name]).toContain(name);
    }
  });

  it("normalizes legacy config while retaining managed asset metadata", () => {
    const normalized = normalizeConfig({
      macros: [
        {
          ...rhaiMacro,
          program: undefined,
          steps: [{ type: "delay", durationMs: 25 }],
        } as unknown as MacroRule & { steps: MacroStep[] },
      ],
      assets: [
        {
          id: "asset_button_abc",
          name: "按钮",
          fileName: "asset_button_abc.png",
          width: 12,
          height: 8,
        },
      ],
    });
    expect(normalized.schemaVersion).toBe(2);
    expect(normalized.assets[0].fileName).toBe("asset_button_abc.png");
    expect(normalized.macros[0].program).toEqual({
      kind: "macro",
      steps: [{ type: "delay", durationMs: 25 }],
    });
  });

  it("accepts only the managed PNG/JPEG import extensions", () => {
    expect(isSupportedAssetFileName("button.PNG")).toBe(true);
    expect(isSupportedAssetFileName("button.jpeg")).toBe(true);
    expect(isSupportedAssetFileName("button.webp")).toBe(false);
    expect(isSupportedAssetFileName("button.png.exe")).toBe(false);
  });
});
