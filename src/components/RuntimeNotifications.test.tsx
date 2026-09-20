// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import { RuntimeNotifications } from "./RuntimeNotifications";

const { take, acknowledge } = vi.hoisted(() => ({
  take: vi.fn(),
  acknowledge: vi.fn(),
}));
vi.mock("../lib/tauri", () => ({
  takeRuntimeNotification: take,
  acknowledgeRuntimeNotification: acknowledge,
}));

afterEach(() => {
  vi.clearAllMocks();
});

it("renders plain notification text and dismissal only resumes polling", async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  acknowledge.mockResolvedValue(true);
  take
    .mockResolvedValueOnce({
      id: 1,
      title: "完成",
      message: "<script>not executable</script>",
    })
    .mockResolvedValue(null);
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  await act(async () => {
    root.render(<RuntimeNotifications />);
  });
  expect(host.querySelector('[role="alertdialog"]')?.textContent).toContain(
    "完成",
  );
  expect(host.querySelector("script")).toBeNull();
  expect(take).toHaveBeenCalledTimes(1);
  await act(async () => {
    host.querySelector("button")?.click();
  });
  expect(host.querySelector('[role="alertdialog"]')).toBeNull();
  expect(take).toHaveBeenCalledTimes(2);
  expect(acknowledge).toHaveBeenCalledWith(1);
  await act(async () => {
    root.unmount();
  });
  host.remove();
});
