# Rhai automation

AutoFlow configuration schema v2 keeps the common macro metadata and stores
the executable content in one `program` value:

```json
{
  "schemaVersion": 2,
  "macros": [
    {
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
    }
  ]
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
find_image(file_name, region_x, region_y, region_width, region_height, threshold) -> Map
wait_image(file_name, region_x, region_y, region_width, region_height, threshold, timeout_ms, poll_ms) -> Map
find_image(file_name, region_x, region_y, region_width, region_height, threshold, options) -> Map
wait_image(file_name, region_x, region_y, region_width, region_height, threshold, timeout_ms, poll_ms, options) -> Map
```

The optional image-search map uses `mode: "auto"`, `"exact"`, or `"fast"`,
`prefer_last: true|false`, and `max_candidates` from 1 to 32. `auto` checks
the previous hit, searches a half-resolution pyramid, refines a small set of
candidates at original resolution, and only uses a parallel full-resolution
fallback when the coarse result is uncertain. `exact` always uses the full
resolution matcher; `fast` skips that fallback. Invalid values return a stable
Chinese validation error. The default calls keep their original signatures and
default to `auto`.

Image result maps retain `found`, coordinates, dimensions and `score`, and add
`total_ms`, `capture_ms`, `prepare_ms`, `coarse_ms`, `refine_ms`,
`fallback_ms`, `candidate_count`, `previous_hit_used`, `fallback_used`, and
`matcher_mode`. These fields are diagnostic only and do not change old scripts.

Wait values are non-negative integers, coordinates and wheel values are i32,
buttons are `left`, `right`, `middle`, `x1` or `x2`, and text/key arguments are
strings. Compatible source uses only top-level calls and can be converted to
`MacroStep[]`; `press` and `click` become balanced down/up pairs. Strings are
JSON-escaped so quotes, backslashes, newlines and Unicode survive a round trip.

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
decoded lazily and cached as a prepared grayscale template plus a half-resolution
pyramid. Cache entries invalidate when the resource file name, metadata or
SHA-256 changes; the cache is bounded to 64 entries and 64 MiB. Deletion requires confirmation when a script still
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
