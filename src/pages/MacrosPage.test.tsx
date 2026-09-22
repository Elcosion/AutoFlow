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
  RhaiEditor: ({
    onCheck,
    onChange,
    value,
  }: {
    onCheck: () => void;
    onChange: (value: string) => void;
    value: string;
  }) => (
    <>
      <button onClick={onCheck}>检查 Rhai</button>
      <textarea
        aria-label="测试 Rhai 源码"
        onChange={(event) => onChange(event.currentTarget.value)}
        value={value}
      />
    </>
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

type TestReport = {
  unverifiedCalls: Array<{
    name: string;
    line: number;
    column: number;
    reason: string;
  }>;
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
  expect(button(view.host, "关闭")).toBeTruthy();
  await act(async () => vi.advanceTimersByTimeAsync(2000));
  expect(view.host.textContent).toContain("不代表运行成功");
  await act(async () => button(view.host, "关闭")?.click());
  expect(view.host.textContent).not.toContain("不代表运行成功");
  await act(async () => button(view.host, "检查 Rhai")?.click());
  const closeButton = view.host.querySelector<HTMLButtonElement>(
    'button[aria-label="关闭检查结果"]',
  );
  expect(closeButton?.type).toBe("button");
  await act(async () =>
    closeButton?.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
    ),
  );
  expect(view.host.textContent).not.toContain("不代表运行成功");
  await view.close();
});

it("keeps ordinary success and syntax failures as an explicit result", async () => {
  const view = await mount();
  await act(async () => button(view.host, "源码")?.click());
  await act(async () => button(view.host, "检查 Rhai")?.click());
  expect(view.host.textContent).toContain("语法检查通过");

  const editor = view.host.querySelector<HTMLTextAreaElement>(
    'textarea[aria-label="测试 Rhai 源码"]',
  );
  expect(editor).toBeTruthy();
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "value",
    )?.set?.call(editor, "unknown_api()");
    editor!.dispatchEvent(new Event("input", { bubbles: true }));
  });
  expect(view.host.textContent).toContain("内容已更改，结果已过期，请重新检查");
  await act(async () => button(view.host, "检查 Rhai")?.click());
  expect(view.host.textContent).toContain("语法检查失败");
  expect(view.host.textContent).toContain("关闭");
  expect(view.host.querySelector(".macro-source-error")).toBeNull();
  await act(async () => button(view.host, "关闭")?.click());
  expect(view.host.textContent).not.toContain("语法检查失败");
  expect(view.host.querySelector(".macro-source-error")).toBeNull();
  await view.close();
});

it("keeps the result stale even when the source is restored exactly", async () => {
  const view = await mount();
  await act(async () => button(view.host, "源码")?.click());
  await act(async () => button(view.host, "检查 Rhai")?.click());
  const editor = view.host.querySelector<HTMLTextAreaElement>(
    'textarea[aria-label="测试 Rhai 源码"]',
  );
  expect(editor).toBeTruthy();
  const original = editor!.value;
  const setValue = async (value: string) => {
    await act(async () => {
      Object.getOwnPropertyDescriptor(
        HTMLTextAreaElement.prototype,
        "value",
      )?.set?.call(editor, value);
      editor!.dispatchEvent(new Event("input", { bubbles: true }));
    });
  };
  await setValue(`${original}\nwait_ms(11);`);
  await setValue(original);
  expect(view.host.textContent).toContain("内容已更改，结果已过期，请重新检查");
  expect(view.host.textContent).toContain("语法检查通过");
  await view.close();
});

