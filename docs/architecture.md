# AutoFlow 架构（阶段 2）

## 目标

阶段 2 完成一条可验证的宏闭环：React 编辑和保存规则，Tauri IPC 调度 Rust，Windows HookService 负责键鼠录制与安全播放。复杂条件、OCR、找图和流程图继续留在范围之外。

```text
React UI
  │ Tauri IPC
  ▼
Rust Core
  ├─ ConfigService  配置模型、校验、版本字段
  ├─ StorageService 配置文件、备份、损坏恢复
  ├─ HookService    Windows 低级键盘/鼠标监听
  ├─ Recorder       延迟、键盘、点击、拖拽和滚轮步骤转换
  ├─ MacroPlayer    单实例播放、循环、速度和按键释放保护
  ├─ SafetyService  全局开关、紧急停止、状态清理
  ├─ TrayService（后续阶段）
  └─ StartupService（后续阶段）
```

## 当前实现

- `src/`：React + TypeScript 页面、导航、快捷键/文本扩展/宏编辑器和视觉系统。
- `src/lib/routes.ts`：轻量 hash 路由。
- `src/lib/tauri.ts`：前端唯一的 Tauri 调用入口；浏览器模式回退到 localStorage，宏录制/播放在浏览器模式返回可操作错误。
- `src-tauri/src/config.rs`：`AppConfig`、快捷键、文本扩展、宏规则和宏步骤模型及校验。
- `src-tauri/src/storage.rs`：从应用配置目录加载 `config.json`，写入前生成 `.bak`，损坏时恢复默认配置。
- `src-tauri/src/hook.rs`：Windows `WH_KEYBOARD_LL` 与 `WH_MOUSE_LL`；支持映射、组合启动、文本扩展、录制和播放。
- `src-tauri/src/commands.rs`：`get_config`、`save_config`、`start_macro_recording`、`stop_macro_recording`、`play_macro`、`stop_macro` 等 IPC command。

## 录制与播放边界

录制时键盘按下/释放分开保存；普通鼠标移动不保存，只有按住鼠标按钮拖拽时保存压缩后的关键点。相邻事件时间差大于 8ms 会转为 `delay` 步骤。AutoFlow 自身窗口的录制输入会被过滤。

播放器支持单次、固定次数、按住循环和开关循环。播放线程用可中断等待检查停止标志，并在正常结束、紧急停止或配置切换时释放所有仍被模拟按下的按键和鼠标按钮。任意时刻只允许一个宏播放。

HookService 忽略带注入标志的事件，避免文本扩展和宏播放再次触发自己。当前实现面向 Windows MVP，复杂键盘布局、多显示器坐标、管理员窗口、锁屏和远程桌面仍需人工验收。

## IPC 约定

异步 command 返回 `Result<T, AppError>`。`AppError` 包含稳定的 `code`、可操作的中文 `message` 和可选 `detail`，前端展示可读错误。平台相关实现只放在 Rust 服务模块，React 页面通过 command 或状态层访问。
