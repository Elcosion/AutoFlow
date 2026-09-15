# Vision matcher benchmark — 2026-09-15

固定纯内存输入：`1920x1080` BGRA、模板 `45x41`、阈值 `0.92`、seed `0xA11CE`。运行命令：

```powershell
$env:CARGO_TARGET_DIR='src-tauri/target-vision-next'
cargo run --release --manifest-path src-tauri/Cargo.toml --bin vision_bench
```

## Before：旧串行全分辨率 NCC

同一轮运行中的 `baseline_match_ms`，使用原来的 serial `imageproc::match_template`：

| case | 结果坐标 | 耗时 |
| --- | --- | ---: |
| hit | `1875,1039` | 2316 ms |
| miss | none | 2334 ms |
| repeat-hit | `1875,1039` | 2327 ms |
| moved-hit | `812,437` | 2319 ms |

## After：金字塔 + 候选 NMS + 局部精确 NCC

| case | 结果坐标 | 总耗时 | coarse | refine | fallback | previous hit |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| hit | `1875,1039` | 106 ms | 35 ms | 1 ms | 0 ms | false |
| miss | none | 111 ms | 39 ms | 1 ms | 0 ms | false |
| repeat-hit | `1875,1039` | 2 ms | 0 ms | 1 ms | 0 ms | true |
| moved-hit | `812,437` | 110 ms | 35 ms | 3 ms | 0 ms | true |

优化后四个场景均与基线坐标一致；相对旧路径约为 `21.8x`、`21.0x`、`1163x`、`21.1x`。本 benchmark 不包含真实屏幕 capture，用于比较 matcher 本身，不设置硬性耗时断言。

fallback 仍保留：仅在粗匹配高置信、局部精确分数处于阈值不确定区间时触发；单元测试 `uncertain_coarse_result_uses_parallel_full_fallback` 覆盖该路径。
