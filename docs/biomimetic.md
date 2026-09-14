# V2 仿生行为训练

第一阶段只覆盖“鼠标移动 → 点击”。训练数据和运行模型分开保存：

- `behavior-sessions-v2/` 保存 `BehaviorSessionV2` 原始事件、时间戳和采集元数据。
- `behavior-profiles-v2/` 保存不含 `rawEvents` 的 `BehaviorProfileV2`。
- `config.json` 只保存两个目录的索引、当前默认档案和行为策略。

## 数据流

```text
WH_MOUSE_LL 原始事件
  -> Segmenter：按重复坐标、静止间隔和下一个动作切分
  -> Extractor：计算时间、路径效率、峰值速度、曲率、越过和修正
  -> PointerModel：按距离、方向、点击上下文和目标宽度分桶
  -> Sampler：从经验分布采样，样本不足时显式 fallback
  -> Runtime：生成可取消的 Windows 输入计划
```

切分阈值集中在 `SegmentationConfig`。乱序时间戳会拒绝，重复坐标不会参与速度计算，极短轨迹会进入丢弃原因统计。采样点不足不会被标记为“已训练”。

## 运行策略

`BehaviorPolicy` 包含：

- `enabled`、`profileId`
- `timingStrength`、`pointerPathStrength`、`pauseStrength`
- `correctionStrength`、`speedScale`、可选 `seed`

策略优先绑定到具体宏；全局策略只是默认值。所有强度都在 `0..1`。强度为零时，运行时直接走原始 `move_to`/`click` 路径，不分段、不改速、不改轨迹。

固定 `seed` 会复现同一个轨迹。每次生成结果都包含 `bucket`、`sampledFeatures` 和可选 `fallbackReason`，因此运行时默认值不会伪装成训练结果。

F12 与现有取消标志在每个轨迹点和点击停顿之间检查；坐标、点数和延迟均有安全上限。输入执行仍通过现有 Windows `SendInput` 边界完成。

## Rhai API

### Target width semantics

`targetWidth` / `target_width` is a runtime hint supplied by a real caller such as a
vision result or known control rectangle. It affects Fitts-like timing and the bounded
correction/overshoot envelope, but it is never invented and written back as a training
measurement. The current Windows hook records coordinates and clicks but not target
geometry, so training buckets normally use `targetWidth: unknown`.

### Retention and diagnostics

Raw events are always kept in bounded memory during recording. The retention switch
only decides whether `SessionV2` is written to `behavior-sessions-v2`; `ProfileV2` is
still written with `sourceRetention: ephemeral` when persistence is off. Each runtime
session owns one seed and monotonic `actionIndex`; fixed seeds replay a whole playback
while repeated actions still receive different derived action seeds. Runtime results
and Windows debug logs expose bucket, trained, fallback level/reason, movement timing,
click dwell, and relevant sampled features. Per-action diagnostics use debug-level
logging so normal operation does not emit a high-frequency info log; enable debug
logging with `AUTOFLOW_BEHAVIOR_DEBUG=1` when manually validating a macro.

V2 API 是原生注册函数，不是脚本别名：

```rhai
bio_move_to(820, 430);
bio_move_to(820, 430, #{ target_width: 80, intent: "click" });
bio_click("left", 820, 430);
bio_type_text("示例文本", #{ mode: "normal" });
```

这些函数使用当前宏绑定的 `BehaviorPolicy`，不读取 UI 当前选中的档案。原有 `move_to`、`click` 等普通 API 保持原始输入语义；是否使用 V2 行为由 `bio_*` API 或宏绑定策略明确决定。

## UI 质量指标

行为页面展示有效轨迹数、移动后点击关联数、各 bucket coverage、丢弃原因和模型质量（不足/可用/良好），并明确标记真实训练数据与运行时 fallback。平均鼠标速度不是主要质量指标。

## 下一阶段

- 键盘按键类别、hold/flight/overlap 和 typing burst
- 滚轮 burst
- 拖拽模型
- 多档案条件模型合并
- 训练样本与生成样本可视化对比
