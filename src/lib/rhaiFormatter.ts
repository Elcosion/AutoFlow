export type RhaiFormatterError = {
  code: "lexical";
  message: string;
  line: number;
  column: number;
};

export type RhaiFormatterResult =
  | { ok: true; source: string; changed: boolean }
  | { ok: false; source: string; error: RhaiFormatterError };

type TokenKind =
  | "word"
  | "number"
  | "string"
  | "line-comment"
  | "block-comment"
  | "operator"
  | "punctuation";

type Token = {
  kind: TokenKind;
  value: string;
  line: number;
  column: number;
};

type LexResult =
  | { tokens: Token[]; error?: undefined }
  | { tokens: Token[]; error: RhaiFormatterError };

const openingDelimiters = new Set(["{", "(", "["]);
const closingDelimiters = new Map([
  ["}", "{"],
  [")", "("],
  ["]", "["],
]);
const controlKeywords = new Set([
  "if",
  "for",
  "while",
  "switch",
  "match",
  "catch",
]);
const unaryKeywords = new Set(["if", "for", "while", "return", "throw", "in"]);
const multiCharacterOperators = [
  "**=",
  "<<=",
  ">>=",
  "!in",
  "=>",
  "==",
  "!=",
  "<=",
  ">=",
  "&&",
  "||",
  "+=",
  "-=",
  "*=",
  "/=",
  "%=",
  "&=",
  "|=",
  "^=",
  "**",
  "..=",
  "..",
  "::",
  "<<",
  ">>",
  "??",
  "?.",
  "?[",
];
const unsupportedOperators = [
  "===",
  "!==",
  "&&=",
  "||=",
  "??=",
  "...",
  "++",
  "--",
  "->",
  "~",
];
const operatorCharacters = new Set("+-*/%=&|^!<>".split(""));
const punctuationCharacters = new Set("{}()[];,.:#".split(""));

function advancePosition(
  text: string,
  initialLine: number,
  initialColumn: number,
): { line: number; column: number } {
  let line = initialLine;
  let column = initialColumn;
  for (let index = 0; index < text.length; index += 1) {
    const character = text[index];
    if (character === "\r") {
      if (text[index + 1] === "\n") index += 1;
      line += 1;
      column = 1;
    } else if (character === "\n") {
      line += 1;
      column = 1;
    } else {
      column += 1;
    }
  }
  return { line, column };
}

function lexicalError(
  message: string,
  line: number,
  column: number,
): RhaiFormatterError {
  return { code: "lexical", message, line, column };
}

function isIdentifierStart(character: string | undefined): boolean {
  return character !== undefined && /[\p{ID_Start}_]/u.test(character);
}

function isIdentifierPart(character: string | undefined): boolean {
  return character !== undefined && /[\p{ID_Continue}_]/u.test(character);
}

function isDigit(character: string | undefined): boolean {
  return character !== undefined && /[0-9]/.test(character);
}

function scanNumber(
  source: string,
  index: number,
  line: number,
  column: number,
):
  | { end: number; error?: undefined }
  | { end?: undefined; error: RhaiFormatterError } {
  const radixPrefix =
    source[index] === "0" ? source[index + 1]?.toLowerCase() : undefined;
  if (radixPrefix === "x" || radixPrefix === "o" || radixPrefix === "b") {
    const validDigit =
      radixPrefix === "x"
        ? (value: string) => /[0-9a-f]/i.test(value)
        : radixPrefix === "o"
          ? (value: string) => /[0-7]/.test(value)
          : (value: string) => /[01]/.test(value);
    let end = index + 2;
    let digitCount = 0;
    while (
      end < source.length &&
      (validDigit(source[end]) || source[end] === "_")
    ) {
      if (source[end] !== "_") digitCount += 1;
      end += 1;
    }
    if (
      digitCount === 0 ||
      isIdentifierPart(source[end]) ||
      isDigit(source[end])
    ) {
      return {
        error: lexicalError("无法安全识别数字字面量", line, column),
      };
    }
    return { end };
  }

  let end = index + 1;
  while (isDigit(source[end]) || source[end] === "_") end += 1;

  if (source[end] === "." && source[end + 1] !== ".") {
    const afterPeriod = source[end + 1];
    if (isDigit(afterPeriod)) {
      end += 1;
      while (isDigit(source[end]) || source[end] === "_") end += 1;
    } else if (
      afterPeriod !== undefined &&
      afterPeriod !== "_" &&
      !isIdentifierStart(afterPeriod)
    ) {
      end += 1;
    }
  }

  if (source[end] === "e" || source[end] === "E") {
    let exponentEnd = end + 1;
    if (source[exponentEnd] === "+" || source[exponentEnd] === "-") {
      exponentEnd += 1;
    } else if (!isDigit(source[exponentEnd])) {
      return {
        error: lexicalError("无法安全识别数字字面量", line, column),
      };
    }
    const digitStart = exponentEnd;
    while (isDigit(source[exponentEnd]) || source[exponentEnd] === "_") {
      exponentEnd += 1;
    }
    const exponent = source.slice(digitStart, exponentEnd).replaceAll("_", "");
    if (exponent.length === 0) {
      return {
        error: lexicalError("无法安全识别数字字面量", line, column),
      };
    }
    end = exponentEnd;
  }

  if (isIdentifierStart(source[end])) {
    return {
      error: lexicalError("无法安全识别数字字面量", line, column),
    };
  }
  return { end };
}

