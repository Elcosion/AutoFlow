import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { BulkActions } from "../components/BulkActions";
import { PageHeader } from "../components/PageHeader";
import { useAppConfig } from "../lib/config";
import { useAutoSave } from "../lib/useAutoSave";
import { newRuleId, type HotkeyRule } from "../types/config";

const signatureOf = (keys: string[]) =>
  keys.map((key) => key.trim().toUpperCase()).join("+");

const modifierOrder = ["Ctrl", "Alt", "Shift", "Win"];

function normalizeCapturedKey(event: ReactKeyboardEvent<HTMLInputElement>) {
  const namedKeys: Record<string, string> = {
    Alt: "Alt",
    Backspace: "Backspace",
    CapsLock: "CapsLock",
    Control: "Ctrl",
    Enter: "Enter",
    Escape: "Esc",
    Meta: "Win",
    Shift: "Shift",
    Space: "Space",
    Tab: "Tab",
    ArrowDown: "Down",
    ArrowLeft: "Left",
    ArrowRight: "Right",
    ArrowUp: "Up",
  };
  if (namedKeys[event.key]) return namedKeys[event.key];
  if (/^Key[A-Z]$/.test(event.code)) return event.code.slice(3);
  if (/^Digit[0-9]$/.test(event.code)) return event.code.slice(5);
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(event.key)) return event.key;
  return null;
}

function canonicalizeKeys(keys: string[]) {
  return [...new Set(keys)].sort((left, right) => {
    const leftIndex = modifierOrder.indexOf(left);
    const rightIndex = modifierOrder.indexOf(right);
    if (leftIndex >= 0 || rightIndex >= 0) {
      return (
        (leftIndex < 0 ? modifierOrder.length : leftIndex) -
        (rightIndex < 0 ? modifierOrder.length : rightIndex)
      );
    }
    return left.localeCompare(right);
  });
}

const blankRule = (existing: HotkeyRule[]): HotkeyRule => {
  const used = new Set(existing.map((rule) => signatureOf(rule.triggerKeys)));
  const candidates = ["F8", "F9", "F10", "F11", "F13", "F14", "F15"];
  const trigger = candidates.find((key) => !used.has(key)) ?? "F16";
  return {
    id: newRuleId("hotkey"),
    name: "新快捷键",
    enabled: false,
    triggerKeys: [trigger],
    action: { type: "remap", target: "Esc" },
  };
};

const copyRule = (rule: HotkeyRule): HotkeyRule => ({
  ...rule,
  triggerKeys: [...rule.triggerKeys],
  action: { ...rule.action },
});

