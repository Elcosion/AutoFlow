export type TextSnapshot = {
  value: string;
  selectionStart: number;
  selectionEnd: number;
};

export type TextHistory = {
  undo: TextSnapshot[];
  redo: TextSnapshot[];
};

export const emptyTextHistory = (): TextHistory => ({ undo: [], redo: [] });

function appendRecentSnapshots(
  snapshots: TextSnapshot[],
  current: TextSnapshot,
  limit: number,
): TextSnapshot[] {
  return limit > 0 ? [...snapshots, current].slice(-limit) : [];
}

export function recordTextEdit(
  history: TextHistory,
  current: TextSnapshot,
  limit = 100,
): TextHistory {
  return {
    undo: appendRecentSnapshots(history.undo, current, limit),
    redo: [],
  };
}

export function undoTextEdit(
  history: TextHistory,
  current: TextSnapshot,
): { history: TextHistory; snapshot: TextSnapshot } | null {
  const snapshot = history.undo.at(-1);
  if (!snapshot) return null;
  return {
    history: {
      undo: history.undo.slice(0, -1),
      redo: [...history.redo, current],
    },
    snapshot,
  };
}

export function redoTextEdit(
  history: TextHistory,
  current: TextSnapshot,
): { history: TextHistory; snapshot: TextSnapshot } | null {
  const snapshot = history.redo.at(-1);
  if (!snapshot) return null;
  return {
    history: {
      undo: [...history.undo, current],
      redo: history.redo.slice(0, -1),
    },
    snapshot,
  };
}
