# AutoFlow

AutoFlow 是面向中文 Windows 用户的快捷键、文本扩展与轻量宏工具。当前仓库已经推进到“阶段 2：宏可操作”：可以在桌面端录制、编辑、播放并保存键鼠宏，由 Rust 在 Windows 后台监听输入。

## 当前可以直接使用什么

- 快捷键映射：例如 `CapsLock → Esc`，支持单键和组合键配置。
- 快捷键启动程序：例如 `Ctrl + Alt + T → wt.exe`。
- 文本扩展：例如输入 `@@` 后替换成邮箱或多行文本。
- 全局总开关、规则启停、紧急停止按钮和配置校验。
- 宏录制：键盘按下/释放、鼠标移动轨迹、点击、滚轮和事件间延迟。
- 宏编辑：步骤增删、上下排序、等待/按键/文本参数编辑，支持单次、固定次数、按住循环和开关循环。
- 宏播放：单实例限制、整体速度、按键/鼠标释放保护和 F12 紧急停止。
- 浏览器预览：运行 `npm run dev` 时可以体验界面，配置保存在浏览器 localStorage；真正的全局键盘能力需要桌面端。

托盘、开机启动和 AutoHotkey 适配仍未开放；复杂条件、OCR、找图和流程图也不在当前范围内。

## 第一次试用

1. 在 Windows 安装 Node.js 20+、Rust stable、WebView2 和 Visual Studio C++ Build Tools。
2. 在本目录安装依赖并启动桌面端：

   ```powershell
   pnpm install
   pnpm run desktop:dev
   ```

3. 打开“快捷键”，选中预置的 `CapsLock → Esc`，点“启用规则”。
4. 在记事本里测试，再回到“文本扩展”新建 `@@ → 你的邮箱`，保存后启用。
5. 进入“宏”，点击“新建宏”，可以先用“＋ 等待/按键/文本”添加步骤，或点击“开始录制”后切换到目标窗口操作，再点“停止录制”。
6. 保存宏后用“测试播放”验证；需要绑定全局触发时填写 `Ctrl + F8` 并启用宏。
7. 如果需要完全停止输入服务，进入“设置”点击“立即停止”；关闭“全局总开关”会暂停全部规则。

也可以用 `pnpm run dev` 先看界面和编辑流程，但浏览器预览不会监听系统全局键盘。

## 开发环境

- Windows 10/11
- Node.js 20+
- Rust stable、Cargo
- Tauri 2 的 Windows 依赖：WebView2 与 Visual Studio C++ Build Tools

## 常用命令

在 `autoflow` 目录执行：

```powershell
pnpm install
pnpm run dev
pnpm run desktop:dev
pnpm run build
pnpm run test:run
pnpm run typecheck
pnpm run format:check
pnpm run check
```

构建桌面安装包：

```powershell
pnpm run desktop:build
```

只构建不生成安装包：

```powershell
pnpm exec tauri build --no-bundle
```

Rust 单独检查：

```powershell
cd src-tauri
cargo fmt --all -- --check
cargo test --offline
cargo clippy --all-targets --all-features --offline -- -D warnings
```

## 配置与安全边界

桌面端配置由 Rust 保存到 Tauri 的应用配置目录 `config.json`，写入前会校验并保留 `.bak` 备份；解析损坏时会将旧文件改名为 `.corrupt` 并恢复默认配置。配置包含 `schemaVersion`，规则和扩展都使用稳定 `id`。

当前 Windows 输入服务只处理明确启用的规则，忽略自己注入的键盘事件和 AutoFlow 自身窗口的录制操作；切换配置或点击紧急停止时会清理已按下状态，并释放正在模拟的目标键和鼠标按钮。首次启用规则请先在记事本验证。

## 项目结构

- `src/`：React 页面、导航、编辑器和视觉系统。
- `src/lib/tauri.ts`：前端唯一的 Tauri 调用入口，并提供浏览器预览回退。
- `src-tauri/src/config.rs`：配置模型与校验。
- `src-tauri/src/storage.rs`：配置读写和损坏恢复。
- `src-tauri/src/hook.rs`：Windows 低级键盘/鼠标监听、按键映射、程序启动、文本扩展、宏录制与播放。
- `src-tauri/src/commands.rs`：前端 IPC command。
- `docs/`：架构、数据格式和阶段决策。
