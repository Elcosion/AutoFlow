import {
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type SyntheticEvent,
} from "react";

export const RHAI_API_NAMES = [
  "wait_ms",
  "wait_random_ms",
  "key_down",
  "key_up",
  "press",
  "move_to",
  "mouse_down",
  "mouse_up",
  "click",
  "scroll",
  "type_text",
  "bio_press",
  "bio_move_to",
  "bio_click",
  "bio_type_text",
  "bio_scroll",
  "is_cancelled",
  "active_window_title",
  "window_exists",
  "window_rect",
  "wait_window",
  "pixel_matches",
  "wait_pixel",
  "find_image",
  "wait_image",
];

export const RHAI_API_SIGNATURES: Record<string, string> = {
  click: "click(button, x, y)",
  bio_press: "bio_press(key)",
  bio_move_to: "bio_move_to(x, y)",
  bio_click: "bio_click(button, x, y)",
  bio_type_text: "bio_type_text(text)",
  bio_scroll: "bio_scroll(deltaX, deltaY)",
  active_window_title: "active_window_title() -> String",
  window_exists: "window_exists(title)",
  window_rect: "window_rect(title) -> Map",
  wait_window: "wait_window(title, timeoutMs, pollMs)",
  pixel_matches: "pixel_matches(x, y, r, g, b, tolerance)",
  wait_pixel: "wait_pixel(x, y, r, g, b, tolerance, timeoutMs, pollMs)",
  find_image: "find_image(assetId, x, y, width, height, threshold)",
  wait_image:
    "wait_image(assetId, x, y, width, height, threshold, timeoutMs, pollMs)",
};

