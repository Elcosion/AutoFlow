import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type SyntheticEvent,
} from "react";
import {
  completeRhaiSource,
  insertRhaiReferenceSnippet,
  RHAI_API_NAMES,
  RHAI_API_SIGNATURES,
  RHAI_API_COMPLETIONS,
  RHAI_COMPLETION_NAMES,
  RHAI_SYNTAX_COMPLETIONS,
  type RhaiReferenceSnippet,
} from "../lib/rhaiCompletions";
import {
  emptyTextHistory,
  recordTextEdit,
  redoTextEdit,
  undoTextEdit,
  type TextSnapshot,
} from "../lib/textHistory";

export { RHAI_API_NAMES, RHAI_API_SIGNATURES };

type RhaiEditorProps = {
  value: string;
  onChange: (value: string) => void;
  onSave: () => void;
  onCheck: () => void;
  onFormat: () => string | undefined;
  errorLine?: number | null;
  errorColumn?: number | null;
  disabled?: boolean;
  snippets?: RhaiReferenceSnippet[];
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
      /\b(let|const|if|else|for|in|while|loop|fn|return|break|continue|true|false)\b/g,
      '<span class="rhai-token-keyword">$1</span>',
    )
    .replace(
      /\b(-?\d+(?:\.\d+)?)\b/g,
      '<span class="rhai-token-number">$1</span>',
    )
    .replace(
      /\b(stop_with_message|wait_ms|wait_random_ms|key_down|key_up|press|move_to|mouse_down|mouse_up|click|scroll|type_text|bio_press|bio_move_to|bio_click|bio_type_text|bio_scroll|is_cancelled|active_window_title|window_exists|window_rect|wait_window|pixel_matches|wait_pixel|find_image|wait_image)(?=\s*\()/g,
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
  snippets = [],
}: RhaiEditorProps) {
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const highlightRef = useRef<HTMLPreElement | null>(null);
  const lineNumbersRef = useRef<HTMLDivElement | null>(null);
  const historyRef = useRef(emptyTextHistory());
  const expectedValueRef = useRef<string | null>(null);
  const [currentLine, setCurrentLine] = useState(1);
  const [suggestions, setSuggestions] = useState<string[]>([]);
  const [snippetQuery, setSnippetQuery] = useState("");
  const [collapsedSnippetCategories, setCollapsedSnippetCategories] = useState(
    () => new Set<string>(),
  );

  const highlighted = useMemo(() => highlight(value) || " ", [value]);
  const lineCount = Math.max(1, value.split("\n").length);
  const snippetGroups = useMemo(() => {
    const groups = new Map<string, RhaiReferenceSnippet[]>();
    snippets.forEach((snippet) => {
      const group = groups.get(snippet.category);
      if (group) group.push(snippet);
      else groups.set(snippet.category, [snippet]);
    });
    return Array.from(groups.entries());
  }, [snippets]);
  const visibleSnippetGroups = useMemo(() => {
    const query = snippetQuery.trim().toLocaleLowerCase();
    if (!query) return snippetGroups;
    return snippetGroups
      .map(
        ([category, groupSnippets]) =>
          [
            category,
            groupSnippets.filter((snippet) =>
              [
                snippet.category,
                snippet.name,
                snippet.description,
                snippet.apiName ?? "",
                snippet.code,
              ]
                .join("\n")
                .toLocaleLowerCase()
                .includes(query),
            ),
          ] as const,
      )
      .filter(([, groupSnippets]) => groupSnippets.length > 0);
  }, [snippetGroups, snippetQuery]);
  const allSnippetCategoriesCollapsed =
    snippetGroups.length > 0 &&
    snippetGroups.every(([category]) =>
      collapsedSnippetCategories.has(category),
    );
  const visibleSnippetCount = visibleSnippetGroups.reduce(
    (count, [, groupSnippets]) => count + groupSnippets.length,
    0,
  );

  useEffect(() => {
    if (expectedValueRef.current === value) {
      expectedValueRef.current = null;
      return;
    }
    historyRef.current = emptyTextHistory();
  }, [value]);

  const currentSnapshot = (): TextSnapshot => {
    const textarea = textareaRef.current;
    return {
      value,
      selectionStart: Math.min(
        textarea?.selectionStart ?? value.length,
        value.length,
      ),
      selectionEnd: Math.min(
        textarea?.selectionEnd ?? value.length,
        value.length,
      ),
    };
  };

  const focusSelection = (selectionStart: number, selectionEnd: number) => {
    window.requestAnimationFrame(() => {
      const textarea = textareaRef.current;
      if (!textarea) return;
      textarea.focus();
      textarea.setSelectionRange(selectionStart, selectionEnd);
    });
  };

  const commitValue = (
    nextValue: string,
    selectionStart?: number,
    selectionEnd = selectionStart,
  ) => {
    if (nextValue === value) return;
    historyRef.current = recordTextEdit(historyRef.current, currentSnapshot());
    expectedValueRef.current = nextValue;
    onChange(nextValue);
    if (selectionStart !== undefined) {
      focusSelection(selectionStart, selectionEnd ?? selectionStart);
    }
  };

  const applyHistory = (direction: "undo" | "redo") => {
    const result =
      direction === "undo"
        ? undoTextEdit(historyRef.current, currentSnapshot())
        : redoTextEdit(historyRef.current, currentSnapshot());
    if (!result) return;
    historyRef.current = result.history;
    expectedValueRef.current = result.snapshot.value;
    onChange(result.snapshot.value);
    setSuggestions([]);
    focusSelection(
      result.snapshot.selectionStart,
      result.snapshot.selectionEnd,
    );
  };

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
        ? RHAI_COMPLETION_NAMES.filter((name) => name.startsWith(partial))
        : [],
    );
  };

  const replaceSuggestion = (name: string) => {
    const textarea = textareaRef.current;
    if (!textarea) return;
    const edit = completeRhaiSource(
      textarea.value,
      textarea.selectionStart,
      textarea.selectionEnd,
      name,
    );
    if (!edit) return;
    commitValue(edit.value, edit.selectionStart, edit.selectionEnd);
    setSuggestions([]);
  };

  const insertReferenceSnippet = (snippet: RhaiReferenceSnippet) => {
    const textarea = textareaRef.current;
    if (!textarea) return;
    const edit = insertRhaiReferenceSnippet(
      textarea.value,
      textarea.selectionStart,
      textarea.selectionEnd,
      snippet.code,
    );
    commitValue(edit.value, edit.selectionStart, edit.selectionEnd);
    setSuggestions([]);
  };

  const toggleSnippetCategory = (category: string) => {
    setCollapsedSnippetCategories((current) => {
      const next = new Set(current);
      if (next.has(category)) next.delete(category);
      else next.add(category);
      return next;
    });
  };

  const toggleAllSnippetCategories = () => {
    setCollapsedSnippetCategories(
      allSnippetCategoriesCollapsed
        ? new Set()
        : new Set(snippetGroups.map(([category]) => category)),
    );
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "z") {
      event.preventDefault();
      applyHistory(event.shiftKey ? "redo" : "undo");
      return;
    }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "y") {
      event.preventDefault();
      applyHistory("redo");
      return;
    }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
      event.preventDefault();
      onSave();
      return;
    }
    if (
      suggestions.length > 0 &&
      (event.key === "Tab" || event.key === "Enter")
    ) {
      event.preventDefault();
      replaceSuggestion(suggestions[0]);
      return;
    }
    if (event.key === "Tab") {
      event.preventDefault();
      const textarea = event.currentTarget;
      const start = textarea.selectionStart;
      const end = textarea.selectionEnd;
      const next = `${value.slice(0, start)}  ${value.slice(end)}`;
      commitValue(next, start + 2, start + 2);
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
        commitValue(
          `${value.slice(0, start)}${insertion}${value.slice(textarea.selectionEnd)}`,
          start + insertion.length,
          start + insertion.length,
        );
      }
    }
  };

  return (
    <div className="rhai-editor-workspace">
      <div className="rhai-editor-main">
        <div className="rhai-editor-toolbar">
          <div className="rhai-editor-actions">
            <button onClick={onCheck} type="button">
              检查语法
            </button>
            <button
              onClick={() => {
                const formatted = onFormat();
                if (formatted !== undefined) commitValue(formatted);
              }}
              type="button"
            >
              格式化
            </button>
          </div>
          <span className="rhai-editor-shortcuts">
            <kbd>Tab</kbd>/<kbd>Enter</kbd> 补全
            <i aria-hidden="true" />
            <kbd>Ctrl Z</kbd> 撤销
            <i aria-hidden="true" />
            <kbd>Ctrl S</kbd> 保存
          </span>
        </div>
        <div className="rhai-editor-shell">
          <div
            aria-hidden="true"
            className="rhai-line-numbers"
            ref={lineNumbersRef}
          >
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
                commitValue(event.target.value);
                updateCursor(event);
              }}
              onClick={updateCursor}
              onKeyDown={handleKeyDown}
              onKeyUp={updateCursor}
              onScroll={(event) => {
                if (highlightRef.current) {
                  highlightRef.current.scrollTop =
                    event.currentTarget.scrollTop;
                  highlightRef.current.scrollLeft =
                    event.currentTarget.scrollLeft;
                }
                if (lineNumbersRef.current) {
                  lineNumbersRef.current.scrollTop =
                    event.currentTarget.scrollTop;
                }
              }}
              onSelect={updateCursor}
              ref={textareaRef}
              spellCheck={false}
              value={value}
            />
            {suggestions.length > 0 ? (
              <div className="rhai-autocomplete" role="listbox">
                {suggestions.map((name) => {
                  const completion =
                    RHAI_SYNTAX_COMPLETIONS[name] ??
                    RHAI_API_COMPLETIONS[
                      name as keyof typeof RHAI_API_COMPLETIONS
                    ];
                  return (
                    <button
                      key={name}
                      onMouseDown={(event) => event.preventDefault()}
                      onClick={() => replaceSuggestion(name)}
                      type="button"
                    >
                      {completion?.signature ?? `${name}()`}
                    </button>
                  );
                })}
              </div>
            ) : null}
          </div>
        </div>
        {errorLine ? (
          <div className="rhai-error-position">
            第 {errorLine} 行第 {errorColumn ?? 1} 列
          </div>
        ) : null}
      </div>
      {snippets.length > 0 ? (
        <aside className="rhai-snippet-panel">
          <div className="rhai-snippet-heading">
            <div className="rhai-snippet-title-row">
              <div>
                <strong>参考代码</strong>
                <span>
                  {snippetQuery.trim()
                    ? `${visibleSnippetCount} 个匹配片段`
                    : `${snippets.length} 个可插入片段`}
                </span>
              </div>
              <button onClick={toggleAllSnippetCategories} type="button">
                {allSnippetCategoriesCollapsed ? "全部展开" : "全部收起"}
              </button>
            </div>
            <label className="rhai-snippet-search">
              <span aria-hidden="true">⌕</span>
              <input
                aria-label="搜索参考代码"
                onChange={(event) => setSnippetQuery(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Escape") setSnippetQuery("");
                }}
                placeholder="搜索 API、功能或参数"
                type="search"
                value={snippetQuery}
              />
            </label>
          </div>
          <div className="rhai-snippet-list">
            {visibleSnippetGroups.map(([category, groupSnippets]) => {
              const isExpanded = !collapsedSnippetCategories.has(category);
              return (
                <section
                  className="rhai-snippet-group"
                  data-category={category}
                  key={category}
                >
                  <button
                    aria-expanded={isExpanded}
                    className="rhai-snippet-group-toggle"
                    onClick={() => toggleSnippetCategory(category)}
                    type="button"
                  >
                    <span>
                      <strong>{category}</strong>
                      <small>{groupSnippets.length} 个</small>
                    </span>
                    <span aria-hidden="true" className="rhai-snippet-chevron">
                      {isExpanded ? "−" : "+"}
                    </span>
                  </button>
                  {isExpanded ? (
                    <div className="rhai-snippet-group-body">
                      {groupSnippets.map((snippet) => (
                        <article className="rhai-snippet-card" key={snippet.id}>
                          <div className="rhai-snippet-card-title">
                            <strong>{snippet.name}</strong>
                            {snippet.apiName ? <small>API</small> : null}
                          </div>
                          <p>{snippet.description}</p>
                          <button
                            onMouseDown={(event) => event.preventDefault()}
                            onClick={() => insertReferenceSnippet(snippet)}
                            type="button"
                          >
                            <span aria-hidden="true">＋</span> 插入代码
                          </button>
                        </article>
                      ))}
                    </div>
                  ) : null}
                </section>
              );
            })}
            {visibleSnippetGroups.length === 0 ? (
              <div className="rhai-snippet-empty">
                <strong>没有匹配的参考代码</strong>
                <span>尝试搜索 API 名称、功能或参数。</span>
              </div>
            ) : null}
          </div>
        </aside>
      ) : null}
    </div>
  );
}
