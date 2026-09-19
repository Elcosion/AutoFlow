import { invoke } from "@tauri-apps/api/core";

export type RuntimeNotification = {
  id: number;
  title: string;
  message: string;
};

export async function takeRuntimeNotification(): Promise<RuntimeNotification | null> {
  if (!isTauriRuntime()) return null;
  return invoke<RuntimeNotification | null>("take_runtime_notification");
}

export async function acknowledgeRuntimeNotification(
  id: number,
): Promise<boolean> {
  if (!isTauriRuntime()) return true;
  return invoke<boolean>("acknowledge_runtime_notification", { id });
}
import {
  defaultConfig,
  type AppConfig,
  type AutomationAsset,
  type AutomationProgram,
  type BehaviorPolicy,
  type BehaviorModelConfig,
  type BehaviorProfileV2,
  type BehaviorSessionV2,
  type BiomimeticInput,
  type BehaviorProfile,
  type MacroRule,
  type MacroStep,
  type MacroTarget,
} from "../types/config";

export type RuntimeStatus = {
  appName: string;
  version: string;
  coreState: string;
  source: "tauri" | "browser-preview";
};

export type MacroRecordingStatus = {
  active: boolean;
  captureStarted: boolean;
  stepCount: number;
  captureMouseMove: boolean;
  captureMouseClicks: boolean;
  targetLocked: boolean;
  targetName?: string;
};

export type MacroRecordingResult = {
  steps: MacroStep[];
  target?: MacroTarget;
};

export type MacroPlaybackStatus = {
  running: boolean;
  currentStep: number;
  totalSteps: number;
  lastError?: string;
  playbackId: number;
  macroId?: string;
  macroName?: string;
  programKind: "macro" | "rhai" | "unknown";
  actionKind?: string;
  actionSummary?: string;
  elapsedMs: number;
  phase:
    | "idle"
    | "starting"
    | "running"
    | "stopping"
    | "cleaning"
    | "completed"
    | "stopped"
    | "failed"
    | "cleanup_failed"
    | "fault_locked"
    | "shutting_down";
  cleanupStatus: "not_started" | "pending" | "safe" | "failed" | "unknown";
  overlayVisible: boolean;
};

export type BehaviorRecordingStatus = {
  active: boolean;
  captureStarted: boolean;
  durationMs: number;
  eventCount: number;
  keyboardEvents: number;
  mouseEvents: number;
  wheelEvents: number;
  capped: boolean;
  persistingRawSession: boolean;
  sessionName?: string;
};

export type BehaviorApiFunction = {
  name: string;
  signature: string;
  description: string;
};

export type BehaviorApi = {
  version: number;
  profileId: string;
  profileName: string;
  functions: BehaviorApiFunction[];
  source: string;
};

type TauriStatus = {
  app_name: string;
  version: string;
  core_state: string;
};

export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

const browserConfigKey = "autoflow.config.v1";

function normalizeMacro(value: MacroRule): MacroRule {
  const legacy = value as MacroRule & {
    steps?: MacroStep[];
    program?: AutomationProgram;
  };
  const inputProgram =
    legacy.program?.kind === "macro" && Array.isArray(legacy.program.steps)
      ? legacy.program
      : legacy.program?.kind === "rhai" &&
          typeof legacy.program.source === "string"
        ? {
            kind: "rhai" as const,
            source: legacy.program.source,
            apiVersion: 1 as const,
          }
        : { kind: "macro" as const, steps: legacy.steps ?? [] };
  const steps = Array.isArray(legacy.steps)
    ? legacy.steps
    : inputProgram.kind === "macro"
      ? inputProgram.steps
      : [];
  const program =
    inputProgram.kind === "macro"
      ? { kind: "macro" as const, steps }
      : inputProgram;

  const { steps: _legacySteps, ...withoutLegacySteps } = legacy;
  return {
    ...withoutLegacySteps,
    program,
    recordMouseMove: value.recordMouseMove !== false,
    recordMouseClicks: value.recordMouseClicks !== false,
    behaviorPolicy:
      value.behaviorPolicy === undefined
        ? undefined
        : normalizeBehaviorPolicy(value.behaviorPolicy),
  };
}

