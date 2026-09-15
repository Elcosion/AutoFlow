import { describe, expect, it } from "vitest";
import {
  classifyMacroSource,
  macroToSource,
  parseMacroSource,
} from "./macroSource";
import { normalizeConfig } from "./tauri";
import { RHAI_API_NAMES, RHAI_API_SIGNATURES } from "../components/RhaiEditor";
import { isSupportedAssetFileName } from "../components/AssetManager";
import type { BehaviorProfileV2, MacroRule, MacroStep } from "../types/config";

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
    source:
      'let title = active_window_title();\nwait_image("button.png", 0, 0, 400, 300, 0.9, 1000, 100);',
  },
};

describe("vision automation frontend contracts", () => {
  it("classifies vision calls as advanced and preserves their source", () => {
    expect(classifyMacroSource('window_exists("Editor");')).toBe("advanced");
    if (rhaiMacro.program.kind !== "rhai")
      throw new Error("expected Rhai macro");
    expect(macroToSource(rhaiMacro)).toBe(rhaiMacro.program.source);
  });

  it("classifies custom stop popups as advanced Rhai", () => {
    expect(classifyMacroSource('stop_with_message("完成");')).toBe("advanced");
    expect(RHAI_API_NAMES).toContain("stop_with_message");
    expect(RHAI_API_SIGNATURES.stop_with_message).toContain(
      "stop_with_message",
    );
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
    expect(normalized.schemaVersion).toBe(6);
    expect(normalized.macroFiles).toEqual([]);
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

  it("normalizes nested macro policy and V2 model fallback metadata", () => {
    const normalized = normalizeConfig({
      behaviorProfilesV2: [
        {
          id: "profile-v2",
          name: "Profile",
          apiVersion: 2,
          sourceSessionIds: ["session-v2"],
          createdAtMs: 1,
          sourceRetention: "ephemeral",
          coverage: {
            rawEventCount: 0,
            pointerEpisodeCount: 0,
            validPointerEpisodeCount: 0,
            clickAssociatedPointerEpisodeCount: 0,
            clickEpisodeCount: 0,
            discardedEventCount: 0,
            discardedReasons: {},
            bucketCoverage: [],
            quality: "insufficient",
          },
          pointerModel: {
            buckets: [],
            totalEpisodeCount: 0,
            validEpisodeCount: 0,
            discardedEpisodeCount: 0,
          },
          clickModel: { buckets: [], totalClickCount: 0, validClickCount: 0 },
        } as unknown as BehaviorProfileV2,
      ],
      macros: [
        {
          ...rhaiMacro,
          behaviorPolicy: {
            enabled: true,
            profileId: "profile-v2",
            timingStrength: 4,
            pointerPathStrength: -1,
            pauseStrength: 0.25,
            correctionStrength: 0.4,
            speedScale: 99,
            seed: 12.9,
          },
        },
      ],
    });
    expect(normalized.macros[0].behaviorPolicy).toMatchObject({
      enabled: true,
      profileId: "profile-v2",
      timingStrength: 1,
      pointerPathStrength: 0,
      speedScale: 4,
      seed: 12,
    });
    expect(normalized.behaviorProfilesV2[0].modelConfig.minBucketSamples).toBe(
      3,
    );
    expect(normalized.behaviorProfilesV2[0].pointerModel.buckets).toEqual([]);
  });

  it("keeps a macro policy when compatible source is edited", () => {
    const policyMacro: MacroRule = {
      ...rhaiMacro,
      program: { kind: "macro", steps: [] },
      behaviorPolicy: {
        enabled: true,
        profileId: "profile-v2",
        timingStrength: 0.5,
        pointerPathStrength: 0.4,
        pauseStrength: 0.3,
        correctionStrength: 0.2,
        speedScale: 1.1,
        seed: 9,
      },
    };
    const parsed = parseMacroSource("move_to(10, 20);", policyMacro);
    expect(parsed.behaviorPolicy).toEqual(policyMacro.behaviorPolicy);
  });
});
