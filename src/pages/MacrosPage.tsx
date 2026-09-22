import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { BulkActions } from "../components/BulkActions";
import { AssetManager } from "../components/AssetManager";
import { PageHeader } from "../components/PageHeader";
import { RhaiEditor } from "../components/RhaiEditor";
import { useAppConfig, toErrorMessage } from "../lib/config";
import { useAutoSave } from "../lib/useAutoSave";
import { shouldHydrateSourceDraft } from "../lib/sourceDraft";
import { formatRhaiSource } from "../lib/rhaiFormatter";
import {
  RHAI_API_REFERENCE_SNIPPETS,
  type RhaiReferenceSnippet,
} from "../lib/rhaiCompletions";
import {
  newRuleId,
  macroSteps,
  type BehaviorPolicy,
  type MacroMode,
  type MacroRule,
  type MacroStep,
} from "../types/config";
import {
  getMacroPlaybackStatus,
  getMacroRecordingStatus,
  openDataDirectory,
  playMacro,
  startMacroRecording,
  setMacroRecordingOptions,
  stopMacro,
  stopMacroRecording,
  validateRhaiSource,
  type RhaiValidationReport,
} from "../lib/tauri";
import { nextCapturedKeys } from "../lib/triggerCapture";
import {
  holdTriggerDraftError,
  recoverHoldTriggerRepair,
} from "../lib/holdTrigger";
import {
  classifyMacroSource,
  MacroSourceError,
  macroToSource,
  parseMacroSource,
} from "../lib/macroSource";

const macroSignature = (keys: string[]) =>
  keys.map((key) => key.trim().toUpperCase()).join("+");

const uniqueMacroName = (existing: MacroRule[], baseName: string) => {
  const used = new Set(
    existing.map((macro) => macro.name.trim().toLocaleLowerCase()),
  );
  let suffix = 1;
  while (true) {
    const candidate = suffix === 1 ? baseName : `${baseName} (${suffix})`;
    if (!used.has(candidate.toLocaleLowerCase())) return candidate;
    suffix += 1;
  }
};

const RECORDING_SHORTCUT_LABEL = "Ctrl + Shift + F9";
type MacroEditorView = "visual" | "source";

type MacroUndoEntry = {
  label: string;
  macros: MacroRule[];
  selectedId: string | null;
};

const rhaiReferenceSnippets: RhaiReferenceSnippet[] = [
  {
    id: "statement-let",
    category: "基础语句",
    name: "变量 let",
    description: "声明一个可在后续步骤中复用的变量。",
    code: `let value = 100; // value：变量名；100：参考初始值`,
  },
  {
    id: "statement-if",
    category: "基础语句",
    name: "条件 if",
    description: "条件成立时执行代码块。",
    code: `let condition = true; // condition：需要判断的布尔条件
if condition {
  // 条件成立时执行
}`,
  },
  {
    id: "statement-if-else",
    category: "基础语句",
    name: "条件 if / else",
    description: "分别处理条件成立和不成立的情况。",
    code: `let condition = true; // condition：需要判断的布尔条件
if condition {
  // 条件成立时执行
} else {
  // 条件不成立时执行
}`,
  },
  {
    id: "statement-for",
    category: "基础语句",
    name: "计数循环 for",
    description: "按指定次数重复执行代码块。",
    code: `for index in 0..10 { // index：当前序号；0..10：循环范围
  // 每次循环执行
}`,
  },
  {
    id: "statement-while",
    category: "基础语句",
    name: "条件循环 while",
    description: "条件为真时持续执行，并展示安全退出写法。",
    code: `let attempts = 0; // attempts：已尝试次数
while attempts < 10 { // 10：最多尝试次数
  attempts += 1;
  wait_ms(200);
}`,
  },
  {
    id: "statement-loop",
    category: "基础语句",
    name: "循环 loop / break",
    description: "持续循环，并在满足条件时安全退出。",
    code: `let count = 0; // count：当前循环次数
loop {
  count += 1;
  if count >= 10 { // 10：最大循环次数
    break;
  }
}`,
  },
  {
    id: "statement-function",
    category: "基础语句",
    name: "函数 fn / return",
    description: "定义带参数和返回值的可复用函数。",
    code: `fn double_value(value) { // value：函数参数
  return value * 2; // 返回计算结果
}

let result = double_value(5); // result：函数返回值`,
  },
  ...RHAI_API_REFERENCE_SNIPPETS,
  {
    id: "copy-and-type",
    category: "组合示例",
    name: "复制后输入文本",
    description: "模拟 Ctrl+C，等待片刻后输入文本。",
    code: `let copy_key = "C"; // copy_key：复制快捷键
let output_text = "AutoFlow 示例文本"; // output_text：要输入的内容

key_down("Ctrl");
press(copy_key);
key_up("Ctrl");
wait_ms(300); // 等待剪贴板稳定
type_text(output_text);`,
  },
  {
    id: "click-and-wait",
    category: "组合示例",
    name: "坐标点击并等待",
    description: "移动到指定坐标、点击并等待。",
    code: `let target_x = 820; // target_x：目标横坐标
let target_y = 430; // target_y：目标纵坐标
let button = "left"; // button：鼠标按钮

move_to(target_x, target_y);
click(button, target_x, target_y);
wait_ms(300);`,
  },
  {
    id: "random-scroll",
    category: "组合示例",
    name: "随机间隔滚轮",
    description: "以随机间隔向下滚动。",
    code: `let min_delay = 300; // min_delay：最短等待毫秒数
let max_delay = 800; // max_delay：最长等待毫秒数
let scroll_y = -120; // scroll_y：垂直滚动量

wait_random_ms(min_delay, max_delay);
scroll(0, scroll_y);`,
  },
  {
    id: "stop-message",
    category: "组合示例",
    name: "停止并弹窗提示",
    description: "停止当前脚本并显示自定义标题和内容。",
    code: `let dialog_title = "AutoFlow"; // dialog_title：弹窗标题
let dialog_message = "任务已完成"; // dialog_message：提示内容
stop_with_message(dialog_title, dialog_message);`,
  },
  {
    id: "window-image-click",
    category: "组合示例",
    name: "窗口内查找图片并点击",
    description: "在指定窗口内查找素材图片并点击中心。",
    code: `let window_title = "记事本"; // window_title：窗口标题关键字
let image_file = "confirm_button.png"; // image_file：完整文件名，必须包含后缀
let threshold = 0.90; // threshold：匹配阈值 0–1

let window = window_rect(window_title);
if window.found {
  let result = find_image(image_file, window.x, window.y, window.width, window.height, threshold);
  if result.found {
    click("left", result.center_x, result.center_y);
  }
}`,
  },
  {
    id: "wait-pixel",
    category: "组合示例",
    name: "等待像素颜色出现",
    description: "等待指定坐标接近目标颜色。",
    code: `let x = 100; // x：屏幕横坐标
let y = 200; // y：屏幕纵坐标
let red = 32; // red：目标红色通道
let green = 64; // green：目标绿色通道
let blue = 128; // blue：目标蓝色通道
let tolerance = 8; // tolerance：颜色容差
let timeout_ms = 5000; // timeout_ms：最长等待毫秒数
let poll_ms = 200; // poll_ms：轮询间隔毫秒数

wait_pixel(x, y, red, green, blue, tolerance, timeout_ms, poll_ms);`,
  },
];