export function normalizeBehaviorPolicy(
  value: Partial<BehaviorPolicy> | null | undefined,
  fallback: BehaviorPolicy = defaultConfig.behaviorPolicy,
): BehaviorPolicy {
  const policy = value ?? {};
  const numberInRange = (
    candidate: unknown,
    defaultValue: number,
    minimum: number,
    maximum: number,
  ) =>
    typeof candidate === "number" && Number.isFinite(candidate)
      ? Math.min(maximum, Math.max(minimum, candidate))
      : defaultValue;
  const profileId =
    typeof policy.profileId === "string" && policy.profileId.trim().length > 0
      ? policy.profileId
      : null;
  const seed =
    typeof policy.seed === "number" &&
    Number.isFinite(policy.seed) &&
    policy.seed >= 0
      ? Math.floor(policy.seed)
      : undefined;
  return {
    enabled:
      typeof policy.enabled === "boolean" ? policy.enabled : fallback.enabled,
    profileId,
    timingStrength: numberInRange(
      policy.timingStrength,
      fallback.timingStrength,
      0,
      1,
    ),
    pointerPathStrength: numberInRange(
      policy.pointerPathStrength,
      fallback.pointerPathStrength,
      0,
      1,
    ),
    pauseStrength: numberInRange(
      policy.pauseStrength,
      fallback.pauseStrength,
      0,
      1,
    ),
    correctionStrength: numberInRange(
      policy.correctionStrength,
      fallback.correctionStrength,
      0,
      1,
    ),
    speedScale: numberInRange(policy.speedScale, fallback.speedScale, 0.1, 4),
    ...(seed === undefined ? {} : { seed }),
  };
}

const defaultBehaviorModelConfig: BehaviorModelConfig = {
  minBucketSamples: 3,
  maxExemplarsPerBucket: 32,
  minQualityEpisodes: 3,
  usableQualityEpisodes: 8,
  goodQualityEpisodes: 16,
  minBucketCoverage: 0.5,
};

function normalizeBehaviorModelConfig(
  value: Partial<BehaviorModelConfig> | null | undefined,
): BehaviorModelConfig {
  const input = value ?? {};
  const integer = (candidate: unknown, fallback: number, maximum?: number) => {
    const next =
      typeof candidate === "number" && Number.isFinite(candidate)
        ? Math.max(1, Math.floor(candidate))
        : fallback;
    return maximum === undefined ? next : Math.min(maximum, next);
  };
  const minQualityEpisodes = integer(
    input.minQualityEpisodes,
    defaultBehaviorModelConfig.minQualityEpisodes,
  );
  const usableQualityEpisodes = Math.max(
    minQualityEpisodes,
    integer(
      input.usableQualityEpisodes,
      defaultBehaviorModelConfig.usableQualityEpisodes,
    ),
  );
  return {
    minBucketSamples: integer(
      input.minBucketSamples,
      defaultBehaviorModelConfig.minBucketSamples,
    ),
    maxExemplarsPerBucket: integer(
      input.maxExemplarsPerBucket,
      defaultBehaviorModelConfig.maxExemplarsPerBucket,
      256,
    ),
    minQualityEpisodes,
    usableQualityEpisodes,
    goodQualityEpisodes: Math.max(
      usableQualityEpisodes,
      integer(
        input.goodQualityEpisodes,
        defaultBehaviorModelConfig.goodQualityEpisodes,
      ),
    ),
    minBucketCoverage:
      typeof input.minBucketCoverage === "number" &&
      Number.isFinite(input.minBucketCoverage)
        ? Math.min(1, Math.max(0, input.minBucketCoverage))
        : defaultBehaviorModelConfig.minBucketCoverage,
  };
}

