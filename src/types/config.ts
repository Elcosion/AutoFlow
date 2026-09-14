export type HotkeyActionType = "remap" | "launch";

export type HotkeyAction = {
  type: HotkeyActionType;
  target: string;
};

export type HotkeyRule = {
  id: string;
  name: string;
  enabled: boolean;
  triggerKeys: string[];
  action: HotkeyAction;
};

export type TextExpansionRule = {
  id: string;
  name: string;
  abbreviation: string;
  replacement: string;
  enabled: boolean;
  caseSensitive: boolean;
  sensitive: boolean;
};

export type MacroMode = "once" | "repeat" | "hold" | "toggle";
export type KeyAction = "down" | "up";
export type MouseButton = "left" | "right" | "middle" | "x1" | "x2";

export type MacroStep =
  | { type: "delay"; durationMs: number; durationMaxMs?: number }
  | { type: "key"; key: string; action: KeyAction }
  | {
      type: "mouseButton";
      button: MouseButton;
      action: KeyAction;
      x: number;
      y: number;
    }
  | { type: "mouseMove"; x: number; y: number }
  | { type: "wheel"; deltaX: number; deltaY: number }
  | { type: "text"; text: string };

export type MacroTarget = {
  processName: string;
};

export type AutomationProgram =
  | { kind: "macro"; steps: MacroStep[] }
  | { kind: "rhai"; source: string; apiVersion: 1 };

export type AutomationAsset = {
  id: string;
  name: string;
  fileName: string;
  width: number;
  height: number;
  sha256?: string;
};

export type MacroRule = {
  id: string;
  name: string;
  enabled: boolean;
  triggerKeys: string[];
  mode: MacroMode;
  repeatCount: number;
  speed: number;
  recordMouseMove: boolean;
  recordMouseClicks: boolean;
  target?: MacroTarget;
  behaviorPolicy?: BehaviorPolicy;
  program: AutomationProgram;
};

export type BehaviorDistribution = {
  samples: number;
  mean: number;
  stdDev: number;
  min: number;
  max: number;
  p95: number;
};

export type BehaviorEvent =
  | {
      type: "key";
      timestampMs: number;
      vk: number;
      scanCode: number;
      isDown: boolean;
    }
  | { type: "mouseMove"; timestampMs: number; x: number; y: number }
  | {
      type: "mouseButton";
      timestampMs: number;
      button: number;
      isDown: boolean;
      x: number;
      y: number;
    }
  | {
      type: "wheel";
      timestampMs: number;
      deltaX: number;
      deltaY: number;
      x: number;
      y: number;
    };

export type BehaviorProfile = {
  id: string;
  name: string;
  apiVersion: 1;
  sampleCount: number;
  durationMs: number;
  keyboardEvents: number;
  mouseEvents: number;
  wheelEvents: number;
  keyHoldMs: BehaviorDistribution;
  keyIntervalMs: BehaviorDistribution;
  clickHoldMs: BehaviorDistribution;
  clickIntervalMs: BehaviorDistribution;
  mouseSpeedPxPerSec: BehaviorDistribution;
  mousePauseMs: BehaviorDistribution;
  mouseDirectionChangeRate: number;
  mouseJitterPx: number;
  rawEvents: BehaviorEvent[];
};

export type BiomimeticInput = {
  id: string;
  name: string;
  apiVersion: number;
  sourceProfileIds: string[];
  createdAtMs: number;
  profile: BehaviorProfile;
};

export type BehaviorPolicy = {
  enabled: boolean;
  profileId: string | null;
  timingStrength: number;
  pointerPathStrength: number;
  pauseStrength: number;
  correctionStrength: number;
  speedScale: number;
  seed?: number;
};

export type ModelQuality = "insufficient" | "usable" | "good";
export type BehaviorSourceRetention = "persisted" | "ephemeral";

export type BehaviorModelConfig = {
  minBucketSamples: number;
  maxExemplarsPerBucket: number;
  minQualityEpisodes: number;
  usableQualityEpisodes: number;
  goodQualityEpisodes: number;
  minBucketCoverage: number;
};

export type PointerFeatureSummary = {
  samples: number;
  min: number;
  p10: number;
  p25: number;
  p50: number;
  p75: number;
  p90: number;
  max: number;
};

export type PointerBucketModel = {
  key: {
    distance: string;
    direction: string;
    followedByClick: boolean;
    targetWidth: string;
  };
  validSampleCount: number;
  coverage: number;
  fallbackLevel: number;
  exemplars: Array<Record<string, number>>;
  features: Record<string, PointerFeatureSummary>;
};

