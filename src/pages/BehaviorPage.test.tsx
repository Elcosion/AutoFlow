// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { defaultConfig } from "../types/config";
import { BehaviorPage } from "./BehaviorPage";

const api = vi.hoisted(() => ({
  deleteProfile: vi.fn(),
  deleteSession: vi.fn(),
  discard: vi.fn(),
  generate: vi.fn(),
  openDirectory: vi.fn(),
  retrain: vi.fn(),
  start: vi.fn(),
  status: vi.fn(),
  stop: vi.fn(),
}));
const app = vi.hoisted(() => ({
  hook: vi.fn(),
  persist: vi.fn(),
  refresh: vi.fn(),
  setError: vi.fn(),
}));

vi.mock("../lib/tauri", () => ({
  deleteBehaviorProfileV2: api.deleteProfile,
  deleteBehaviorSessionV2: api.deleteSession,
  discardBehaviorRecording: api.discard,
  generateBehaviorApi: api.generate,
  getBehaviorRecordingStatus: api.status,
  openDataDirectory: api.openDirectory,
  retrainBehaviorProfileV2: api.retrain,
  startBehaviorRecording: api.start,
  stopBehaviorRecording: api.stop,
}));
vi.mock("../lib/config", () => ({
  toErrorMessage: (reason: unknown) =>
    reason instanceof Error ? reason.message : String(reason),
  useAppConfig: app.hook,
}));

const baseStatus = {
  active: false,
  pending: false,
  incomplete: false,
  captureStarted: false,
  durationMs: 0,
  eventCount: 8,
  keyboardEvents: 0,
  mouseEvents: 8,
  wheelEvents: 0,
  capped: false,
  persistingRawSession: false,
  sessionName: "fixture",
};

beforeEach(() => {
  vi.clearAllMocks();
  app.persist.mockResolvedValue(defaultConfig);
  app.refresh.mockResolvedValue(undefined);
  app.hook.mockReturnValue({
    config: defaultConfig,
    loading: false,
    saving: false,
    error: null,
    setError: app.setError,
    persist: app.persist,
    refresh: app.refresh,
  });
  api.discard.mockResolvedValue(undefined);
});

afterEach(() => {
  vi.restoreAllMocks();
  document.body.replaceChildren();
});

async function mount() {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  await act(async () => {
    root.render(<BehaviorPage />);
  });
  return {
    host,
    async close() {
      await act(async () => root.unmount());
      host.remove();
    },
  };
}

it("status refresh exposes a pending claim but never starts recording", async () => {
  api.status.mockResolvedValue({ ...baseStatus, pending: true });
  api.stop.mockResolvedValue({ id: "trained" });
  const view = await mount();

  expect(api.status).toHaveBeenCalled();
  expect(api.start).not.toHaveBeenCalled();
  expect(view.host.querySelector('[data-recording-action="start"]')).toBeNull();
  const claim = view.host.querySelector<HTMLButtonElement>(
    '[data-recording-action="claim"]',
  );
  expect(claim).not.toBeNull();
  await act(async () => claim?.click());
  expect(api.stop).toHaveBeenCalledTimes(1);
  expect(api.start).not.toHaveBeenCalled();
  await view.close();
});

it("incomplete capture cannot train and discard requires explicit confirmation", async () => {
  api.status.mockResolvedValue({
    ...baseStatus,
    pending: true,
    incomplete: true,
  });
  const confirm = vi
    .spyOn(window, "confirm")
    .mockReturnValueOnce(false)
    .mockReturnValue(true);
  const view = await mount();

  expect(view.host.querySelector('[data-recording-action="claim"]')).toBeNull();
  const discard = view.host.querySelector<HTMLButtonElement>(
    '[data-recording-action="discard"]',
  );
  await act(async () => discard?.click());
  expect(api.discard).not.toHaveBeenCalled();
  await act(async () => discard?.click());
  expect(confirm).toHaveBeenCalledTimes(2);
  expect(api.discard).toHaveBeenCalledTimes(1);
  expect(api.start).not.toHaveBeenCalled();
  await view.close();
});

it("status refresh failure does not start or discard a recording", async () => {
  api.status.mockRejectedValue(new Error("status unavailable"));
  const view = await mount();
  expect(api.start).not.toHaveBeenCalled();
  expect(api.discard).not.toHaveBeenCalled();
  await view.close();
});

it("claim failure leaves the pending decision visible without starting", async () => {
  api.status.mockResolvedValue({ ...baseStatus, pending: true });
  api.stop.mockRejectedValue(new Error("training failed"));
  const view = await mount();
  const claim = view.host.querySelector<HTMLButtonElement>(
    '[data-recording-action="claim"]',
  );
  await act(async () => claim?.click());
  expect(app.setError).toHaveBeenCalledWith("training failed");
  expect(
    view.host.querySelector('[data-recording-action="claim"]'),
  ).not.toBeNull();
  expect(view.host.querySelector('[data-recording-action="start"]')).toBeNull();
  expect(api.start).not.toHaveBeenCalled();
  await view.close();
});
