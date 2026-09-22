import { useEffect, useRef, useState } from "react";
import {
  cursorPosition,
  currentMonitor,
  getCurrentWindow,
  monitorFromPoint,
  type Monitor,
} from "@tauri-apps/api/window";
import { LogicalSize, PhysicalPosition } from "@tauri-apps/api/dpi";
import {
  getMacroPlaybackStatus,
  isTauriRuntime,
  type MacroPlaybackStatus,
} from "../lib/tauri";
import {
  formatElapsed,
  playbackActionLabel,
  observedPlaybackPhase,
  playbackPhaseLabel,
  playbackProgressLabel,
  playbackPollInterval,
  PLAYBACK_OVERLAY_IDLE_POLL_MS,
  shortPlaybackError,
  shouldShowPlaybackOverlay,
  unavailablePlaybackStatus,
} from "../lib/playbackOverlay";

const OVERLAY_SIZE = new LogicalSize(320, 178);

async function positionOnCurrentTargetMonitor(
  overlay: ReturnType<typeof getCurrentWindow>,
): Promise<void> {
  const cursor = await cursorPosition();
  const monitor: Monitor | null =
    (await monitorFromPoint(cursor.x, cursor.y)) ?? (await currentMonitor());
  if (!monitor) return;

  const physicalSize = OVERLAY_SIZE.toPhysical(monitor.scaleFactor);
  const margin = Math.round(16 * monitor.scaleFactor);
  const x =
    monitor.workArea.position.x +
    monitor.workArea.size.width -
    physicalSize.width -
    margin;
  const y = monitor.workArea.position.y + margin;
  await overlay.setPosition(new PhysicalPosition(Math.round(x), Math.round(y)));
}

export function PlaybackOverlay() {
  const [status, setStatus] = useState<MacroPlaybackStatus | null>(null);
  const latestPlaybackId = useRef(0);
  const polling = useRef(false);

  useEffect(() => {
    document.body.classList.add("playback-overlay-body");
    if (!isTauriRuntime()) {
      return () => document.body.classList.remove("playback-overlay-body");
    }

    const overlay = getCurrentWindow();
    let active = true;
    let timer: number | undefined;
    let poll: () => Promise<void>;
    const schedule = (delay: number) => {
      if (!active) return;
      timer = window.setTimeout(() => {
        timer = undefined;
        void poll();
      }, delay);
    };
    poll = async () => {
      if (!active) return;
      if (polling.current) {
        schedule(PLAYBACK_OVERLAY_IDLE_POLL_MS);
        return;
      }
      polling.current = true;
      try {
        const next = await getMacroPlaybackStatus();
        if (!active) return;
        setStatus(next);
        if (
          next.playbackId !== 0 &&
          next.playbackId !== latestPlaybackId.current
        ) {
          latestPlaybackId.current = next.playbackId;
          await positionOnCurrentTargetMonitor(overlay);
        }
        if (shouldShowPlaybackOverlay(next)) {
          await overlay.show();
        } else {
          await overlay.hide();
        }
        schedule(playbackPollInterval(next));
      } catch (error) {
        // The overlay is deliberately best-effort. A closed or unavailable
        // overlay must never affect macro execution or F12 cleanup.
        console.warn("[playback-overlay] 更新悬浮窗失败", error);
        if (active) setStatus((current) => unavailablePlaybackStatus(current));
        schedule(PLAYBACK_OVERLAY_IDLE_POLL_MS);
      } finally {
        polling.current = false;
      }
    };

    void poll();
    return () => {
      active = false;
      if (timer !== undefined) window.clearTimeout(timer);
      void overlay.hide().catch(() => undefined);
      document.body.classList.remove("playback-overlay-body");
    };
  }, []);

  if (!shouldShowPlaybackOverlay(status)) return null;
  const error = shortPlaybackError(status?.lastError);
  const cleanupFailed = status?.phase === "cleanup_failed";

  return (
    <main className="playback-overlay-root" aria-live="polite">
      <section
        className={`playback-overlay-card ${cleanupFailed ? "is-danger" : ""}`}
      >
        <div className="playback-overlay-heading">
          <span className="playback-overlay-dot" />
          <strong>{status?.macroName}</strong>
          <span className="playback-overlay-phase">
            {status
              ? playbackPhaseLabel(observedPlaybackPhase(status))
              : playbackPhaseLabel("unknown")}
          </span>
        </div>
        <div className="playback-overlay-action">
          {status && playbackActionLabel(status)}
        </div>
        <div className="playback-overlay-meta">
          <span>{status && playbackProgressLabel(status)}</span>
          <span>{formatElapsed(status?.elapsedMs ?? 0)}</span>
        </div>
        {error ? <div className="playback-overlay-error">{error}</div> : null}
        <div className="playback-overlay-hint">
          F12 可随时停止，输入状态将在清理完成后结束
        </div>
      </section>
    </main>
  );
}
