import { invoke } from "@tauri-apps/api/core";
import {
  defaultConfig,
  type AppConfig,
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
  stepCount: number;
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

function normalizeConfig(value: Partial<AppConfig>): AppConfig {
  return {
    ...defaultConfig,
    ...value,
    hotkeys: Array.isArray(value.hotkeys)
      ? value.hotkeys
      : defaultConfig.hotkeys,
    textExpansions: Array.isArray(value.textExpansions)
      ? value.textExpansions
      : defaultConfig.textExpansions,
    macros: Array.isArray(value.macros) ? value.macros : defaultConfig.macros,
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
  if (!isTauriRuntime()) {
    window.localStorage.setItem(browserConfigKey, JSON.stringify(config));
    return config;
  }
  return invoke<AppConfig>("save_config", { config });
}

export async function emergencyStop(): Promise<void> {
  if (isTauriRuntime()) await invoke("emergency_stop");
}

export async function startMacroRecording(): Promise<void> {
  if (!isTauriRuntime()) {
    throw new Error("宏录制需要在 Windows 桌面端运行");
  }
  await invoke("start_macro_recording");
}

export async function stopMacroRecording(): Promise<MacroRecordingResult> {
  if (!isTauriRuntime()) {
    throw new Error("宏录制需要在 Windows 桌面端运行");
  }
  return invoke<MacroRecordingResult>("stop_macro_recording");
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
