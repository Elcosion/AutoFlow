# 多尺度视觉匹配 benchmark（2026-09-16）

命令：

```powershell
$env:CARGO_TARGET_DIR = "src-tauri/target-scale-next"
cargo run --release --manifest-path src-tauri/Cargo.toml --bin vision_bench
```

这是纯内存、固定种子的 1920×1080 测试；`baseline_match_ms` 是保留的
原始 1.0 倍串行 NCC，缩放命中时它预期可能找不到，因为 baseline 不做缩放。
benchmark 不包含易受机器负载影响的硬毫秒断言。

| case                    | expected_xy | optimized_xy | expected_scale | matched_scale | total_ms | scale_search_ms | coarse_ms | refine_ms | fallback_ms | candidates | previous | fallback | matched size |
| ----------------------- | ----------: | -----------: | -------------: | ------------: | -------: | --------------: | --------: | --------: | ----------: | ---------: | :------: | :------: | -----------: |
| hit                     |   1875,1039 |    1875,1039 |           1.00 |        1.0000 |      169 |             169 |       160 |         6 |           0 |          8 |  false   |  false   |        45×41 |
| miss                    |        none |        -1,-1 |           none |          none |      162 |             162 |       152 |         7 |           0 |          8 |  false   |  false   |          0×0 |
| scale-0.80-hit          |   1883,1046 |    1883,1046 |           0.80 |        0.8000 |      162 |             162 |       153 |         6 |           0 |          8 |  false   |  false   |        36×33 |
| scale-1.00-hit          |   1874,1038 |    1874,1038 |           1.00 |        1.0000 |      162 |             162 |       153 |         7 |           0 |          8 |  false   |  false   |        45×41 |
| scale-1.25-hit          |   1863,1028 |    1863,1028 |           1.25 |        1.2500 |      163 |             163 |       154 |         6 |           0 |          8 |  false   |  false   |        56×51 |
| scale-1.50-hit          |   1851,1017 |    1851,1017 |           1.50 |        1.5000 |      164 |             164 |       156 |         6 |           0 |          8 |  false   |  false   |        68×62 |
| multiscale-miss         |        none |        -1,-1 |           none |          none |      166 |             166 |       155 |         8 |           0 |          8 |  false   |  false   |          0×0 |
| repeat-hit              |   1875,1039 |    1875,1039 |           1.00 |        1.0000 |        8 |               8 |         3 |         4 |           0 |          8 |   true   |  false   |        45×41 |
| moved-hit               |     812,437 |      812,437 |           1.00 |        1.0000 |      165 |             165 |       155 |         7 |           0 |          8 |  false   |  false   |        45×41 |
| scale-1.25-hit (repeat) |   1863,1028 |    1863,1028 |           1.25 |        1.2500 |       11 |              11 |         6 |         5 |           0 |          8 |   true   |  false   |        56×51 |
| scale-moved-hit         |     812,437 |      812,437 |           1.25 |        1.2500 |      182 |             181 |       163 |        15 |           0 |          8 |   true   |  false   |        56×51 |
| scale-changed-hit       |   1851,1017 |    1851,1017 |           1.50 |        1.5000 |       10 |              10 |         5 |         4 |           0 |          8 |   true   |  false   |        68×62 |

本次 run 的完整诊断还报告默认候选为
`0.67,0.80,0.83,1.00,1.20,1.25,1.50`。首次多尺度命中均低于 200ms，
未命中为 162ms，同位置同尺度重复命中为 8ms；所有命中坐标、尺度和实际
模板尺寸与预期一致，未命中没有生成匹配尺度或尺寸。
