import { getCurrentWindow } from "@tauri-apps/api/window";
import type { MouseEvent } from "react";
import { isTauriRuntime } from "../lib/tauri";

function withDesktopWindow(
  action: (window: ReturnType<typeof getCurrentWindow>) => Promise<unknown>,
) {
  if (!isTauriRuntime()) return;
  void action(getCurrentWindow());
}

export function WindowTitlebar() {
  const startDragging = (event: MouseEvent<HTMLElement>) => {
    if (
      event.button !== 0 ||
      (event.target instanceof Element && event.target.closest(".window-controls"))
    ) {
      return;
    }
    withDesktopWindow((window) => window.startDragging());
  };

  return (
    <header className="window-titlebar" onMouseDown={startDragging}>
      <div className="window-titlebar-brand">
        <span aria-hidden="true" className="window-titlebar-mark">
          ●
        </span>
        <span>AutoFlow</span>
      </div>
      <div className="window-controls">
        <button
          aria-label="最小化"
          onClick={() => withDesktopWindow((window) => window.minimize())}
          onMouseDown={(event) => event.stopPropagation()}
          type="button"
        >
          −
        </button>
        <button
          aria-label="最大化或还原"
          onClick={() => withDesktopWindow((window) => window.toggleMaximize())}
          onMouseDown={(event) => event.stopPropagation()}
          type="button"
        >
          □
        </button>
        <button
          aria-label="关闭"
          className="window-close-button"
          onClick={() => withDesktopWindow((window) => window.close())}
          onMouseDown={(event) => event.stopPropagation()}
          type="button"
        >
          ×
        </button>
      </div>
    </header>
  );
}
