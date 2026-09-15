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
});