function findOperator(source: string, index: number): string | null {
  for (const operator of multiCharacterOperators) {
    if (
      source.startsWith(operator, index) &&
      (operator !== "!in" || !isIdentifierPart(source[index + operator.length]))
    ) {
      return operator;
    }
  }
  const character = source[index];
  return operatorCharacters.has(character) ? character : null;
}

function lex(source: string): LexResult {
  const tokens: Token[] = [];
  const delimiters: Array<{ value: string; line: number; column: number }> = [];
  let index = 0;
  let line = 1;
  let column = 1;

  const fail = (error: RhaiFormatterError): LexResult => ({ tokens, error });

  const consume = (end: number) => {
    const position = advancePosition(source.slice(index, end), line, column);
    index = end;
    line = position.line;
    column = position.column;
  };

  const addToken = (
    kind: TokenKind,
    end: number,
    startLine: number,
    startColumn: number,
  ) => {
    const value = source.slice(index, end);
    tokens.push({ kind, value, line: startLine, column: startColumn });
    const delimiterValue = value === "?[" ? "[" : value;
    if (openingDelimiters.has(delimiterValue)) {
      delimiters.push({
        value: delimiterValue,
        line: startLine,
        column: startColumn,
      });
    } else {
      const expectedOpening = closingDelimiters.get(value);
      if (expectedOpening !== undefined) {
        const opening = delimiters.at(-1);
        if (!opening || opening.value !== expectedOpening) {
          throw lexicalError(
            `括号不匹配：${value} 没有对应的 ${expectedOpening}`,
            startLine,
            startColumn,
          );
        }
        delimiters.pop();
      }
    }
    consume(end);
  };

  try {
    while (index < source.length) {
      const startLine = line;
      const startColumn = column;
      const character = source[index];

      if (/\s/.test(character)) {
        let end = index + 1;
        while (end < source.length && /\s/.test(source[end])) end += 1;
        consume(end);
        continue;
      }

      if (character === "#") {
        let hashEnd = index;
        while (source[hashEnd] === "#") hashEnd += 1;
        if (source[hashEnd] === '"') {
          const terminator = '"' + "#".repeat(hashEnd - index);
          const end = source.indexOf(terminator, hashEnd + 1);
          if (end < 0) {
            return fail(lexicalError("未闭合字符串", startLine, startColumn));
          }
          addToken("string", end + terminator.length, startLine, startColumn);
          continue;
        }
        if (source[index + 1] !== "{") {
          return fail(
            lexicalError("无法安全识别符号 #", startLine, startColumn),
          );
        }
      }

      if (character === '"' || character === "'" || character === "`") {
        let end = index + 1;
        let escaped = false;
        let closed = false;
        while (end < source.length) {
          const current = source[end];
          if (escaped) {
            escaped = false;
            end += 1;
            continue;
          }
          if (current === "\\") {
            escaped = true;
            end += 1;
            continue;
          }
          if (character === "`" && current === "$" && source[end + 1] === "{") {
            return fail(
              lexicalError("暂不安全格式化插值字符串", startLine, startColumn),
            );
          }
          if (current === character) {
            end += 1;
            closed = true;
            break;
          }
          end += 1;
        }
        if (!closed) {
          return fail(lexicalError("未闭合字符串", startLine, startColumn));
        }
        addToken("string", end, startLine, startColumn);
        continue;
      }

      if (character === "/" && source[index + 1] === "/") {
        let end = index + 2;
        while (
          end < source.length &&
          source[end] !== "\r" &&
          source[end] !== "\n"
        ) {
          end += 1;
        }
        addToken("line-comment", end, startLine, startColumn);
        continue;
      }

      if (character === "/" && source[index + 1] === "*") {
        let end = index + 2;
        let level = 1;
        while (end < source.length && level > 0) {
          if (source.startsWith("/*", end)) {
            level += 1;
            end += 2;
          } else if (source.startsWith("*/", end)) {
            level -= 1;
            end += 2;
          } else {
            end += 1;
          }
        }
        if (level !== 0) {
          return fail(lexicalError("未闭合块注释", startLine, startColumn));
        }
        addToken("block-comment", end, startLine, startColumn);
        continue;
      }

      if (isIdentifierStart(character)) {
        let end = index + 1;
        while (isIdentifierPart(source[end])) end += 1;
        addToken("word", end, startLine, startColumn);
        continue;
      }

      if (isDigit(character)) {
        const number = scanNumber(source, index, startLine, startColumn);
        if (number.error) return fail(number.error);
        addToken("number", number.end, startLine, startColumn);
        continue;
      }

      const operator = findOperator(source, index);
      const unsupportedOperator = unsupportedOperators.find((candidate) =>
        source.startsWith(candidate, index),
      );
      if (unsupportedOperator !== undefined) {
        return fail(
          lexicalError(
            `无法安全识别操作符 ${unsupportedOperator}`,
            startLine,
            startColumn,
          ),
        );
      }
      if (operator !== null) {
        addToken("operator", index + operator.length, startLine, startColumn);
        continue;
      }

      if (!punctuationCharacters.has(character)) {
        return fail(
          lexicalError(`无法安全识别符号 ${character}`, startLine, startColumn),
        );
      }
      addToken("punctuation", index + 1, startLine, startColumn);
    }
  } catch (error) {
    if (
      typeof error === "object" &&
      error !== null &&
      "code" in error &&
      (error as { code?: unknown }).code === "lexical"
    ) {
      return fail(error as RhaiFormatterError);
    }
    throw error;
  }

  const unclosed = delimiters.at(-1);
  if (unclosed) {
    return fail(
      lexicalError(
        `未闭合分隔符 ${unclosed.value}`,
        unclosed.line,
        unclosed.column,
      ),
    );
  }
  return { tokens };
}

