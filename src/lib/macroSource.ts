import type { MacroRule, MacroStep, MouseButton } from "../types/config";

export type MacroSourceKind = "compatible" | "advanced";

export class MacroSourceError extends Error {
  readonly code: "syntax" | "advanced";
  readonly line: number;
  readonly column: number;

  constructor(
    message: string,
    options: {
      code?: "syntax" | "advanced";
      line?: number;
      column?: number;
    } = {},
  ) {
    super(message);
    this.name = "MacroSourceError";
    this.code = options.code ?? "syntax";
    this.line = options.line ?? 1;
    this.column = options.column ?? 1;
  }
}

const compatibleFunctions = new Set([
  "wait_ms",
  "wait_random_ms",
  "key_down",
  "key_up",
  "press",
  "click",
  "move_to",
  "mouse_down",
  "mouse_up",
  "scroll",
  "type_text",
]);

const advancedOnlyFunctions = [
  "stop_with_message",
  "active_window_title",
  "window_exists",
  "window_rect",
  "wait_window",
  "pixel_matches",
  "wait_pixel",
  "find_image",
  "wait_image",
  "is_cancelled",
];

const keyActions = new Set(["down", "up"]);
const mouseButtons = new Set<MouseButton>([
  "left",
  "right",
  "middle",
  "x1",
  "x2",
]);

function sourceError(
  message: string,
  line: number,
  column = 1,
): MacroSourceError {
  return new MacroSourceError(`第 ${line} 行第 ${column} 列：${message}`, {
    line,
    column,
  });
}

function stripStringsAndComments(source: string): string {
  let result = "";
  let quote: '"' | "'" | null = null;
  let escaped = false;
  let comment = false;

  for (let index = 0; index < source.length; index += 1) {
    const character = source[index];
    if (comment) {
      result += character === "\n" ? "\n" : " ";
      if (character === "\n") comment = false;
      continue;
    }
    if (quote) {
      result += character === "\n" ? "\n" : " ";
      if (escaped) escaped = false;
      else if (character === "\\") escaped = true;
      else if (character === quote) quote = null;
      continue;
    }
    if (character === "/" && source[index + 1] === "/") {
      result += "  ";
      comment = true;
      index += 1;
      continue;
    }
    if (character === '"' || character === "'") {
      quote = character;
      result += " ";
      continue;
    }
    result += character;
  }
  return result;
}

export function classifyMacroSource(source: string): MacroSourceKind {
  const code = stripStringsAndComments(source);
  if (
    /\b(?:let|const|if|else|for|while|loop|fn|function|import|export|eval|return)\b/.test(
      code,
    ) ||
    /(^|\n)\s*[A-Za-z_][A-Za-z0-9_]*\s*=/.test(code) ||
    /[{}]/.test(code) ||
    advancedOnlyFunctions.some((name) =>
      new RegExp(`\\b${name}\\s*\\(`).test(code),
    ) ||
    /\bclick\s*\([^,()]+,\s*[^,()]+\s*\)/.test(code)
  ) {
    return "advanced";
  }
  return "compatible";
}

function jsonString(value: string): string {
  return JSON.stringify(value);
}

function formatCall(name: string, args: Array<string | number>): string {
  return `${name}(${args
    .map((argument) =>
      typeof argument === "string" ? jsonString(argument) : String(argument),
    )
    .join(", ")});`;
}

export function macroToSource(macro: MacroRule): string {
  if (macro.program.kind !== "macro") return macro.program.source;

  return macro.program.steps
    .map((step) => {
      switch (step.type) {
        case "delay":
          return step.durationMaxMs !== undefined &&
            step.durationMaxMs > step.durationMs
            ? formatCall("wait_random_ms", [
                step.durationMs,
                step.durationMaxMs,
              ])
            : formatCall("wait_ms", [step.durationMs]);
        case "key":
          return formatCall(step.action === "down" ? "key_down" : "key_up", [
            step.key,
          ]);
        case "mouseButton":
          return formatCall(
            step.action === "down" ? "mouse_down" : "mouse_up",
            [step.button, step.x, step.y],
          );
        case "mouseMove":
          return formatCall("move_to", [step.x, step.y]);
        case "wheel":
          return formatCall("scroll", [step.deltaX, step.deltaY]);
        case "text":
          return formatCall("type_text", [step.text]);
      }
    })
    .join("\n");
}

function stripInlineComment(line: string): string {
  let quote: '"' | "'" | null = null;
  let escaped = false;
  for (let index = 0; index < line.length - 1; index += 1) {
    const character = line[index];
    if (quote) {
      if (escaped) escaped = false;
      else if (character === "\\") escaped = true;
      else if (character === quote) quote = null;
      continue;
    }
    if (character === '"' || character === "'") {
      quote = character;
      continue;
    }
    if (character === "/" && line[index + 1] === "/")
      return line.slice(0, index);
  }
  return line;
}