it("keeps advanced validation failures visible until dismissed", async () => {
  app.hook.mockReturnValue({
    config: {
      ...defaultConfig,
      macros: [
        {
          ...macro,
          program: {
            kind: "rhai",
            source: "let value = 1;",
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
  api.validate.mockRejectedValue(new Error("验证服务不可用"));
  const view = await mount();
  await act(async () => button(view.host, "检查 Rhai")?.click());
  expect(view.host.textContent).toContain("语法检查失败");
  expect(view.host.textContent).toContain("验证服务不可用");
  expect(view.host.querySelector(".macro-source-error")).toBeNull();
  await act(async () => vi.advanceTimersByTimeAsync(2000));
  expect(view.host.textContent).toContain("验证服务不可用");
  await act(async () => button(view.host, "关闭")?.click());
  expect(view.host.textContent).not.toContain("验证服务不可用");
  expect(view.host.querySelector(".macro-source-error")).toBeNull();
  await view.close();
});

it("cancels a pending check when the source is edited", async () => {
  let resolveCheck: ((value: TestReport) => void) | undefined;
  api.validate.mockImplementation(
    () =>
      new Promise<TestReport>((resolve) => {
        resolveCheck = resolve;
      }),
  );
  app.hook.mockReturnValue({
    config: {
      ...defaultConfig,
      macros: [
        {
          ...macro,
          program: {
            kind: "rhai",
            source: "let value = 1;",
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
  const view = await mount();
  await act(async () => button(view.host, "检查 Rhai")?.click());
  expect(view.host.textContent).toContain("正在检查当前脚本，请稍候");
  const editor = view.host.querySelector<HTMLTextAreaElement>(
    'textarea[aria-label="测试 Rhai 源码"]',
  );
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "value",
    )?.set?.call(editor, "let value = 2;");
    editor!.dispatchEvent(new Event("input", { bubbles: true }));
  });
  expect(view.host.textContent).not.toContain("正在检查当前脚本，请稍候");
  await act(async () =>
    resolveCheck?.({
      unverifiedCalls: [
        { name: "late", line: 1, column: 1, reason: "late result" },
      ],
    }),
  );
  expect(view.host.textContent).not.toContain("late result");
  await view.close();
});

it("does not reopen a dismissed pending result after a late response", async () => {
  let resolveCheck: ((value: TestReport) => void) | undefined;
  api.validate.mockImplementation(
    () =>
      new Promise<TestReport>((resolve) => {
        resolveCheck = resolve;
      }),
  );
  app.hook.mockReturnValue({
    config: {
      ...defaultConfig,
      macros: [
        {
          ...macro,
          program: {
            kind: "rhai",
            source: "let value = 1;",
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
  const view = await mount();
  await act(async () => button(view.host, "检查 Rhai")?.click());
  expect(view.host.textContent).toContain("正在检查当前脚本，请稍候");
  await act(async () => button(view.host, "关闭")?.click());
  expect(
    view.host.querySelector('button[aria-label="关闭检查结果"]'),
  ).toBeNull();
  await act(async () =>
    resolveCheck?.({
      unverifiedCalls: [
        { name: "late", line: 1, column: 1, reason: "late response" },
      ],
    }),
  );
  expect(view.host.textContent).not.toContain("late response");
  await view.close();
});

it("does not let an older asynchronous check replace the newest result", async () => {
  const pending: Array<{ resolve: (value: TestReport) => void }> = [];
  api.validate.mockImplementation(
    () =>
      new Promise<TestReport>((resolve) => {
        pending.push({ resolve });
      }),
  );
  app.hook.mockReturnValue({
    config: {
      ...defaultConfig,
      macros: [
        {
          ...macro,
          program: { kind: "rhai", source: "let value = 1;", apiVersion: 1 },
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
  const view = await mount();
  await act(async () => button(view.host, "检查 Rhai")?.click());
  await act(async () => button(view.host, "检查 Rhai")?.click());
  expect(pending).toHaveLength(2);
  await act(async () =>
    pending[1].resolve({
      unverifiedCalls: [
        { name: "newest", line: 1, column: 1, reason: "newest result" },
      ],
    }),
  );
  await act(async () =>
    pending[0].resolve({
      unverifiedCalls: [
        { name: "older", line: 1, column: 1, reason: "older result" },
      ],
    }),
  );
  expect(view.host.textContent).toContain("newest result");
  expect(view.host.textContent).not.toContain("older result");
  await view.close();
});

it("keeps unrelated notices on their existing short timeout", async () => {
  const view = await mount();
  const overlayToggle = view.host.querySelector<HTMLInputElement>(
    'input[type="checkbox"]',
  );
  expect(overlayToggle).toBeTruthy();
  await act(async () => overlayToggle?.click());
  expect(view.host.querySelector(".success-banner")).toBeTruthy();
  await act(async () => vi.advanceTimersByTimeAsync(1800));
  expect(view.host.querySelector(".success-banner")).toBeNull();
  await view.close();
});

it("clears a result when switching macros even when their source matches", async () => {
  const source = "let value = 1;";
  const second = { ...macro, id: "second-macro", name: "第二个宏" };
  const advanced = (item: MacroRule): MacroRule => ({
    ...item,
    program: { kind: "rhai", source, apiVersion: 1 },
  });
  app.hook.mockReturnValue({
    config: { ...defaultConfig, macros: [advanced(macro), advanced(second)] },
    loading: false,
    saving: false,
    error: null,
    setError: app.setError,
    persist: app.persist,
    refresh: app.refresh,
  });
  api.validate.mockResolvedValue({
    unverifiedCalls: [
      { name: "first", line: 1, column: 1, reason: "first macro" },
    ],
  });
  const view = await mount();
  await act(async () => button(view.host, "检查 Rhai")?.click());
  expect(view.host.textContent).toContain("first macro");
  const secondRow = [...view.host.querySelectorAll('[role="button"]')].find(
    (element) => element.textContent?.includes("第二个宏"),
  );
  expect(secondRow).toBeTruthy();
  await act(async () => (secondRow as HTMLElement).click());
  expect(view.host.textContent).not.toContain("first macro");
  await view.close();
});

it("does not bleed a pending result into another macro with the same source", async () => {
  let resolveCheck: ((value: TestReport) => void) | undefined;
  api.validate.mockImplementation(
    () =>
      new Promise<TestReport>((resolve) => {
        resolveCheck = resolve;
      }),
  );
  const source = "let value = 1;";
  const first = {
    ...macro,
    program: { kind: "rhai" as const, source, apiVersion: 1 },
  };
  const second = {
    ...macro,
    id: "second-pending-macro",
    name: "第二个等待宏",
    program: { kind: "rhai" as const, source, apiVersion: 1 },
  };
  app.hook.mockReturnValue({
    config: { ...defaultConfig, macros: [first, second] },
    loading: false,
    saving: false,
    error: null,
    setError: app.setError,
    persist: app.persist,
    refresh: app.refresh,
  });
  const view = await mount();
  await act(async () => button(view.host, "检查 Rhai")?.click());
  const secondRow = [...view.host.querySelectorAll('[role="button"]')].find(
    (element) => element.textContent?.includes("第二个等待宏"),
  );
  expect(secondRow).toBeTruthy();
  await act(async () => (secondRow as HTMLElement).click());
  expect(
    view.host.querySelector('button[aria-label="关闭检查结果"]'),
  ).toBeNull();
  await act(async () =>
    resolveCheck?.({
      unverifiedCalls: [
        { name: "late", line: 1, column: 1, reason: "wrong macro result" },
      ],
    }),
  );
  expect(view.host.textContent).not.toContain("wrong macro result");
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
