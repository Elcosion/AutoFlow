import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { BulkActions } from "../components/BulkActions";
import { PageHeader } from "../components/PageHeader";
import { useAppConfig, toErrorMessage } from "../lib/config";
import { useAutoSave } from "../lib/useAutoSave";
import {
  newRuleId,
  type MacroMode,
  type MacroRule,
  type MacroStep,
} from "../types/config";
import {
  getMacroPlaybackStatus,
  getMacroRecordingStatus,
  playMacro,
  startMacroRecording,
  stopMacro,
  stopMacroRecording,
} from "../lib/tauri";

const macroSignature = (keys: string[]) =>
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

const blankMacro = (existing: MacroRule[]): MacroRule => {
  const used = new Set(
    existing.map((macro) => macroSignature(macro.triggerKeys)),
  );
  const candidates = ["F8", "F9", "F10", "F11", "Ctrl+F8", "Ctrl+F9"];
  const trigger =
    candidates.find((candidate) => !used.has(candidate)) ??
    `F${17 + existing.length}`;
  const triggerKeys = trigger.includes("+") ? trigger.split("+") : [trigger];
  return {
    id: newRuleId("macro"),
    name: "新宏",
    enabled: false,
    triggerKeys,
    mode: "once",
    repeatCount: 1,
    speed: 1,
    steps: [],
  };
};

const copyMacro = (macro: MacroRule): MacroRule => ({
  ...macro,
  triggerKeys: [...macro.triggerKeys],
  steps: macro.steps.map((step) => ({ ...step })),
});

const modeLabels: Record<MacroMode, string> = {
  once: "单次",
  repeat: "固定次数",
  hold: "按住循环",
  toggle: "开关循环",
};

const modeDescriptions: Record<MacroMode, string> = {
  once: "触发一次，只执行一轮步骤。",
  repeat: "触发一次，连续执行指定的循环次数。",
  hold: "按住触发组合时持续循环，松开后停止。",
  toggle: "按一次开始循环，再按一次相同组合停止。",
};

function stepTitle(step: MacroStep): string {
  switch (step.type) {
    case "delay":
      return step.durationMaxMs !== undefined && step.durationMaxMs > step.durationMs
        ? `随机等待 ${step.durationMs}–${step.durationMaxMs} ms`
        : `等待 ${step.durationMs} ms`;
    case "key":
      return `${step.action === "down" ? "按下" : "释放"} ${step.key}`;
    case "mouseButton":
      return `${step.action === "down" ? "按下" : "释放"} 鼠标${step.button}`;
    case "mouseMove":
      return `移动到 (${step.x}, ${step.y})`;
    case "wheel":
      return `滚轮 (${step.deltaX}, ${step.deltaY})`;
    case "text":
      return `输入文本 “${step.text.slice(0, 24)}${step.text.length > 24 ? "…" : ""}”`;
  }
}

