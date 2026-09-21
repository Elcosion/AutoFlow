// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { RuntimeSafety } from "./RuntimeSafety";
const { status, recover } = vi.hoisted(() => ({
  status: vi.fn(),
  recover: vi.fn(),
}));
vi.mock("../lib/tauri", () => ({
  getMacroPlaybackStatus: status,
  recoverInputSafety: recover,
}));
beforeEach(() => vi.useFakeTimers());
afterEach(() => {
  vi.useRealTimers();
  vi.clearAllMocks();
});
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
  status.mockResolvedValue({
    phase: "fault_locked",
    phaseObservation: "confirmed",
    cleanupStatus: "safe",
  });
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
it("unconfirmed phase or cleanup cannot enable recovery", async () => {
  status.mockResolvedValue({
    phase: "fault_locked",
    phaseObservation: "confirmed",
    cleanupStatus: "unknown",
  });
  const view = await mount();
  expect(view.host.querySelector("button")?.disabled).toBe(true);
  expect(recover).not.toHaveBeenCalled();
  await view.close();

  status.mockResolvedValue({
    phase: "fault_locked",
    phaseObservation: "unavailable",
    cleanupStatus: "safe",
  });
  const unknown = await mount();
  expect(unknown.host.textContent).not.toContain("输入安全已锁定");
  expect(unknown.host.querySelector("button")).toBeNull();
  await unknown.close();
});

it("transitions confirmed fault to query-unknown to normal without stale lock UI", async () => {
  status
    .mockResolvedValueOnce({
      phase: "fault_locked",
      phaseObservation: "confirmed",
      cleanupStatus: "safe",
    })
    .mockRejectedValueOnce({ code: "safety_service_busy" })
    .mockResolvedValue({
      phase: "idle",
      phaseObservation: "confirmed",
      cleanupStatus: "not_started",
    });
  const view = await mount();
  expect(view.host.textContent).toContain("输入安全已锁定");

  await act(async () => vi.advanceTimersByTimeAsync(250));
  expect(view.host.textContent).toContain("输入安全状态暂不可确认");
  expect(view.host.textContent).not.toContain("输入安全已锁定");
  expect(view.host.querySelector("button")).toBeNull();
  expect(recover).not.toHaveBeenCalled();

  await act(async () => vi.advanceTimersByTimeAsync(250));
  expect(view.host.querySelector("aside")).toBeNull();
  expect(recover).not.toHaveBeenCalled();
  await view.close();
});