export function normalizeConfig(value: Partial<AppConfig>): AppConfig {
  const behaviorProfiles = Array.isArray(value.behaviorProfiles)
    ? value.behaviorProfiles.map((profile) => ({
        ...profile,
        rawEvents: Array.isArray(profile.rawEvents) ? profile.rawEvents : [],
      }))
    : defaultConfig.behaviorProfiles;
  const biomimeticInputs = Array.isArray(value.biomimeticInputs)
    ? (value.biomimeticInputs as BiomimeticInput[]).map((input) => ({
        ...input,
        profile: {
          ...input.profile,
          rawEvents: Array.isArray(input.profile?.rawEvents)
            ? input.profile.rawEvents
            : [],
        },
      }))
    : defaultConfig.biomimeticInputs;
  const behaviorSessionsV2 = Array.isArray(value.behaviorSessionsV2)
    ? (value.behaviorSessionsV2 as BehaviorSessionV2[])
    : defaultConfig.behaviorSessionsV2;
  const behaviorProfilesV2 = Array.isArray(value.behaviorProfilesV2)
    ? (value.behaviorProfilesV2 as BehaviorProfileV2[]).map((profile) => {
        const modelConfig = normalizeBehaviorModelConfig(profile.modelConfig);
        const pointerBuckets = Array.isArray(profile.pointerModel?.buckets)
          ? profile.pointerModel.buckets.map((bucket) => {
              const validSampleCount = Number.isFinite(bucket.validSampleCount)
                ? Math.max(0, Math.floor(bucket.validSampleCount))
                : 0;
              const trainingReady =
                validSampleCount >= modelConfig.minBucketSamples;
              return {
                ...bucket,
                validSampleCount,
                fallbackLevel: trainingReady ? 0 : 1,
                trainingReady,
                trainingFallbackReason: trainingReady
                  ? undefined
                  : "insufficient_samples",
                exemplars: Array.isArray(bucket.exemplars)
                  ? bucket.exemplars
                  : [],
              };
            })
          : [];
        const eligibleEpisodeCount = pointerBuckets
          .filter((bucket) => bucket.trainingReady)
          .reduce((sum, bucket) => sum + bucket.validSampleCount, 0);
        const validPointerEpisodeCount =
          typeof profile.coverage.validPointerEpisodeCount === "number" &&
          Number.isFinite(profile.coverage.validPointerEpisodeCount)
            ? Math.max(0, profile.coverage.validPointerEpisodeCount)
            : 0;
        const eligibleCoverage =
          validPointerEpisodeCount > 0
            ? Math.min(1, eligibleEpisodeCount / validPointerEpisodeCount)
            : 0;
        const hasMinimumSamples =
          validPointerEpisodeCount >= modelConfig.minQualityEpisodes &&
          pointerBuckets.length > 0;
        const reachesGoodSamples =
          validPointerEpisodeCount >= modelConfig.usableQualityEpisodes &&
          validPointerEpisodeCount >= modelConfig.goodQualityEpisodes;
        const quality = !hasMinimumSamples
          ? "insufficient"
          : !reachesGoodSamples ||
              eligibleCoverage < modelConfig.minBucketCoverage
            ? "usable"
            : "good";
        const bucketCoverage = Array.isArray(profile.coverage.bucketCoverage)
          ? profile.coverage.bucketCoverage.map((bucket) => {
              const validSampleCount = Number.isFinite(bucket.validSampleCount)
                ? Math.max(0, Math.floor(bucket.validSampleCount))
                : 0;
              const trainingReady =
                validSampleCount >= modelConfig.minBucketSamples;
              return {
                ...bucket,
                validSampleCount,
                fallbackLevel: trainingReady ? 0 : 1,
                trainingReady,
                trainingFallbackReason: trainingReady
                  ? undefined
                  : "insufficient_samples",
              };
            })
          : [];
        return {
          ...profile,
          modelConfig,
          pointerModel: {
            ...profile.pointerModel,
            buckets: pointerBuckets,
          },
          clickModel: {
            ...profile.clickModel,
            buckets: Array.isArray(profile.clickModel?.buckets)
              ? profile.clickModel.buckets.map((bucket) => ({
                  ...bucket,
                  fallbackLevel:
                    Number.isFinite(bucket.validSampleCount) &&
                    bucket.validSampleCount >= modelConfig.minBucketSamples
                      ? 0
                      : 1,
                }))
              : [],
          },
          sourceRetention:
            profile.sourceRetention === "ephemeral" ? "ephemeral" : "persisted",
          coverage: {
            ...profile.coverage,
            clickAssociatedPointerEpisodeCount:
              typeof profile.coverage.clickAssociatedPointerEpisodeCount ===
                "number" &&
              Number.isFinite(
                profile.coverage.clickAssociatedPointerEpisodeCount,
              )
                ? profile.coverage.clickAssociatedPointerEpisodeCount
                : 0,
            bucketCoverage,
            quality,
            eligibleEpisodeCount,
            eligibleCoverage,
            qualityFilteredPointerEpisodeCount:
              typeof profile.coverage.qualityFilteredPointerEpisodeCount ===
                "number" &&
              Number.isFinite(
                profile.coverage.qualityFilteredPointerEpisodeCount,
              )
                ? profile.coverage.qualityFilteredPointerEpisodeCount
                : 0,
          },
        };
      })
    : defaultConfig.behaviorProfilesV2;
  const policyValue = value.behaviorPolicy as
    Partial<BehaviorPolicy> | undefined;
  const behaviorPolicy = normalizeBehaviorPolicy(policyValue);
  return {
    ...defaultConfig,
    ...value,
    schemaVersion: 6,
    hotkeys: Array.isArray(value.hotkeys)
      ? value.hotkeys
      : defaultConfig.hotkeys,
    textExpansions: Array.isArray(value.textExpansions)
      ? value.textExpansions
      : defaultConfig.textExpansions,
    macros: Array.isArray(value.macros)
      ? value.macros.map(normalizeMacro)
      : defaultConfig.macros,
    assets: Array.isArray(value.assets) ? value.assets : defaultConfig.assets,
    behaviorProfiles: behaviorProfiles as BehaviorProfile[],
    biomimeticInputs,
    behaviorSessionsV2: behaviorSessionsV2 as BehaviorSessionV2[],
    behaviorProfilesV2: behaviorProfilesV2 as BehaviorProfileV2[],
    activeBehaviorProfileV2Id:
      typeof value.activeBehaviorProfileV2Id === "string" ||
      value.activeBehaviorProfileV2Id === null
        ? value.activeBehaviorProfileV2Id
        : defaultConfig.activeBehaviorProfileV2Id,
    behaviorPolicy,
    activeBehaviorProfileId:
      typeof value.activeBehaviorProfileId === "string" ||
      value.activeBehaviorProfileId === null
        ? value.activeBehaviorProfileId
        : defaultConfig.activeBehaviorProfileId,
    biomimeticEnabled:
      typeof value.biomimeticEnabled === "boolean"
        ? value.biomimeticEnabled
        : defaultConfig.biomimeticEnabled,
    biomimeticIntensity:
      typeof value.biomimeticIntensity === "number" &&
      Number.isFinite(value.biomimeticIntensity)
        ? Math.min(1, Math.max(0, value.biomimeticIntensity))
        : defaultConfig.biomimeticIntensity,
    selectedBehaviorProfileIds: Array.isArray(value.selectedBehaviorProfileIds)
      ? value.selectedBehaviorProfileIds.filter(
          (id): id is string => typeof id === "string",
        )
      : value.activeBehaviorProfileId
        ? [value.activeBehaviorProfileId]
        : defaultConfig.selectedBehaviorProfileIds,
    selectedBiomimeticInputIds: Array.isArray(value.selectedBiomimeticInputIds)
      ? value.selectedBiomimeticInputIds.filter(
          (id): id is string => typeof id === "string",
        )
      : defaultConfig.selectedBiomimeticInputIds,
    retainBehaviorRecords:
      typeof value.retainBehaviorRecords === "boolean"
        ? value.retainBehaviorRecords
        : defaultConfig.retainBehaviorRecords,
  };
}