export function MacrosPage() {
  const { config, loading, saving, error, setError, persist } = useAppConfig();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [draft, setDraft] = useState<MacroRule | null>(null);
  const [recording, setRecording] = useState(false);
  const [recordingStepCount, setRecordingStepCount] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [playbackStep, setPlaybackStep] = useState<{
    current: number;
    total: number;
  } | null>(null);
  const [playCountdown, setPlayCountdown] = useState<number | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [draggingStepIndex, setDraggingStepIndex] = useState<number | null>(
    null,
  );
  const [dragOverStepIndex, setDragOverStepIndex] = useState<number | null>(
    null,
  );
  const dragStateRef = useRef<{
    fromIndex: number;
    pointerId: number;
  } | null>(null);
  const dragOverStepRef = useRef<number | null>(null);
  const triggerCaptureActive = useRef(false);
  const playbackTimerRef = useRef<number | null>(null);
  const configRef = useRef(config);
  const recordingMacroIdRef = useRef<string | null>(null);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());

  useEffect(() => {
    configRef.current = config;
  }, [config]);

  useEffect(
    () => () => {
      if (playbackTimerRef.current !== null) {
        window.clearInterval(playbackTimerRef.current);
      }
    },
    [],
  );

  useEffect(() => {
    if (!selectedId && config.macros[0]) setSelectedId(config.macros[0].id);
  }, [config.macros, selectedId]);

  useEffect(() => {
    const next = config.macros.find((macro) => macro.id === selectedId);
    setDraft((current) => {
      if (next) return copyMacro(next);
      return current?.id === selectedId ? current : null;
    });
  }, [config.macros, selectedId]);

  useEffect(() => {
    if (!playing) return;
    let active = true;
    const timer = window.setInterval(() => {
      void getMacroPlaybackStatus()
        .then((status) => {
          if (!active) return;
          setPlaybackStep({
            current: status.currentStep,
            total: status.totalSteps,
          });
          if (!status.running) {
            setPlaying(false);
            if (status.lastError) setError(status.lastError);
          }
        })
        .catch(() => undefined);
    }, 250);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, [playing]);

  useEffect(() => {
    if (!recording) return;
    let active = true;
    const refresh = () => {
      void getMacroRecordingStatus()
        .then((status) => {
          if (!active) return;
          setRecordingStepCount(status.stepCount);
        })
        .catch(() => undefined);
    };
    refresh();
    const timer = window.setInterval(refresh, 180);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, [recording]);

  useEffect(() => {
    if (draggingStepIndex === null) return;

    const handlePointerMove = (event: PointerEvent) => {
      const target = document.elementFromPoint(event.clientX, event.clientY);
      const row = target?.closest<HTMLElement>("[data-macro-step-index]");
      const nextIndex = row
        ? Number(row.dataset.macroStepIndex)
        : dragOverStepRef.current;
      if (
        Number.isInteger(nextIndex) &&
        nextIndex !== dragOverStepRef.current
      ) {
        dragOverStepRef.current = nextIndex;
        setDragOverStepIndex(nextIndex);
      }
    };

    const finishPointerDrag = () => {
      const dragState = dragStateRef.current;
      const targetIndex = dragOverStepRef.current;
      if (dragState && targetIndex !== null) {
        moveStepTo(dragState.fromIndex, targetIndex);
      }
      dragStateRef.current = null;
      dragOverStepRef.current = null;
      setDraggingStepIndex(null);
      setDragOverStepIndex(null);
    };

    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", finishPointerDrag, { once: true });
    window.addEventListener("pointercancel", finishPointerDrag, { once: true });
    return () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", finishPointerDrag);
      window.removeEventListener("pointercancel", finishPointerDrag);
    };
  }, [draggingStepIndex, draft?.steps.length]);

  const selected = draft;
  const showNotice = (message: string) => {
    setNotice(message);
    window.setTimeout(() => setNotice(null), 1800);
  };

  const save = async (macros: MacroRule[], message: string) => {
    try {
      await persist({ ...config, macros });
      showNotice(message);
    } catch {
      // The hook exposes the actionable error in the page.
    }
  };

  const updateDraft = (patch: Partial<MacroRule>) => {
    setDraft((current) => (current ? { ...current, ...patch } : current));
  };

  const captureTrigger = (event: ReactKeyboardEvent<HTMLInputElement>) => {
    const key = normalizeCapturedKey(event);
    if (!key || event.repeat) return;
    event.preventDefault();
    event.stopPropagation();
    const currentKeys = triggerCaptureActive.current
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
    triggerCaptureActive.current = true;
    updateDraft({ triggerKeys: canonicalizeKeys(nextKeys) });
  };

  const validateDraft = (candidate: MacroRule) => {
    if (!candidate.name.trim()) return "请先填写宏名称";
    if (candidate.enabled && candidate.triggerKeys.length === 0)
      return "启用宏前至少需要设置一个触发键";
    if (candidate.enabled && candidate.steps.length === 0)
      return "启用宏前请先录制或添加至少一个步骤";
    if (candidate.steps.some((step) => step.type === "text" && !step.text))
      return "文本步骤不能为空，请填写内容或删除该步骤";
    return undefined;
  };

  const savedSelected = config.macros.find(
    (macro) => macro.id === selected?.id,
  );

  useAutoSave(selected, savedSelected, validateDraft, async (nextDraft) => {
    const next = config.macros.map((macro) =>
      macro.id === nextDraft.id ? nextDraft : macro,
    );
    await persist({ ...config, macros: next });
  });

  const addMacro = () => {
    const macro = blankMacro(config.macros);
    setSelectedId(macro.id);
    setDraft(macro);
    void save([...config.macros, macro], "已创建宏");
  };

  const removeSelected = () => {
    if (!selected) return;
    void stopMacro();
    const next = config.macros.filter((macro) => macro.id !== selected.id);
    setSelectedId(next[0]?.id ?? null);
    setSelectedIds((current) => {
      const nextIds = new Set(current);
      nextIds.delete(selected.id);
      return nextIds;
    });
    void save(next, "宏已删除");
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
      current.size === config.macros.length
        ? new Set()
        : new Set(config.macros.map((macro) => macro.id)),
    );
  };

  const removeSelectedMacros = () => {
    if (selectedIds.size === 0) return;
    void stopMacro();
    const next = config.macros.filter((macro) => !selectedIds.has(macro.id));
    if (selected && selectedIds.has(selected.id)) {
      setSelectedId(next[0]?.id ?? null);
    }
    setSelectedIds(new Set());
    void save(next, `已删除 ${config.macros.length - next.length} 个宏`);
  };

  const toggleMacro = (macro: MacroRule) => {
    const candidate = selected?.id === macro.id ? selected : macro;
    if (!candidate.enabled && candidate.steps.length === 0) {
      setError("这个宏还没有步骤，录制或添加步骤后才能启用");
      return;
    }
    const next = config.macros.map((item) =>
      item.id === macro.id
        ? { ...candidate, enabled: !candidate.enabled }
        : item,
    );
    setDraft((current) =>
      current?.id === macro.id
        ? { ...current, enabled: !candidate.enabled }
        : current,
    );
    void save(next, candidate.enabled ? "宏已停用" : "宏已启用");
  };

  const addStep = (step: MacroStep) =>
    updateDraft({ steps: [...(selected?.steps ?? []), step] });

  const updateStep = (index: number, step: MacroStep) => {
    if (!selected) return;
    updateDraft({
      steps: selected.steps.map((item, itemIndex) =>
        itemIndex === index ? step : item,
      ),
    });
  };

  const moveStep = (index: number, direction: -1 | 1) => {
    if (!selected) return;
    const nextIndex = index + direction;
    if (nextIndex < 0 || nextIndex >= selected.steps.length) return;
    const steps = [...selected.steps];
    [steps[index], steps[nextIndex]] = [steps[nextIndex], steps[index]];
    updateDraft({ steps });
  };

  const moveStepTo = (fromIndex: number, toIndex: number) => {
    if (!selected || fromIndex === toIndex) return;
    if (
      fromIndex < 0 ||
      toIndex < 0 ||
      fromIndex >= selected.steps.length ||
      toIndex >= selected.steps.length
    ) {
      return;
    }
    const steps = [...selected.steps];
    const [moved] = steps.splice(fromIndex, 1);
    steps.splice(toIndex, 0, moved);
    updateDraft({ steps });
  };

  const deleteStep = (index: number) => {
    if (selected)
      updateDraft({
        steps: selected.steps.filter((_, itemIndex) => itemIndex !== index),
      });
  };

  const duplicateStep = (index: number) => {
    if (!selected) return;
    const steps = [...selected.steps];
    steps.splice(index + 1, 0, { ...steps[index] });
    updateDraft({ steps });
  };

  const startRecording = async () => {
    try {
      let macro = selected;
      if (!selected) {
        macro = blankMacro(configRef.current.macros);
        await persist({
          ...configRef.current,
          macros: [...configRef.current.macros, macro],
        });
        setSelectedId(macro.id);
        setDraft(macro);
      }
      await startMacroRecording();
      recordingMacroIdRef.current = macro?.id ?? null;
      setRecordingStepCount(0);
      setRecording(true);
      showNotice(
        `正在录制“${macro?.name ?? "新宏"}”：切到任意软件操作，完成后返回此处停止录制`,
      );
    } catch (reason) {
      setError(toErrorMessage(reason));
    }
  };

  const finishRecording = async () => {
    try {
      const recorded = await stopMacroRecording();
      setRecording(false);
      setRecordingStepCount(0);
      const currentConfig = configRef.current;
      const recordingMacroId = recordingMacroIdRef.current;
      const recordedMacro =
        currentConfig.macros.find((macro) => macro.id === recordingMacroId) ??
        (selected?.id === recordingMacroId ? selected : undefined);
      if (!recordedMacro) {
        throw new Error("找不到正在录制的宏，请重新开始录制");
      }
      const nextMacro = {
        ...recordedMacro,
        steps: [...recordedMacro.steps, ...recorded.steps],
        target: undefined,
      };
      const next = currentConfig.macros.map((macro) =>
        macro.id === nextMacro.id ? nextMacro : macro,
      );
      await persist({ ...currentConfig, macros: next });
      setDraft(copyMacro(nextMacro));
      recordingMacroIdRef.current = null;
      showNotice(`已录制并保存 ${recorded.steps.length} 个步骤`);
    } catch (reason) {
      setError(toErrorMessage(reason));
      setRecording(false);
      setRecordingStepCount(0);
      recordingMacroIdRef.current = null;
    }
  };

  const runSelected = async () => {
    if (!selected || selected.steps.length === 0) {
      setError("请先录制或添加至少一个宏步骤");
      return;
    }
    if (playbackTimerRef.current !== null) return;
    const macroToPlay = copyMacro(selected);
    let remaining = 3;
    setPlaybackStep(null);
    setPlayCountdown(remaining);
    showNotice("3 秒后开始播放：现在请切换到目标窗口");
    playbackTimerRef.current = window.setInterval(() => {
      remaining -= 1;
      if (remaining > 0) {
        setPlayCountdown(remaining);
        return;
      }
      if (playbackTimerRef.current !== null) {
        window.clearInterval(playbackTimerRef.current);
        playbackTimerRef.current = null;
      }
      setPlayCountdown(null);
      void playMacro(macroToPlay)
        .then(() => {
          setPlaying(true);
          showNotice("宏正在目标窗口播放，按 F12 可停止");
        })
        .catch((reason) => {
          setPlaying(false);
          setError(toErrorMessage(reason));
        });
    }, 1000);
  };

  const stopPlaying = async () => {
    if (playbackTimerRef.current !== null) {
      window.clearInterval(playbackTimerRef.current);
      playbackTimerRef.current = null;
      setPlayCountdown(null);
    }
    await stopMacro();
    setPlaying(false);
    setPlaybackStep(null);
    showNotice("宏播放已停止");
  };

  return (
    <div className="page-stack">
      <PageHeader
        title="宏"
        action={
          <div className="header-actions">
            <button
              className="button button-secondary"
              onClick={addMacro}
              type="button"
            >
              ＋ 新建宏
            </button>
            {recording ? (
              <button
                className="button button-danger"
                onClick={() => void finishRecording()}
                type="button"
              >
                ■ 停止录制
              </button>
            ) : (
              <button
                className="button button-primary"
                onClick={() => void startRecording()}
                type="button"
              >
                ● 开始录制
              </button>
            )}
          </div>
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
      {recording ? (
        <div className="macro-recording-guide">
          <strong>正在录制</strong>
          <span>
            切换到任何软件后直接操作。键盘、点击、拖拽和滚轮都会被记录；不绑定任何窗口。完成后回到这里点击“停止录制”，结果会立即保存。
          </span>
          <span className="recording-progress">
            正在捕获操作 · 已捕获 {recordingStepCount} 步
          </span>
        </div>
      ) : null}
      {playCountdown !== null ? (
        <div className="macro-playback-guide">
          <strong>{playCountdown} 秒后播放</strong>
          <span>请立刻切换到需要执行宏的窗口。</span>
          <button onClick={() => void stopPlaying()} type="button">
            取消
          </button>
        </div>
      ) : null}
      {playing ? (
        <div className="macro-playback-guide">
          <strong>正在执行宏</strong>
          <span>
            {playbackStep
              ? `已发送第 ${playbackStep.current} / ${playbackStep.total} 步`
              : "正在连接输入服务…"}
          </span>
          <button onClick={() => void stopPlaying()} type="button">
            停止
          </button>
        </div>
      ) : null}
      {notice ? <div className="success-banner">✓ {notice}</div> : null}
      <div className="macro-layout">
        <section className="macro-list-panel">
          <div className="panel-heading">
            <div>
              <strong>我的宏</strong>
              <span>{config.macros.length} 个宏</span>
            </div>
            <BulkActions
              count={selectedIds.size}
              onDelete={removeSelectedMacros}
              onToggleAll={toggleAllSelected}
              total={config.macros.length}
            />
          </div>
          {loading ? (
            <div className="panel-loading">正在读取本机配置…</div>
          ) : config.macros.length === 0 ? (
            <div className="panel-empty">
              还没有宏
              <br />
              <button className="text-button" onClick={addMacro} type="button">
                创建第一个宏
              </button>
            </div>
          ) : (
            <div className="rule-list">
              {config.macros.map((macro) => (
                <div
                  className={`rule-list-item ${selectedId === macro.id ? "is-selected" : ""}`}
                  key={macro.id}
                  onClick={() => setSelectedId(macro.id)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                      event.preventDefault();
                      setSelectedId(macro.id);
                    }
                  }}
                  role="button"
                  tabIndex={0}
                >
                  <input
                    aria-label={`选择 ${macro.name}`}
                    checked={selectedIds.has(macro.id)}
                    className="rule-select"
                    onChange={() => toggleSelected(macro.id)}
                    onClick={(event) => event.stopPropagation()}
                    type="checkbox"
                  />
                  <span className="rule-icon macro">▶</span>
                  <span className="rule-list-copy">
                    <strong>{macro.name}</strong>
                    <span>
                      {macro.triggerKeys.join(" + ")} · {macro.steps.length} 步
                      · {modeLabels[macro.mode]}
                    </span>
                  </span>
                  <button
                    className={`mini-toggle ${macro.enabled ? "on" : ""}`}
                    aria-label={`${macro.name}${macro.enabled ? "停用" : "启用"}`}
                    aria-pressed={macro.enabled}
                    onClick={(event) => {
                      event.stopPropagation();
                      toggleMacro(macro);
                    }}
                    type="button"
                  >
                    <i />
                  </button>
                </div>
              ))}
            </div>
          )}
          <button className="list-add" onClick={addMacro} type="button">
            ＋ 新建宏
          </button>
        </section>
        <section className="macro-editor-panel">
          {selected ? (
            <>
              <div className="editor-heading">
                <div>
                  <span className="editor-kicker">宏编辑器</span>
                  <h2>{selected.name || "未命名宏"}</h2>
                </div>
                <button
                  className="danger-link"
                  onClick={removeSelected}
                  type="button"
                >
                  删除宏
                </button>
              </div>
              <div className="macro-settings-grid">
                <div className="form-section">
                  <label htmlFor="macro-name">宏名称</label>
                  <input
                    id="macro-name"
                    value={selected.name}
                    onChange={(event) =>
                      updateDraft({ name: event.target.value })
                    }
                    placeholder="例如：每日重复操作"
                  />
                </div>
                <div className="form-section">
                  <label htmlFor="macro-trigger">触发组合</label>
                  <div className="trigger-capture-field">
                    <input
                      id="macro-trigger"
                      value={selected.triggerKeys.join(" + ")}
                      onBlur={() => {
                        triggerCaptureActive.current = false;
                      }}
                      onFocus={() => {
                        triggerCaptureActive.current = false;
                      }}
                      onKeyDown={captureTrigger}
                      placeholder="点击后按下组合键"
                      readOnly
                    />
                  </div>
                  <div className="form-help">
                    点击输入框后直接按组合键，例如 Ctrl、Alt、再按 F8。
                  </div>
                </div>
                <div className="form-section">
                  <label htmlFor="macro-mode">运行模式</label>
                  <select
                    id="macro-mode"
                    value={selected.mode}
                    onChange={(event) =>
                      updateDraft({ mode: event.target.value as MacroMode })
                    }
                  >
                    {Object.entries(modeLabels).map(([value, label]) => (
                      <option key={value} value={value}>
                        {label}
                      </option>
                    ))}
                  </select>
                  <div className="form-help">
                    {modeDescriptions[selected.mode]}
                  </div>
                </div>
                {selected.mode === "repeat" ? (
                  <div className="form-section">
                    <label htmlFor="macro-repeat">循环次数</label>
                    <input
                      id="macro-repeat"
                      min={1}
                      type="number"
                      value={selected.repeatCount}
                      onChange={(event) =>
                        updateDraft({
                          repeatCount: Math.max(
                            1,
                            Number(event.target.value) || 1,
                          ),
                        })
                      }
                    />
                  </div>
                ) : null}
                <div className="form-section">
                  <label htmlFor="macro-speed">
                    整体速度 <span>1.0× 为原速</span>
                  </label>
                  <input
                    id="macro-speed"
                    min={0.05}
                    max={10}
                    step={0.05}
                    type="number"
                    value={selected.speed}
                    onChange={(event) =>
                      updateDraft({
                        speed: Math.max(0.05, Number(event.target.value) || 1),
                      })
                    }
                  />
                </div>
              </div>
              <div className="step-toolbar">
                <div>
                  <strong>步骤列表</strong>
                  <span>{selected.steps.length} 步</span>
                </div>
                <div className="step-add-actions">
                  <button
                    onClick={() =>
                      addStep({ type: "delay", durationMs: 300 })
                    }
                    type="button"
                  >
                    ＋ 等待
                  </button>
                  <button
                    onClick={() =>
                      addStep({ type: "key", key: "Enter", action: "down" })
                    }
                    type="button"
                  >
                    ＋ 按键
                  </button>
                  <button
                    onClick={() =>
                      addStep({
                        type: "mouseButton",
                        button: "left",
                        action: "down",
                        x: 0,
                        y: 0,
                      })
                    }
                    type="button"
                  >
                    ＋ 点击
                  </button>
                  <button
                    onClick={() => addStep({ type: "text", text: "" })}
                    type="button"
                  >
                    ＋ 文本
                  </button>
                  <button
                    onClick={() => addStep({ type: "mouseMove", x: 0, y: 0 })}
                    type="button"
                  >
                    ＋ 移动
                  </button>
                  <button
                    onClick={() =>
                      addStep({ type: "wheel", deltaX: 0, deltaY: -120 })
                    }
                    type="button"
                  >
                    ＋ 滚轮
                  </button>
                </div>
              </div>
              <div className="macro-steps">
                {selected.steps.length === 0 ? (
                  <div className="macro-empty-steps">
                    <span>01</span>
                    <div>
                      <strong>还没有步骤</strong>
                      <small>点击开始录制，或从右上角手动添加。</small>
                    </div>
                  </div>
                ) : (
                  selected.steps.map((step, index) => (
                    <div
                      className={`macro-step-row ${
                        draggingStepIndex === index ? "is-dragging" : ""
                      } ${
                        dragOverStepIndex === index &&
                        draggingStepIndex !== index
                          ? "is-drag-over"
                          : ""
                      }`}
                      data-macro-step-index={index}
                      key={`${selected.id}-${index}`}
                    >
                      <span
                        aria-label="拖动排序"
                        className="step-drag-handle"
                        onPointerDown={(event) => {
                          if (event.button !== 0) return;
                          event.preventDefault();
                          event.stopPropagation();
                          dragStateRef.current = {
                            fromIndex: index,
                            pointerId: event.pointerId,
                          };
                          dragOverStepRef.current = index;
                          setDraggingStepIndex(index);
                          setDragOverStepIndex(index);
                        }}
                        role="button"
                        tabIndex={0}
                        title="拖动排序"
                      >
                        ⋮⋮
                      </span>
                      <span className="step-number">
                        {String(index + 1).padStart(2, "0")}
                      </span>
                      <span className={`step-type step-type-${step.type}`}>
                        {step.type === "delay"
                          ? "时"
                          : step.type === "key"
                            ? "键"
                            : step.type === "text"
                              ? "文"
                              : "鼠"}
                      </span>
                      <div className="step-main">
                        <strong>{stepTitle(step)}</strong>
                        {step.type === "delay" ? (
                          <>
                            <div className="delay-range-fields">
                              <input
                                aria-label="等待最小毫秒数"
                                type="number"
                                min={0}
                                value={step.durationMs}
                                onChange={(event) =>
                                  updateStep(index, {
                                    ...step,
                                    durationMs: Math.max(
                                      0,
                                      Number(event.target.value) || 0,
                                    ),
                                  })
                                }
                                placeholder="固定毫秒"
                              />
                              <span>至</span>
                              <input
                                aria-label="等待最大毫秒数，可选"
                                type="number"
                                min={0}
                                value={step.durationMaxMs ?? ""}
                                onChange={(event) => {
                                  const value = event.target.value;
                                  updateStep(index, {
                                    ...step,
                                    durationMaxMs:
                                      value === ""
                                        ? undefined
                                        : Math.max(0, Number(value) || 0),
                                  });
                                }}
                                placeholder="最大值（可选）"
                              />
                            </div>
                            <small className="delay-range-help">
                              只填左侧为固定等待；填写右侧后每次从区间随机抽取。
                            </small>
                          </>
                        ) : null}
                        {step.type === "key" ? (
                          <div className="step-field-row">
                            <select
                              aria-label="按键动作"
                              value={step.action}
                              onChange={(event) =>
                                updateStep(index, {
                                  ...step,
                                  action: event.target.value as "down" | "up",
                                })
                              }
                            >
                              <option value="down">按下</option>
                              <option value="up">释放</option>
                            </select>
                            <input
                              aria-label="按键名称"
                              value={step.key}
                              onChange={(event) =>
                                updateStep(index, {
                                  ...step,
                                  key: event.target.value,
                                })
                              }
                            />
                          </div>
                        ) : null}
                        {step.type === "mouseButton" ? (
                          <div className="step-field-grid">
                            <select
                              aria-label="鼠标按钮"
                              value={step.button}
                              onChange={(event) =>
                                updateStep(index, {
                                  ...step,
                                  button: event.target
                                    .value as typeof step.button,
                                })
                              }
                            >
                              <option value="left">左键</option>
                              <option value="right">右键</option>
                              <option value="middle">中键</option>
                              <option value="x1">侧键 1</option>
                              <option value="x2">侧键 2</option>
                            </select>
                            <select
                              aria-label="鼠标动作"
                              value={step.action}
                              onChange={(event) =>
                                updateStep(index, {
                                  ...step,
                                  action: event.target.value as "down" | "up",
                                })
                              }
                            >
                              <option value="down">按下</option>
                              <option value="up">释放</option>
                            </select>
                            <input
                              aria-label="鼠标 X 坐标"
                              type="number"
                              value={step.x}
                              onChange={(event) =>
                                updateStep(index, {
                                  ...step,
                                  x: Number(event.target.value) || 0,
                                })
                              }
                            />
                            <input
                              aria-label="鼠标 Y 坐标"
                              type="number"
                              value={step.y}
                              onChange={(event) =>
                                updateStep(index, {
                                  ...step,
                                  y: Number(event.target.value) || 0,
                                })
                              }
                            />
                          </div>
                        ) : null}
                        {step.type === "mouseMove" ? (
                          <div className="step-field-row">
                            <input
                              aria-label="移动 X 坐标"
                              type="number"
                              value={step.x}
                              onChange={(event) =>
                                updateStep(index, {
                                  ...step,
                                  x: Number(event.target.value) || 0,
                                })
                              }
                            />
                            <input
                              aria-label="移动 Y 坐标"
                              type="number"
                              value={step.y}
                              onChange={(event) =>
                                updateStep(index, {
                                  ...step,
                                  y: Number(event.target.value) || 0,
                                })
                              }
                            />
                          </div>
                        ) : null}
                        {step.type === "wheel" ? (
                          <div className="step-field-row">
                            <input
                              aria-label="水平滚动量"
                              type="number"
                              value={step.deltaX}
                              onChange={(event) =>
                                updateStep(index, {
                                  ...step,
                                  deltaX: Number(event.target.value) || 0,
                                })
                              }
                            />
                            <input
                              aria-label="垂直滚动量"
                              type="number"
                              value={step.deltaY}
                              onChange={(event) =>
                                updateStep(index, {
                                  ...step,
                                  deltaY: Number(event.target.value) || 0,
                                })
                              }
                            />
                          </div>
                        ) : null}
                        {step.type === "text" ? (
                          <textarea
                            aria-label="输入文本"
                            rows={2}
                            value={step.text}
                            onChange={(event) =>
                              updateStep(index, {
                                ...step,
                                text: event.target.value,
                              })
                            }
                          />
                        ) : null}
                      </div>
                      <div className="step-controls">
                        <button
                          aria-label="上移步骤"
                          disabled={index === 0}
                          onClick={() => moveStep(index, -1)}
                          type="button"
                        >
                          ↑
                        </button>
                        <button
                          aria-label="下移步骤"
                          disabled={index === selected.steps.length - 1}
                          onClick={() => moveStep(index, 1)}
                          type="button"
                        >
                          ↓
                        </button>
                        <button
                          aria-label="复制步骤"
                          onClick={() => duplicateStep(index)}
                          type="button"
                        >
                          ⧉
                        </button>
                        <button
                          aria-label="删除步骤"
                          onClick={() => deleteStep(index)}
                          type="button"
                        >
                          ×
                        </button>
                      </div>
                    </div>
                  ))
                )}
              </div>
              <div className="editor-footer macro-editor-footer">
                <div>
                  <span
                    className={`status-label ${selected.enabled ? "enabled" : "disabled"}`}
                  >
                    <i />
                    {selected.enabled ? "触发已启用" : "触发已停用"}
                  </span>
                  <span className="save-hint">
                    {saving ? "正在保存…" : "已自动保存"}
                  </span>
                </div>
                <div className="editor-actions">
                  {playing ? (
                    <button
                      className="button button-danger"
                      onClick={() => void stopPlaying()}
                      type="button"
                    >
                      停止播放
                    </button>
                  ) : (
                    <button
                      className="button button-secondary"
                      onClick={() => void runSelected()}
                      type="button"
                    >
                      测试播放
                    </button>
                  )}
                  <button
                    className={`button ${selected.enabled ? "button-soft-danger" : "button-primary"}`}
                    onClick={() => toggleMacro(selected)}
                    type="button"
                  >
                    {selected.enabled ? "停用宏" : "启用宏"}
                  </button>
                </div>
              </div>
            </>
          ) : (
            <div className="editor-empty">
              <div className="empty-icon macro">▶</div>
              <h2>选择一个宏开始编辑</h2>
            </div>
          )}
        </section>
      </div>
    </div>
  );
}
