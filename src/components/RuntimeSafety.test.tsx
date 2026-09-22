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
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
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

it("ignores a stale fault poll across successful recovery until a fresh query", async () => {
  const stalePoll = deferred<{
    phase: "fault_locked";
    phaseObservation: "confirmed";
    cleanupStatus: "safe";
    lastError: string;
  }>();
  status
    .mockResolvedValueOnce({
      phase: "fault_locked",
      phaseObservation: "confirmed",
      cleanupStatus: "safe",
      lastError: "initial fault",
    })
    .mockReturnValueOnce(stalePoll.promise)
    .mockResolvedValueOnce({
      phase: "fault_locked",
      phaseObservation: "confirmed",
      cleanupStatus: "safe",
      lastError: "fresh fault",
    });
  recover.mockResolvedValue(undefined);
  const view = await mount();

  await act(async () => vi.advanceTimersByTimeAsync(250));
  expect(status).toHaveBeenCalledTimes(2);
  await act(async () => view.host.querySelector("button")?.click());
  expect(view.host.querySelector("aside")).toBeNull();

  await act(async () => {
    stalePoll.resolve({
      phase: "fault_locked",
      phaseObservation: "confirmed",
      cleanupStatus: "safe",
      lastError: "stale fault",
    });
    await stalePoll.promise;
  });
  expect(view.host.querySelector("aside")).toBeNull();

  await act(async () => vi.advanceTimersByTimeAsync(0));
  expect(status).toHaveBeenCalledTimes(3);
  expect(view.host.textContent).toContain("fresh fault");
  expect(view.host.textContent).not.toContain("stale fault");
  expect(view.host.querySelector("button")?.disabled).toBe(false);
  await view.close();
});

it("does not start a queued poll through an in-flight recovery", async () => {
  const recovery = deferred<undefined>();
  status
    .mockResolvedValueOnce({
      phase: "fault_locked",
      phaseObservation: "confirmed",
      cleanupStatus: "safe",
    })
    .mockResolvedValueOnce({
      phase: "idle",
      phaseObservation: "confirmed",
      cleanupStatus: "not_started",
    });
  recover.mockReturnValue(recovery.promise);
  const view = await mount();

  await act(async () => view.host.querySelector("button")?.click());
  await act(async () => vi.advanceTimersByTimeAsync(250));
  expect(status).toHaveBeenCalledTimes(1);

  await act(async () => {
    recovery.resolve(undefined);
    await recovery.promise;
  });
  await act(async () => vi.advanceTimersByTimeAsync(0));
  expect(status).toHaveBeenCalledTimes(2);
  expect(view.host.querySelector("aside")).toBeNull();
  await view.close();
});

it("does not publish recovery or polling results after unmount", async () => {
  const stalePoll = deferred<{
    phase: "fault_locked";
    phaseObservation: "confirmed";
    cleanupStatus: "safe";
  }>();
  const recovery = deferred<undefined>();
  status
    .mockResolvedValueOnce({
      phase: "fault_locked",
      phaseObservation: "confirmed",
      cleanupStatus: "safe",
    })
    .mockReturnValueOnce(stalePoll.promise);
  recover.mockReturnValue(recovery.promise);
  const view = await mount();

  await act(async () => vi.advanceTimersByTimeAsync(250));
  await act(async () => view.host.querySelector("button")?.click());
  await view.close();
  await act(async () => {
    recovery.resolve(undefined);
    stalePoll.resolve({
      phase: "fault_locked",
      phaseObservation: "confirmed",
      cleanupStatus: "safe",
    });
    await Promise.all([recovery.promise, stalePoll.promise]);
    await vi.advanceTimersByTimeAsync(1000);
  });
  expect(status).toHaveBeenCalledTimes(2);
});
