import { useEffect, useState } from "react";
import {
  getMacroPlaybackStatus,
  recoverInputSafety,
  type MacroPlaybackStatus,
} from "../lib/tauri";

/** Observer only. Recovery validates safety; it never starts or resumes work. */
export function RuntimeSafety() {
  const [status, setStatus] = useState<MacroPlaybackStatus | null>(null);
  const [recovering, setRecovering] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const snapshot = await getMacroPlaybackStatus();
        if (active) setStatus(snapshot);
      } catch (failure) {
        if (
          active &&
          (failure as { code?: string })?.code !== "safety_service_busy"
        ) {
          setError("安全状态查询失败，请停止测试；不会自动恢复或重新播放。");
        }
      }
      if (active) timer = setTimeout(() => void poll(), 250);
    }
    void poll();
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, []);

  async function recover() {
    if (
      recovering ||
      status?.phase !== "fault_locked" ||
      status.cleanupStatus === "unknown"
    )
      return;
    setRecovering(true);
    setError(null);
    try {
      await recoverInputSafety();
      setStatus(null);
    } catch (failure) {
      setError(
        (failure as { message?: string })?.message ??
          "安全恢复失败，请检查急停通道和输入清理状态。",
      );
    } finally {
      setRecovering(false);
    }
  }
  if (status?.phase !== "fault_locked" && !error) return null;
  return (
    <aside
      className="runtime-safety-notice"
      role="alert"
      aria-label="输入安全状态"
    >
      <strong>输入安全已锁定</strong>
      <p>
        {status?.lastError ??
          "安全通道或输入清理状态需要重新确认。请先松开宏触发键。"}
      </p>
      <p>恢复仅重新验证安全状态，不会继续或重新播放宏。</p>
      {error && <p role="status">{error}</p>}
      <button
        type="button"
        disabled={
          recovering ||
          status?.phase !== "fault_locked" ||
          status.cleanupStatus === "unknown"
        }
        onClick={() => void recover()}
      >
        {recovering ? "正在验证安全状态…" : "验证并解除安全锁定"}
      </button>
      {status?.cleanupStatus === "unknown" && (
        <p>
          安全服务已失联，不能在线解除锁定。请停止测试并确认进程及输入状态。
        </p>
      )}
    </aside>
  );
}