function isWordLike(token: Token | null): boolean {
  return (
    token !== null &&
    (token.kind === "word" ||
      token.kind === "number" ||
      token.kind === "string")
  );
}

function isUnaryOperator(operator: string, previous: Token | null): boolean {
  if (operator !== "-" && operator !== "!") return false;
  if (previous === null) return true;
  if (previous.kind === "operator") return true;
  if (["(", "[", "{", ",", ";", ":", "?"].includes(previous.value)) {
    return true;
  }
  return previous.kind === "word" && unaryKeywords.has(previous.value);
}

function formatTokens(tokens: Token[], source: string): string {
  const newline = source.includes("\r\n") ? "\r\n" : "\n";
  const preserveTrailingNewline = /(?:\r\n|\r|\n)$/.test(source);
  let output = "";
  let indent = 0;
  let lineStart = true;
  let lineHasContent = false;
  let parenDepth = 0;
  let bracketDepth = 0;
  let previousSyntax: Token | null = null;
  let previousToken: Token | null = null;

  const writeRaw = (value: string) => {
    if (!value) return;
    if (lineStart) {
      output += "  ".repeat(indent);
      lineStart = false;
    }
    output += value;
    const lastNewline = Math.max(
      value.lastIndexOf("\n"),
      value.lastIndexOf("\r"),
    );
    if (lastNewline >= 0 && /^[\r\n]$/.test(value.slice(-1))) {
      lineStart = true;
      lineHasContent = false;
    } else {
      lineStart = false;
      lineHasContent = true;
    }
  };

  const space = () => {
    if (!lineStart && !/[\s]$/.test(output)) output += " ";
  };

  const newLine = (preserveTrailingSpaces = false) => {
    if (lineStart) return;
    if (!preserveTrailingSpaces) output = output.replace(/[ \t]+$/u, "");
    output += newline;
    lineStart = true;
    lineHasContent = false;
  };

  const nextToken = (index: number) => tokens[index + 1] ?? null;

  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index];
    const next = nextToken(index);

    if (token.kind === "line-comment") {
      if (!lineStart) space();
      writeRaw(token.value);
      newLine(true);
      previousToken = token;
      continue;
    }
    if (token.kind === "block-comment") {
      if (!lineStart) space();
      writeRaw(token.value);
      previousToken = token;
      continue;
    }

    if (token.value === "{") {
      if (previousSyntax?.value !== "#") space();
      writeRaw(token.value);
      indent += 1;
      if (next?.value !== "}") newLine();
    } else if (token.value === "}") {
      indent = Math.max(0, indent - 1);
      if (lineHasContent && previousToken?.value !== "{") newLine();
      writeRaw(token.value);
      const nextIsContinuation =
        next?.kind === "line-comment" ||
        next?.kind === "block-comment" ||
        next?.value === ";" ||
        next?.value === "," ||
        next?.value === ")" ||
        next?.value === "]" ||
        next?.value === "." ||
        next?.value === "::";
      const nextIsBranch =
        next?.kind === "word" &&
        ["else", "catch", "finally", "while"].includes(next.value);
      if (nextIsBranch) space();
      else if (!nextIsContinuation) newLine();
    } else if (token.value === "(") {
      if (
        previousSyntax?.kind === "word" &&
        controlKeywords.has(previousSyntax.value)
      ) {
        space();
      }
      writeRaw(token.value);
      parenDepth += 1;
    } else if (token.value === ")") {
      writeRaw(token.value);
      parenDepth = Math.max(0, parenDepth - 1);
    } else if (token.value === "[") {
      writeRaw(token.value);
      bracketDepth += 1;
    } else if (token.value === "]") {
      writeRaw(token.value);
      bracketDepth = Math.max(0, bracketDepth - 1);
    } else if (token.value === ";") {
      writeRaw(token.value);
      if (
        parenDepth === 0 &&
        bracketDepth === 0 &&
        next?.kind !== "line-comment" &&
        next?.kind !== "block-comment"
      ) {
        newLine();
      } else {
        space();
      }
    } else if (token.value === ",") {
      writeRaw(token.value);
      if (next?.value !== ")" && next?.value !== "]" && next?.value !== "}") {
        space();
      }
    } else if (token.value === ".") {
      writeRaw(token.value);
    } else if (token.value === ":") {
      writeRaw(token.value);
      if (next?.value !== ":") space();
    } else if (token.kind === "operator") {
      const range = token.value === ".." || token.value === "..=";
      const member =
        token.value === "::" || token.value === "?." || token.value === "?[";
      const unary = isUnaryOperator(token.value, previousSyntax);
      if (range || member) {
        writeRaw(token.value);
        if (token.value === "?[") bracketDepth += 1;
      } else if (unary) {
        if (
          isWordLike(previousSyntax) ||
          previousSyntax?.value === ")" ||
          (token.value === "-" && previousSyntax?.value === "-")
        )
          space();
        writeRaw(token.value);
      } else {
        space();
        writeRaw(token.value);
        space();
      }
    } else {
      if (
        previousSyntax !== null &&
        (isWordLike(previousSyntax) ||
          previousSyntax.value === ")" ||
          previousSyntax.value === "]" ||
          previousSyntax.value === "}" ||
          previousSyntax.value === "," ||
          previousSyntax.value === ":")
      ) {
        space();
      }
      writeRaw(token.value);
    }

    previousSyntax = token;
    previousToken = token;
  }

  if (previousToken?.kind !== "line-comment") {
    output = output.replace(/[ \t]+$/u, "");
  }
  if (output.endsWith(newline)) output = output.slice(0, -newline.length);
  if (preserveTrailingNewline) output += newline;
  return output;
}

