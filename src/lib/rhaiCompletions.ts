export type RhaiCompletionSpec = {
  signature: string;
  insertText: string;
  selectionText?: string;
};

export type RhaiReferenceSnippet = {
  id: string;
  category:
    | "基础语句"
    | "脚本控制"
    | "等待控制"
    | "键盘与文本"
    | "鼠标操作"
    | "仿生操作"
    | "窗口操作"
    | "图像与像素"
    | "组合示例";
  name: string;
  description: string;
  code: string;
  apiName?: string;
};

const call = (
  signature: string,
  lines: string[],
  selectionText?: string,
): RhaiCompletionSpec => ({
  signature,
  insertText:
    lines.length === 0
      ? `${signature.slice(0, signature.indexOf("(") + 1)});`
      : `${signature.slice(0, signature.indexOf("("))}(\n  ${lines
          .map((line, index) => {
            if (index === lines.length - 1) return line;
            const commentIndex = line.indexOf(" //");
            return commentIndex >= 0
              ? `${line.slice(0, commentIndex)},${line.slice(commentIndex)}`
              : `${line},`;
          })
          .join("\n  ")}\n);`,
  selectionText,
});

export const RHAI_API_COMPLETIONS = {
  stop_with_message: call(
    "stop_with_message(title, message)",
    ['"AutoFlow" // title：弹窗标题', '"任务已完成" // message：提示内容'],
    "AutoFlow",
  ),
  wait_ms: call(
    "wait_ms(durationMs)",
    ["300 // durationMs：等待毫秒数"],
    "300",
  ),
  wait_random_ms: call(
    "wait_random_ms(minMs, maxMs)",
    ["300 // minMs：最短等待毫秒数", "800 // maxMs：最长等待毫秒数"],
    "300",
  ),
  key_down: call("key_down(key)", ['"Ctrl" // key：按键名称'], "Ctrl"),
  key_up: call("key_up(key)", ['"Ctrl" // key：按键名称'], "Ctrl"),
  press: call("press(key)", ['"Enter" // key：按键名称'], "Enter"),
  move_to: call(
    "move_to(x, y)",
    ["820 // x：屏幕横坐标", "430 // y：屏幕纵坐标"],
    "820",
  ),
  mouse_down: call(
    "mouse_down(button, x, y)",
    [
      '"left" // button：left、right、middle、x1 或 x2',
      "820 // x：屏幕横坐标",
      "430 // y：屏幕纵坐标",
    ],
    "left",
  ),
  mouse_up: call(
    "mouse_up(button, x, y)",
    [
      '"left" // button：left、right、middle、x1 或 x2',
      "820 // x：屏幕横坐标",
      "430 // y：屏幕纵坐标",
    ],
    "left",
  ),
  click: call(
    "click(button, x, y)",
    [
      '"left" // button：left、right、middle、x1 或 x2',
      "820 // x：屏幕横坐标",
      "430 // y：屏幕纵坐标",
    ],
    "left",
  ),
  scroll: call(
    "scroll(deltaX, deltaY)",
    ["0 // deltaX：水平滚动量", "-120 // deltaY：垂直滚动量"],
    "0",
  ),
  type_text: call(
    "type_text(text)",
    ['"AutoFlow 示例文本" // text：需要输入的文本'],
    "AutoFlow 示例文本",
  ),
  bio_press: call("bio_press(key)", ['"Enter" // key：按键名称'], "Enter"),
  bio_move_to: call(
    "bio_move_to(x, y)",
    ["820 // x：目标横坐标", "430 // y：目标纵坐标"],
    "820",
  ),
  bio_click: call(
    "bio_click(button, x, y)",
    [
      '"left" // button：鼠标按钮',
      "820 // x：目标横坐标",
      "430 // y：目标纵坐标",
    ],
    "left",
  ),
  bio_type_text: call(
    "bio_type_text(text)",
    ['"AutoFlow 示例文本" // text：需要仿生输入的文本'],
    "AutoFlow 示例文本",
  ),
  bio_scroll: call(
    "bio_scroll(deltaX, deltaY)",
    ["0 // deltaX：水平滚动量", "-120 // deltaY：垂直滚动量"],
    "0",
  ),
  is_cancelled: {
    signature: "is_cancelled() -> bool",
    insertText: "is_cancelled()",
  },
  active_window_title: {
    signature: "active_window_title() -> String",
    insertText: "active_window_title()",
  },
  window_exists: call(
    "window_exists(title)",
    ['"记事本" // title：窗口标题关键字'],
    "记事本",
  ),
  window_rect: call(
    "window_rect(title) -> Map",
    ['"记事本" // title：窗口标题关键字'],
    "记事本",
  ),
  wait_window: call(
    "wait_window(title, timeoutMs, pollMs)",
    [
      '"记事本" // title：窗口标题关键字',
      "5000 // timeoutMs：最长等待毫秒数",
      "200 // pollMs：轮询间隔毫秒数",
    ],
    "记事本",
  ),
  pixel_matches: call(
    "pixel_matches(x, y, r, g, b, tolerance)",
    [
      "100 // x：屏幕横坐标",
      "200 // y：屏幕纵坐标",
      "32 // r：目标红色通道",
      "64 // g：目标绿色通道",
      "128 // b：目标蓝色通道",
      "8 // tolerance：颜色容差",
    ],
    "100",
  ),
  wait_pixel: call(
    "wait_pixel(x, y, r, g, b, tolerance, timeoutMs, pollMs)",
    [
      "100 // x：屏幕横坐标",
      "200 // y：屏幕纵坐标",
      "32 // r：目标红色通道",
      "64 // g：目标绿色通道",
      "128 // b：目标蓝色通道",
      "8 // tolerance：颜色容差",
      "5000 // timeoutMs：最长等待毫秒数",
      "200 // pollMs：轮询间隔毫秒数",
    ],
    "100",
  ),
  find_image: call(
    "find_image(fileName, x, y, width, height, threshold)",
    [
      '"confirm_button.png" // fileName：完整文件名，必须包含后缀',
      "0 // x：查找区域横坐标",
      "0 // y：查找区域纵坐标",
      "1920 // width：查找区域宽度",
      "1080 // height：查找区域高度",
      "0.90 // threshold：匹配阈值 0–1",
    ],
    "confirm_button.png",
  ),
  wait_image: call(
    "wait_image(fileName, x, y, width, height, threshold, timeoutMs, pollMs)",
    [
      '"confirm_button.png" // fileName：完整文件名，必须包含后缀',
      "0 // x：查找区域横坐标",
      "0 // y：查找区域纵坐标",
      "1920 // width：查找区域宽度",
      "1080 // height：查找区域高度",
      "0.90 // threshold：匹配阈值 0–1",
      "5000 // timeoutMs：最长等待毫秒数",
      "200 // pollMs：轮询间隔毫秒数",
    ],
    "confirm_button.png",
  ),
} satisfies Record<string, RhaiCompletionSpec>;

