import { describe, expect, it } from "vitest";
import { formatRhaiSource } from "./rhaiFormatter";

function formatted(source: string): string {
  const result = formatRhaiSource(source);
  expect(result.ok).toBe(true);
  if (!result.ok) throw new Error(result.error.message);
  return result.source;
}

describe("formatRhaiSource", () => {
  it("formats control-flow blocks and nested calls with stable indentation", () => {
    expect(formatted('if true{wait_ms(100);if false{stop_with_message("x");}}'))
      .toBe(`if true {
  wait_ms(100);
  if false {
    stop_with_message("x");
  }
}`);
  });

  it("formats functions, comma-separated arguments, and binary operators", () => {
    expect(formatted("fn add(a,b){return a+b*-1;}")).toBe(`fn add(a, b) {
  return a + b * -1;
}`);
  });

  it("keeps every supported Rhai number literal as one opaque token", () => {
    const source =
      "let values=[0xFF_FF,0b1010_0101,0o7_55,1_000,3.14_15,6.02e+2_3];";
    const result = formatted(source);

    expect(result).toBe(
      "let values = [0xFF_FF, 0b1010_0101, 0o7_55, 1_000, 3.14_15, 6.02e+2_3];",
    );
    expect(formatRhaiSource(result)).toEqual({
      ok: true,
      source: result,
      changed: false,
    });
  });

  it("formats Unicode identifiers and remains idempotent", () => {
    const source = "let 计数=1_000;if 计数>0{let 结果=计数+1;}";
    const expected = `let 计数 = 1_000;
if 计数 > 0 {
  let 结果 = 计数 + 1;
}`;

    expect(formatted(source)).toBe(expected);
    expect(formatRhaiSource(expected)).toEqual({
      ok: true,
      source: expected,
      changed: false,
    });
  });

  it.each([
    ["reviewer reproduction", "let value=- -1;", "let value = - -1;"],
    ["long unary chain", "let value=- - - -1;", "let value = - - - -1;"],
  ])("preserves token boundaries for %s", (_name, source, expected) => {
    const first = formatRhaiSource(source);
    expect(first).toEqual({ ok: true, source: expected, changed: true });
    expect(formatRhaiSource(expected)).toEqual({
      ok: true,
      source: expected,
      changed: false,
    });
  });

  it("fails closed when formatting would merge distinct tokens", () => {
    const source = "let x = . .;";

    expect(formatRhaiSource(source)).toEqual({
      ok: false,
      source,
      error: {
        code: "lexical",
        message: "格式化会改变词法 token 边界，已保留原文",
        line: 1,
        column: 9,
      },
    });
  });

  it("maps output re-lex failures back to the first related source token", () => {
    const source = "if true{let x = . . .;}";

    expect(formatRhaiSource(source)).toEqual({
      ok: false,
      source,
      error: {
        code: "lexical",
        message: "格式化结果无法安全重新词法化：无法安全识别操作符 ...",
        line: 1,
        column: 17,
      },
    });
  });

  it("formats maps, arrays, ranges, members, closures, unary values, and branches", () => {
    const source =
      "let config=#{values:[1,2],range:0..=10};let mapped=config.values.map(|value|if value>0{value}else{-value});let add=|x,y|x+y;switch mapped.len(){0=>false,_=>true}";
    const result = formatted(source);

    expect(result).toContain("#{");
    expect(result).toContain("values: [1, 2]");
    expect(result).toContain("0..=10");
    expect(result).toContain("config.values.map(");
    expect(result).toContain("| value |");
    expect(result).toContain("} else {");
    expect(result).toContain("-value");
    expect(result).toContain("switch mapped.len() {");
    expect(result).toContain("0 => false");
    expect(formatRhaiSource(result)).toEqual({
      ok: true,
      source: result,
      changed: false,
    });
  });

  it("keeps safe-navigation, optional-index, and not-in operators intact", () => {
    const result = formatted(
      "let value=config?.values?[0];if value !in [1,2]{return;}",
    );

    expect(result).toContain("config?.values?[0]");
    expect(result).toContain("value !in [1, 2]");
    expect(formatRhaiSource(result)).toEqual({
      ok: true,
      source: result,
      changed: false,
    });
  });

  it("preserves string and comment bodies byte-for-byte", () => {
    const source =
      'let text="{ ; // /* */ \\"quoted\\""; // keep { ;\n/* block { ; // */\nwait_ms(100);';
    const result = formatted(source);

    expect(result).toContain('"{ ; // /* */ \\"quoted\\""');
    expect(result).toContain("// keep { ;");
    expect(result).toContain("/* block { ; // */");
    expect(formatted("// trailing comment  ")).toBe("// trailing comment  ");
  });

  it("preserves raw, backtick, and nested-comment bodies byte-for-byte", () => {
    const raw = '##"raw { ; // " # }"##';
    const backtick = "`value { ; } and \\`tick\\``";
    const comment = "/* outer { /* inner ; */ still outer } */";
    const result = formatted(
      `let raw=${raw};let text=${backtick};${comment}wait_ms(1);`,
    );

    expect(result).toContain(raw);
    expect(result).toContain(backtick);
    expect(result).toContain(comment);
    expect(formatRhaiSource(result)).toEqual({
      ok: true,
      source: result,
      changed: false,
    });
  });

  it("formats ordinary stop_with_message calls like other function calls", () => {
    expect(formatted('stop_with_message("done", 100);')).toBe(
      'stop_with_message("done", 100);',
    );
  });

  it.each([
    ["unclosed brace", "if true {", 1, 9],
    ["unclosed parenthesis", "wait_ms(100", 1, 8],
    ["unclosed bracket", "let values = [1", 1, 14],
  ])("rejects %s at its opening location", (_name, source, line, column) => {
    const result = formatRhaiSource(source);
    expect(result).toEqual({
      ok: false,
      source,
      error: expect.objectContaining({
        code: "lexical",
        line,
        column,
      }),
    });
  });

  it.each([
    ["brace", "if true ]", 1, 9],
    ["parenthesis", "wait_ms(100]", 1, 12],
    ["bracket", "let values = (1]", 1, 16],
  ])(
    "rejects mismatched %s at the closing token",
    (_name, source, line, column) => {
      const result = formatRhaiSource(source);
      expect(result).toEqual({
        ok: false,
        source,
        error: expect.objectContaining({
          code: "lexical",
          line,
          column,
        }),
      });
    },
  );

  it.each([
    ["double-quoted string", 'wait_ms("unterminated', 1, 9],
    ["single-quoted string", "wait_ms('unterminated", 1, 9],
    ["block comment", "wait_ms(100); /* unterminated", 1, 15],
  ])(
    "rejects %s without changing the original source",
    (_name, source, line, column) => {
      const result = formatRhaiSource(source);
      expect(result).toEqual({
        ok: false,
        source,
        error: expect.objectContaining({
          code: "lexical",
          line,
          column,
        }),
      });
    },
  );

  it.each([
    ["malformed hexadecimal", "let x=0xGG;", 1, 7, "无法安全识别数字字面量"],
    ["malformed exponent", "let x=1e+;", 1, 7, "无法安全识别数字字面量"],
    ["unknown symbol", "let x=@value;", 1, 7, "无法安全识别符号 @"],
    [
      "unsupported JavaScript operator",
      "let x=value===1;",
      1,
      12,
      "无法安全识别操作符 ===",
    ],
    [
      "interpolated string",
      "let x=`value ${item}`;",
      1,
      7,
      "暂不安全格式化插值字符串",
    ],
  ])("fails closed for %s", (_name, source, line, column, message) => {
    expect(formatRhaiSource(source)).toEqual({
      ok: false,
      source,
      error: {
        code: "lexical",
        message,
        line,
        column,
      },
    });
  });

  it("reports unchanged canonical source and is idempotent", () => {
    const source = `if true {
  wait_ms(100);
}
`;
    const first = formatRhaiSource(source);
    expect(first).toEqual({ ok: true, source, changed: false });
    expect(formatRhaiSource(first.ok ? first.source : "")).toEqual(first);
  });

  it("normalizes CRLF while preserving the final newline decision", () => {
    const source = "if true{\r\nwait_ms(100);\r\n}\r\n";
    const expected = "if true {\r\n  wait_ms(100);\r\n}\r\n";
    expect(formatted(source)).toBe(expected);
    expect(formatRhaiSource(expected)).toEqual({
      ok: true,
      source: expected,
      changed: false,
    });
    expect(formatted("wait_ms(100);")).toBe("wait_ms(100);");
  });
});
