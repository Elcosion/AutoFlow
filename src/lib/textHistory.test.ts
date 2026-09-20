import { describe, expect, it } from "vitest";
import {
  emptyTextHistory,
  recordTextEdit,
  redoTextEdit,
  undoTextEdit,
  type TextSnapshot,
} from "./textHistory";

const snapshot = (value: string): TextSnapshot => ({
  value,
  selectionStart: value.length,
  selectionEnd: value.length,
});

describe("Rhai editor text history", () => {
  it("undoes and redoes source edits", () => {
    let history = emptyTextHistory();
    history = recordTextEdit(history, snapshot("wait_"));
    const undone = undoTextEdit(history, snapshot("wait_ms(300);"));
    expect(undone?.snapshot.value).toBe("wait_");

    const redone = redoTextEdit(undone!.history, undone!.snapshot);
    expect(redone?.snapshot.value).toBe("wait_ms(300);");
  });

  it("clears redo history when a new edit is recorded", () => {
    const history = {
      undo: [snapshot("a")],
      redo: [snapshot("abc")],
    };
    expect(recordTextEdit(history, snapshot("ab")).redo).toEqual([]);
  });

  it.each([0, -1])("does not retain undo history for a non-positive limit (%i)", (limit) => {
    const history = {
      undo: [snapshot("a"), snapshot("ab")],
      redo: [snapshot("abc")],
    };

    expect(recordTextEdit(history, snapshot("abcd"), limit)).toEqual({
      undo: [],
      redo: [],
    });
  });

  it.each([
    [1, ["abcd"]],
    [2, ["ab", "abcd"]],
  ] as const)("retains the most recent %i undo snapshots", (limit, expected) => {
    const history = {
      undo: [snapshot("a"), snapshot("ab")],
      redo: [],
    };

    expect(recordTextEdit(history, snapshot("abcd"), limit).undo).toEqual(
      expected.map(snapshot),
    );
  });

  it("does not mutate frozen history inputs when recording an edit", () => {
    const history: {
      undo: TextSnapshot[];
      redo: TextSnapshot[];
    } = {
      undo: [snapshot("a"), snapshot("ab"), snapshot("abc")],
      redo: [snapshot("abcd")],
    };
    const current = snapshot("abcde");
    const originalHistory = {
      undo: [...history.undo],
      redo: [...history.redo],
    };

    Object.freeze(history.undo[0]);
    Object.freeze(history.undo[1]);
    Object.freeze(history.undo[2]);
    Object.freeze(history.redo[0]);
    Object.freeze(current);
    Object.freeze(history.undo);
    Object.freeze(history.redo);
    Object.freeze(history);

    const result = recordTextEdit(history, current, 2);

    expect(history).toEqual(originalHistory);
    expect(result.undo).not.toBe(history.undo);
    expect(result.redo).not.toBe(history.redo);
    expect(result.undo).toEqual([history.undo[2], current]);
    expect(result.redo).toEqual([]);
  });
});