export type BehaviorProfileV2 = {
  id: string;
  name: string;
  apiVersion: 2;
  sourceSessionIds: string[];
  createdAtMs: number;
  sourceRetention: BehaviorSourceRetention;
  modelConfig: BehaviorModelConfig;
  coverage: {
    rawEventCount: number;
    pointerEpisodeCount: number;
    validPointerEpisodeCount: number;
    clickAssociatedPointerEpisodeCount: number;
    clickEpisodeCount: number;
    discardedEventCount: number;
    discardedReasons: Record<string, number>;
    bucketCoverage: Array<{
      bucket: string;
      validSampleCount: number;
      coverage: number;
      fallbackLevel: number;
    }>;
    quality: ModelQuality;
  };
  pointerModel: {
    buckets: PointerBucketModel[];
    totalEpisodeCount: number;
    validEpisodeCount: number;
    discardedEpisodeCount: number;
  };
  clickModel: {
    buckets: Array<{
      button: MouseButton;
      followedByMove: boolean;
      validSampleCount: number;
      coverage: number;
      preClickDwellMs: PointerFeatureSummary;
      holdMs: PointerFeatureSummary;
      postClickDwellMs: PointerFeatureSummary;
      fallbackLevel: number;
    }>;
    totalClickCount: number;
    validClickCount: number;
  };
  typingModel?: unknown;
  scrollModel?: unknown;
};

export type BehaviorSessionV2 = {
  id: string;
  name: string;
  apiVersion: 2;
  createdAtMs: number;
  durationMs: number;
  taskTag?: string;
  captureMetadata: {
    platform: string;
    hook: string;
    screenWidth?: number;
    screenHeight?: number;
    retainedRawEvents: boolean;
    droppedEventCount: number;
  };
  rawEvents: BehaviorEvent[];
};

export function macroSteps(rule: MacroRule): MacroStep[] {
  return rule.program.kind === "macro" ? rule.program.steps : [];
}

export function withMacroSteps(rule: MacroRule, steps: MacroStep[]): MacroRule {
  return {
    ...rule,
    program: { kind: "macro", steps },
  };
}

export type AppConfig = {
  schemaVersion: number;
  globalEnabled: boolean;
  emergencyStop: string;
  navigationAutoCollapse: boolean;
  launchAtStartup: boolean;
  hotkeys: HotkeyRule[];
  textExpansions: TextExpansionRule[];
  macros: MacroRule[];
  assets: AutomationAsset[];
  behaviorProfiles: BehaviorProfile[];
  biomimeticInputs: BiomimeticInput[];
  activeBehaviorProfileId: string | null;
  selectedBehaviorProfileIds: string[];
  selectedBiomimeticInputIds: string[];
  biomimeticEnabled: boolean;
  biomimeticIntensity: number;
  retainBehaviorRecords: boolean;
  behaviorSessionsV2: BehaviorSessionV2[];
  behaviorProfilesV2: BehaviorProfileV2[];
  activeBehaviorProfileV2Id: string | null;
  behaviorPolicy: BehaviorPolicy;
};

export const defaultConfig: AppConfig = {
  schemaVersion: 5,
  globalEnabled: true,
  emergencyStop: "F12",
  navigationAutoCollapse: false,
  launchAtStartup: false,
  hotkeys: [
    {
      id: "capslock-to-escape",
      name: "CapsLock 改为 Esc",
      enabled: false,
      triggerKeys: ["CapsLock"],
      action: { type: "remap", target: "Esc" },
    },
    {
      id: "open-terminal",
      name: "打开 Windows Terminal",
      enabled: false,
      triggerKeys: ["Ctrl", "Alt", "T"],
      action: { type: "launch", target: "wt.exe" },
    },
  ],
  textExpansions: [
    {
      id: "common-email",
      name: "常用邮箱",
      abbreviation: "@@",
      replacement: "example@example.com",
      enabled: false,
      caseSensitive: false,
      sensitive: false,
    },
  ],
  macros: [],
  assets: [],
  behaviorProfiles: [],
  biomimeticInputs: [],
  activeBehaviorProfileId: null,
  selectedBehaviorProfileIds: [],
  selectedBiomimeticInputIds: [],
  biomimeticEnabled: false,
  biomimeticIntensity: 0.65,
  retainBehaviorRecords: true,
  behaviorSessionsV2: [],
  behaviorProfilesV2: [],
  activeBehaviorProfileV2Id: null,
  behaviorPolicy: {
    enabled: false,
    profileId: null,
    timingStrength: 0,
    pointerPathStrength: 0,
    pauseStrength: 0,
    correctionStrength: 0.15,
    speedScale: 1,
  },
};

export function newRuleId(prefix: string): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return `${prefix}-${crypto.randomUUID()}`;
  }
  return `${prefix}-${Date.now()}`;
}
