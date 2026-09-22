import { describe, expect, it } from "vitest";
import {
  formatElapsed,
  observedPlaybackPhase,
  playbackActionLabel,
  playbackPhaseLabel,
  playbackPollInterval,
  playbackProgressLabel,
  PLAYBACK_OVERLAY_IDLE_POLL_MS,
  PLAYBACK_OVERLAY_POLL_MS,
  shouldShowPlaybackOverlay,
  shortPlaybackError,
  unavailablePlaybackStatus,
} from "./playbackOverlay";
import type { MacroPlaybackStatus } from "./tauri";

const status = (
  patch: Partial<MacroPlaybackStatus> = {},
): MacroPlaybackStatus => ({
  running: true,
  currentStep: 2,
  totalSteps: 5,
  playbackId: 7,
  macroId: "macro-1",
  macroName: "测试宏",
  programKind: "macro",
  actionKind: "mouse_move",
  actionSummary: "移动鼠标至 (10, 20)",
  elapsedMs: 1234,
  phase: "running",
  phaseObservation: "confirmed",
  phaseProvenance: "controller_state",
  cleanupStatus: "not_started",
  overlayVisible: true,
  ...patch,
});

describe("playback overlay status mapping", () => {
  it("only shows for an active or recent playback snapshot", () => {
    expect(shouldShowPlaybackOverlay(status())).toBe(true);
    expect(shouldShowPlaybackOverlay(status({ overlayVisible: false }))).toBe(
      false,
    );
    expect(shouldShowPlaybackOverlay(status({ macroName: undefined }))).toBe(
      false,
    );
    expect(
      shouldShowPlaybackOverlay(
        status({ running: false, phase: "idle", overlayVisible: true }),
      ),
    ).toBe(false);
    expect(
      shouldShowPlaybackOverlay(
        status({
          running: false,
          phase: "cleanup_failed",
          overlayVisible: true,
        }),
      ),
    ).toBe(true);
  });

  it("distinguishes graph steps from Rhai API calls", () => {
    expect(playbackProgressLabel(status())).toBe("第 2 / 5 步");
    expect(
      playbackProgressLabel(
        status({ programKind: "rhai", currentStep: 4, totalSteps: 0 }),
      ),
    ).toBe("第 4 次 API 调用");
    expect(
      playbackActionLabel(
        status({ programKind: "rhai", actionSummary: undefined }),
      ),
    ).toBe("Rhai API 调用");
  });

  it("formats phase, elapsed time, and bounded errors", () => {
    expect(playbackPhaseLabel("cleaning")).toBe("正在清理输入状态");
    expect(playbackPhaseLabel("cleanup_failed")).toBe("输入清理失败");
    expect(playbackPhaseLabel("unknown")).toBe("状态暂不可确认");
    expect(formatElapsed(1234)).toBe("1.2 s");
    expect(formatElapsed(61_000)).toBe("1m 01s");
    expect(shortPlaybackError(" a\n b ")).toBe("a b");
    expect(shortPlaybackError("x".repeat(200))).toHaveLength(118);
  });

  it("maps unavailable observations and query failures to unknown, never idle", () => {
    const unavailable = status({
      phase: "fault_locked",
      phaseObservation: "unavailable",
      phaseProvenance: "controller_busy",
    });
    expect(observedPlaybackPhase(unavailable)).toBe("unknown");
    expect(playbackPhaseLabel(observedPlaybackPhase(unavailable))).not.toBe(
      "待机",
    );

    const failedQuery = unavailablePlaybackStatus(status());
    expect(failedQuery.phase).toBe("unknown");
    expect(failedQuery.phaseObservation).toBe("unavailable");
    expect(failedQuery.phaseProvenance).toBe("transport_error");
    expect(shouldShowPlaybackOverlay(failedQuery)).toBe(true);
    expect(playbackPollInterval(failedQuery)).toBe(PLAYBACK_OVERLAY_POLL_MS);
  });

  it("uses high-frequency polling only while the overlay is visible", () => {
    expect(playbackPollInterval(status())).toBe(PLAYBACK_OVERLAY_POLL_MS);
    expect(playbackPollInterval(status({ overlayVisible: false }))).toBe(
      PLAYBACK_OVERLAY_IDLE_POLL_MS,
    );
    expect(
      playbackPollInterval(
        status({ running: false, phase: "idle", overlayVisible: false }),
      ),
    ).toBe(PLAYBACK_OVERLAY_IDLE_POLL_MS);
  });
});