function firstTokenMismatch(original: Token[], formatted: Token[]): number {
  const sharedLength = Math.min(original.length, formatted.length);
  for (let index = 0; index < sharedLength; index += 1) {
    if (
      original[index].kind !== formatted[index].kind ||
      original[index].value !== formatted[index].value
    ) {
      return index;
    }
  }
  return original.length === formatted.length ? -1 : sharedLength;
}

export function formatRhaiSource(source: string): RhaiFormatterResult {
  const result = lex(source);
  if (result.error) return { ok: false, source, error: result.error };
  const formatted = formatTokens(result.tokens, source);
  const verification = lex(formatted);
  if (verification.error) {
    const prefixMismatch = firstTokenMismatch(
      result.tokens.slice(0, verification.tokens.length),
      verification.tokens,
    );
    const relatedIndex =
      prefixMismatch >= 0 ? prefixMismatch : verification.tokens.length;
    const token = result.tokens[relatedIndex] ?? result.tokens.at(-1);
    return {
      ok: false,
      source,
      error: lexicalError(
        `格式化结果无法安全重新词法化：${verification.error.message}`,
        token?.line ?? 1,
        token?.column ?? 1,
      ),
    };
  }
  const mismatch = firstTokenMismatch(result.tokens, verification.tokens);
  if (mismatch >= 0) {
    const token = result.tokens[mismatch] ?? result.tokens.at(-1);
    return {
      ok: false,
      source,
      error: lexicalError(
        "格式化会改变词法 token 边界，已保留原文",
        token?.line ?? 1,
        token?.column ?? 1,
      ),
    };
  }
  return { ok: true, source: formatted, changed: formatted !== source };
}
