import type { MacroPlaybackStatus } from "./tauri";

export const PLAYBACK_OVERLAY_POLL_MS = 180;
export const PLAYBACK_OVERLAY_IDLE_POLL_MS = 1200;

const terminalPhases = new Set<MacroPlaybackStatus["phase"]>([
  "completed",
  "stopped",
  "failed",
  "cleanup_failed",
  "fault_locked",
]);

export function shouldShowPlaybackOverlay(
  status: MacroPlaybackStatus | null,
): boolean {
  if (!status?.overlayVisible || !status.macroName) return false;
  return (
    status.running ||
    status.phase === "cleaning" ||
    terminalPhases.has(status.phase)
  );
}

export function playbackPollInterval(
  status: MacroPlaybackStatus | null,
): number {
  return shouldShowPlaybackOverlay(status)
    ? PLAYBACK_OVERLAY_POLL_MS
    : PLAYBACK_OVERLAY_IDLE_POLL_MS;
}

export function playbackPhaseLabel(
  phase: MacroPlaybackStatus["phase"],
): string {
  switch (phase) {
    case "starting":
      return "正在启动";
    case "fault_locked":
      return "输入安全已锁定";
    case "shutting_down":
      return "正在关闭并清理";
    case "running":
      return "正在执行";
    case "stopping":
      return "正在停止";
    case "cleaning":
      return "正在清理输入状态";
    case "completed":
      return "已完成";
    case "stopped":
      return "已停止";
    case "failed":
      return "执行失败";
    case "cleanup_failed":
      return "输入清理失败";
    case "idle":
    default:
      return "待机";
  }
}

export function playbackActionLabel(status: MacroPlaybackStatus): string {
  if (status.actionSummary) return status.actionSummary;
  if (status.programKind === "rhai" && status.currentStep > 0) {
    return "Rhai API 调用";
  }
  return "准备执行";
}

export function playbackProgressLabel(status: MacroPlaybackStatus): string {
  if (status.programKind === "rhai") {
    return status.currentStep > 0
      ? `第 ${status.currentStep} 次 API 调用`
      : "等待第一次 API 调用";
  }
  if (status.totalSteps <= 0) return "脚本动作数量未知";
  return `第 ${Math.min(status.currentStep, status.totalSteps)} / ${status.totalSteps} 步`;
}

export function formatElapsed(milliseconds: number): string {
  const seconds = Math.max(0, milliseconds) / 1000;
  if (seconds < 60) return `${seconds.toFixed(1)} s`;
  const minutes = Math.floor(seconds / 60);
  return `${minutes}m ${(seconds - minutes * 60).toFixed(0).padStart(2, "0")}s`;
}

export function shortPlaybackError(message?: string): string | null {
  if (!message) return null;
  const normalized = message.replace(/\s+/g, " ").trim();
  if (!normalized) return null;
  return normalized.length > 120 ? `${normalized.slice(0, 117)}…` : normalized;
}
