# Rhai automation

AutoFlow configuration schema v2 keeps the common macro metadata and stores
the executable content in one `program` value:

```json
{
  "schemaVersion": 2,
  "macros": [{
    "id": "macro-example",
    "name": "Daily action",
    "enabled": false,
    "triggerKeys": ["Ctrl", "F8"],
    "mode": "once",
    "repeatCount": 1,
    "speed": 1,
    "recordMouseMove": true,
    "recordMouseClicks": true,
    "program": {
      "kind": "macro",
      "steps": [{ "type": "delay", "durationMs": 300 }]
    }
  }]
}
```

`program.kind = "macro"` is produced by recording and edited graphically.
`program.kind = "rhai"` is `{ "source": "...", "apiVersion": 1 }` and is
used for scripts containing variables, conditions, loops or user functions.
Schema v1 and rules containing only the old `steps` field are migrated into
the macro variant before validation and persistence.

## API v1

The allowed AutoFlow functions are:

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
active_window_title() -> String
window_exists(title_query) -> bool
window_rect(title_query) -> Map { found, x, y, width, height }
wait_window(title_query, timeout_ms, poll_ms) -> bool
pixel_matches(x, y, red, green, blue, tolerance) -> bool
wait_pixel(x, y, red, green, blue, tolerance, timeout_ms, poll_ms) -> bool
find_image(asset_id, region_x, region_y, region_width, region_height, threshold) -> Map
wait_image(asset_id, region_x, region_y, region_width, region_height, threshold, timeout_ms, poll_ms) -> Map
```

Wait values are non-negative integers, coordinates and wheel values are i32,
buttons are `left`, `right`, `middle`, `x1` or `x2`, and text/key arguments are
strings. Compatible source uses only top-level calls and can be converted to
`MacroStep[]`; `press` and `click` become balanced down/up pairs. Strings are
JSON-escaped so quotes, backslashes, newlines and Unicode survive a round trip.

Window queries use physical screen pixels. Matching is case-insensitive and
uses title substrings. `window_rect` returns the extended outer frame in screen
coordinates, including negative coordinates on secondary monitors. Regions
crossing monitor boundaries are captured per monitor and composited into one
physical-coordinate ROI. The capture backend uses Windows Graphics Capture first and
falls back to DXGI Desktop Duplication for monitor regions after device/API
failure.

Images are imported into `<AppData>/assets/images/`. The configuration stores
only `AutomationAsset` metadata and a generated safe file name. Rhai receives
only an asset ID; it cannot open arbitrary paths. PNG/JPEG bytes are validated,
the canonical path is checked against the managed directory, templates are
decoded lazily and cached with file metadata invalidation, and the cache is
bounded to 64 templates. Deletion requires confirmation when a script still
mentions the asset.

The recommended polling interval is 150–250ms. Polls are interruptible in
25ms slices, count toward the shared 100,000-operation limit, and have a
120-second maximum timeout. Captures are rate-limited, no capture loop runs
while idle, and only the latest frame is retained by each active capture
session. Run `cargo run --bin vision_bench -- --duration-seconds 600` for the
repeatable CPU/capture/pixel/template/cache diagnostic; its duration is not a
CI timing assertion.

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
