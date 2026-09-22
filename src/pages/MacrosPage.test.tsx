// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { defaultConfig, type MacroRule } from "../types/config";
import { MacrosPage } from "./MacrosPage";

const api = vi.hoisted(() => ({
  playbackStatus: vi.fn(),
  play: vi.fn(),
  recordingStatus: vi.fn(),
  stop: vi.fn(),
  validate: vi.fn(),
}));
const app = vi.hoisted(() => ({
  hook: vi.fn(),
  persist: vi.fn(),
  refresh: vi.fn(),
  setError: vi.fn(),
}));

vi.mock("../components/AssetManager", () => ({ AssetManager: () => null }));
vi.mock("../components/BulkActions", () => ({ BulkActions: () => null }));
vi.mock("../components/PageHeader", () => ({ PageHeader: () => null }));
vi.mock("../components/RhaiEditor", () => ({
  RhaiEditor: ({ onCheck }: { onCheck: () => void }) => (
    <button onClick={onCheck}>检查 Rhai</button>
  ),
}));
vi.mock("../lib/useAutoSave", () => ({ useAutoSave: () => undefined }));
vi.mock("../lib/config", () => ({
  toErrorMessage: (reason: unknown) =>
    reason instanceof Error ? reason.message : String(reason),
  useAppConfig: app.hook,
}));
vi.mock("../lib/tauri", () => ({
  getMacroPlaybackStatus: api.playbackStatus,
  getMacroRecordingStatus: api.recordingStatus,
  openDataDirectory: vi.fn(),
  playMacro: api.play,
  setMacroRecordingOptions: vi.fn().mockResolvedValue(undefined),
  startMacroRecording: vi.fn(),
  stopMacro: api.stop,
  stopMacroRecording: vi.fn(),
  validateRhaiSource: api.validate,
}));

const macro: MacroRule = {
  id: "polling-test",
  name: "轮询测试",
  enabled: true,
  triggerKeys: ["F8"],
  mode: "once",
  repeatCount: 1,
  speed: 1,
  recordMouseMove: false,
  recordMouseClicks: false,
  program: { kind: "macro", steps: [{ type: "delay", durationMs: 10 }] },
};

function button(host: HTMLElement, label: string) {
  return [...host.querySelectorAll("button")].find(
    (candidate) => candidate.textContent?.trim() === label,
  );
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.clearAllMocks();
  api.play.mockResolvedValue(undefined);
  api.recordingStatus.mockResolvedValue({
    active: false,
    captureStarted: false,
    stepCount: 0,
    captureMouseMove: false,
    captureMouseClicks: false,
  });
  api.stop.mockResolvedValue(undefined);
  api.validate.mockResolvedValue({ unverifiedCalls: [] });
  app.persist.mockResolvedValue(defaultConfig);
  app.refresh.mockResolvedValue(undefined);
  app.hook.mockReturnValue({
    config: { ...defaultConfig, macros: [macro] },
    loading: false,
    saving: false,
    error: null,
    setError: app.setError,
    persist: app.persist,
    refresh: app.refresh,
  });
});

it("keeps dynamic-call diagnostics visible after static inspection", async () => {
  app.hook.mockReturnValue({
    config: {
      ...defaultConfig,
      macros: [
        {
          ...macro,
          program: {
            kind: "rhai",
            source: "let x = 10; move_to(x, 20)",
            apiVersion: 1,
          },
        },
      ],
    },
    loading: false,
    saving: false,
    error: null,
    setError: app.setError,
    persist: app.persist,
    refresh: app.refresh,
  });
  api.validate.mockResolvedValue({
    unverifiedCalls: [
      { name: "move_to", line: 1, column: 13, reason: "变量类型无法静态确认" },
    ],
  });
  const view = await mount();
  await act(async () => button(view.host, "源码")?.click());
  await act(async () => button(view.host, "检查 Rhai")?.click());
  expect(view.host.textContent).toContain("1 处调用未验证");
  expect(view.host.textContent).toContain(
    "move_to（第 1 行第 13 列）：变量类型无法静态确认",
  );
  expect(view.host.textContent).toContain("不代表运行成功");
  await view.close();
});

afterEach(() => {
  vi.useRealTimers();
  document.body.replaceChildren();
});

async function mount() {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  await act(async () => root.render(<MacrosPage />));
  return {
    host,
    async close() {
      await act(async () => root.unmount());
      host.remove();
    },
  };
}

it.each([
  {
    label: "unavailable observation",
    middleStatus: { phaseObservation: "unavailable" },
  },
  { label: "legacy missing observation", middleStatus: {} },
])(
  "keeps stop controls and polling through $label",
  async ({ middleStatus }) => {
    api.playbackStatus
      .mockResolvedValueOnce({
        running: true,
        phaseObservation: "confirmed",
        currentStep: 1,
        totalSteps: 3,
      })
      .mockResolvedValueOnce({
        running: false,
        ...middleStatus,
        currentStep: 1,
        totalSteps: 3,
      })
      .mockResolvedValueOnce({
        running: false,
        phaseObservation: "confirmed",
        currentStep: 3,
        totalSteps: 3,
      });
    const view = await mount();

    await act(async () => button(view.host, "测试播放")?.click());
    await act(async () => vi.advanceTimersByTimeAsync(3000));
    expect(api.play).toHaveBeenCalledTimes(1);
    expect(button(view.host, "停止播放")).toBeTruthy();

    await act(async () => vi.advanceTimersByTimeAsync(250));
    expect(api.playbackStatus).toHaveBeenCalledTimes(1);
    expect(button(view.host, "停止播放")).toBeTruthy();

    await act(async () => vi.advanceTimersByTimeAsync(250));
    expect(api.playbackStatus).toHaveBeenCalledTimes(2);
    expect(button(view.host, "停止播放")).toBeTruthy();

    await act(async () => vi.advanceTimersByTimeAsync(250));
    expect(api.playbackStatus).toHaveBeenCalledTimes(3);
    expect(button(view.host, "停止播放")).toBeUndefined();
    expect(button(view.host, "测试播放")).toBeTruthy();
    await view.close();
  },
);
