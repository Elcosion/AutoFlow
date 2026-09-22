// @vitest-environment jsdom
import { act, StrictMode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RuntimeNotifications } from "./RuntimeNotifications";

const mocks = vi.hoisted(() => ({
  take: vi.fn(),
  acknowledge: vi.fn(),
  isTauri: vi.fn(),
  getCurrentWindow: vi.fn(),
  show: vi.fn(),
  unminimize: vi.fn(),
  setFocus: vi.fn(),
}));

vi.mock("../lib/tauri", () => ({
  takeRuntimeNotification: mocks.take,
  acknowledgeRuntimeNotification: mocks.acknowledge,
  isTauriRuntime: mocks.isTauri,
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: mocks.getCurrentWindow,
}));

let root: Root | undefined;
let host: HTMLDivElement | undefined;

async function renderNotifications(strict = false) {
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () => {
    root?.render(
      strict ? (
        <StrictMode>
          <RuntimeNotifications />
        </StrictMode>
      ) : (
        <RuntimeNotifications />
      ),
    );
    await Promise.resolve();
  });
  return host;
}

async function pollAgain() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(200);
  });
}

function setPlatform(
  userAgentDataPlatform: string | undefined,
  platform: string,
  userAgent: string,
) {
  Object.defineProperties(navigator, {
    userAgentData: {
      configurable: true,
      value:
        userAgentDataPlatform === undefined
          ? undefined
          : { platform: userAgentDataPlatform },
    },
    platform: { configurable: true, value: platform },
    userAgent: { configurable: true, value: userAgent },
  });
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  vi.useFakeTimers();
  setPlatform("Windows", "Win32", "Mozilla/5.0 (Windows NT 10.0)");
  vi.spyOn(console, "warn").mockImplementation(() => {});
  mocks.take.mockResolvedValue(null);
  mocks.acknowledge.mockResolvedValue(true);
  mocks.isTauri.mockReturnValue(false);
  mocks.show.mockResolvedValue(undefined);
  mocks.unminimize.mockResolvedValue(undefined);
  mocks.setFocus.mockResolvedValue(undefined);
  mocks.getCurrentWindow.mockReturnValue({
    show: mocks.show,
    unminimize: mocks.unminimize,
    setFocus: mocks.setFocus,
  });
});

afterEach(async () => {
  if (root) await act(async () => root?.unmount());
  host?.remove();
  root = undefined;
  host = undefined;
  vi.useRealTimers();
  vi.clearAllMocks();
  vi.restoreAllMocks();
});

it("renders plain notification text and acknowledges without executing content", async () => {
  mocks.take.mockResolvedValueOnce({
    id: 1,
    title: "完成",
    message: "<script>not executable</script>",
  });
  const view = await renderNotifications();
  expect(view.querySelector('[role="alertdialog"]')?.textContent).toContain(
    "完成",
  );
  expect(view.querySelector("script")).toBeNull();
  await act(async () => view.querySelector("button")?.click());
  expect(view.querySelector('[role="alertdialog"]')).toBeNull();
  expect(mocks.acknowledge).toHaveBeenCalledWith(1);
});

it("upgrades the same id on Windows Tauri and presents it once", async () => {
  mocks.isTauri.mockReturnValue(true);
  mocks.take
    .mockResolvedValueOnce({ id: 2, title: "完成", message: "内容" })
    .mockResolvedValue({
      id: 2,
      title: "完成",
      message: "内容",
      mode: "foreground",
    });
  await renderNotifications();
  expect(mocks.getCurrentWindow).not.toHaveBeenCalled();
  await pollAgain();
  expect(mocks.show).toHaveBeenCalledTimes(1);
  expect(mocks.unminimize).toHaveBeenCalledTimes(1);
  expect(mocks.setFocus).toHaveBeenCalledTimes(1);
  await pollAgain();
  expect(mocks.getCurrentWindow).toHaveBeenCalledTimes(1);
});

it("downgrades a foreground notification on non-Windows Tauri", async () => {
  setPlatform("macOS", "Win32", "Mozilla/5.0 (Windows NT 10.0)");
  mocks.isTauri.mockReturnValue(true);
  mocks.take.mockResolvedValueOnce({
    id: 9,
    title: "完成",
    message: "内容",
    mode: "foreground",
  });
  const view = await renderNotifications();
  expect(view.querySelector('[role="alertdialog"]')).not.toBeNull();
  expect(mocks.getCurrentWindow).not.toHaveBeenCalled();
  expect(console.warn).toHaveBeenCalledWith(
    "runtime notification foreground requires Windows",
  );
});

it("fails closed on an unknown Tauri platform", async () => {
  setPlatform(undefined, "", "AutoFlowTest");
  mocks.isTauri.mockReturnValue(true);
  mocks.take.mockResolvedValueOnce({
    id: 10,
    title: "完成",
    message: "内容",
    mode: "foreground",
  });
  const view = await renderNotifications();
  expect(view.querySelector('[role="alertdialog"]')).not.toBeNull();
  expect(mocks.getCurrentWindow).not.toHaveBeenCalled();
  expect(console.warn).toHaveBeenCalledWith(
    "runtime notification foreground requires Windows",
  );
});

