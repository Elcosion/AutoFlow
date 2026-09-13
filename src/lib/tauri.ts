import { invoke } from "@tauri-apps/api/core";
import {
  defaultConfig,
  type AppConfig,
  type AutomationAsset,
  type AutomationProgram,
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
        ? { kind: "rhai" as const, source: legacy.program.source, apiVersion: 1 as const }
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
  };
}

export function normalizeConfig(value: Partial<AppConfig>): AppConfig {
  return {
    ...defaultConfig,
    ...value,
    schemaVersion: 2,
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
  return normalizeConfig(await invoke<AppConfig>("save_config", { config: persisted }));
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

export async function isMacroPlaying(): Promise<boolean> {
  if (!isTauriRuntime()) return false;
  return invoke<boolean>("is_macro_playing");
}

export async function getMacroPlaybackStatus(): Promise<MacroPlaybackStatus> {
  if (!isTauriRuntime()) {
    return { running: false, currentStep: 0, totalSteps: 0 };
  }
  return invoke<MacroPlaybackStatus>("get_macro_playback_status");
}

export async function validateRhaiSource(source: string): Promise<void> {
  if (!isTauriRuntime()) return;
  await invoke("validate_rhai_source", { source });
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

export async function renameAsset(assetId: string, name: string): Promise<AutomationAsset> {
  if (!isTauriRuntime()) {
    throw new Error("图像资源管理需要在 Windows 桌面端运行");
  }
  return invoke<AutomationAsset>("rename_asset", { assetId, name });
}

export async function deleteAsset(assetId: string, confirmed: boolean): Promise<void> {
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
