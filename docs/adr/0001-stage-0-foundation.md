# ADR-0001：阶段 0 的桌面工程基础

- 状态：已采用
- 日期：2026-07-23

## 背景

AutoFlow 面向 Windows 中文用户，需要桌面窗口、Rust 原生能力和可快速迭代的中文界面。规格要求首轮先完成骨架，不提前编写钩子和宏播放器。

## 决策

1. 使用 Tauri 2 作为桌面外壳。
2. 使用 React + TypeScript + Vite 作为前端。
3. 阶段 0 使用轻量 hash 路由，不引入第二套 UI 框架。
4. 使用 CSS 变量建立单一视觉主题，默认浅色并提供系统深色回退。
5. Rust command 统一返回可序列化的 `AppError`。
6. 原生 Windows API、AHK 和宏状态机按后续阶段逐步加入。

## 结果

工程可以在浏览器预览模式查看页面，也可以由 Tauri 启动桌面窗口。前端不会因为本地预览没有 Tauri runtime 而崩溃；桌面端再通过 `get_app_status` 连接 Rust Core。

## 未决事项

持久化使用 SQLite 还是版本化 JSON、AHK runtime 的打包方式以及全局钩子库，需要在对应阶段结合 Windows 构建和人工安全验收后决定。
