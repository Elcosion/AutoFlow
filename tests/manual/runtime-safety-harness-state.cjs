(function attachHarnessState(root, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) {
    module.exports = api;
  }
  root.AutoFlowHarnessState = api;
})(typeof globalThis === "object" ? globalThis : this, function createApi() {
  "use strict";

  const locationNumber = (value) =>
    Number.isInteger(value) && value >= 0 ? value : 0;

  const rawKeyboardEvidence = (event) => ({
    key: String(event.key ?? ""),
    code: String(event.code ?? ""),
    location: locationNumber(event.location),
    repeat: Boolean(event.repeat),
    ctrlKey: Boolean(event.ctrlKey),
    shiftKey: Boolean(event.shiftKey),
    altKey: Boolean(event.altKey),
    metaKey: Boolean(event.metaKey),
  });

  // `code` identifies the physical key and stays stable when Shift or layout
  // changes event.key. Some browsers/devices omit it. The conservative
  // fallback folds ASCII letter case only; unrelated Unicode keys are never
  // normalized into the same identity.
  const fallbackKey = (key) => (/^[A-Z]$/i.test(key) ? key.toUpperCase() : key);

  const keyboardIdentity = (raw) => {
    const code = raw.code.trim();
    if (code) {
      return `code:${code}@${raw.location}`;
    }
    const trimmedKey = raw.key.trim();
    if (trimmedKey === "" || trimmedKey === "Unidentified") {
      return null;
    }
    return `fallback:${JSON.stringify(fallbackKey(raw.key))}@${raw.location}`;
  };

  const cloneHeldKeys = (heldKeys) =>
    [...heldKeys.entries()].map(([identity, raw]) => ({ identity, ...raw }));

  const createHarnessState = () => {
    const heldKeys = new Map();
    const heldButtons = new Set();
    let session = 1;
    let known = true;
    let uncertaintyReasons = [];
    let previousSessionSnapshot = null;
    let ambiguousKeySequence = 0;
    let completedClickCount = 0;
    let lastClick = null;

    const snapshot = () => ({
      session,
      observationState: known ? "known" : "uncertain",
      uncertaintyReasons: [...uncertaintyReasons],
      heldKeys: cloneHeldKeys(heldKeys),
      heldButtons: [...heldButtons].sort((left, right) => left - right),
      completedClickCount,
      lastClick: lastClick === null ? null : { ...lastClick },
    });

    const markUncertain = (reason) => {
      known = false;
      if (!uncertaintyReasons.includes(reason)) {
        uncertaintyReasons.push(reason);
      }
      return snapshot();
    };

    const windowEvent = (type, visibilityState) => {
      if (type === "blur") {
        return markUncertain("window_blur");
      }
      if (type === "visibilitychange" && visibilityState === "hidden") {
        return markUncertain("document_hidden");
      }
      if (type === "focus" || type === "visibilitychange") {
        // Events missed while the page was not observing cannot be recovered.
        return snapshot();
      }
      throw new TypeError(`Unsupported window event: ${type}`);
    };

    const keyEvent = (type, event) => {
      if (type !== "keydown" && type !== "keyup") {
        throw new TypeError(`Unsupported keyboard event: ${type}`);
      }
      const raw = rawKeyboardEvidence(event);
      let identity = keyboardIdentity(raw);
      if (identity === null) {
        ambiguousKeySequence += 1;
        identity = `ambiguous:${session}:${ambiguousKeySequence}@${raw.location}`;
        markUncertain("ambiguous_keyboard_identity");
        const transition =
          type === "keydown"
            ? "ambiguous_pressed_unpairable"
            : "ambiguous_release_unmatched";
        if (type === "keydown") {
          // Each event remains distinct. A later ambiguous keyup cannot guess
          // which physical key to clear, so no unrelated event is merged.
          heldKeys.set(identity, raw);
        }
        return { type, identity, transition, raw, state: snapshot() };
      }
      let transition;
      if (type === "keydown") {
        transition = heldKeys.has(identity) ? "repeat_or_duplicate" : "pressed";
        if (!heldKeys.has(identity)) {
          heldKeys.set(identity, raw);
        }
      } else if (heldKeys.delete(identity)) {
        transition = "released";
      } else {
        transition = "unmatched_release";
        markUncertain("unmatched_keyup");
      }
      return { type, identity, transition, raw, state: snapshot() };
    };

    const pointerEvent = (type, event) => {
      const button = Number.isInteger(event.button) ? event.button : -1;
      let transition;
      if (type === "pointerdown") {
        transition = heldButtons.has(button) ? "duplicate" : "pressed";
        heldButtons.add(button);
      } else if (type === "pointercancel") {
        transition = heldButtons.has(button)
          ? "cancelled_held_state_retained"
          : "cancelled_without_tracked_down";
        // pointercancel does not prove a physical release. Retain any tracked
        // down state and make the observation uncertainty explicit.
        markUncertain("pointer_cancelled");
      } else if (type === "pointerup") {
        transition = heldButtons.delete(button)
          ? "released"
          : "unmatched_release";
        if (transition === "unmatched_release") {
          markUncertain(`unmatched_${type}`);
        }
      } else {
        throw new TypeError(`Unsupported pointer event: ${type}`);
      }
      return {
        type,
        button,
        transition,
        x: Number(event.clientX ?? 0),
        y: Number(event.clientY ?? 0),
        state: snapshot(),
      };
    };

    const clickEvent = (event) => {
      const source = event ?? {};
      completedClickCount += 1;
      lastClick = {
        button:
          Number.isInteger(source.button) &&
          source.button >= 0 &&
          source.button <= 4
            ? source.button
            : null,
        clientX: Number.isFinite(source.clientX) ? source.clientX : null,
        clientY: Number.isFinite(source.clientY) ? source.clientY : null,
        timeStamp:
          Number.isFinite(source.timeStamp) && source.timeStamp >= 0
            ? source.timeStamp
            : null,
        isTrusted: source.isTrusted === true,
      };
      return { type: "click", lastClick: { ...lastClick }, state: snapshot() };
    };

    const startNewSession = (reason) => {
      previousSessionSnapshot = { reason, ...snapshot() };
      session += 1;
      heldKeys.clear();
      heldButtons.clear();
      ambiguousKeySequence = 0;
      completedClickCount = 0;
      lastClick = null;
      known = false;
      uncertaintyReasons = ["new_session_without_physical_snapshot"];
      return {
        previous: previousSessionSnapshot,
        current: snapshot(),
      };
    };

    return {
      keyEvent,
      pointerEvent,
      clickEvent,
      markUncertain,
      windowEvent,
      startNewSession,
      snapshot,
      previousSessionSnapshot: () => previousSessionSnapshot,
    };
  };

  return {
    createHarnessState,
    keyboardIdentity,
    rawKeyboardEvidence,
  };
});