type RhaiEditorProps = {
  value: string;
  onChange: (value: string) => void;
  onSave: () => void;
  onCheck: () => void;
  onFormat: () => void;
  errorLine?: number | null;
  errorColumn?: number | null;
  disabled?: boolean;
};

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function highlight(value: string): string {
  const escaped = escapeHtml(value);
  return escaped
    .replace(/(\/\/.*)$/gm, '<span class="rhai-token-comment">$1</span>')
    .replace(
      /(&quot;(?:\\.|[^&]|&(?!quot;))*?&quot;)/g,
      '<span class="rhai-token-string">$1</span>',
    )
    .replace(
      /\b(let|const|if|else|for|while|fn|return|true|false)\b/g,
      '<span class="rhai-token-keyword">$1</span>',
    )
    .replace(
      /\b(-?\d+(?:\.\d+)?)\b/g,
      '<span class="rhai-token-number">$1</span>',
    )
    .replace(
      /\b(wait_ms|wait_random_ms|key_down|key_up|press|move_to|mouse_down|mouse_up|click|scroll|type_text|bio_press|bio_move_to|bio_click|bio_type_text|bio_scroll|is_cancelled|active_window_title|window_exists|window_rect|wait_window|pixel_matches|wait_pixel|find_image|wait_image)(?=\s*\()/g,
      '<span class="rhai-token-api">$1</span>',
    );
}

function lineAt(value: string, position: number): number {
  return value.slice(0, position).split("\n").length;
}

export function RhaiEditor({
  value,
  onChange,
  onSave,
  onCheck,
  onFormat,
  errorLine,
  errorColumn,
  disabled = false,
}: RhaiEditorProps) {
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const highlightRef = useRef<HTMLPreElement | null>(null);
  const [currentLine, setCurrentLine] = useState(1);
  const [suggestions, setSuggestions] = useState<string[]>([]);

  const highlighted = useMemo(() => highlight(value) || " ", [value]);
  const lineCount = Math.max(1, value.split("\n").length);

  const updateCursor = (event: SyntheticEvent<HTMLTextAreaElement>) => {
    const target = event.currentTarget;
    const position = target.selectionStart;
    setCurrentLine(lineAt(target.value, position));
    const beforeCursor = target.value.slice(0, position);
    const partial = /(?:^|\s)([A-Za-z_][A-Za-z0-9_]*)$/.exec(
      beforeCursor.split("\n").pop() ?? "",
    )?.[1];
    setSuggestions(
      partial && !beforeCursor.trimEnd().endsWith("(")
        ? RHAI_API_NAMES.filter((name) => name.startsWith(partial))
        : [],
    );
  };

  const replaceSuggestion = (name: string) => {
    const textarea = textareaRef.current;
    if (!textarea) return;
    const position = textarea.selectionStart;
    const lineStart = textarea.value.lastIndexOf("\n", position - 1) + 1;
    const partial =
      textarea.value
        .slice(lineStart, position)
        .match(/[A-Za-z_][A-Za-z0-9_]*$/)?.[0] ?? "";
    const next = `${textarea.value.slice(0, position - partial.length)}${name}(${textarea.value.slice(position)}`;
    onChange(next);
    setSuggestions([]);
    window.requestAnimationFrame(() => {
      textarea.focus();
      const nextPosition = position - partial.length + name.length + 1;
      textarea.setSelectionRange(nextPosition, nextPosition);
    });
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
      event.preventDefault();
      onSave();
      return;
    }
    if (event.key === "Tab") {
      event.preventDefault();
      const textarea = event.currentTarget;
      const start = textarea.selectionStart;
      const end = textarea.selectionEnd;
      const next = `${value.slice(0, start)}  ${value.slice(end)}`;
      onChange(next);
      window.requestAnimationFrame(() => {
        textarea.setSelectionRange(start + 2, start + 2);
      });
      return;
    }
    if (event.key === "Enter") {
      const textarea = event.currentTarget;
      const lineStart =
        value.lastIndexOf("\n", textarea.selectionStart - 1) + 1;
      const indentation = value.slice(lineStart).match(/^\s*/)?.[0] ?? "";
      const line = value.slice(lineStart, textarea.selectionStart);
      if (indentation || line.trimEnd().endsWith("{")) {
        event.preventDefault();
        const extraIndent = line.trimEnd().endsWith("{") ? "  " : "";
        const insertion = `\n${indentation}${extraIndent}`;
        const start = textarea.selectionStart;
        onChange(
          `${value.slice(0, start)}${insertion}${value.slice(textarea.selectionEnd)}`,
        );
        window.requestAnimationFrame(() => {
          const nextPosition = start + insertion.length;
          textarea.setSelectionRange(nextPosition, nextPosition);
        });
      }
    }
  };

  return (
    <>
      <div className="rhai-editor-toolbar">
        <button onClick={onCheck} type="button">
          检查语法
        </button>
        <button onClick={onFormat} type="button">
          格式化
        </button>
        <span>Ctrl+S 保存</span>
      </div>
      <div className="rhai-editor-shell">
        <div aria-hidden="true" className="rhai-line-numbers">
          {Array.from({ length: lineCount }, (_, index) => {
            const line = index + 1;
            return (
              <span
                className={`${line === currentLine ? "is-current" : ""} ${line === errorLine ? "is-error" : ""}`}
                key={line}
              >
                {line}
              </span>
            );
          })}
        </div>
        <div className="rhai-editor-code">
          <pre
            aria-hidden="true"
            className="rhai-highlight"
            dangerouslySetInnerHTML={{ __html: `${highlighted}\n` }}
            ref={highlightRef}
          />
          <textarea
            aria-label="Rhai 脚本编辑器"
            className="macro-source-textarea rhai-editor-textarea"
            disabled={disabled}
            onChange={(event) => {
              onChange(event.target.value);
              updateCursor(event);
            }}
            onClick={updateCursor}
            onKeyDown={handleKeyDown}
            onKeyUp={updateCursor}
            onScroll={(event) => {
              if (highlightRef.current) {
                highlightRef.current.scrollTop = event.currentTarget.scrollTop;
                highlightRef.current.scrollLeft =
                  event.currentTarget.scrollLeft;
              }
            }}
            onSelect={updateCursor}
            ref={textareaRef}
            spellCheck={false}
            value={value}
          />
          {suggestions.length > 0 ? (
            <div className="rhai-autocomplete" role="listbox">
              {suggestions.map((name) => (
                <button
                  key={name}
                  onMouseDown={(event) => event.preventDefault()}
                  onClick={() => replaceSuggestion(name)}
                  type="button"
                >
                  {RHAI_API_SIGNATURES[name] ?? `${name}()`}
                </button>
              ))}
            </div>
          ) : null}
        </div>
      </div>
      {errorLine ? (
        <div className="rhai-error-position">
          第 {errorLine} 行第 {errorColumn ?? 1} 列
        </div>
      ) : null}
    </>
  );
}