it("presents a foreground id exactly once under React StrictMode", async () => {
  mocks.isTauri.mockReturnValue(true);
  mocks.take.mockResolvedValue({
    id: 8,
    title: "完成",
    message: "内容",
    mode: "foreground",
  });
  await renderNotifications(true);
  expect(mocks.show).toHaveBeenCalledTimes(1);
  expect(mocks.unminimize).toHaveBeenCalledTimes(1);
  expect(mocks.setFocus).toHaveBeenCalledTimes(1);
});

describe.each(["show", "unminimize", "setFocus"] as const)(
  "%s rejection",
  (action) => {
    it("keeps the alert dismissible and continues independent actions", async () => {
      mocks.isTauri.mockReturnValue(true);
      mocks[action].mockRejectedValueOnce(new Error("window failure"));
      mocks.take.mockResolvedValueOnce({
        id: 3,
        title: "完成",
        message: "内容",
        mode: "foreground",
      });
      const view = await renderNotifications();
      expect(view.querySelector('[role="alertdialog"]')).not.toBeNull();
      if (action !== "setFocus") expect(mocks.setFocus).toHaveBeenCalledOnce();
      await act(async () => view.querySelector("button")?.click());
      expect(mocks.acknowledge).toHaveBeenCalledWith(3);
      expect(view.querySelector('[role="alertdialog"]')).toBeNull();
    });
  },
);

it.each([
  ["legacy", {}, true],
  ["background", { mode: "background" }, true],
  ["unknown", { mode: "urgent" }, true],
  ["browser", { mode: "foreground" }, false],
])("%s notification performs no window calls", async (_name, extra, tauri) => {
  mocks.isTauri.mockReturnValue(tauri);
  mocks.take.mockResolvedValueOnce({
    id: 4,
    title: "完成",
    message: "内容",
    ...extra,
  });
  await renderNotifications();
  expect(mocks.getCurrentWindow).not.toHaveBeenCalled();
  if (_name === "browser") expect(console.warn).not.toHaveBeenCalled();
});

it("suppresses an in-flight same-id poll response after acknowledgement", async () => {
  let resolvePoll!: (value: unknown) => void;
  const delayed = new Promise((resolve) => {
    resolvePoll = resolve;
  });
  mocks.take
    .mockResolvedValueOnce({ id: 5, title: "完成", message: "内容" })
    .mockReturnValueOnce(delayed);
  const view = await renderNotifications();
  await act(async () => {
    vi.advanceTimersByTime(200);
    await Promise.resolve();
  });
  await act(async () => view.querySelector("button")?.click());
  await act(async () => resolvePoll({ id: 5, title: "完成", message: "内容" }));
  expect(view.querySelector('[role="alertdialog"]')).toBeNull();
});

it("cancels pending polling work after unmount", async () => {
  let resolveTake!: (value: unknown) => void;
  mocks.isTauri.mockReturnValue(true);
  mocks.take.mockReturnValueOnce(
    new Promise((resolve) => {
      resolveTake = resolve;
    }),
  );
  const view = await renderNotifications();
  await act(async () => root?.unmount());
  root = undefined;
  await act(async () =>
    resolveTake({
      id: 6,
      title: "完成",
      message: "内容",
      mode: "foreground",
    }),
  );
  expect(view.textContent).toBe("");
  expect(mocks.getCurrentWindow).not.toHaveBeenCalled();
});

it("invalidates a pending foreground step before batched acknowledgement clears state", async () => {
  let resolveShow!: () => void;
  let resolveAcknowledge!: (value: boolean) => void;
  mocks.isTauri.mockReturnValue(true);
  mocks.show.mockReturnValueOnce(
    new Promise<void>((resolve) => {
      resolveShow = resolve;
    }),
  );
  mocks.acknowledge.mockReturnValueOnce(
    new Promise<boolean>((resolve) => {
      resolveAcknowledge = resolve;
    }),
  );
  mocks.take.mockResolvedValueOnce({
    id: 7,
    title: "完成",
    message: "内容",
    mode: "foreground",
  });
  const view = await renderNotifications();
  expect(mocks.show).toHaveBeenCalledOnce();
  await act(async () => {
    view.querySelector("button")?.click();
    resolveAcknowledge(true);
    await Promise.resolve();
    expect(view.querySelector('[role="alertdialog"]')).not.toBeNull();
    resolveShow();
    await Promise.resolve();
  });
  expect(mocks.unminimize).not.toHaveBeenCalled();
  expect(mocks.setFocus).not.toHaveBeenCalled();
  expect(view.querySelector('[role="alertdialog"]')).toBeNull();
});
