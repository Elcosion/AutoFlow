import { describe, expect, it } from "vitest";
import {
  classifyMacroSource,
  macroToSource,
  parseCompatibleMacroSource,
  parseMacroSource,
} from "./macroSource";
import type { MacroRule } from "../types/config";

const sampleMacro: MacroRule = {
  id: "macro-source-test",
  name: "源码测试",
  enabled: false,
  triggerKeys: ["Ctrl", "F8"],
  mode: "once",
  repeatCount: 1,
  speed: 1,
  recordMouseMove: true,
  recordMouseClicks: false,
  program: {
    kind: "macro",
    steps: [
      { type: "delay", durationMs: 300, durationMaxMs: 800 },
      { type: "key", key: "Enter", action: "down" },
      { type: "mouseMove", x: 120, y: 240 },
      { type: "wheel", deltaX: 0, deltaY: -120 },
      { type: "text", text: "双引号 \\\"换行\\n中文" },
    ],
  },
};

describe("Rhai compatible macro source", () => {
  it("round-trips every supported MacroStep through Rhai source", () => {
    const source = macroToSource(sampleMacro);
    const parsed = parseMacroSource(source, sampleMacro);
    expect(parsed.program).toEqual(sampleMacro.program);
    expect(source).toContain("wait_random_ms(300, 800);");
    expect(source).toContain(
      `type_text(${JSON.stringify(sampleMacro.program.kind === "macro" ? sampleMacro.program.steps[4].type === "text" ? sampleMacro.program.steps[4].text : "" : "")});`,
    );
  });

  it("preserves strings with quotes, backslashes, newlines and Unicode", () => {
    const steps = parseCompatibleMacroSource(
      'type_text("\\\"quoted\\\" \\\\ path\\n中文");',
    );
    expect(steps).toEqual([
      { type: "text", text: '"quoted" \\ path\n中文' },
    ]);
  });

  it("detects advanced control flow without confusing string contents", () => {
    expect(classifyMacroSource('type_text("if for let");')).toBe("compatible");
    expect(classifyMacroSource("let count = 2;\nwhile count > 0 { count -= 1; }")).toBe(
      "advanced",
    );
  });

  it("reports the line and column for an invalid API call", () => {
    expect(() =>
      parseCompatibleMacroSource("wait_ms(20);\nmove(1, 2);"),
    ).toThrow("第 2 行第 1 列");
  });

  it("expands press and click into balanced down/up steps", () => {
    expect(
      parseCompatibleMacroSource('press("A");\nclick("left");'),
    ).toEqual([
      { type: "key", key: "A", action: "down" },
      { type: "key", key: "A", action: "up" },
      { type: "mouseButton", button: "left", action: "down", x: 0, y: 0 },
      { type: "mouseButton", button: "left", action: "up", x: 0, y: 0 },
    ]);
  });

  it("supports the coordinate-aware compatible click signature", () => {
    expect(classifyMacroSource('click("right", 820, 430);')).toBe("compatible");
    expect(parseCompatibleMacroSource('click("right", 820, 430);')).toEqual([
      { type: "mouseButton", button: "right", action: "down", x: 820, y: 430 },
      { type: "mouseButton", button: "right", action: "up", x: 820, y: 430 },
    ]);
  });

  it("keeps the old JSON source format readable", () => {
    const source = JSON.stringify({
      id: sampleMacro.id,
      name: sampleMacro.name,
      steps: [{ type: "delay", durationMs: 300 }],
    });
    const parsed = parseMacroSource(source, sampleMacro);
    expect(parsed.program).toEqual({
      kind: "macro",
      steps: [{ type: "delay", durationMs: 300 }],
    });
  });
});