type RhaiApiName = keyof typeof RHAI_API_COMPLETIONS;

const RHAI_API_REFERENCE_META = {
  stop_with_message: {
    category: "脚本控制",
    description: "立即停止当前脚本，并以后台模式显示自定义标题和提示内容。",
  },
  is_cancelled: {
    category: "脚本控制",
    description: "检查当前脚本是否已经收到停止请求。",
    code: "let cancelled = is_cancelled(); // cancelled：是否已请求停止",
  },
  wait_ms: {
    category: "等待控制",
    description: "让脚本等待固定的毫秒数。",
  },
  wait_random_ms: {
    category: "等待控制",
    description: "在最短与最长时间之间随机等待。",
  },
  key_down: {
    category: "键盘与文本",
    description: "按下指定按键，并保持按下状态。",
  },
  key_up: {
    category: "键盘与文本",
    description: "释放此前按下的指定按键。",
  },
  press: {
    category: "键盘与文本",
    description: "完整按下并释放一次指定按键。",
  },
  type_text: {
    category: "键盘与文本",
    description: "按普通输入方式键入指定文本。",
  },
  move_to: {
    category: "鼠标操作",
    description: "将鼠标直接移动到指定屏幕坐标。",
  },
  mouse_down: {
    category: "鼠标操作",
    description: "在指定坐标按下鼠标按钮并保持。",
  },
  mouse_up: {
    category: "鼠标操作",
    description: "在指定坐标释放鼠标按钮。",
  },
  click: {
    category: "鼠标操作",
    description: "在指定屏幕坐标执行一次鼠标点击。",
  },
  scroll: {
    category: "鼠标操作",
    description: "按给定的水平和垂直增量滚动。",
  },
  bio_press: {
    category: "仿生操作",
    description: "使用仿生节奏按下并释放指定按键。",
  },
  bio_move_to: {
    category: "仿生操作",
    description: "使用当前仿生 Profile 移动到指定坐标。",
  },
  bio_click: {
    category: "仿生操作",
    description: "使用仿生移动和点击特征点击指定坐标。",
  },
  bio_type_text: {
    category: "仿生操作",
    description: "使用仿生输入节奏键入指定文本。",
  },
  bio_scroll: {
    category: "仿生操作",
    description: "使用仿生滚动特征执行滚轮操作。",
  },
  active_window_title: {
    category: "窗口操作",
    description: "读取当前活动窗口的标题。",
    code: "let title = active_window_title(); // title：当前活动窗口标题",
  },
  window_exists: {
    category: "窗口操作",
    description: "判断是否存在标题包含指定文字的窗口。",
    code: `let exists = window_exists(
  "记事本" // title：窗口标题关键字
); // exists：是否找到窗口`,
  },
  window_rect: {
    category: "窗口操作",
    description: "读取匹配窗口的位置、尺寸和查找状态。",
    code: `let window = window_rect(
  "记事本" // title：窗口标题关键字
); // window：包含 found、x、y、width、height`,
  },
  wait_window: {
    category: "窗口操作",
    description: "等待指定窗口出现，直到成功或超时。",
  },
  pixel_matches: {
    category: "图像与像素",
    description: "判断指定坐标的像素是否接近目标颜色。",
    code: `let matched = pixel_matches(
  100, // x：屏幕横坐标
  200, // y：屏幕纵坐标
  32, // r：目标红色通道
  64, // g：目标绿色通道
  128, // b：目标蓝色通道
  8 // tolerance：颜色容差
); // matched：颜色是否匹配`,
  },
  wait_pixel: {
    category: "图像与像素",
    description: "轮询指定像素，直到颜色匹配或等待超时。",
  },
  find_image: {
    category: "图像与像素",
    description: "在指定屏幕区域中查找图像资源。",
    code: `let result = find_image(
  "confirm_button.png", // fileName：完整文件名，必须包含后缀
  0, // x：查找区域横坐标
  0, // y：查找区域纵坐标
  1920, // width：查找区域宽度
  1080, // height：查找区域高度
  0.90 // threshold：匹配阈值 0–1
); // result：包含 found、center_x、center_y 等结果`,
  },
  wait_image: {
    category: "图像与像素",
    description: "等待图像资源在指定区域出现，直到成功或超时。",
  },
} satisfies Record<
  RhaiApiName,
  {
    category: RhaiReferenceSnippet["category"];
    description: string;
    code?: string;
  }
