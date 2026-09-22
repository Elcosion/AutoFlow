# 数据格式约定（当前写出 schema 6）

桌面端当前使用 `config.json`，由 Tauri 应用配置目录管理；浏览器预览使用 localStorage，不会写入桌面配置文件。

## 当前写出布局

当前运行时常量 `SCHEMA_VERSION` 为 6。下面是当前 `config.json` 中与宏存储相关的字段摘录（不是完整文件）：

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
      "name": "每日重复操作",
      "fileName": "每日重复操作.json"
    }
  ]
}
```

保存配置时，根配置保留 `macros: []`，并通过 `macroFiles` 保存宏脚本索引。每个索引对应一个 `MacroRule` JSON 文件，位于应用数据目录的 `data/scripts/` 下；文件名由宏名称生成并受存储层校验。

宏脚本文件的图形宏示例（已安全禁用、没有触发键）：

```json
{
  "id": "macro-example",
  "name": "每日重复操作",
  "enabled": false,
  "triggerKeys": [],
  "mode": "repeat",
  "repeatCount": 10,
  "speed": 1.0,
  "recordMouseMove": true,
  "recordMouseClicks": true,
  "program": {
    "kind": "macro",
    "steps": [
      { "type": "delay", "durationMs": 320 },
      {
        "type": "mouseButton",
        "button": "left",
        "action": "down",
        "x": 820,
        "y": 430
      },
      {
        "type": "mouseButton",
        "button": "left",
        "action": "up",
        "x": 820,
        "y": 430
      },
      { "type": "text", "text": "测试" },
      { "type": "key", "key": "Enter", "action": "down" }
    ]
  }
}
```

同一个 `MacroRule` 也可以使用非空的 Rhai 程序；其 `program` 形状固定为：

```json
{
  "kind": "rhai",
  "source": "wait_ms(100);",
  "apiVersion": 1
}
```

`mode` 支持 `once`、`repeat`、`hold` 和 `toggle`。`repeatCount` 只在
`repeat` 模式使用，`speed` 必须大于 0。图形宏步骤类型包括 `delay`、
`key`、`mouseButton`、`mouseMove`、`wheel` 和 `text`。

## 快捷键与文本扩展

快捷键规则的 `action.type` 支持 `remap` 和 `launch`；文本扩展规则使用
`abbreviation`、`replacement`、`enabled` 和 `caseSensitive` 字段。组合键使用
`triggerKeys` 数组表达，例如 `["Ctrl", "Alt", "T"]`。

## 历史兼容读取与校验边界

- 缺失 `schemaVersion` 按 v1 读取；根配置读取后会将 schema 版本归一为 6 再写出。
  schema 1 和 schema 2 都是可迁移读取的历史输入，不应称为非法。
- 宏规则只有在缺失 `program` 且存在旧的顶层 `steps` 时，才会把这些步骤包装为
  `program.kind = "macro"`；已有 `program`（包括 Rhai）会保留其类型和内容。
- `schemaVersion > 6` 会被拒绝，并提示使用支持该版本的 AutoFlow；当前版本不会
  猜测未来格式。
- 启用宏必须包含触发组合；Hold 触发器必须恰好包含一个普通键，修饰键只能作为
  修饰键；示例中的宏保持停用且 `triggerKeys` 为空，不能直接触发。
- Rhai 程序的 `apiVersion` 当前只能是 1，`source` 不能为空；桌面编辑器保存前和
  桌面外部脚本加载时会做 Rust Rhai 安全校验，浏览器预览不提供该校验，通用配置
  验证只检查这两个基本字段。
- 写入先使用 `.json.tmp`，替换已有文件时保留旧的 `.json.bak`。`config.json`
  无法解析时会尝试复制为 `config.json.corrupt`（复制失败会忽略），然后加载并保存
  安全默认配置；程序不会自动执行任何宏。
- 已索引的 `data/scripts/*.json` 在正式文件不可读时可回退到同名 `.json.bak`；
  回退也失败的索引条目会被移除。内容无效的脚本会以停用状态和 `importError`
  保留供编辑修复；任何情况下都不会被自动启用。
- 读取未知字段时忽略；未来 schema 迁移必须增加对应测试。
