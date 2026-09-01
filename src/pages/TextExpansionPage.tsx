import { useEffect, useState } from "react";
import { BulkActions } from "../components/BulkActions";
import { PageHeader } from "../components/PageHeader";
import { useAppConfig } from "../lib/config";
import { useAutoSave } from "../lib/useAutoSave";
import { newRuleId, type TextExpansionRule } from "../types/config";

const blankRule = (existing: TextExpansionRule[]): TextExpansionRule => {
  const used = new Set(
    existing.map((rule) => rule.abbreviation.trim().toLocaleLowerCase()),
  );
  const candidates = [";;", ";mail", ";addr", ";phone", ";1"];
  const abbreviation =
    candidates.find((candidate) => !used.has(candidate)) ?? `;${Date.now()}`;
  return {
    id: newRuleId("text"),
    name: "新文本扩展",
    abbreviation,
    replacement: "",
    enabled: false,
    caseSensitive: false,
    sensitive: false,
  };
};

const copyRule = (rule: TextExpansionRule): TextExpansionRule => ({ ...rule });

export function TextExpansionPage() {
  const { config, loading, saving, error, setError, persist } = useAppConfig();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [draft, setDraft] = useState<TextExpansionRule | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [revealSensitive, setRevealSensitive] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());

  useEffect(() => {
    if (!selectedId && config.textExpansions[0])
      setSelectedId(config.textExpansions[0].id);
  }, [config.textExpansions, selectedId]);

  useEffect(() => {
    const next = config.textExpansions.find((rule) => rule.id === selectedId);
    setDraft(next ? copyRule(next) : null);
  }, [config.textExpansions, selectedId]);

  useEffect(() => {
    setRevealSensitive(false);
  }, [selectedId]);

  const selected = draft;
  const duplicateAbbreviation = Boolean(
    selected &&
    config.textExpansions.some(
      (rule) =>
        rule.id !== selected.id &&
        rule.abbreviation.trim().toLocaleLowerCase() ===
          selected.abbreviation.trim().toLocaleLowerCase(),
    ),
  );
  const showNotice = (message: string) => {
    setNotice(message);
    window.setTimeout(() => setNotice(null), 1800);
  };

  const save = async (textExpansions: TextExpansionRule[], message: string) => {
    try {
      await persist({ ...config, textExpansions });
      showNotice(message);
    } catch {
      // The hook exposes the actionable error in the page.
    }
  };

  const updateDraft = (patch: Partial<TextExpansionRule>) => {
    setDraft((current) => (current ? { ...current, ...patch } : current));
  };

  const validateDraft = (candidate: TextExpansionRule) => {
    if (!candidate.name.trim()) return "请先填写扩展名称";
    if (!candidate.abbreviation.trim()) return "请填写触发缩写";
    if (candidate.enabled && !candidate.replacement)
      return "启用前请填写替换内容";
    if (duplicateAbbreviation)
      return "这个缩写已经被另一条扩展使用，请修改后再保存";
    return undefined;
  };

  const savedSelected = config.textExpansions.find(
    (rule) => rule.id === selected?.id,
  );

  useAutoSave(selected, savedSelected, validateDraft, async (nextDraft) => {
    const next = config.textExpansions.map((rule) =>
      rule.id === nextDraft.id ? nextDraft : rule,
    );
    await persist({ ...config, textExpansions: next });
  });

  const addRule = () => {
    const rule = blankRule(config.textExpansions);
    setSelectedId(rule.id);
    setDraft(rule);
    void save([...config.textExpansions, rule], "已添加文本扩展");
  };

  const removeSelected = () => {
    if (!selected) return;
    const next = config.textExpansions.filter(
      (rule) => rule.id !== selected.id,
    );
    setSelectedId(next[0]?.id ?? null);
    setSelectedIds((current) => {
      const nextIds = new Set(current);
      nextIds.delete(selected.id);
      return nextIds;
    });
    void save(next, "文本扩展已删除");
  };

  const toggleSelected = (id: string) => {
    setSelectedIds((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const toggleAllSelected = () => {
    setSelectedIds((current) =>
      current.size === config.textExpansions.length
        ? new Set()
        : new Set(config.textExpansions.map((rule) => rule.id)),
    );
  };

  const removeSelectedRules = () => {
    if (selectedIds.size === 0) return;
    const next = config.textExpansions.filter(
      (rule) => !selectedIds.has(rule.id),
    );
    if (selected && selectedIds.has(selected.id)) {
      setSelectedId(next[0]?.id ?? null);
    }
    setSelectedIds(new Set());
    void save(
      next,
      `已删除 ${config.textExpansions.length - next.length} 条文本扩展`,
    );
  };

  const toggleRule = (rule: TextExpansionRule) => {
    const candidate = selected?.id === rule.id ? selected : rule;
    const next = config.textExpansions.map((item) =>
      item.id === rule.id
        ? { ...candidate, enabled: !candidate.enabled }
        : item,
    );
    setDraft((current) =>
      current?.id === rule.id
        ? { ...current, enabled: !candidate.enabled }
        : current,
    );
    void save(next, candidate.enabled ? "扩展已停用" : "扩展已启用");
  };

  return (
    <div className="page-stack">
      <PageHeader
        title="文本扩展"
        action={
          <button
            className="button button-primary"
            onClick={addRule}
            type="button"
          >
            ＋ 添加文本扩展
          </button>
        }
      />
      {error ? (
        <div className="error-banner">
          <strong>{error}</strong>
          <button onClick={() => setError(null)} type="button">
            知道了
          </button>
        </div>
      ) : null}
      {notice ? <div className="success-banner">✓ {notice}</div> : null}
      <div className="rule-layout">
        <section className="rule-list-panel">
          <div className="panel-heading">
            <div>
              <strong>我的文本扩展</strong>
              <span>{config.textExpansions.length} 条内容</span>
            </div>
            <BulkActions
              count={selectedIds.size}
              onDelete={removeSelectedRules}
              onToggleAll={toggleAllSelected}
              total={config.textExpansions.length}
            />
          </div>
          {loading ? (
            <div className="panel-loading">正在读取本机配置…</div>
          ) : config.textExpansions.length === 0 ? (
            <div className="panel-empty">
              还没有扩展
              <br />
              <button className="text-button" onClick={addRule} type="button">
                添加第一条
              </button>
            </div>
          ) : (
            <div className="rule-list">
              {config.textExpansions.map((rule) => (
                <div
                  className={`rule-list-item ${selectedId === rule.id ? "is-selected" : ""}`}
                  key={rule.id}
                  onClick={() => setSelectedId(rule.id)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                      event.preventDefault();
                      setSelectedId(rule.id);
                    }
                  }}
                  role="button"
                  tabIndex={0}
                >
                  <input
                    aria-label={`选择 ${rule.name}`}
                    checked={selectedIds.has(rule.id)}
                    className="rule-select"
                    onChange={() => toggleSelected(rule.id)}
                    onClick={(event) => event.stopPropagation()}
                    type="checkbox"
                  />
                  <span className="rule-icon text">文</span>
                  <span className="rule-list-copy">
                    <strong>{rule.name}</strong>
                    <span>
                      <code>{rule.abbreviation}</code> <em>→</em>{" "}
                      {rule.sensitive
                        ? "••••••••"
                        : rule.replacement || "未填写内容"}
                    </span>
                  </span>
                  <button
                    className={`mini-toggle ${rule.enabled ? "on" : ""}`}
                    aria-label={`${rule.name}${rule.enabled ? "停用" : "启用"}`}
                    aria-pressed={rule.enabled}
                    onClick={(event) => {
                      event.stopPropagation();
                      toggleRule(rule);
                    }}
                    type="button"
                  >
                    <i />
                  </button>
                </div>
              ))}
            </div>
          )}
          <button className="list-add" onClick={addRule} type="button">
            ＋ 新建扩展
          </button>
        </section>
        <section className="editor-panel">
          {selected ? (
            <>
              <div className="editor-heading">
                <div>
                  <span className="editor-kicker">文本扩展规则</span>
                  <h2>{selected.name || "未命名扩展"}</h2>
                </div>
                <button
                  className="danger-link"
                  onClick={removeSelected}
                  type="button"
                >
                  删除规则
                </button>
              </div>
              <div className="form-section">
                <label htmlFor="text-name">规则名称</label>
                <input
                  id="text-name"
                  value={selected.name}
                  onChange={(event) =>
                    updateDraft({ name: event.target.value })
                  }
                  placeholder="例如：常用邮箱"
                />
              </div>
              <div className="form-section">
                <label htmlFor="text-abbreviation">
                  输入缩写 <span>1–32 个字符</span>
                </label>
                <input
                  id="text-abbreviation"
                  value={selected.abbreviation}
                  onChange={(event) =>
                    updateDraft({ abbreviation: event.target.value })
                  }
                  placeholder="例如：@@"
                />
                <div className="form-help">
                  在记事本或浏览器输入这个缩写，最后一个字符输入完成时会自动替换。
                </div>
                {duplicateAbbreviation ? (
                  <div className="field-warning">
                    ⚠ 这个缩写已经被另一条扩展使用，请换一个缩写。
                  </div>
                ) : null}
              </div>
              <div className="form-section">
                <div className="field-label-row">
                  <label htmlFor="text-replacement">替换内容</label>
                  {selected.sensitive ? (
                    <button
                      className="field-action"
                      onClick={() => setRevealSensitive((current) => !current)}
                      type="button"
                    >
                      {revealSensitive ? "隐藏内容" : "显示内容"}
                    </button>
                  ) : null}
                </div>
                <textarea
                  className={
                    selected.sensitive && !revealSensitive ? "secret-field" : ""
                  }
                  id="text-replacement"
                  rows={6}
                  value={selected.replacement}
                  onChange={(event) =>
                    updateDraft({ replacement: event.target.value })
                  }
                  placeholder="输入邮箱、地址或多行模板…"
                />
                <div className="form-help">
                  AutoFlow 使用 Unicode 键盘事件发送，不会改动当前剪贴板。
                </div>
              </div>
              <label className="check-row">
                <input
                  type="checkbox"
                  checked={selected.sensitive}
                  onChange={(event) => {
                    setRevealSensitive(false);
                    updateDraft({ sensitive: event.target.checked });
                  }}
                />
                <span>
                  <strong>敏感内容</strong>
                  <small>在列表中隐藏，编辑时默认遮罩，适合密码等内容</small>
                </span>
              </label>
              <label className="check-row">
                <input
                  type="checkbox"
                  checked={selected.caseSensitive}
                  onChange={(event) =>
                    updateDraft({ caseSensitive: event.target.checked })
                  }
                />
                <span>
                  <strong>区分大小写</strong>
                  <small>例如 @@ 和 @@A 作为不同缩写处理</small>
                </span>
              </label>
              <div className="editor-footer">
                <div>
                  <span
                    className={`status-label ${selected.enabled ? "enabled" : "disabled"}`}
                  >
                    <i /> {selected.enabled ? "运行中" : "已停用"}
                  </span>
                  <span className="save-hint">
                    {saving ? "正在保存…" : "已自动保存"}
                  </span>
                </div>
                <div className="editor-actions">
                  <button
                    className={`button ${selected.enabled ? "button-soft-danger" : "button-primary"}`}
                    onClick={() => toggleRule(selected)}
                    type="button"
                  >
                    {selected.enabled ? "停用扩展" : "启用扩展"}
                  </button>
                </div>
              </div>
            </>
          ) : (
            <div className="editor-empty">
              <div className="empty-icon text">文</div>
              <h2>选择一条扩展开始编辑</h2>
            </div>
          )}
        </section>
      </div>
    </div>
  );
}