export async function getConfig(): Promise<AppConfig> {
  if (!isTauriRuntime()) {
    const saved = window.localStorage.getItem(browserConfigKey);
    if (!saved) return defaultConfig;
    try {
      return normalizeConfig(JSON.parse(saved) as Partial<AppConfig>);
    } catch {
      return defaultConfig;
    }
  }
  return normalizeConfig(await invoke<AppConfig>("get_config"));
}

export async function saveConfig(config: AppConfig): Promise<AppConfig> {
  const normalized = normalizeConfig(config);
  if (!isTauriRuntime()) {
    const persisted = JSON.parse(JSON.stringify(normalized)) as AppConfig;
    persisted.macros.forEach((macro) => {
      delete (macro as unknown as { steps?: MacroStep[] }).steps;
    });
    window.localStorage.setItem(browserConfigKey, JSON.stringify(persisted));
    return normalized;
  }
  const persisted = JSON.parse(JSON.stringify(normalized)) as AppConfig;
  persisted.macros.forEach((macro) => {
    delete (macro as unknown as { steps?: MacroStep[] }).steps;
  });
  return normalizeConfig(
    await invoke<AppConfig>("save_config", { config: persisted }),
  );
}

export async function emergencyStop(): Promise<void> {
  if (isTauriRuntime()) await invoke("emergency_stop");
}