function parseArguments(
  text: string,
  line: number,
  openingColumn: number,
): Array<string | number> {
  const args: Array<string | number> = [];
  let current = "";
  let quote: '"' | "'" | null = null;
  let escaped = false;

  const push = () => {
    const token = current.trim();
    if (!token) {
      if (args.length > 0 || current.length > 0) {
        throw sourceError("参数不能为空", line, openingColumn);
      }
      return;
    }
    if (token.startsWith('"') || token.startsWith("'")) {
      if (token.startsWith("'")) {
        throw sourceError("字符串必须使用双引号", line, openingColumn);
      }
      try {
        const parsed = JSON.parse(token) as unknown;
        if (typeof parsed !== "string") throw new Error("not a string");
        args.push(parsed);
      } catch {
        throw sourceError("字符串转义无效", line, openingColumn);
      }
      return;
    }
    if (!/^-?\d+$/.test(token)) {
      throw sourceError(
        "兼容宏参数必须是双引号字符串或整数",
        line,
        openingColumn,
      );
    }
    const parsed = Number(token);
    if (!Number.isSafeInteger(parsed)) {
      throw sourceError("整数超出安全范围", line, openingColumn);
    }
    args.push(parsed);
  };

  for (const character of text) {
    if (quote) {
      current += character;
      if (escaped) escaped = false;
      else if (character === "\\") escaped = true;
      else if (character === quote) quote = null;
      continue;
    }
    if (character === '"' || character === "'") {
      quote = character;
      current += character;
    } else if (character === ",") {
      push();
      current = "";
    } else {
      current += character;
    }
  }
  if (quote) throw sourceError("字符串缺少结束双引号", line, openingColumn);
  push();
  return args;
}

function stringArgument(
  args: Array<string | number>,
  index: number,
  name: string,
  line: number,
): string {
  const value = args[index];
  if (typeof value !== "string") {
    throw sourceError(`${name} 的第 ${index + 1} 个参数必须是字符串`, line);
  }
  return value;
}

function integerArgument(
  args: Array<string | number>,
  index: number,
  name: string,
  line: number,
): number {
  const value = args[index];
  if (typeof value !== "number") {
    throw sourceError(`${name} 的第 ${index + 1} 个参数必须是整数`, line);
  }
  return value;
}

function argumentCount(
  args: Array<string | number>,
  count: number,
  name: string,
  line: number,
) {
  if (args.length !== count) {
    throw sourceError(
      `${name} 需要 ${count} 个参数，实际得到 ${args.length} 个`,
      line,
    );
  }
}

function parseCompatibleStatement(lineText: string, line: number): MacroStep {
  const text = stripInlineComment(lineText).trim();
  const match = /^([A-Za-z_][A-Za-z0-9_]*)\s*\((.*)\)\s*;$/.exec(text);
  if (!match) throw sourceError("每行必须是以分号结尾的 API 调用", line);
  const [, name, rawArguments] = match;
  if (!compatibleFunctions.has(name)) {
    throw sourceError(`未知函数 ${name}；兼容宏只能调用 AutoFlow API`, line);
  }
  const openingColumn = Math.max(1, text.indexOf("(") + 1);
  const args = parseArguments(rawArguments, line, openingColumn);

  switch (name) {
    case "wait_ms": {
      argumentCount(args, 1, name, line);
      const durationMs = integerArgument(args, 0, name, line);
      if (durationMs < 0) throw sourceError("wait_ms 参数不能为负数", line);
      return { type: "delay", durationMs };
    }
    case "wait_random_ms": {
      argumentCount(args, 2, name, line);
      const durationMs = integerArgument(args, 0, name, line);
      const durationMaxMs = integerArgument(args, 1, name, line);
      if (durationMs < 0 || durationMaxMs < 0) {
        throw sourceError("wait_random_ms 参数不能为负数", line);
      }
      if (durationMaxMs < durationMs) {
        throw sourceError("wait_random_ms 的最大值不能小于最小值", line);
      }
      return { type: "delay", durationMs, durationMaxMs };
    }
    case "key_down":
    case "key_up":
    case "press": {
      argumentCount(args, 1, name, line);
      const key = stringArgument(args, 0, name, line);
      if (!key) throw sourceError(`${name} 的按键名不能为空`, line);
      return {
        type: "key",
        key,
        action: name === "key_up" ? "up" : "down",
      };
    }
    case "click": {
      if (args.length !== 1 && args.length !== 3) {
        throw sourceError(
          `${name} 需要 1 或 3 个参数，实际得到 ${args.length} 个`,
          line,
        );
      }
      const button = stringArgument(args, 0, name, line) as MouseButton;
      if (!mouseButtons.has(button)) {
        throw sourceError(
          `${name} 的鼠标按钮必须是 left、right、middle、x1 或 x2`,
          line,
        );
      }
      return {
        type: "mouseButton",
        button,
        action: "down",
        x: args.length === 3 ? integerArgument(args, 1, name, line) : 0,
        y: args.length === 3 ? integerArgument(args, 2, name, line) : 0,
      };
    }
    case "move_to": {
      argumentCount(args, 2, name, line);
      return {
        type: "mouseMove",
        x: integerArgument(args, 0, name, line),
        y: integerArgument(args, 1, name, line),
      };
    }
    case "mouse_down":
    case "mouse_up": {
      argumentCount(args, 3, name, line);
      const button = stringArgument(args, 0, name, line) as MouseButton;
      if (!mouseButtons.has(button)) {
        throw sourceError(
          `${name} 的鼠标按钮必须是 left、right、middle、x1 或 x2`,
          line,
        );
      }
      return {
        type: "mouseButton",
        button,
        action: name === "mouse_up" ? "up" : "down",
        x: integerArgument(args, 1, name, line),
        y: integerArgument(args, 2, name, line),
      };
    }
    case "scroll":
      argumentCount(args, 2, name, line);
      return {
        type: "wheel",
        deltaX: integerArgument(args, 0, name, line),
        deltaY: integerArgument(args, 1, name, line),
      };
    case "type_text":
      argumentCount(args, 1, name, line);
      return { type: "text", text: stringArgument(args, 0, name, line) };
  }
  throw sourceError(`未知函数 ${name}`, line);
}