export function HotkeysPage() {
  const { config, loading, saving, error, setError, persist } = useAppConfig();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [draft, setDraft] = useState<HotkeyRule | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const captureActive = useRef(false);

  useEffect(() => {
    if (!selectedId && config.hotkeys[0]) setSelectedId(config.hotkeys[0].id);
  }, [config.hotkeys, selectedId]);

  useEffect(() => {
    const next = config.hotkeys.find((rule) => rule.id === selectedId);
    setDraft(next ? copyRule(next) : null);
  }, [config.hotkeys, selectedId]);

  const selected = draft;
  const duplicateTrigger = useMemo(() => {
    if (!selected) return false;
    const signature = signatureOf(selected.triggerKeys);
    return config.hotkeys.some(
      (rule) =>
        rule.id !== selected.id && signatureOf(rule.triggerKeys) === signature,
    );
  }, [config.hotkeys, selected]);

  const showNotice = (message: string) => {
    setNotice(message);
    window.setTimeout(() => setNotice(null), 1800);
  };

  const save = async (nextHotkeys: HotkeyRule[], message: string) => {
    try {
      await persist({ ...config, hotkeys: nextHotkeys });
      showNotice(message);
    } catch {
      // The hook exposes the actionable error in the page.
    }
  };

  const updateDraft = (patch: Partial<HotkeyRule>) => {
    setDraft((current) => (current ? { ...current, ...patch } : current));
  };

  const captureTrigger = (event: ReactKeyboardEvent<HTMLInputElement>) => {
    const key = normalizeCapturedKey(event);
    if (!key || event.repeat) return;
    event.preventDefault();
    event.stopPropagation();
    const currentKeys = captureActive.current
      ? [...(selected?.triggerKeys ?? []), key]
      : [key];
    const nextKeys = modifierOrder.includes(key)
      ? currentKeys
      : [
          ...currentKeys.filter((currentKey) =>
            modifierOrder.includes(currentKey),
          ),
          key,
        ];
    captureActive.current = true;
    updateDraft({ triggerKeys: canonicalizeKeys(nextKeys) });
  };

  const validateDraft = (candidate: HotkeyRule) => {
    if (!candidate.name.trim()) return "请先填写规则名称";
    if (candidate.triggerKeys.length === 0) return "请至少填写一个触发键";
    if (!candidate.action.target.trim()) return "请填写快捷键动作的目标";
    if (duplicateTrigger)
      return "这个触发组合已经被另一条规则使用，请修改后再保存";
    return undefined;
  };

  const savedSelected = config.hotkeys.find((rule) => rule.id === selected?.id);

  useAutoSave(selected, savedSelected, validateDraft, async (nextDraft) => {
    const next = config.hotkeys.map((rule) =>
      rule.id === nextDraft.id ? nextDraft : rule,
    );
    await persist({ ...config, hotkeys: next });
  });

  const addRule = () => {
    const rule = blankRule(config.hotkeys);
    setSelectedId(rule.id);
    setDraft(rule);
    void save([...config.hotkeys, rule], "已添加快捷键");
  };

  const removeSelected = () => {
    if (!selected) return;
    const next = config.hotkeys.filter((rule) => rule.id !== selected.id);
    setSelectedId(next[0]?.id ?? null);
    setSelectedIds((current) => {
      const nextIds = new Set(current);
      nextIds.delete(selected.id);
      return nextIds;
    });
    void save(next, "规则已删除");
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
      current.size === config.hotkeys.length
        ? new Set()
        : new Set(config.hotkeys.map((rule) => rule.id)),
    );
  };

  const removeSelectedRules = () => {
    if (selectedIds.size === 0) return;
    const next = config.hotkeys.filter((rule) => !selectedIds.has(rule.id));
    if (selected && selectedIds.has(selected.id)) {
      setSelectedId(next[0]?.id ?? null);
    }
    setSelectedIds(new Set());
    void save(next, `已删除 ${config.hotkeys.length - next.length} 条规则`);
  };

  const toggleRule = (rule: HotkeyRule) => {
    const candidate = selected?.id === rule.id ? selected : rule;
    const next = config.hotkeys.map((item) =>
      item.id === rule.id
        ? { ...candidate, enabled: !candidate.enabled }
        : item,
    );
    setDraft((current) =>
      current?.id === rule.id
        ? { ...current, enabled: !candidate.enabled }
        : current,
    );
    void save(next, candidate.enabled ? "规则已停用" : "规则已启用");
  };

  return (
    <div className="page-stack">
      <PageHeader
        title="快捷键"
        action={
          <button
            className="button button-primary"
            onClick={addRule}
            type="button"
          >
            ＋ 添加快捷键
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
              <strong>我的快捷键</strong>
              <span>{config.hotkeys.length} 条规则</span>
            </div>
            <BulkActions
              count={selectedIds.size}
              onDelete={removeSelectedRules}
              onToggleAll={toggleAllSelected}
              total={config.hotkeys.length}
            />
          </div>
          {loading ? (
            <div className="panel-loading">正在读取本机配置…</div>
          ) : config.hotkeys.length === 0 ? (
            <div className="panel-empty">
              还没有规则
              <br />
              <button className="text-button" onClick={addRule} type="button">
                添加第一条
              </button>
            </div>
          ) : (
            <div className="rule-list">
              {config.hotkeys.map((rule) => (
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
                  <span
                    className={`rule-icon ${rule.action.type === "launch" ? "launch" : "remap"}`}
                  >
                    {rule.action.type === "launch" ? "↗" : "↔"}
                  </span>
                  <span className="rule-list-copy">
                    <strong>{rule.name}</strong>
                    <span>
                      {rule.triggerKeys.join(" + ")} <em>→</em>{" "}
                      {rule.action.type === "remap"
                        ? rule.action.target
                        : "启动程序"}
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
            ＋ 新建规则
          </button>
        </section>
        <section className="editor-panel">
          {selected ? (
            <>
              <div className="editor-heading">
                <div>
                  <span className="editor-kicker">快捷键规则</span>
                  <h2>{selected.name || "未命名规则"}</h2>
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
                <label htmlFor="hotkey-name">规则名称</label>
                <input
                  id="hotkey-name"
                  value={selected.name}
                  onChange={(event) =>
                    updateDraft({ name: event.target.value })
                  }
                  placeholder="例如：CapsLock 改为 Esc"
                />
              </div>
              <div className="form-section">
                <label htmlFor="hotkey-trigger">
                  触发组合 <span>点击输入框后直接按下组合键</span>
                </label>
                <div className="hotkey-capture-row">
                  <input
                    id="hotkey-trigger"
                    aria-label="触发组合，点击后按键"
                    readOnly
                    value={selected.triggerKeys.join(" + ")}
                    onBlur={() => {
                      captureActive.current = false;
                    }}
                    onFocus={() => {
                      captureActive.current = false;
                    }}
                    onKeyDown={captureTrigger}
                    placeholder="点击后按 Ctrl + Alt + T"
                  />
                </div>
                <div className="form-help">
                  例如点击输入框后依次按住 Ctrl、Alt，再按
                  T；松开后组合会保留在这里。
                </div>
                {duplicateTrigger ? (
                  <div className="field-warning">
                    ⚠ 这个组合已经被另一条规则使用，保存后可能产生冲突。
                  </div>
                ) : null}
              </div>
              <div className="form-section">
                <label htmlFor="hotkey-action">执行动作</label>
                <select
                  id="hotkey-action"
                  value={selected.action.type}
                  onChange={(event) =>
                    updateDraft({
                      action: {
                        ...selected.action,
                        type: event.target
                          .value as HotkeyRule["action"]["type"],
                      },
                    })
                  }
                >
                  <option value="remap">映射为另一个按键</option>
                  <option value="launch">启动程序</option>
                </select>
              </div>
              <div className="form-section">
                <label htmlFor="hotkey-target">
                  {selected.action.type === "remap"
                    ? "目标按键"
                    : "启动程序（每行一个）"}
                </label>
                {selected.action.type === "remap" ? (
                  <input
                    id="hotkey-target"
                    value={selected.action.target}
                    onChange={(event) =>
                      updateDraft({
                        action: {
                          ...selected.action,
                          target: event.target.value,
                        },
                      })
                    }
                    placeholder="例如：Esc"
                  />
                ) : (
                  <textarea
                    id="hotkey-target"
                    rows={3}
                    value={selected.action.target}
                    onChange={(event) =>
                      updateDraft({
                        action: {
                          ...selected.action,
                          target: event.target.value,
                        },
                      })
                    }
                    placeholder={"例如：wt.exe\nnotepad.exe"}
                  />
                )}
                <div className="form-help">
                  {selected.action.type === "remap"
                    ? "常用：Esc、Enter、Space、A–Z。"
                    : "每行启动一个程序，也可以在程序名后填写参数。"}
                </div>
              </div>
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
                    {selected.enabled ? "停用规则" : "启用规则"}
                  </button>
                </div>
              </div>
            </>
          ) : (
            <div className="editor-empty">
              <div className="empty-icon">↔</div>
              <h2>选择一条规则开始编辑</h2>
            </div>
          )}
        </section>
      </div>
    </div>
  );
}
