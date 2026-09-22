import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect, useRef, useState } from "react";
import {
  acknowledgeRuntimeNotification,
  isTauriRuntime,
  takeRuntimeNotification,
  type RuntimeNotification,
} from "../lib/tauri";

type NormalizedNotification = RuntimeNotification & {
  mode: "background" | "foreground";
};

function normalizeNotification(
  notification: RuntimeNotification,
): NormalizedNotification {
  return {
    ...notification,
    mode: notification.mode === "foreground" ? "foreground" : "background",
  };
}

function sameNotification(
  left: NormalizedNotification,
  right: NormalizedNotification,
) {
  return (
    left.id === right.id &&
    left.title === right.title &&
    left.message === right.message &&
    left.mode === right.mode
  );
}

function isWindowsPlatform() {
  if (typeof navigator === "undefined") return false;
  const userAgentData = (
    navigator as Navigator & { userAgentData?: { platform?: unknown } }
  ).userAgentData;
  if (typeof userAgentData?.platform === "string" && userAgentData.platform) {
    return /^windows$/i.test(userAgentData.platform);
  }
  if (navigator.platform) return /^win/i.test(navigator.platform);
  return /windows/i.test(navigator.userAgent);
}

/** Presentation owns no execution state. Dismissing never starts/resumes a run. */
export function RuntimeNotifications() {
  const [notification, setNotification] =
    useState<NormalizedNotification | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const acknowledged = useRef(new Set<number>());
  const presentedForeground = useRef(new Set<number>());
  const foregroundInFlight = useRef(new Set<Promise<void>>());
  const dismissingId = useRef<number | null>(null);
  const [presentationRetry, setPresentationRetry] = useState(0);
  const foregroundEpoch = useRef(0);
  const pollInFlight = useRef<Promise<void> | null>(null);
  const mounted = useRef(false);
  async function dismiss() {
    if (!notification || confirming || dismissingId.current === notification.id)
      return;
    const dismissed = notification;
    // Publish synchronously: a same-id background -> foreground upgrade
    // cannot schedule a new focus while the ACK request is in flight.
    dismissingId.current = dismissed.id;
    setConfirming(true);
    let acknowledgedSuccessfully = false;
    try {
      // Do not clear the backend input barrier while a delayed window focus
      // call for this notice could still complete and steal the next target.
      await Promise.all([...foregroundInFlight.current]);
      if (await acknowledgeRuntimeNotification(dismissed.id)) {
        acknowledgedSuccessfully = true;
        // Invalidate an in-flight window sequence before React schedules the
        // dialog clear; effect cleanup may not run until the batch commits.
        foregroundEpoch.current += 1;
        acknowledged.current.add(dismissed.id);
        presentedForeground.current.delete(dismissed.id);
        if (mounted.current) {
          setNotification((current) =>
            current?.id === dismissed.id ? null : current,
          );
          setError(null);
        }
      } else {
        if (mounted.current) {
          setError("通知确认暂未完成，请重试；宏不会重新启动。");
        }
      }
    } catch {
      if (mounted.current) {
        setError("通知服务暂时不可用，请重试；宏不会重新启动。");
      }
    } finally {
      if (!acknowledgedSuccessfully && dismissingId.current === dismissed.id) {
        dismissingId.current = null;
        if (mounted.current) setPresentationRetry((value) => value + 1);
      }
      // On success keep this id fenced until React unmounts/replaces it;
      // older IPC polls must never enqueue late window focus after ACK.
      if (mounted.current) setConfirming(false);
    }
  }
  useEffect(() => {
    mounted.current = true;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    function startPoll() {
      if (disposed) return;
      if (pollInFlight.current) {
        void pollInFlight.current.finally(startPoll);
        return;
      }
      const request = (async () => {
        try {
          const next = await takeRuntimeNotification();
          if (disposed) return;
          if (next && !acknowledged.current.has(next.id)) {
            const normalized = normalizeNotification(next);
            setNotification((current) => {
              if (current && sameNotification(current, normalized))
                return current;
              // A fault may be inserted ahead of a non-fault notice. Switch
              // to the new front; the displaced notice remains in the queue.
              return normalized;
            });
          }
        } catch {
          // Presentation/IPC failure must not affect the input controller.
        }
      })();
      pollInFlight.current = request;
      void request.finally(() => {
        if (pollInFlight.current === request) pollInFlight.current = null;
        acknowledged.current.clear();
        if (!disposed) timer = setTimeout(startPoll, 200);
      });
    }
    startPoll();
    return () => {
      disposed = true;
      mounted.current = false;
      if (timer) clearTimeout(timer);
    };
  }, []);

  useEffect(() => {
    if (
      !notification ||
      notification.mode !== "foreground" ||
      dismissingId.current === notification.id ||
      !isTauriRuntime()
    ) {
      return;
    }
    let cancelled = false;
    let token: number | undefined;
    const isCurrent = () =>
      !cancelled &&
      token !== undefined &&
      foregroundEpoch.current === token &&
      dismissingId.current !== notification.id;
    const present = async () => {
      // Deferring the claim lets React StrictMode finish its development-only
      // setup/cleanup probe without consuming this notification's one attempt.
      await Promise.resolve();
      if (
        cancelled ||
        dismissingId.current === notification.id ||
        presentedForeground.current.has(notification.id)
      )
        return;
      presentedForeground.current.add(notification.id);
      token = foregroundEpoch.current + 1;
      foregroundEpoch.current = token;
      if (!isWindowsPlatform()) {
        console.warn("runtime notification foreground requires Windows");
        return;
      }
      let window;
      if (!isCurrent()) return;
      try {
        window = getCurrentWindow();
      } catch {
        console.warn("runtime notification foreground getCurrentWindow failed");
        return;
      }
      if (!isCurrent()) return;
      try {
        await window.show();
      } catch {
        console.warn("runtime notification foreground show failed");
      }
      if (!isCurrent()) return;
      try {
        await window.unminimize();
      } catch {
        console.warn("runtime notification foreground unminimize failed");
      }
      if (!isCurrent()) return;
      try {
        await window.setFocus();
      } catch {
        console.warn("runtime notification foreground setFocus failed");
      }
      if (!isCurrent()) return;
    };
    const promise = present();
    foregroundInFlight.current.add(promise);
    void promise.finally(() => {
      foregroundInFlight.current.delete(promise);
    });
    return () => {
      cancelled = true;
      if (token !== undefined && foregroundEpoch.current === token) {
        foregroundEpoch.current += 1;
      }
    };
  }, [notification?.id, notification?.mode, presentationRetry]);

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