const usesRecordingShortcut = (keys: string[]) => {
  const normalized = new Set(keys.map((key) => key.trim().toUpperCase()));
  return (
    normalized.size === 3 &&
    ["CTRL", "SHIFT", "F9"].every((key) => normalized.has(key))
  );
};

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
    name: uniqueMacroName(existing, "新宏"),
    enabled: false,
    triggerKeys,
    mode: "once",
    repeatCount: 1,
    speed: 1,
    recordMouseMove: true,
    recordMouseClicks: true,
    program: { kind: "macro", steps: [] },
  };
};

const copyMacro = (macro: MacroRule): MacroRule => ({
  ...macro,
  triggerKeys: [...macro.triggerKeys],
  program:
    macro.program.kind === "macro"
      ? { kind: "macro", steps: macroSteps(macro).map((step) => ({ ...step })) }
      : { ...macro.program },
});

const copyBehaviorPolicy = (policy: BehaviorPolicy): BehaviorPolicy => ({
  ...policy,
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
  hold: "快捷键需且只能包含一个普通保持键；按住普通键并释放全部修饰键后开始，松开普通键停止。",
  toggle: "按一次开始循环，再按一次相同组合停止。",
};

function stepTitle(step: MacroStep): string {
  switch (step.type) {
    case "delay":
      return step.durationMaxMs !== undefined &&
        step.durationMaxMs > step.durationMs
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
  const { config, loading, saving, error, setError, persist, refresh } =
    useAppConfig();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [draft, setDraft] = useState<MacroRule | null>(null);
  const [editorView, setEditorView] = useState<MacroEditorView>("visual");
  const [sourceText, setSourceText] = useState("");
  const [sourceDirty, setSourceDirty] = useState(false);
  const [sourceError, setSourceError] = useState<string | null>(null);
  const [sourceInspection, setSourceInspection] = useState<{
    source: string;
    report: RhaiValidationReport;
  } | null>(null);
  const [sourceErrorLine, setSourceErrorLine] = useState<number | null>(null);
  const [sourceErrorColumn, setSourceErrorColumn] = useState<number | null>(
    null,
  );
  const [recording, setRecording] = useState(false);
  const [recordingCaptureStarted, setRecordingCaptureStarted] = useState(false);
  const [recordingStepCount, setRecordingStepCount] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [playbackStep, setPlaybackStep] = useState<{
    current: number;
    total: number;
  } | null>(null);
  const [playCountdown, setPlayCountdown] = useState<number | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [undoStack, setUndoStack] = useState<MacroUndoEntry[]>([]);
  const [undoing, setUndoing] = useState(false);
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
  const selectedIdRef = useRef(selectedId);
  const undoStackRef = useRef(undoStack);
  const undoLastMacroChangeRef = useRef<() => Promise<void>>(async () => {});
  const editorMacroIdRef = useRef<string | null>(null);
  const recordingMacroIdRef = useRef<string | null>(null);
  const finishingRecordingRef = useRef(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());

  useEffect(() => {
    configRef.current = config;
  }, [config]);

  useEffect(() => {
    selectedIdRef.current = selectedId;
  }, [selectedId]);

  useEffect(() => {
    undoStackRef.current = undoStack;
  }, [undoStack]);

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
    const next = config.macros.find((macro) => macro.id === selectedId);
    const previousMacroId = editorMacroIdRef.current;
    if (!shouldHydrateSourceDraft(previousMacroId, selectedId, sourceDirty)) {
      return;
    }
    if (previousMacroId !== selectedId) {
      setEditorView(next?.program.kind === "rhai" ? "source" : "visual");
      editorMacroIdRef.current = selectedId;
    }
    setSourceText(next ? macroToSource(next) : "");
    setSourceDirty(false);
    setSourceError(next?.importError ?? null);
    setSourceErrorLine(null);
    setSourceErrorColumn(null);
  }, [config.macros, selectedId, sourceDirty]);

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
          if (status.phaseObservation === "confirmed" && !status.running) {
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
  }, [draggingStepIndex, draft ? macroSteps(draft).length : 0]);

  const selected = draft;
  const selectedSteps = selected ? macroSteps(selected) : [];
  const selectedSourceKind = classifyMacroSource(sourceText);
  const hasCustomBehaviorPolicy = selected?.behaviorPolicy !== undefined;
  const selectedBehaviorPolicy =
    selected?.behaviorPolicy ?? config.behaviorPolicy;
  const selectedBehaviorProfile = selectedBehaviorPolicy.profileId
    ? config.behaviorProfilesV2.find(
        (profile) => profile.id === selectedBehaviorPolicy.profileId,
      )
    : undefined;

  const setBehaviorPolicyMode = (custom: boolean) => {
    if (!selected) return;
    updateDraft({
      behaviorPolicy: custom
        ? copyBehaviorPolicy(selected.behaviorPolicy ?? config.behaviorPolicy)
        : undefined,
    });
  };

  const updateBehaviorPolicy = (patch: Partial<BehaviorPolicy>) => {
    if (!selected) return;
    updateDraft({
      behaviorPolicy: {
        ...copyBehaviorPolicy(selected.behaviorPolicy ?? config.behaviorPolicy),
        ...patch,
      },
    });
  };

  useEffect(() => {
    if (!selected) return;
    void setMacroRecordingOptions(
      selected.recordMouseMove !== false,
      selected.recordMouseClicks !== false,
    ).catch(() => undefined);
  }, [selected?.id, selected?.recordMouseMove, selected?.recordMouseClicks]);

  const showNotice = (message: string) => {
    setNotice(message);
    window.setTimeout(() => setNotice(null), 1800);
  };

  const updatePlaybackOverlay = async (enabled: boolean) => {
    try {
      const savedConfig = await persist({
        ...configRef.current,
        showPlaybackOverlay: enabled,
      });
      configRef.current = savedConfig;
      showNotice(enabled ? "已开启播放进度悬浮窗" : "已关闭播放进度悬浮窗");
    } catch (reason) {
      setError(toErrorMessage(reason));
    }
  };

  const pushMacroUndo = (entry: MacroUndoEntry) => {
    setUndoStack((current) => {
      const next = [...current, entry].slice(-50);
      undoStackRef.current = next;
      return next;
    });
  };

  const persistMacroChange = async (
    macros: MacroRule[],
    label: string,
    showSavedNotice = false,
  ) => {
    const currentConfig = configRef.current;
    const previousMacros = currentConfig.macros.map(copyMacro);
    const previousSelectedId = selectedIdRef.current;
    if (JSON.stringify(previousMacros) === JSON.stringify(macros)) {
      return currentConfig;
    }
    const savedConfig = await persist({ ...currentConfig, macros });
    configRef.current = savedConfig;
    pushMacroUndo({
      label,
      macros: previousMacros,
      selectedId: previousSelectedId,
    });
    if (showSavedNotice) showNotice(label);
    return savedConfig;
  };

  const undoLastMacroChange = async () => {
    const entry = undoStackRef.current.at(-1);
    if (!entry || undoing || saving) return;
    setUndoing(true);
    try {
      await stopMacro();
      const currentConfig = configRef.current;
      const savedConfig = await persist({
        ...currentConfig,
        macros: entry.macros.map(copyMacro),
      });
      configRef.current = savedConfig;
      setUndoStack((current) => {
        const next = current.slice(0, -1);
        undoStackRef.current = next;
        return next;
      });
      const restoredId = savedConfig.macros.some(
        (macro) => macro.id === entry.selectedId,
      )
        ? entry.selectedId
        : (savedConfig.macros[0]?.id ?? null);
      const restoredMacro = savedConfig.macros.find(
        (macro) => macro.id === restoredId,
      );
      setSelectedId(restoredId);
      setSelectedIds(new Set());
      setDraft(restoredMacro ? copyMacro(restoredMacro) : null);
      setSourceText(restoredMacro ? macroToSource(restoredMacro) : "");
      setSourceDirty(false);
      setSourceError(restoredMacro?.importError ?? null);
      showNotice(`已撤销：${entry.label}`);
    } catch (reason) {
      setError(toErrorMessage(reason));
    } finally {
      setUndoing(false);
    }
  };

  undoLastMacroChangeRef.current = undoLastMacroChange;

  useEffect(() => {
    const handleUndoShortcut = (event: KeyboardEvent) => {
      if (
        event.defaultPrevented ||
        !(event.ctrlKey || event.metaKey) ||
        event.shiftKey ||
        event.key.toLowerCase() !== "z"
      ) {
        return;
      }
      const target = event.target;
      if (
        target instanceof HTMLInputElement ||
        target instanceof HTMLTextAreaElement ||
        target instanceof HTMLSelectElement ||
        (target instanceof HTMLElement && target.isContentEditable)
      ) {
        return;
      }
      event.preventDefault();
      void undoLastMacroChangeRef.current();
    };
    window.addEventListener("keydown", handleUndoShortcut);
    return () => window.removeEventListener("keydown", handleUndoShortcut);
  }, []);

  const openScriptFolder = async () => {
    try {
      await openDataDirectory("scripts");
      showNotice("已打开脚本文件夹");
    } catch (reason) {
      setError(toErrorMessage(reason));
    }
  };

  const refreshScriptFolder = async () => {
    try {
      await refresh();
      showNotice("已重新扫描脚本文件夹");
    } catch (reason) {
      setError(toErrorMessage(reason));
    }
  };

  const save = async (macros: MacroRule[], message: string) => {
    try {
      await persistMacroChange(macros, message, true);
    } catch {
      // The hook exposes the actionable error in the page.
    }
  };

  type MacroDraftPatch = Partial<MacroRule> & { steps?: MacroStep[] };

  const updateDraft = (patch: MacroDraftPatch) => {
    setDraft((current) => {
      if (!current) return current;
      const { steps: legacySteps, ...metadata } = patch;
      const next = { ...current, ...metadata };
      if (legacySteps !== undefined) {
        next.program = { kind: "macro", steps: legacySteps };
      }
      return recoverHoldTriggerRepair(next);
    });
  };

  const captureTrigger = (event: ReactKeyboardEvent<HTMLInputElement>) => {
    const key = normalizeCapturedKey(event);
    if (!key || event.repeat) return;
    event.preventDefault();
    event.stopPropagation();
    const currentKeys = triggerCaptureActive.current
      ? (selected?.triggerKeys ?? [])
      : [];
    triggerCaptureActive.current = true;
    updateDraft({ triggerKeys: nextCapturedKeys(currentKeys, key) });
  };

  const validateDraft = (candidate: MacroRule) => {
    const holdTriggerError = holdTriggerDraftError(candidate);
    if (holdTriggerError) return holdTriggerError;
    if (candidate.importError) return candidate.importError;
    if (
      candidate.behaviorPolicy?.profileId &&
      !configRef.current.behaviorProfilesV2.some(
        (profile) => profile.id === candidate.behaviorPolicy?.profileId,
      )
    ) {
      return "自定义仿生策略绑定的 V2 Profile 不存在，请改为全局策略或选择现有 Profile";
    }
    const candidateSteps =
      candidate.program.kind === "macro" ? candidate.program.steps : [];
    if (candidate.program.kind === "rhai") {
      if (!candidate.name.trim()) return "请输入宏名称";
      if (usesRecordingShortcut(candidate.triggerKeys))
        return `${RECORDING_SHORTCUT_LABEL} 保留为录制快捷键，请换一个宏触发组合`;
      if (candidate.enabled && candidate.triggerKeys.length === 0)
        return "启用宏前至少需要设置一个触发键";
      if (!candidate.program.source.trim()) return "高级 Rhai 脚本不能为空";
      return undefined;
    }
    if (!candidate.name.trim()) return "请先填写宏名称";
    if (usesRecordingShortcut(candidate.triggerKeys))
      return `${RECORDING_SHORTCUT_LABEL} 保留为录制快捷键，请换一个宏触发组合`;
    if (candidate.enabled && candidate.triggerKeys.length === 0)
      return "启用宏前至少需要设置一个触发键";
    if (candidate.enabled && candidateSteps.length === 0)
      return "启用宏前请先录制或添加至少一个步骤";
    if (candidateSteps.some((step) => step.type === "text" && !step.text))
      return "文本步骤不能为空，请填写内容或删除该步骤";
    return undefined;
  };

  const openSourceEditor = () => {
    if (!selected) return;
    setSourceText(macroToSource(selected));
    setSourceDirty(false);
    setSourceError(null);
    setEditorView("source");
  };

  const showSourceError = (reason: unknown) => {
    if (reason instanceof MacroSourceError) {
      setSourceErrorLine(reason.line);
      setSourceErrorColumn(reason.column);
    } else {
      setSourceErrorLine(null);
      setSourceErrorColumn(null);
    }
    setSourceError(toErrorMessage(reason));
  };

  const checkSource = async () => {
    try {
      if (classifyMacroSource(sourceText) === "advanced") {
        const report = await validateRhaiSource(sourceText);
        setSourceInspection({ source: sourceText, report });
        setSourceError(null);
        const first = report.unverifiedCalls[0];
        showNotice(
          first
            ? `静态检查未发现确定错误；${report.unverifiedCalls.length} 处调用尚未验证。首处：${first.name}（第 ${first.line ?? "?"} 行第 ${first.column ?? "?"} 列）：${first.reason}。运行结果仍需实际验证`
            : "静态语法和已注册 API 调用检查未发现确定错误；运行结果仍需实际验证",
        );
        setSourceErrorLine(null);
        setSourceErrorColumn(null);
        return;
      }
      if (!selected) return;
      parseMacroSource(sourceText, selected);
      setSourceError(null);
      setSourceErrorLine(null);
      setSourceErrorColumn(null);
      showNotice("语法检查通过");
    } catch (reason) {
      showSourceError(reason);
    }
  };

  const formatSource = () => {
    const result = formatRhaiSource(sourceText);
    if (!result.ok) {
      setSourceError(result.error.message);
      setSourceErrorLine(result.error.line);
      setSourceErrorColumn(result.error.column);
      return undefined;
    }
    setSourceError(null);
    setSourceErrorLine(null);
    setSourceErrorColumn(null);
    if (!result.changed) {
      showNotice("源码已是规范格式");
      return undefined;
    }
    showNotice("源码已格式化");
    return result.source;
  };

  const applySourceAndSwitch = async (openVisualConfiguration = false) => {
    if (!selected) return;
    if (classifyMacroSource(sourceText) === "advanced") {
      if (selected.program.kind !== "rhai") {
        const confirmed = window.confirm(
          "该源码包含循环、条件、变量或函数定义，无法转换为图形宏步骤。确认保存为高级 Rhai 脚本吗？保存后将保留源码；仍可进入配置界面调整触发与仿生策略，但不能编辑图形步骤。",
        );
        if (!confirmed) {
          setSourceError(
            "高级脚本未保存；如需图形宏，请删除循环、条件、变量和函数定义。",
          );
          return;
        }
      }
      const candidate: MacroRule = {
        ...selected,
        importError: undefined,
        program: { kind: "rhai", source: sourceText, apiVersion: 1 },
      };
      const validationError = validateDraft(candidate);
      if (validationError) {
        setSourceError(validationError);
        return;
      }
      try {
        await validateRhaiSource(sourceText);
        const currentConfig = configRef.current;
        const next = currentConfig.macros.map((macro) =>
          macro.id === candidate.id ? candidate : macro,
        );
        const savedConfig = await persistMacroChange(
          next,
          "编辑高级 Rhai 脚本",
        );
        const savedMacro = savedConfig.macros.find(
          (macro) => macro.id === candidate.id,
        );
        setDraft(copyMacro(savedMacro ?? candidate));
        setSourceText(macroToSource(savedMacro ?? candidate));
        setSourceDirty(false);
        setSourceError(null);
        setSourceErrorLine(null);
        setSourceErrorColumn(null);
        if (openVisualConfiguration) {
          setEditorView("visual");
          showNotice("高级 Rhai 脚本已保存，可调整宏配置");
        } else {
          showNotice("高级 Rhai 脚本已保存");
        }
      } catch (reason) {
        showSourceError(reason);
      }
      return;
    }
    try {
      const candidate = {
        ...parseMacroSource(sourceText, selected),
        importError: undefined,
      };
      const validationError = validateDraft(candidate);
      if (validationError) {
        setSourceError(validationError);
        return;
      }
      const currentConfig = configRef.current;
      const next = currentConfig.macros.map((macro) =>
        macro.id === candidate.id ? candidate : macro,
      );
      const savedConfig = await persistMacroChange(next, "编辑宏源码");
      const savedMacro = savedConfig.macros.find(
        (macro) => macro.id === candidate.id,
      );
      setDraft(copyMacro(savedMacro ?? candidate));
      setSourceText(macroToSource(savedMacro ?? candidate));
      setSourceDirty(false);
      setSourceError(null);
      setSourceErrorLine(null);
      setSourceErrorColumn(null);
      setEditorView("visual");
      showNotice("源码已应用");
    } catch (reason) {
      showSourceError(reason);
    }
  };

  const savedSelected = config.macros.find(
    (macro) => macro.id === selected?.id,
  );

  useAutoSave(selected, savedSelected, validateDraft, async (nextDraft) => {
    const next = config.macros.map((macro) =>
      macro.id === nextDraft.id ? nextDraft : macro,
    );
    const previous = config.macros.find((macro) => macro.id === nextDraft.id);
    const label = previous?.name !== nextDraft.name ? "重命名宏" : "修改宏配置";
    await persistMacroChange(next, label);
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
    if (candidate.importError) {
      setError(`该宏文件不合法，修复源码后才能启用：${candidate.importError}`);
      return;
    }
    if (
      !candidate.enabled &&
      candidate.program.kind === "macro" &&
      candidate.program.steps.length === 0
    ) {
      setError("这个宏还没有步骤，录制或添加步骤后才能启用");
      return;
    }
    const toggled = { ...candidate, enabled: !candidate.enabled };
    const validationError = validateDraft(toggled);
    if (validationError) {
      setError(validationError);
      return;
    }
    const next = config.macros.map((item) =>
      item.id === macro.id ? toggled : item,
    );
    setDraft((current) => (current?.id === macro.id ? toggled : current));
    void save(next, candidate.enabled ? "宏已停用" : "宏已启用");
  };

  const addStep = (step: MacroStep) =>
    updateDraft({ steps: [...(selected ? macroSteps(selected) : []), step] });

  const updateStep = (index: number, step: MacroStep) => {
    if (!selected) return;
    updateDraft({
      steps: macroSteps(selected).map((item, itemIndex) =>
        itemIndex === index ? step : item,
      ),
    });
  };

  const moveStep = (index: number, direction: -1 | 1) => {
    if (!selected) return;
    const nextIndex = index + direction;
    if (nextIndex < 0 || nextIndex >= macroSteps(selected).length) return;
    const steps = [...macroSteps(selected)];
    [steps[index], steps[nextIndex]] = [steps[nextIndex], steps[index]];
    updateDraft({ steps });
  };

  const moveStepTo = (fromIndex: number, toIndex: number) => {
    if (!selected || fromIndex === toIndex) return;
    if (
      fromIndex < 0 ||
      toIndex < 0 ||
      fromIndex >= macroSteps(selected).length ||
      toIndex >= macroSteps(selected).length
    ) {
      return;
    }
    const steps = [...macroSteps(selected)];
    const [moved] = steps.splice(fromIndex, 1);
    steps.splice(toIndex, 0, moved);
    updateDraft({ steps });
  };

  const deleteStep = (index: number) => {
    if (selected)
      updateDraft({
        steps: macroSteps(selected).filter(
          (_, itemIndex) => itemIndex !== index,
        ),
      });
  };

  const duplicateStep = (index: number) => {
    if (!selected) return;
    const steps = [...macroSteps(selected)];
    steps.splice(index + 1, 0, { ...steps[index] });
    updateDraft({ steps });
  };

  const startRecording = async () => {
    try {
      let macro = selected;
      if (!selected) {
        macro = blankMacro(configRef.current.macros);
        await persistMacroChange(
          [...configRef.current.macros, macro],
          "创建录制宏",
        );
        setSelectedId(macro.id);
        setDraft(macro);
      }
      await startMacroRecording(
        macro?.recordMouseMove !== false,
        macro?.recordMouseClicks !== false,
      );
      recordingMacroIdRef.current = macro?.id ?? null;
      setRecordingCaptureStarted(false);
      setRecordingStepCount(0);
      setRecording(true);
      showNotice(
        `正在录制“${macro?.name ?? "新宏"}”：先切到目标软件，按 ${RECORDING_SHORTCUT_LABEL} 停止`,
      );
    } catch (reason) {
      setError(toErrorMessage(reason));
    }
  };

  const finishRecording = async (discardTrailingMouseInput = false) => {
    if (finishingRecordingRef.current) return;
    finishingRecordingRef.current = true;
    try {
      const recorded = await stopMacroRecording(discardTrailingMouseInput);
      setRecording(false);
      setRecordingCaptureStarted(false);
      setRecordingStepCount(0);
      const currentConfig = configRef.current;
      const recordingMacroId = recordingMacroIdRef.current;
      const recordedMacro =
        currentConfig.macros.find((macro) => macro.id === recordingMacroId) ??
        (selected?.id === recordingMacroId ? selected : undefined);
      if (!recordedMacro) {
        throw new Error("找不到正在录制的宏，请重新开始录制");
      }
      const recordedSteps =
        recordedMacro.program.kind === "macro"
          ? recordedMacro.program.steps
          : [];
      const nextMacro = {
        ...recordedMacro,
        program: {
          kind: "macro" as const,
          steps: [...recordedSteps, ...recorded.steps],
        },
        target: undefined,
      };
      const next = currentConfig.macros.map((macro) =>
        macro.id === nextMacro.id ? nextMacro : macro,
      );
      await persistMacroChange(next, "保存录制步骤");
      setDraft(copyMacro(nextMacro));
      recordingMacroIdRef.current = null;
      showNotice(`已录制并保存 ${recorded.steps.length} 个步骤`);
    } catch (reason) {
      setError(toErrorMessage(reason));
      setRecording(false);
      setRecordingCaptureStarted(false);
      setRecordingStepCount(0);
      recordingMacroIdRef.current = null;
    } finally {
      finishingRecordingRef.current = false;
    }
  };

  useEffect(() => {
    let active = true;
    const refresh = () => {
      void getMacroRecordingStatus()
        .then((status) => {
          if (!active) return;
          setRecordingStepCount(status.stepCount);
          setRecordingCaptureStarted(status.captureStarted);

          if (status.active && !recording) {
            const baseMacro = selected ?? blankMacro(configRef.current.macros);
            const macro = {
              ...baseMacro,
              recordMouseMove: status.captureMouseMove,
              recordMouseClicks: status.captureMouseClicks,
            };
            recordingMacroIdRef.current = macro.id;
            if (!selected) {
              const macros = [...configRef.current.macros, macro];
              setSelectedId(macro.id);
              setDraft(copyMacro(macro));
              void persistMacroChange(macros, "创建录制宏").catch((reason) =>
                setError(toErrorMessage(reason)),
              );
            } else {
              setDraft((current) =>
                current?.id === macro.id ? { ...current, ...macro } : current,
              );
            }
            setRecording(true);
            showNotice(
              `正在录制：请先切到目标窗口，再按 ${RECORDING_SHORTCUT_LABEL} 停止`,
            );
          } else if (
            !status.active &&
            recording &&
            !finishingRecordingRef.current
          ) {
            void finishRecording(false);
          }
        })
        .catch(() => undefined);
    };

    refresh();
    const timer = window.setInterval(refresh, 180);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, [recording, selected?.id]);

  const runSelected = async () => {
    if (selected?.importError) {
      setError(`该宏文件不合法，修复源码后才能运行：${selected.importError}`);
      return;
    }
    if (
      !selected ||
      (selected.program.kind === "macro" && selectedSteps.length === 0) ||
      (selected.program.kind === "rhai" && !selected.program.source.trim())
    ) {
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
        description="宏与高级 Rhai 脚本会自动保存到 data/scripts，每个宏对应一个独立文件。"
        action={
          <div className="header-actions">
            <button
              className="button button-secondary"
              disabled={undoStack.length === 0 || saving || undoing}
              onClick={() => void undoLastMacroChange()}
              title={
                undoStack.length > 0
                  ? `撤销：${undoStack.at(-1)?.label}`
                  : "暂无可撤销操作"
              }
              type="button"
            >
              ↶ {undoing ? "正在撤销" : "撤销"}
            </button>
            <button
              className="button button-secondary"
              onClick={() => void refreshScriptFolder()}
              type="button"
            >
              刷新文件
            </button>
            <button
              className="button button-secondary"
              onClick={() => void openScriptFolder()}
              type="button"
            >
              打开脚本文件夹
            </button>
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
                onClick={() => void finishRecording(true)}
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
      <section className="macro-runtime-settings" aria-label="宏运行设置">
        <div>
          <strong>运行设置</strong>
          <p>
            进度悬浮窗只读显示当前宏动作、阶段和耗时，不提供停止按钮，也不会改变输入执行。
          </p>
        </div>
        <label className="toggle-setting">
          <input
            checked={config.showPlaybackOverlay}
            disabled={saving}
            onChange={(event) =>
              void updatePlaybackOverlay(event.target.checked)
            }
            type="checkbox"
          />
          <span>运行时显示进度悬浮窗</span>
        </label>
      </section>
      {recording ? (
        <div className="macro-recording-guide">
          <strong>正在录制</strong>
          <span>
            {recordingCaptureStarted
              ? `已开始捕获，请在目标软件中操作；按 ${RECORDING_SHORTCUT_LABEL} 停止`
              : `请先切到目标软件；AutoFlow 第一次失去焦点后才开始录制。按 ${RECORDING_SHORTCUT_LABEL} 停止`}
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
                    {macro.importError ? (
                      <small className="macro-program-badge invalid">
                        文件不合法
                      </small>
                    ) : null}
                    {macro.program.kind === "rhai" ? (
                      <small className="macro-program-badge">高级 Rhai</small>
                    ) : null}
                    <span>
                      {macro.triggerKeys.join(" + ")} ·{" "}
                      {macro.program.kind === "macro"
                        ? macro.program.steps.length
                        : 0}{" "}
                      步 · {modeLabels[macro.mode]}
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
              <div
                aria-label="宏编辑视图"
                className="editor-view-switcher"
                role="tablist"
              >
                <button
                  aria-selected={editorView === "visual"}
                  className={editorView === "visual" ? "is-active" : ""}
                  disabled={saving}
                  onClick={() => {
                    if (editorView === "visual") return;
                    void applySourceAndSwitch(true);
                  }}
                  role="tab"
                  type="button"
                >
                  {selected.program.kind === "rhai" ? "配置界面" : "图形界面"}
                </button>
                <button
                  aria-selected={editorView === "source"}
                  className={editorView === "source" ? "is-active" : ""}
                  disabled={recording || editorView === "source"}
                  onClick={openSourceEditor}
                  role="tab"
                  type="button"
                >
                  源码
                </button>
              </div>
              {selected.importError ? (
                <div className="error-banner">
                  <strong>导入文件不合法，当前宏已停用且不能运行。</strong>
                  <span>{selected.importError}</span>
                  <span>请进入源码页修改，语法检查通过后保存。</span>
                </div>
              ) : null}
              {editorView === "source" ? (
                <section className="macro-source-editor">
                  <div className="rhai-source-heading">
                    <div>
                      <strong>Rhai 脚本</strong>
                      <span>
                        {selectedSourceKind === "advanced"
                          ? "高级脚本：保留变量、条件、循环和函数，不能转换为图形步骤。"
                          : "兼容宏：每个 AutoFlow API 调用都可安全转换回图形步骤。"}
                      </span>
                    </div>
                    <button
                      className="button button-primary"
                      disabled={saving}
                      onClick={() => void applySourceAndSwitch(true)}
                      type="button"
                    >
                      {selectedSourceKind === "advanced"
                        ? "保存并进入配置界面"
                        : "应用并切回图形界面"}
                    </button>
                  </div>
                  <RhaiEditor
                    errorColumn={sourceError ? sourceErrorColumn : null}
                    errorLine={sourceError ? sourceErrorLine : null}
                    onChange={(value) => {
                      setSourceText(value);
                      setSourceDirty(true);
                      setSourceError(null);
                    }}
                    onCheck={checkSource}
                    onFormat={formatSource}
                    onSave={() => void applySourceAndSwitch()}
                    snippets={rhaiReferenceSnippets}
                    value={sourceText}
                  />
                  <div className="rhai-source-help">
                    <div className="rhai-source-help-summary">
                      <i aria-hidden="true" />
                      <div>
                        <strong>AutoFlow API v1 已就绪</strong>
                        <span>
                          右侧提供 {RHAI_API_REFERENCE_SNIPPETS.length} 个 API
                          调用示例，可搜索并插入到光标位置。
                        </span>
                      </div>
                    </div>
                    <div className="rhai-source-help-tags">
                      <span>安全沙箱</span>
                      <span>支持 Ctrl+Z</span>
                      <span>F12 随时停止</span>
                    </div>
                    <small>
                      高级脚本可使用变量、条件、循环和函数，但不能访问文件、网络、进程或系统命令。
                    </small>
                    <small>
                      静态检查无法保证脚本可以运行；输入、窗口和动态值仍需运行时确认。
                    </small>
                  </div>
                  {selectedSourceKind === "advanced" &&
                  sourceInspection?.source === sourceText ? (
                    <div role="status" className="rhai-source-help">
                      <strong>
                        静态检查：
                        {sourceInspection.report.unverifiedCalls.length}{" "}
                        处调用未验证
                      </strong>
                      {sourceInspection.report.unverifiedCalls.map(
                        (call, index) => (
                          <small
                            key={`${call.name}-${call.line}-${call.column}-${index}`}
                          >
                            {call.name}（第 {call.line ?? "?"} 行第{" "}
                            {call.column ?? "?"} 列）：{call.reason}
                          </small>
                        ),
                      )}
                      <small>
                        检查未发现确定错误不代表运行成功；请在安全条件下人工验证。
                      </small>
                    </div>
                  ) : null}
                  <AssetManager
                    assets={config.assets}
                    onError={(message) => setError(message)}
                    refresh={refresh}
                  />
                  {sourceError ? (
                    <div className="macro-source-error">{sourceError}</div>
                  ) : null}
                </section>
              ) : (
                <>
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
                            speed: Math.max(
                              0.05,
                              Number(event.target.value) || 1,
                            ),
                          })
                        }
                      />
                    </div>
                    <div className="form-section recording-options-section">
                      <label>录入内容</label>
                      <div className="recording-options">
                        <label className="recording-option">
                          <input
                            checked={selected.recordMouseMove !== false}
                            disabled={recording}
                            onChange={(event) =>
                              updateDraft({
                                recordMouseMove: event.target.checked,
                              })
                            }
                            type="checkbox"
                          />
                          <span>鼠标移动</span>
                        </label>
                        <label className="recording-option">
                          <input
                            checked={selected.recordMouseClicks !== false}
                            disabled={recording}
                            onChange={(event) =>
                              updateDraft({
                                recordMouseClicks: event.target.checked,
                              })
                            }
                            type="checkbox"
                          />
                          <span>鼠标点击与滚轮</span>
                        </label>
                      </div>
                      <div className="form-help">
                        录制快捷键：{RECORDING_SHORTCUT_LABEL}
                        。快捷键本身、AutoFlow窗口内操作和首次聚焦目标窗口的动作不会录入宏。
                      </div>
                    </div>
                    <div className="form-section macro-behavior-policy-section">
                      <label htmlFor="macro-behavior-policy-mode">
                        仿生行为策略
                      </label>
                      <select
                        id="macro-behavior-policy-mode"
                        value={hasCustomBehaviorPolicy ? "custom" : "global"}
                        onChange={(event) =>
                          setBehaviorPolicyMode(event.target.value === "custom")
                        }
                      >
                        <option value="global">使用全局默认策略</option>
                        <option value="custom">使用此宏的自定义策略</option>
                      </select>
                      {!hasCustomBehaviorPolicy ? (
                        <div className="form-help">
                          当前宏会跟随行为页中的全局 enabled、档案和强度设置。
                        </div>
                      ) : (
                        <div className="macro-behavior-policy-fields">
                          <label className="recording-option">
                            <input
                              checked={selectedBehaviorPolicy.enabled}
                              onChange={(event) =>
                                updateBehaviorPolicy({
                                  enabled: event.target.checked,
                                })
                              }
                              type="checkbox"
                            />
                            <span>启用仿生输入</span>
                          </label>
                          <label>
                            <span>Profile</span>
                            <select
                              value={selectedBehaviorPolicy.profileId ?? ""}
                              onChange={(event) =>
                                updateBehaviorPolicy({
                                  profileId: event.target.value || null,
                                })
                              }
                            >
                              <option value="">跟随全局当前档案</option>
                              {config.behaviorProfilesV2.map((profile) => (
                                <option key={profile.id} value={profile.id}>
                                  {profile.name} · {profile.coverage.quality}
                                </option>
                              ))}
                            </select>
                          </label>
                          <label>
                            <span>
                              Timing{" "}
                              {Math.round(
                                selectedBehaviorPolicy.timingStrength * 100,
                              )}
                              %
                            </span>
                            <input
                              max="1"
                              min="0"
                              step="0.05"
                              type="range"
                              value={selectedBehaviorPolicy.timingStrength}
                              onChange={(event) =>
                                updateBehaviorPolicy({
                                  timingStrength: Number(event.target.value),
                                })
                              }
                            />
                          </label>
                          <label>
                            <span>
                              Path{" "}
                              {Math.round(
                                selectedBehaviorPolicy.pointerPathStrength *
                                  100,
                              )}
                              %
                            </span>
                            <input
                              max="1"
                              min="0"
                              step="0.05"
                              type="range"
                              value={selectedBehaviorPolicy.pointerPathStrength}
                              onChange={(event) =>
                                updateBehaviorPolicy({
                                  pointerPathStrength: Number(
                                    event.target.value,
                                  ),
                                })
                              }
                            />
                          </label>
                          <label>
                            <span>
                              Pause{" "}
                              {Math.round(
                                selectedBehaviorPolicy.pauseStrength * 100,
                              )}
                              %
                            </span>
                            <input
                              max="1"
                              min="0"
                              step="0.05"
                              type="range"
                              value={selectedBehaviorPolicy.pauseStrength}
                              onChange={(event) =>
                                updateBehaviorPolicy({
                                  pauseStrength: Number(event.target.value),
                                })
                              }
                            />
                          </label>
                          <label>
                            <span>
                              Correction{" "}
                              {Math.round(
                                selectedBehaviorPolicy.correctionStrength * 100,
                              )}
                              %
                            </span>
                            <input
                              max="1"
                              min="0"
                              step="0.05"
                              type="range"
                              value={selectedBehaviorPolicy.correctionStrength}
                              onChange={(event) =>
                                updateBehaviorPolicy({
                                  correctionStrength: Number(
                                    event.target.value,
                                  ),
                                })
                              }
                            />
                          </label>
                          <label>
                            <span>
                              Speed{" "}
                              {selectedBehaviorPolicy.speedScale.toFixed(2)}×
                            </span>
                            <input
                              max="4"
                              min="0.1"
                              step="0.05"
                              type="range"
                              value={selectedBehaviorPolicy.speedScale}
                              onChange={(event) =>
                                updateBehaviorPolicy({
                                  speedScale: Number(event.target.value),
                                })
                              }
                            />
                          </label>
                          <label>
                            <span>Seed（可选）</span>
                            <input
                              min="0"
                              onChange={(event) => {
                                const value = event.target.value.trim();
                                updateBehaviorPolicy({
                                  seed:
                                    value === ""
                                      ? undefined
                                      : Math.max(
                                          0,
                                          Math.floor(Number(value) || 0),
                                        ),
                                });
                              }}
                              placeholder="每次播放随机"
                              type="number"
                              value={selectedBehaviorPolicy.seed ?? ""}
                            />
                          </label>
                          {selectedBehaviorPolicy.profileId &&
                          !selectedBehaviorProfile ? (
                            <div className="form-help">
                              绑定的 Profile 已不存在；保存前请选择现有 Profile
                              或改为跟随全局。
                            </div>
                          ) : selectedBehaviorProfile?.coverage.quality ===
                            "insufficient" ? (
                            <div className="form-help">
                              当前 Profile 样本不足，运行时会使用稳定
                              fallback；建议继续采集。
                            </div>
                          ) : null}
                        </div>
                      )}
                      <p className="runtime-pointer-warning" role="note">
                        <strong>鼠标运行提示：</strong>
                        宏运行期间手动抢动鼠标会与自动轨迹相互干扰，导致速度、曲率和修正等仿生移动特征不稳定。动作之间移动鼠标会成为下一步的新起点；轨迹执行过程中请避免操作鼠标。
                      </p>
                    </div>
                  </div>
                  {selected.program.kind === "rhai" ? (
                    <div className="advanced-script-config-note">
                      <div>
                        <strong>高级 Rhai 脚本已保留</strong>
                        <span>
                          此界面只调整触发方式、运行参数和仿生行为策略，不会把脚本转换或覆盖为图形步骤。
                        </span>
                      </div>
                      <button
                        className="button button-secondary"
                        onClick={openSourceEditor}
                        type="button"
                      >
                        返回编辑脚本
                      </button>
                    </div>
                  ) : (
                    <>
                      <div className="step-toolbar">
                        <div>
                          <strong>步骤列表</strong>
                          <span>{selectedSteps.length} 步</span>
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
                              addStep({
                                type: "key",
                                key: "Enter",
                                action: "down",
                              })
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
                            onClick={() =>
                              addStep({ type: "mouseMove", x: 0, y: 0 })
                            }
                            type="button"
                          >
                            ＋ 移动
                          </button>
                          <button
                            onClick={() =>
                              addStep({
                                type: "wheel",
                                deltaX: 0,
                                deltaY: -120,
                              })
                            }
                            type="button"
                          >
                            ＋ 滚轮
                          </button>
                        </div>
                      </div>
                      <div className="macro-steps">
                        {selectedSteps.length === 0 ? (
                          <div className="macro-empty-steps">
                            <span>01</span>
                            <div>
                              <strong>还没有步骤</strong>
                              <small>点击开始录制，或从右上角手动添加。</small>
                            </div>
                          </div>
                        ) : (
                          selectedSteps.map((step, index) => (
                            <div
                              className={`macro-step-row ${
                                step.type === "delay" ? "is-delay-step" : ""
                              } ${draggingStepIndex === index ? "is-dragging" : ""} ${
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
                              <span
                                className={`step-type step-type-${step.type}`}
                              >
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
                                      <div className="delay-range-input">
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
                                          placeholder="固定时间"
                                        />
                                        <span className="delay-range-unit">
                                          ms
                                        </span>
                                      </div>
                                      <span className="delay-range-separator">
                                        至
                                      </span>
                                      <div className="delay-range-input">
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
                                                  : Math.max(
                                                      0,
                                                      Number(value) || 0,
                                                    ),
                                            });
                                          }}
                                          placeholder="最大时间"
                                        />
                                        <span className="delay-range-unit">
                                          ms
                                        </span>
                                      </div>
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
                                          action: event.target.value as
                                            "down" | "up",
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
                                          action: event.target.value as
                                            "down" | "up",
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
                                          deltaX:
                                            Number(event.target.value) || 0,
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
                                          deltaY:
                                            Number(event.target.value) || 0,
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
                                  disabled={index === selectedSteps.length - 1}
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
                    </>
                  )}
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
                          disabled={Boolean(selected.importError)}
                          onClick={() => void runSelected()}
                          type="button"
                        >
                          测试播放
                        </button>
                      )}
                      <button
                        className={`button ${selected.enabled ? "button-soft-danger" : "button-primary"}`}
                        disabled={Boolean(selected.importError)}
                        onClick={() => toggleMacro(selected)}
                        type="button"
                      >
                        {selected.enabled ? "停用宏" : "启用宏"}
                      </button>
                    </div>
                  </div>
                </>
              )}
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
