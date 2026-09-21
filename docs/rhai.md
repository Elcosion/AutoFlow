# Rhai automation

AutoFlow currently writes configuration schema 6. The root `config.json` keeps
the macro index (`macros: []` plus `macroFiles`); each indexed `MacroRule` is
stored as one JSON file under `<AppData>/data/scripts/`.

The following root JSON is an excerpt of macro-storage fields, not a complete
`AppConfig` file:

```json
{
  "schemaVersion": 6,
  "globalEnabled": true,
  "emergencyStop": "F12",
  "hotkeys": [],
  "textExpansions": [],
  "macros": [],
  "macroFiles": [
    {
      "id": "macro-example",
      "name": "Daily action",
      "fileName": "Daily action.json"
    }
  ]
}
```

The indexed file `data/scripts/Daily action.json` contains the complete
`MacroRule`. This safe Rhai example is disabled and has no trigger keys:

```json
{
  "id": "macro-example",
  "name": "Daily action",
  "enabled": false,
  "triggerKeys": [],
  "mode": "once",
  "repeatCount": 1,
  "speed": 1,
  "recordMouseMove": true,
  "recordMouseClicks": true,
  "program": {
    "kind": "rhai",
    "source": "wait_ms(100);",
    "apiVersion": 1
  }
}
```

For a graphical macro, replace `program` with
`{ "kind": "macro", "steps": [{ "type": "delay", "durationMs": 300 }] }`.
`apiVersion` is currently only `1`, and a Rhai program's `source` must be
non-empty. A missing schema version is read as v1; schema 1, schema 2 and
rules with only the old top-level `steps` field remain readable and migrate to
the nested program form before validation and persistence. The current writer
always emits schema 6; `schemaVersion > 6` is rejected.

## API v1

以下是常用/示例 API，不是穷举清单；重载、参数约束和实际可用函数以
`src-tauri/src/rhai_runtime.rs` 的 `register_api` 为准：

```text
wait_ms(ms)
wait_random_ms(min, max)
key_down(key)
key_up(key)
press(key)
move_to(x, y)
mouse_down(button, x, y)
mouse_up(button, x, y)
click(button)
click(button, x, y)
scroll(delta_x, delta_y)
type_text(text)
is_cancelled()
stop_with_message(message)
stop_with_message(title, message)
stop_with_message(message, options)
active_window_title() -> String
window_exists(title_query) -> bool
window_rect(title_query) -> Map { found, x, y, width, height }
wait_window(title_query, timeout_ms, poll_ms) -> bool
pixel_matches(x, y, red, green, blue, tolerance) -> bool
wait_pixel(x, y, red, green, blue, tolerance, timeout_ms, poll_ms) -> bool
find_image(file_name, region_x, region_y, region_width, region_height, threshold) -> Map
wait_image(file_name, region_x, region_y, region_width, region_height, threshold, timeout_ms, poll_ms) -> Map
find_image(file_name, region_x, region_y, region_width, region_height, threshold, options) -> Map
wait_image(file_name, region_x, region_y, region_width, region_height, threshold, timeout_ms, poll_ms, options) -> Map
```

`stop_with_message` 在安全释放输入后成功结束当前脚本。原有单参数和双字符串
调用都使用后台通知；双字符串调用的第二个参数始终是消息，即使内容恰好是
`"foreground"`。可用 Map 明确选择展示模式：

```rhai
stop_with_message("任务已完成");
stop_with_message("完成", "任务已完成");
stop_with_message("任务已完成", #{ mode: "background" });
stop_with_message("请检查运行结果", #{ mode: "foreground" });
```

`options` 可为空（仍是后台），且只接受字符串字段 `mode`，值只能是
`"background"` 或 `"foreground"`。前台模式是尽力而为的窗口展示请求；若
系统拒绝显示、还原或聚焦，会降级为普通后台展示，通知保持待手动确认，不会
恢复或重新启动脚本。

运行时还注册了取消、仿生输入、窗口、像素和图像诊断等函数；文档示例不构成
独立的兼容性契约。

The optional image-search map uses `mode: "auto"`, `"exact"`, or `"fast"`,
`prefer_last: true|false`, `max_candidates` from 1 to 32, and scale values as
倍率（例如 `1.25` 表示 125%，不是百分数 `125`）。`scale_min` and
`scale_max` are finite values from `0.5` to `2.0`, with `scale_min <=
scale_max`; an optional positive `scale_step` is also bounded by the maximum
candidate count. When omitted, `auto` and `fast` search the common Windows
DPI candidates `0.67, 0.80, 0.83, 1.00, 1.20, 1.25, 1.50, 1.75, 2.00`
(within the selected range). The default range is `0.67`–`2.00`; candidate
generation remains bounded.