export async function startMacroRecording(
  captureMouseMove: boolean,
  captureMouseClicks: boolean,
): Promise<void> {
  if (!isTauriRuntime()) {
    throw new Error("宏录制需要在 Windows 桌面端运行");
  }
  await invoke("start_macro_recording", {
    captureMouseMove,
    captureMouseClicks,
  });
}

export async function setMacroRecordingOptions(
  captureMouseMove: boolean,
  captureMouseClicks: boolean,
): Promise<void> {
  if (!isTauriRuntime()) return;
  await invoke("set_macro_recording_options", {
    captureMouseMove,
    captureMouseClicks,
  });
}

export async function stopMacroRecording(
  discardTrailingMouseInput: boolean,
): Promise<MacroRecordingResult> {
  if (!isTauriRuntime()) {
    throw new Error("宏录制需要在 Windows 桌面端运行");
  }
  return invoke<MacroRecordingResult>("stop_macro_recording", {
    discardTrailingMouseInput,
  });
}

export async function getMacroRecordingStatus(): Promise<MacroRecordingStatus> {
  if (!isTauriRuntime()) {
    throw new Error("宏录制需要在 Windows 桌面端运行");
  }
  return invoke<MacroRecordingStatus>("get_macro_recording_status");
}

export async function playMacro(macroRule: MacroRule): Promise<void> {
  if (!isTauriRuntime()) {
    throw new Error("宏播放需要在 Windows 桌面端运行");
  }
  await invoke("play_macro", { macroRule });
}

export async function stopMacro(): Promise<void> {
  if (isTauriRuntime()) await invoke("stop_macro");
}

export async function recoverInputSafety(): Promise<void> {
  if (!isTauriRuntime()) throw new Error("安全恢复需要 Windows 桌面端");
  await invoke("recover_input_safety");
}

export async function isMacroPlaying(): Promise<boolean> {
  if (!isTauriRuntime()) return false;
  return invoke<boolean>("is_macro_playing");
}

export async function getMacroPlaybackStatus(): Promise<MacroPlaybackStatus> {
  if (!isTauriRuntime()) {
    return {
      running: false,
      currentStep: 0,
      totalSteps: 0,
      playbackId: 0,
      programKind: "unknown",
      elapsedMs: 0,
      phase: "idle",
      cleanupStatus: "not_started",
      overlayVisible: false,
    };
  }
  return invoke<MacroPlaybackStatus>("get_macro_playback_status");
}

export async function validateRhaiSource(source: string): Promise<void> {
  if (!isTauriRuntime()) return;
  await invoke("validate_rhai_source", { source });
}

export async function startBehaviorRecording(name: string): Promise<void> {
  if (!isTauriRuntime()) {
    throw new Error("行为训练需要在 Windows 桌面端运行");
  }
  await invoke("start_behavior_recording", { name });
}

export async function stopBehaviorRecording(): Promise<BehaviorProfileV2> {
  if (!isTauriRuntime()) {
    throw new Error("行为训练需要在 Windows 桌面端运行");
  }
  return invoke<BehaviorProfileV2>("stop_behavior_recording");
}

