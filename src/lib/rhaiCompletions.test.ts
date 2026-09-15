import { describe, expect, it } from "vitest";
import {
  completeRhaiSource,
  insertRhaiReferenceSnippet,
  RHAI_API_COMPLETIONS,
  RHAI_API_NAMES,
  RHAI_API_REFERENCE_SNIPPETS,
  RHAI_COMPLETION_NAMES,
  RHAI_SYNTAX_COMPLETIONS,
} from "./rhaiCompletions";

describe("Rhai editor completions", () => {
  it("inserts a complete API call with documented reference arguments", () => {
    const edit = completeRhaiSource("wai", 3, 3, "wait_window");
    expect(edit?.value).toContain("wait_window(");
    expect(edit?.value).toContain('"记事本", // title：窗口标题关键字');
    expect(edit?.value).toContain("5000, // timeoutMs：最长等待毫秒数");
    expect(edit?.value).toContain("200 // pollMs：轮询间隔毫秒数");
    expect(edit?.value.trimEnd().endsWith(");")).toBe(true);
    expect(edit?.value.slice(edit.selectionStart, edit.selectionEnd)).toBe(
      "记事本",
    );
  });

  it("keeps multiline completion indentation", () => {
    const source = "if ready {\n  cli";
    const edit = completeRhaiSource(
      source,
      source.length,
      source.length,
      "click",
    );
    expect(edit?.value).toContain('\n  click(\n    "left"');
  });

  it("offers complete basic statement blocks", () => {
    for (const name of [
      "let",
      "if",
      "if_else",
      "for",
      "while",
      "loop",
      "fn",
      "return",
      "break",
      "continue",
    ]) {
      expect(RHAI_COMPLETION_NAMES).toContain(name);
      expect(RHAI_SYNTAX_COMPLETIONS[name].insertText.length).toBeGreaterThan(
        name.length,
      );
    }
    expect(RHAI_SYNTAX_COMPLETIONS.for.insertText).toContain("0..10");
    expect(RHAI_SYNTAX_COMPLETIONS.if_else.insertText).toContain("else");
  });

  it("documents every API completion that accepts arguments", () => {
    for (const completion of Object.values(RHAI_API_COMPLETIONS)) {
      const argumentsText = completion.signature.match(/\((.*?)\)/)?.[1] ?? "";
      if (argumentsText) expect(completion.insertText).toContain("//");
    }
  });

  it("provides an insertable reference snippet for every API", () => {
    const referencedApis = new Set(
      RHAI_API_REFERENCE_SNIPPETS.map((snippet) => snippet.apiName),
    );
    expect(referencedApis).toEqual(new Set(RHAI_API_NAMES));
    for (const snippet of RHAI_API_REFERENCE_SNIPPETS) {
      expect(snippet.code).toContain(`${snippet.apiName}(`);
      expect(snippet.description.length).toBeGreaterThan(0);
    }
  });

  it("documents every supported overloaded API call form", () => {
    const signatures = RHAI_API_REFERENCE_SNIPPETS.map(
      (snippet) => snippet.name,
    );
    expect(signatures).toEqual(
      expect.arrayContaining([
        "stop_with_message(message)",
        "stop_with_message(title, message)",
        "click(button)",
        "click(button, x, y)",
        "click(x, y)",
        "bio_move_to(x, y, options)",
        "bio_click(button, x, y, options)",
        "bio_type_text(text, options)",
      ]),
    );
  });

  it("inserts a reference snippet at the current selection", () => {
    const edit = insertRhaiReferenceSnippet(
      "wait_ms(100);\nplaceholder",
      14,
      25,
      'if ready {\n  press("Enter");\n}',
    );
    expect(edit.value).toBe('wait_ms(100);\nif ready {\n  press("Enter");\n}');
    expect(edit.selectionStart).toBe(edit.value.length);
    expect(edit.selectionEnd).toBe(edit.value.length);
  });
});
