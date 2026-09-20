import { useEffect, useState } from "react";
import {
  acknowledgeRuntimeNotification,
  takeRuntimeNotification,
  type RuntimeNotification,
} from "../lib/tauri";

/** Presentation owns no execution state. Dismissing never starts/resumes a run. */
export function RuntimeNotifications() {
  const [notification, setNotification] = useState<RuntimeNotification | null>(
    null,
  );
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  async function dismiss() {
    if (!notification || confirming) return;
    setConfirming(true);
    try {
      if (await acknowledgeRuntimeNotification(notification.id)) {
        setNotification(null);
        setError(null);
      } else {
        setError("通知确认暂未完成，请重试；宏不会重新启动。");
      }
    } catch {
      setError("通知服务暂时不可用，请重试；宏不会重新启动。");
    } finally {
      setConfirming(false);
    }
  }
  useEffect(() => {
    if (notification) return;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const next = await takeRuntimeNotification();
        if (disposed) return;
        if (next) {
          setNotification(next);
          return;
        }
      } catch {
        // Presentation/IPC failure must not affect the input controller.
      }
      if (!disposed) timer = setTimeout(poll, 200);
    }
    void poll();
    return () => {
      disposed = true;
      clearTimeout(timer);
    };
  }, [notification]);

  if (!notification) return null;
  return (
    <div className="runtime-notification-backdrop">
      <section
        className="runtime-notification"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="runtime-notification-title"
        aria-describedby="runtime-notification-message"
      >
        <h2 id="runtime-notification-title">{notification.title}</h2>
        <p id="runtime-notification-message">{notification.message}</p>
        {error && <p role="status">{error}</p>}
        <button
          type="button"
          className="button primary"
          disabled={confirming}
          onClick={() => void dismiss()}
        >
          确认
        </button>
      </section>
    </div>
  );
}