export async function getBehaviorRecordingStatus(): Promise<BehaviorRecordingStatus> {
  if (!isTauriRuntime()) {
    return {
      active: false,
      captureStarted: false,
      durationMs: 0,
      eventCount: 0,
      keyboardEvents: 0,
      mouseEvents: 0,
      wheelEvents: 0,
      capped: false,
      persistingRawSession: false,
    };
  }
  return invoke<BehaviorRecordingStatus>("get_behavior_recording_status");
}

export async function generateBehaviorApi(
  profileId: string,
): Promise<BehaviorApi> {
  if (!isTauriRuntime()) {
    throw new Error("仿生 API 生成需要在 Windows 桌面端运行");
  }
  return invoke<BehaviorApi>("generate_behavior_api", { profileId });
}

export type ManagedDataDirectory =
  "profiles" | "sessions" | "scripts" | "images";

export async function openDataDirectory(
  subdirectory?: ManagedDataDirectory,
): Promise<string> {
  if (!isTauriRuntime()) {
    throw new Error("数据文件夹需要在 Windows 桌面端打开");
  }
  return invoke<string>("open_data_directory", {
    subdirectory: subdirectory ?? null,
  });
}

export async function deleteBehaviorProfileV2(
  profileId: string,
  confirmed: boolean,
): Promise<AppConfig> {
  if (!isTauriRuntime()) {
    throw new Error("V2 行为档案管理需要在 Windows 桌面端运行");
  }
  return normalizeConfig(
    await invoke<AppConfig>("delete_behavior_profile_v2", {
      profileId,
      confirmed,
    }),
  );
}

export async function deleteBehaviorSessionV2(
  sessionId: string,
  confirmed: boolean,
): Promise<AppConfig> {
  if (!isTauriRuntime()) {
    throw new Error("V2 Session 管理需要在 Windows 桌面端运行");
  }
  return normalizeConfig(
    await invoke<AppConfig>("delete_behavior_session_v2", {
      sessionId,
      confirmed,
    }),
  );
}

export async function retrainBehaviorProfileV2(
  sessionId: string,
): Promise<BehaviorProfileV2> {
  if (!isTauriRuntime()) {
    throw new Error("V2 模型重训需要在 Windows 桌面端运行");
  }
  return invoke<BehaviorProfileV2>("retrain_behavior_profile_v2", {
    sessionId,
  });
}

export async function importAsset(
  name: string,
  fileName: string,
  bytes: Uint8Array,
): Promise<AutomationAsset> {
  if (!isTauriRuntime()) {
    throw new Error("图像资源导入需要在 Windows 桌面端运行");
  }
  return invoke<AutomationAsset>("import_asset", {
    name,
    fileName,
    bytes: Array.from(bytes),
  });
}

export async function readAsset(assetId: string): Promise<Uint8Array> {
  if (!isTauriRuntime()) {
    throw new Error("图像资源读取需要在 Windows 桌面端运行");
  }
  const bytes = await invoke<number[]>("read_asset", { assetId });
  return Uint8Array.from(bytes);
}

export async function renameAsset(
  assetId: string,
  fileName: string,
): Promise<AutomationAsset> {
  if (!isTauriRuntime()) {
    throw new Error("图像资源管理需要在 Windows 桌面端运行");
  }
  return invoke<AutomationAsset>("rename_asset", { assetId, fileName });
}

export async function deleteAsset(
  assetId: string,
  confirmed: boolean,
): Promise<void> {
  if (!isTauriRuntime()) {
    throw new Error("图像资源管理需要在 Windows 桌面端运行");
  }
  await invoke("delete_asset", { assetId, confirmed });
}

export async function runVisionDiagnostic(durationMs = 0): Promise<string> {
  if (!isTauriRuntime()) {
    throw new Error("视觉诊断需要在 Windows 桌面端运行");
  }
  return invoke<string>("run_vision_diagnostic", { durationMs });
}

export async function getRuntimeStatus(): Promise<RuntimeStatus> {
  if (!isTauriRuntime()) {
    return {
      appName: "AutoFlow",
      version: "0.1.0",
      coreState: "浏览器预览",
      source: "browser-preview",
    };
  }

  const status = await invoke<TauriStatus>("get_app_status");
  return {
    appName: status.app_name,
    version: status.version,
    coreState: status.core_state,
    source: "tauri",
  };
}