export function parseCompatibleMacroSource(source: string): MacroStep[] {
  if (classifyMacroSource(source) === "advanced") {
    throw new MacroSourceError(
      "该源码包含循环、条件、变量或函数定义，无法表示为图形宏步骤。请确认后将其保存为高级 Rhai 脚本。",
      { code: "advanced", line: 1, column: 1 },
    );
  }
  const steps: MacroStep[] = [];
  source.split(/\r?\n/).forEach((line, index) => {
    if (!stripInlineComment(line).trim()) return;
    const parsed = parseCompatibleStatement(line, index + 1);
    steps.push(parsed);
    if (/^press\s*\(/.test(stripInlineComment(line).trim())) {
      if (parsed.type !== "key") {
        throw sourceError("press 必须接收一个按键字符串", index + 1);
      }
      steps.push({ ...parsed, action: "up" });
    }
    if (/^click\s*\(/.test(stripInlineComment(line).trim())) {
      if (parsed.type !== "mouseButton") {
        throw sourceError("click 必须接收一个鼠标按钮字符串", index + 1);
      }
      steps.push({ ...parsed, action: "up" });
    }
  });
  return steps;
}

type JsonObject = Record<string, unknown>;

function isObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function requiredString(
  value: JsonObject,
  field: string,
  path: string,
): string {
  const result = value[field];
  if (typeof result !== "string")
    throw new Error(`${path}.${field} 必须是字符串`);
  return result;
}

function legacySteps(value: JsonObject): MacroStep[] {
  const steps = value.steps;
  if (!Array.isArray(steps)) throw new Error("旧宏源码的 steps 必须是数组");
  return steps.map((step, index) => {
    if (!isObject(step) || typeof step.type !== "string") {
      throw new Error(`steps[${index}] 必须是有效步骤对象`);
    }
    return step as MacroStep;
  });
}

function parseLegacyJsonSource(
  source: string,
  base: MacroRule | string,
): MacroRule {
  let value: unknown;
  try {
    value = JSON.parse(source);
  } catch {
    throw new Error("JSON 格式错误，请检查括号、逗号和引号");
  }
  if (!isObject(value)) throw new Error("旧宏源码根节点必须是 JSON 对象");
  const expectedId = typeof base === "string" ? base : base.id;
  const id = requiredString(value, "id", "宏");
  if (id !== expectedId)
    throw new Error("id 不允许修改，请保持当前宏的 id 不变");
  if (typeof base === "string") {
    throw new Error("旧 JSON 源码缺少可继承的宏元数据，请重新打开宏编辑器");
  }
  return {
    ...base,
    id,
    name: typeof value.name === "string" ? value.name : base.name,
    program: { kind: "macro", steps: legacySteps(value) },
  };
}

export function parseMacroSource(
  source: string,
  base: MacroRule | string,
): MacroRule {
  if (source.trimStart().startsWith("{")) {
    return parseLegacyJsonSource(source, base);
  }
  if (typeof base === "string") {
    throw new Error("Rhai 源码解析需要当前宏元数据");
  }
  return {
    ...base,
    program: { kind: "macro", steps: parseCompatibleMacroSource(source) },
  };
}