>;

const RHAI_PRIMARY_API_REFERENCE_SNIPPETS: RhaiReferenceSnippet[] = (
  Object.keys(RHAI_API_COMPLETIONS) as RhaiApiName[]
).map((apiName) => {
  const completion = RHAI_API_COMPLETIONS[apiName];
  const metadata = RHAI_API_REFERENCE_META[apiName];
  const defaultCode = completion.insertText.endsWith(";")
    ? completion.insertText
    : `${completion.insertText};`;
  return {
    id: `api-${apiName}`,
    apiName,
    category: metadata.category,
    name: completion.signature,
    description: metadata.description,
    code: "code" in metadata ? metadata.code : defaultCode,
  };
});

const RHAI_API_OVERLOAD_REFERENCE_SNIPPETS: RhaiReferenceSnippet[] = [
  {
    id: "api-stop_with_message-message-only",
    apiName: "stop_with_message",
    category: "脚本控制",
    name: "stop_with_message(message)",
    description: "使用默认标题停止脚本并显示提示内容。",
    code: `stop_with_message(
  "任务已完成" // message：提示内容
);`,
  },
  {
    id: "api-stop_with_message-background",
    apiName: "stop_with_message",
    category: "脚本控制",
    name: "stop_with_message(message, options)",
    description: "停止脚本并明确使用后台通知；空 Map 也默认为后台。",
    code: `stop_with_message(
  "任务已完成", // message：提示内容
  #{ mode: "background" } // mode：后台通知
);`,
  },
  {
    id: "api-stop_with_message-foreground",
    apiName: "stop_with_message",
    category: "脚本控制",
    name: "stop_with_message(message, options)",
    description: "停止脚本并请求将主窗口带到前台显示通知。",
    code: `stop_with_message(
  "需要确认结果", // message：提示内容
  #{ mode: "foreground" } // mode：请求前台通知
);`,
  },
  {
    id: "api-click-current-position",
    apiName: "click",
    category: "鼠标操作",
    name: "click(button)",
    description: "在鼠标当前位置点击指定按钮。",
    code: `click(
  "left" // button：left、right、middle、x1 或 x2
);`,
  },
  {
    id: "api-click-left-position",
    apiName: "click",
    category: "鼠标操作",
    name: "click(x, y)",
    description: "在指定坐标执行一次左键点击。",
    code: `click(
  820, // x：屏幕横坐标
  430 // y：屏幕纵坐标
);`,
  },
  {
    id: "api-bio_move_to-options",
    apiName: "bio_move_to",
    category: "仿生操作",
    name: "bio_move_to(x, y, options)",
    description: "带目标宽度和后续点击意图执行仿生移动。",
    code: `let options = #{
  target_width: 80, // 目标区域宽度
  intent: "click" // move：仅移动；click：随后点击
};
bio_move_to(820, 430, options);`,
  },
  {
    id: "api-bio_click-options",
    apiName: "bio_click",
    category: "仿生操作",
    name: "bio_click(button, x, y, options)",
    description: "指定目标区域宽度后执行仿生移动和点击。",
    code: `let options = #{
  target_width: 80 // 目标区域宽度
};
bio_click("left", 820, 430, options);`,
  },
  {
    id: "api-bio_type_text-options",
    apiName: "bio_type_text",
    category: "仿生操作",
    name: "bio_type_text(text, options)",
    description: "使用指定输入模式执行仿生文本输入。",
    code: `let options = #{
  mode: "normal" // 当前支持 normal
};
bio_type_text("AutoFlow 示例文本", options);`,
  },
];

