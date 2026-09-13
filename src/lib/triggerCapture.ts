const modifierOrder = ["Ctrl", "Alt", "Shift", "Win"];

export function canonicalizeCapturedKeys(keys: string[]) {
  return [...new Set(keys)].sort((left, right) => {
    const leftIndex = modifierOrder.indexOf(left);
    const rightIndex = modifierOrder.indexOf(right);
    if (leftIndex >= 0 || rightIndex >= 0) {
      return (
        (leftIndex < 0 ? modifierOrder.length : leftIndex) -
        (rightIndex < 0 ? modifierOrder.length : rightIndex)
      );
    }
    return left.localeCompare(right);
  });
}

/**
 * Add a key to the current capture session. A non-modifier is the final key
 * of a combination, so it replaces the previous final key while preserving
 * modifiers. This also lets a user change F9 back to F8 without refocusing
 * the field first.
 */
export function nextCapturedKeys(currentKeys: string[], key: string) {
  const modifiers = currentKeys.filter((currentKey) =>
    modifierOrder.includes(currentKey),
  );
  return canonicalizeCapturedKeys([...modifiers, key]);
}
