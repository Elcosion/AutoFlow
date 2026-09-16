# High-DPI vision benchmark (2026-09-16)

Command:

```powershell
$env:CARGO_TARGET_DIR = "src-tauri/target-next"
cargo run --release --manifest-path src-tauri/Cargo.toml --bin vision_bench
```

This is the deterministic in-memory `1920x1080` benchmark with a `45x41`
template and threshold `0.92`. `single_match_ms` is one search attempt;
`wait_total_ms` is reserved for the complete `wait_image` operation. Timings
are diagnostic observations, not millisecond test assertions.

| case                          | expected -> optimized  | expected -> matched scale | total / single ms | robust score | alpha | anchor / preferred | matched size |
| ----------------------------- | ---------------------- | ------------------------- | ----------------: | -----------: | :---: | :----------------: | -----------: |
| hit                           | 1875,1039 -> 1875,1039 | 1.00 -> 1.0000            |         215 / 215 |     1.000000 |  no   |      no / no       |        45x41 |
| miss                          | none -> -1,-1          | none -> none              |         386 / 386 |         none |  no   |      yes / no      |          0x0 |
| scale-0.80-hit                | 1883,1046 -> 1883,1046 | 0.80 -> 0.8000            |         219 / 219 |     1.000000 |  no   |      no / no       |        36x33 |
| scale-1.00-hit                | 1874,1038 -> 1874,1038 | 1.00 -> 1.0000            |         217 / 217 |     1.000000 |  no   |      no / no       |        45x41 |
| scale-1.25-hit                | 1863,1028 -> 1863,1028 | 1.25 -> 1.2500            |         217 / 217 |     1.000000 |  no   |      no / no       |        56x51 |
| scale-1.50-hit                | 1851,1017 -> 1851,1017 | 1.50 -> 1.5000            |         218 / 218 |     1.000000 |  no   |      no / no       |        68x62 |
| scale-1.75-hit                | 1840,1007 -> 1840,1007 | 1.75 -> 1.7500            |         219 / 219 |     1.000000 |  no   |      no / no       |        79x72 |
| scale-2.00-hit                | 1829,997 -> 1829,997   | 2.00 -> 2.0000            |         218 / 218 |     1.000000 |  no   |      no / no       |        90x82 |
| multiscale-miss               | none -> -1,-1          | none -> none              |         393 / 393 |         none |  no   |      yes / no      |          0x0 |
| changed-bottom-left-hit       | 1840,1007 -> 1840,1007 | 1.75 -> 1.7500            |         219 / 219 |     1.000000 |  no   |      no / no       |        79x72 |
| changed-bottom-left-moved-hit | 812,437 -> 812,437     | 2.00 -> 2.0000            |         219 / 219 |     1.000000 |  no   |      no / no       |        90x82 |
| similar-distractor            | 1420,720 -> 1420,720   | 1.00 -> 1.0000            |         218 / 218 |     1.000000 |  no   |      no / no       |        45x41 |
| robust-miss                   | none -> -1,-1          | none -> none              |         389 / 389 |         none |  no   |      yes / no      |          0x0 |
| anchor-recovery-hit           | 1840,1007 -> 1840,1007 | 1.75 -> 1.7500            |         406 / 406 |     1.000000 |  no   |      yes / no      |        79x72 |
| repeat-hit                    | 1875,1039 -> 1875,1039 | 1.00 -> 1.0000            |             5 / 5 |     1.000000 |  no   |      no / no       |        45x41 |
| moved-hit                     | 812,437 -> 812,437     | 1.00 -> 1.0000            |         218 / 218 |     1.000000 |  no   |      no / no       |        45x41 |
| scale-1.25-hit (repeat)       | 1863,1028 -> 1863,1028 | 1.25 -> 1.2500            |           10 / 10 |     1.000000 |  no   |      no / no       |        56x51 |
| scale-moved-hit               | 812,437 -> 812,437     | 1.25 -> 1.2500            |         239 / 239 |     1.000000 |  no   |      yes / no      |        56x51 |
| scale-changed-hit             | 1851,1017 -> 1851,1017 | 1.50 -> 1.5000            |             8 / 8 |     1.000000 |  no   |      no / no       |        68x62 |
| scale-1.75-repeat             | 1840,1007 -> 1840,1007 | 1.75 -> 1.7500            |             7 / 7 |     1.000000 |  no   |      no / yes      |        79x72 |
| scale-2.00-repeat             | 1829,997 -> 1829,997   | 2.00 -> 2.0000            |             8 / 8 |     1.000000 |  no   |      no / yes      |        90x82 |
| alpha-masked-hit              | 1874,1038 -> 1874,1038 | 1.00 -> 1.0000            |         235 / 235 |     1.000000 |  yes  |      no / no       |        45x41 |

The final benchmark reports the default candidate list as:
`0.67,0.80,0.83,1.00,1.20,1.25,1.50,1.75,2.00`.
The observed first-hit 1.75/2.00 cases stayed below 500 ms, while known-scale
repeats stayed below 10 ms. Misses intentionally leave `matched_scale` and
`robust_score` unset; the complete command output also includes
`scale_search_ms`, `capture_ms`, `prepare_ms`, `coarse_match_ms`,
`refine_match_ms`, `robust_verify_ms`, `fallback_match_ms`, tile counts,
`anchor_candidate_count`, `candidate_count`, and `wait_total_ms`.

## Interpretation

The final score is `0.15 * full_ncc + 0.70 * robust_tile_score + 0.15 *
(passed_tiles / valid_tiles)`. Low-information tiles are excluded, at most
one worst tile is discarded (`min(1, floor(valid_tiles / 5))`), and at least
three passing tiles must span multiple rows and columns; the robust tile gate
is `threshold - 0.18` (never below `0.45`). Anchor recovery is Auto-only,
uses at most two spatially separated high-information tiles, four scale
hypotheses, at most four positions per tile, and at most sixteen generated
anchor candidates (the final refine still honors `max_candidates`). Every
recovered position is converted to the full template top-left and rechecked
by the full/robust verifier.

PNG alpha is an optional source-image mask: alpha-zero pixels are omitted from
final NCC/tile verification and partial alpha is used as a weight. The alpha
image is resized with the grayscale image and is included in the prepared
template cache accounting; no separate mask resource is introduced.

These results cover the automated synthetic and service tests. Real Windows
DPI behavior is still pending manual verification at 100%, 125%, 150%, 175%,
and 200%, including the real `12.png` shortcut-badge case, movement, missing
image, and F12 stop/restart flows.
