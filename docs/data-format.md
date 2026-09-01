# 数据格式约定（阶段 2）

桌面端的当前配置文件名为 `config.json`，由 Tauri 应用配置目录管理。浏览器预览使用 localStorage，不会写入桌面配置文件。

## 顶层结构

```json
{
  "schemaVersion": 1,
  "globalEnabled": true,
  "emergencyStop": "F12",
  "hotkeys": [],
  "textExpansions": [],
  "macros": []
}
```

## 宏规则

```json
{
  "id": "macro-example",
  "name": "每日重复操作",
  "enabled": false,
  "triggerKeys": ["Ctrl", "F8"],
  "mode": "repeat",
  "repeatCount": 10,
  "speed": 1.0,
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
```

`mode` 支持 `once`、`repeat`、`hold` 和 `toggle`。`repeatCount` 只在 `repeat` 模式使用，`speed` 为大于 0 的速度倍数。步骤类型包括 `delay`、`key`、`mouseButton`、`mouseMove`、`wheel` 和 `text`。

## 快捷键与文本扩展

快捷键规则的 `action.type` 支持 `remap` 和 `launch`；文本扩展规则使用 `abbreviation`、`replacement`、`enabled` 和 `caseSensitive` 字段。组合键使用 `triggerKeys` 数组表达，例如 `["Ctrl", "Alt", "T"]`。

## 校验与迁移边界

- 所有持久化对象必须包含 `schemaVersion`。
- 每个规则必须有稳定 `id`；快捷键触发组合不能重复。
- 文本缩写长度为 1–32 个字符，替换内容不能为空。
- 启用的宏必须包含触发组合；固定次数至少执行 1 次；宏速度必须大于 0。
- 写入使用临时文件，并保留旧配置的 `.bak`；解析损坏时保留 `.corrupt` 副本后恢复默认配置。
- 读取未知字段时忽略；未来的 schema 迁移必须增加对应测试。
- 程序启动时不会自动恢复或执行宏。
