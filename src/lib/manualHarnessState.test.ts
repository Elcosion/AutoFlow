import { createRequire } from "node:module";
import { describe, expect, it } from "vitest";

const require = createRequire(import.meta.url);
const { createHarnessState } =
  require("../../tests/manual/runtime-safety-harness-state.cjs") as {
    createHarnessState: () => {
      keyEvent: (type: string, event: Record<string, unknown>) => any;
      pointerEvent: (type: string, event: Record<string, unknown>) => any;
      clickEvent: (event: Record<string, unknown>) => any;
      windowEvent: (type: string, visibilityState: string) => any;
      startNewSession: (reason: string) => any;
      snapshot: () => any;
    };
  };

const key = (keyValue: string, code: string, location = 0, repeat = false) => ({
  key: keyValue,
  code,
  location,
  repeat,
  ctrlKey: false,
  shiftKey: false,
  altKey: false,
  metaKey: false,
});

describe("manual acceptance harness keyboard state", () => {
  it("clears a key when key changes case but code and location stay stable", () => {
    const state = createHarnessState();
    state.keyEvent("keydown", key("A", "KeyA"));
    const released = state.keyEvent("keyup", key("a", "KeyA"));

    expect(released.transition).toBe("released");
    expect(state.snapshot().heldKeys).toEqual([]);
    expect(state.snapshot().observationState).toBe("known");
  });

  it("keeps left and right modifiers separate", () => {
    const state = createHarnessState();
    state.keyEvent("keydown", key("Control", "ControlLeft", 1));
    state.keyEvent("keydown", key("Control", "ControlRight", 2));
    state.keyEvent("keyup", key("Control", "ControlLeft", 1));

    expect(
      state.snapshot().heldKeys.map((entry: any) => entry.identity),
    ).toEqual(["code:ControlRight@2"]);
  });

  it("pairs empty-code ASCII case deterministically without merging Unicode keys", () => {
    const state = createHarnessState();
    state.keyEvent("keydown", key("A", ""));
    expect(state.keyEvent("keyup", key("a", "")).transition).toBe("released");

    state.keyEvent("keydown", key("Ω", ""));
    state.keyEvent("keydown", key("Ж", ""));
    expect(state.snapshot().heldKeys).toHaveLength(2);
    expect(state.keyEvent("keyup", key("Ω", "")).transition).toBe("released");
    expect(state.snapshot().heldKeys[0].identity).toBe('fallback:"Ж"@0');
    expect(state.snapshot().heldKeys[0].key).toBe("Ж");
  });

  it("does not duplicate state for repeated keydown and preserves repeat evidence", () => {
    const state = createHarnessState();
    state.keyEvent("keydown", key("a", "KeyA"));
    const repeated = state.keyEvent("keydown", key("a", "KeyA", 0, true));

    expect(repeated.transition).toBe("repeat_or_duplicate");
    expect(repeated.raw.repeat).toBe(true);
    expect(state.snapshot().heldKeys).toHaveLength(1);
  });

  it("never guesses pairing for empty key and code values", () => {
    const state = createHarnessState();
    state.keyEvent("keydown", key("", ""));
    state.keyEvent("keydown", key("   ", "   "));
    const release = state.keyEvent("keyup", key("", ""));

    expect(release.transition).toBe("ambiguous_release_unmatched");
    expect(state.snapshot().heldKeys).toHaveLength(2);
    expect(state.snapshot().observationState).toBe("uncertain");
    expect(state.snapshot().uncertaintyReasons).toContain(
      "ambiguous_keyboard_identity",
    );
  });

  it("treats trimmed Unidentified events as distinct and unpairable", () => {
    const state = createHarnessState();
    const first = state.keyEvent("keydown", key("Unidentified", ""));
    const second = state.keyEvent("keydown", key(" Unidentified ", " "));
    const release = state.keyEvent("keyup", key("Unidentified", ""));

    expect(first.identity).not.toBe(second.identity);
    expect(release.transition).toBe("ambiguous_release_unmatched");
    expect(state.snapshot().heldKeys).toHaveLength(2);
    expect(state.snapshot().observationState).toBe("uncertain");
  });

  it("keeps pointer state and marks uncertainty after pointercancel", () => {
    const state = createHarnessState();
    state.pointerEvent("pointerdown", { button: 0, clientX: 10, clientY: 20 });
    const cancelled = state.pointerEvent("pointercancel", {
      button: 0,
      clientX: 11,
      clientY: 21,
    });

    expect(cancelled.transition).toBe("cancelled_held_state_retained");
    expect(state.snapshot().heldButtons).toEqual([0]);
    expect(state.snapshot().observationState).toBe("uncertain");
    expect(state.snapshot().uncertaintyReasons).toContain("pointer_cancelled");

    const unmatchedState = createHarnessState();
    const unmatched = unmatchedState.pointerEvent("pointercancel", {
      button: 2,
      clientX: 0,
      clientY: 0,
    });
    expect(unmatched.transition).toBe("cancelled_without_tracked_down");
    expect(unmatchedState.snapshot().observationState).toBe("uncertain");
  });

  it("projects real click evidence without replacing pointerup state", () => {
    const state = createHarnessState();
    state.pointerEvent("pointerdown", {
      button: 0,
      clientX: 10,
      clientY: 20,
    });
    const click = state.clickEvent({
      button: 0,
      clientX: 10.5,
      clientY: 20.25,
      timeStamp: 42,
      isTrusted: true,
    });

    expect(click.lastClick).toEqual({
      button: 0,
      clientX: 10.5,
      clientY: 20.25,
      timeStamp: 42,
      isTrusted: true,
    });
    expect(state.snapshot().completedClickCount).toBe(1);
    expect(state.snapshot().heldButtons).toEqual([0]);
    expect(state.pointerEvent("pointerup", { button: 0 }).transition).toBe(
      "released",
    );
    expect(state.snapshot().heldButtons).toEqual([]);

    state.clickEvent({
      button: 5,
      clientX: Number.NaN,
      clientY: Number.POSITIVE_INFINITY,
      timeStamp: -1,
      isTrusted: 1,
    });
    expect(state.snapshot().completedClickCount).toBe(2);
    expect(state.snapshot().lastClick).toEqual({
      button: null,
      clientX: null,
      clientY: null,
      timeStamp: null,
      isTrusted: false,
    });
  });

  it("marks blur and hidden visibility uncertain and focus cannot restore certainty", () => {
    const state = createHarnessState();
    state.windowEvent("blur", "visible");
    state.windowEvent("visibilitychange", "hidden");

    expect(state.snapshot().observationState).toBe("uncertain");
    expect(state.snapshot().uncertaintyReasons).toEqual([
      "window_blur",
      "document_hidden",
    ]);
    state.windowEvent("focus", "visible");
    state.windowEvent("visibilitychange", "visible");
    expect(state.snapshot().observationState).toBe("uncertain");
  });

  it("starts a new uncertain session with the prior state snapshot", () => {
    const state = createHarnessState();
    state.keyEvent("keydown", key("a", "KeyA"));
    state.clickEvent({
      button: 0,
      clientX: 1,
      clientY: 2,
      timeStamp: 3,
      isTrusted: true,
    });
    const reset = state.startNewSession("operator_clear");

    expect(reset.previous.heldKeys).toHaveLength(1);
    expect(reset.previous.completedClickCount).toBe(1);
    expect(reset.previous.lastClick.button).toBe(0);
    expect(reset.current.session).toBe(2);
    expect(reset.current.heldKeys).toEqual([]);
    expect(reset.current.completedClickCount).toBe(0);
    expect(reset.current.lastClick).toBeNull();
    expect(reset.current.observationState).toBe("uncertain");
    expect(reset.current.uncertaintyReasons).toContain(
      "new_session_without_physical_snapshot",
    );
  });
});
