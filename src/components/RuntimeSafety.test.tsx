// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import { RuntimeSafety } from "./RuntimeSafety";
const { status, recover } = vi.hoisted(() => ({
  status: vi.fn(),
  recover: vi.fn(),
}));
vi.mock("../lib/tauri", () => ({
  getMacroPlaybackStatus: status,
  recoverInputSafety: recover,
}));
afterEach(() => vi.clearAllMocks());
async function mount() {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  await act(async () => {
    root.render(<RuntimeSafety />);
  });
  return {
    host,
    async close() {
      await act(async () => root.unmount());
      host.remove();
    },
  };
}
it("recovery failure keeps lock visible; successful explicit recovery never plays", async () => {
  status.mockResolvedValue({ phase: "fault_locked", cleanupStatus: "safe" });
  recover
    .mockRejectedValueOnce({ message: "检测通道未就绪" })
    .mockResolvedValue(undefined);
  const view = await mount();
  expect(recover).not.toHaveBeenCalled();
  expect(view.host.textContent).toContain("不会继续或重新播放宏");
  await act(async () => view.host.querySelector("button")?.click());
  expect(view.host.textContent).toContain("检测通道未就绪");
  await act(async () => view.host.querySelector("button")?.click());
  expect(recover).toHaveBeenCalledTimes(2);
  expect(view.host.querySelector("aside")).toBeNull();
  await view.close();
});
it("unknown cleanup disables recovery and busy polling does not report a fault", async () => {
  status.mockResolvedValue({ phase: "fault_locked", cleanupStatus: "unknown" });
  const view = await mount();
  expect(view.host.querySelector("button")?.disabled).toBe(true);
  expect(recover).not.toHaveBeenCalled();
  await view.close();
  status.mockRejectedValue({ code: "safety_service_busy" });
  const busy = await mount();
  expect(busy.host.querySelector("aside")).toBeNull();
  await busy.close();
});