RHAI_API_OVERLOAD_REFERENCE_SNIPPETS.push(
  {
    id: "api-find_image-options",
    apiName: "find_image",
    category: "图像与像素",
    name: "find_image(fileName, x, y, width, height, threshold, options)",
    description:
      "使用多尺度 auto、exact 或 fast 模式查找图像；scale 使用倍率而不是百分数。",
    code: `let result = find_image(
  "confirm_button.png",
  0,
  0,
  1920,
  1080,
  0.90,
  #{
    mode: "auto", // auto / exact / fast
    scale_min: 0.67, // 最小倍率（不是百分数）
    scale_max: 2.00, // 最大倍率（不是百分数）
    // scale_step: 0.10, // 可选；候选数量最多 16 个
    prefer_last: true,
    max_candidates: 8
  }
);
// result diagnostics include robust_score, alpha_mask_used, anchor_recovery_used,
// preferred_scale_hit, single_match_ms and wait_total_ms. A miss uses ().`,
  },
  {
    id: "api-wait_image-options",
    apiName: "wait_image",
    category: "图像与像素",
    name: "wait_image(fileName, x, y, width, height, threshold, timeoutMs, pollMs, options)",
    description:
      "轮询查找图像的多尺度高级重载，保留 timeout、poll_ms 和 F12 取消语义。",
    code: `let result = wait_image(
  "confirm_button.png",
  0,
  0,
  1920,
  1080,
  0.90,
  10000,
  200,
  #{
    mode: "auto", // auto / exact / fast
    scale_min: 0.67, // 最小倍率（不是百分数）
    scale_max: 2.00, // 最大倍率（不是百分数）
    prefer_last: true,
    max_candidates: 8
  }
);
// wait_total_ms is the complete wait duration; single_match_ms is one attempt.`,
  },
);

export const RHAI_API_REFERENCE_SNIPPETS: RhaiReferenceSnippet[] = [
  ...RHAI_PRIMARY_API_REFERENCE_SNIPPETS,
  ...RHAI_API_OVERLOAD_REFERENCE_SNIPPETS,
];

