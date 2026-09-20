import type { MacroRule } from "../types/config";

export const HOLD_TRIGGER_REPAIR_REASON =
  "按住循环快捷键需要且只能包含一个普通键，Ctrl/Shift/Alt/Win 只能作为修饰键";

const modifierKeys = new Set(["CTRL", "CONTROL", "SHIFT", "ALT", "WIN"]);
const namedOwnerKeys = new Set([
  "ESC",
  "ENTER",
  "SPACE",
  "TAB",
  "BACKSPACE",
  "CAPSLOCK",
  "LEFT",
  "RIGHT",
  "UP",
  "DOWN",
]);

const canonicalOwnerKey = (key: string) => {
  if (/^[A-Z0-9]$/.test(key) || namedOwnerKeys.has(key)) return key;
  if (/^F[0-9]+$/.test(key)) {
    const number = Number(key.slice(1));
    if (Number.isInteger(number) && number >= 1 && number <= 24) {
      return `F${number}`;
    }
  }
  return null;
};

export function holdTriggerOwner(
  triggerKeys: readonly string[],
): string | null {
  let owner: string | null = null;
  for (const rawKey of triggerKeys) {
    const key = rawKey.trim().toUpperCase();
    if (modifierKeys.has(key)) continue;
    const canonicalOwner = canonicalOwnerKey(key);
    if (canonicalOwner === null || owner !== null) return null;
    owner = canonicalOwner;
  }
  return owner;
}

const splitImportErrors = (value: string | undefined) =>
  (value ?? "")
    .split("；")
    .map((part) => part.trim())
    .filter(Boolean);

const mergeRepairReason = (value: string | undefined) => {
  const parts = splitImportErrors(value);
  if (!parts.includes(HOLD_TRIGGER_REPAIR_REASON)) {
    parts.push(HOLD_TRIGGER_REPAIR_REASON);
  }
  return parts.join("；");
};

const clearRepairReason = (value: string | undefined) => {
  const retained = splitImportErrors(value).filter(
    (part) => part !== HOLD_TRIGGER_REPAIR_REASON,
  );
  return retained.length > 0 ? retained.join("；") : undefined;
};

export function repairLegacyHoldTrigger(rule: MacroRule): MacroRule {
  const valid =
    rule.mode === "hold" && holdTriggerOwner(rule.triggerKeys) !== null;
  if (rule.mode !== "hold" || valid) {
    return recoverHoldTriggerRepair(rule);
  }
  if (!rule.enabled) return rule;
  return {
    ...rule,
    enabled: false,
    importError: mergeRepairReason(rule.importError),
  };
}

export function recoverHoldTriggerRepair(rule: MacroRule): MacroRule {
  const canClear =
    rule.mode !== "hold" || holdTriggerOwner(rule.triggerKeys) !== null;
  if (!canClear) return rule;
  return {
    ...rule,
    importError: clearRepairReason(rule.importError),
  };
}

export function holdTriggerDraftError(rule: MacroRule): string | undefined {
  if (
    rule.enabled &&
    rule.mode === "hold" &&
    holdTriggerOwner(rule.triggerKeys) === null
  ) {
    return HOLD_TRIGGER_REPAIR_REASON;
  }
  return undefined;
}