`auto` checks the previous position, successful scale and actual dimensions,
performs bounded multi-scale coarse matching, and refines candidates with the
corresponding original-resolution scaled template. Its final verification
combines full-template NCC with a 3x3 robust spatial score: low-information
tiles are ignored, at most the worst tile is discarded, and spatially
separated passing tiles are required. The combined score is
`0.15 * full + 0.70 * robust + 0.15 * (passed_tiles / valid_tiles)`; the
robust gate also requires at least three passing tiles spanning at least two
rows and columns. If ordinary candidates are still
uncertain, `auto` may run bounded anchor recovery using at most two stable
tiles, a few scales and a hard candidate cap; every recovery candidate is then
checked again with the full spatial/robust verifier. `fast` performs the same
multi-scale coarse pass and local refinement but never runs anchor or
full-area recovery.
`exact` deliberately keeps the fixed 1.0 full-resolution NCC baseline and does
not perform cross-scale matching. Invalid values return a stable Chinese
validation error. The default calls keep their original signatures and default
to `auto`.

Image result maps retain `found`, coordinates, dimensions and `score`, and add
`total_ms`, `capture_ms`, `prepare_ms`, `coarse_ms`, `refine_ms`,
`fallback_ms`, `candidate_count`, `previous_hit_used`, `fallback_used`, and
`matcher_mode`, plus `matched_scale`, `scale_candidates`, `scale_search_ms`,
`matched_width`, `matched_height`, `robust_verify_used`, `robust_verify_ms`,
`robust_score`, `valid_tile_count`, `discarded_tile_count`, `alpha_mask_used`,
`anchor_recovery_used`, `anchor_candidate_count`, `preferred_scale_hit`,
`single_match_ms`, and `wait_total_ms`. On a miss, `matched_scale` and
`robust_score` are the Rhai unit value `()` and the matched dimensions are `0`;
a miss never fabricates a scale or robust score. On a hit, width and height
are the actual scaled template dimensions. `wait_total_ms` measures the
complete wait operation; `single_match_ms` measures one search attempt. These
fields are diagnostic only and do not change old scripts.

Wait values are non-negative integers, coordinates and wheel values are i32,
buttons are `left`, `right`, `middle`, `x1` or `x2`, and text/key arguments are
strings. The frontend parser in `src/lib/macroSource.ts` handles compatible
source with only top-level calls and converts it to `MacroStep[]`; `press` and
`click` become balanced down/up pairs. Strings are JSON-escaped so quotes,
backslashes, newlines and Unicode survive a round trip.

Window queries use physical screen pixels. Matching is case-insensitive and
uses title substrings. `window_rect` returns the extended outer frame in screen
coordinates, including negative coordinates on secondary monitors. Regions
crossing monitor boundaries are captured per monitor and composited into one
physical-coordinate ROI. When a requested region extends beyond the available
desktop, the visible intersection is captured and the uncovered portion is
filled with black pixels. The capture backend uses Windows Graphics Capture first and
falls back to DXGI Desktop Duplication for monitor regions after device/API
failure.

Images are stored in `<AppData>/data/images/`. Files copied directly into that
folder are discovered when the configuration is refreshed. Rhai receives the
complete file name, including the `.png`, `.jpg`, or `.jpeg` extension; omitting
the extension is not supported. It cannot open arbitrary paths. PNG/JPEG bytes are validated,
the canonical path is checked against the managed directory, templates are
decoded lazily and cached as a prepared grayscale template plus bounded,
on-demand scaled grayscale templates. If a PNG contains any non-opaque alpha,
alpha-zero pixels are ignored during final verification and partially
transparent pixels are weighted by alpha; opaque RGB/RGBA images keep the
existing behavior. The grayscale template and alpha channel are resized to
the same actual dimensions. This is an optional interpretation of the PNG;
no separate mask file is required and users do not need to author one.
Scaled entries are keyed by resource, file fingerprint, requested scale and
actual scaled dimensions. Cache entries
invalidate when the resource file name, metadata or SHA-256 changes, and a
catalog refresh or deletion clears the corresponding prepared/scaled cache;
the cache is bounded to 64 base entries and 64 MiB, with at most 16 scaled
templates and 8 MiB per prepared template. Deletion requires confirmation when a script still
mentions the asset.

The recommended polling interval is 150–250ms. Polls are interruptible in
25ms slices, count toward the shared 100,000-operation limit, and have a
120-second maximum timeout. Captures are rate-limited, no capture loop runs
while idle, and only the latest frame is retained by each active capture
session. Run `cargo run --release --bin vision_bench` for the deterministic,
pure-memory hit/miss/repeat-hit/moved-hit benchmark. Add
`--capture-diagnostic --duration-seconds 600` only when a real capture trend is
needed; benchmark timings are not CI millisecond assertions.

## Safety limits

Rhai runs in a dedicated engine with no file, network, process, dynamic module
or system-command capability. AutoFlow sets a 100,000-operation limit, a
32-level call-depth limit, 64-level expression/scope limits and a 1 MB string
limit. F12 and the second toggle trigger set the cancellation token. The
execution context always releases every key and mouse button it pressed after
normal completion, cancellation, an error, or thread exit; only one macro or
script may run at a time.

Advanced source is never silently converted to graphical steps. The UI asks
for confirmation once when a compatible macro source contains control flow;
after confirmation it remains a Rhai program and the graphical tab is
disabled. A script can return to the graphical editor only after its source is
edited back to the compatible top-level API grammar and successfully parsed.