export const RHAI_SYNTAX_COMPLETIONS: Record<string, RhaiCompletionSpec> = {
  let: {
    signature: "let variable = value;",
    insertText: "let variable = value; // variable：变量名；value：初始值",
    selectionText: "variable",
  },
  if: {
    signature: "if condition { … }",
    insertText: "if condition {\n  // 条件成立时执行\n}",
    selectionText: "condition",
  },
  if_else: {
    signature: "if condition { … } else { … }",
    insertText:
      "if condition {\n  // 条件成立时执行\n} else {\n  // 条件不成立时执行\n}",
    selectionText: "condition",
  },
  for: {
    signature: "for item in 0..10 { … }",
    insertText:
      "for item in 0..10 {\n  // item：当前循环值；0..10：循环范围\n}",
    selectionText: "item",
  },
  while: {
    signature: "while condition { … }",
    insertText: "while condition {\n  // condition：继续循环的条件\n}",
    selectionText: "condition",
  },
  loop: {
    signature: "loop { … }",
    insertText: "loop {\n  // 循环体；请在满足条件时 break\n}",
  },
  fn: {
    signature: "fn function_name(argument) { … }",
    insertText:
      "fn function_name(argument) {\n  // argument：函数参数\n  return argument;\n}",
    selectionText: "function_name",
  },
  return: {
    signature: "return value;",
    insertText: "return value; // value：返回值",
    selectionText: "value",
  },
  break: {
    signature: "break;",
    insertText: "break; // 立即退出当前循环",
  },
  continue: {
    signature: "continue;",
    insertText: "continue; // 跳过本次循环的剩余语句",
  },
};

export const RHAI_API_NAMES = Object.keys(RHAI_API_COMPLETIONS);
export const RHAI_API_SIGNATURES = Object.fromEntries(
  Object.entries(RHAI_API_COMPLETIONS).map(([name, completion]) => [
    name,
    completion.signature,
  ]),
);
export const RHAI_COMPLETION_NAMES = [
  ...Object.keys(RHAI_SYNTAX_COMPLETIONS),
  ...RHAI_API_NAMES,
];

export type RhaiCompletionEdit = {
  value: string;
  selectionStart: number;
  selectionEnd: number;
};

function indentMultiline(text: string, indentation: string) {
  return text.replaceAll("\n", `\n${indentation}`);
}

export function completeRhaiSource(
  value: string,
  selectionStart: number,
  selectionEnd: number,
  name: string,
): RhaiCompletionEdit | null {
  const completion =
    RHAI_SYNTAX_COMPLETIONS[name] ??
    RHAI_API_COMPLETIONS[name as keyof typeof RHAI_API_COMPLETIONS];
  if (!completion) return null;
  const partial =
    value.slice(0, selectionStart).match(/[A-Za-z_][A-Za-z0-9_]*$/)?.[0] ?? "";
  const replacementStart = selectionStart - partial.length;
  const lineStart = value.lastIndexOf("\n", replacementStart - 1) + 1;
  const indentation =
    value.slice(lineStart, replacementStart).match(/^\s*/)?.[0] ?? "";
  const inserted = indentMultiline(completion.insertText, indentation);
  const nextValue = `${value.slice(0, replacementStart)}${inserted}${value.slice(selectionEnd)}`;
  const selectionOffset = completion.selectionText
    ? inserted.indexOf(completion.selectionText)
    : -1;
  const nextSelectionStart =
    selectionOffset >= 0
      ? replacementStart + selectionOffset
      : replacementStart + inserted.length;
  return {
    value: nextValue,
    selectionStart: nextSelectionStart,
    selectionEnd:
      selectionOffset >= 0
        ? nextSelectionStart + completion.selectionText!.length
        : nextSelectionStart,
  };
}

export function insertRhaiReferenceSnippet(
  value: string,
  selectionStart: number,
  selectionEnd: number,
  code: string,
): RhaiCompletionEdit {
  const before = value.slice(0, selectionStart);
  const after = value.slice(selectionEnd);
  const prefix = before.length > 0 && !before.endsWith("\n") ? "\n\n" : "";
  const suffix = after.length > 0 && !after.startsWith("\n") ? "\n\n" : "";
  const inserted = `${prefix}${code.trim()}${suffix}`;
  const cursor = selectionStart + inserted.length - suffix.length;
  return {
    value: `${before}${inserted}${after}`,
    selectionStart: cursor,
    selectionEnd: cursor,
  };
}
