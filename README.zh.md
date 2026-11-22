# SailTerm

**[English](README.md) | 中文**

> 跨平台桌面 SSH / SFTP / FTP 终端，原生集成 AI —— AI 通过已建立的会话干活，但永远看不到你的凭证。

## 为什么做这个

我是一名开发者，平时要自己维护几台服务器。以前每天的动作是：开一个 SSH 客户端、再开一个文件传输工具、外加一个浏览器标签页——把报错贴给通用 AI，问它怎么办。两件事让我不舒服：

1. **工作流是碎的**——终端、文件、答案之间上下文来回丢失。
2. **通用 agent 有可能把主机、账号、密码、私钥都喂进模型上下文**，我不想拿服务器冒这个险。

所以做了 SailTerm：把终端、文件传输和 AI 装进同一个桌面应用，AI 能干活，但**碰不到凭证**。

我不是专业运维，功能上是按「一个开发者实际需要」来取舍的，肯定会漏掉真正的 SRE 觉得理所当然的东西——欢迎反馈和 PR。

## 密钥不会交给 AI

- 主机、账号、密码、私钥只存在于 Rust 核心进程内，落盘使用 **OS 钥匙串**（不可用时回退 AES-256-GCM 加密文件），永不离开这一层。
- AI 对服务器的一切操作都通过**已建立的 SSH 会话**（`session_id` 句柄）进行——它只说「做什么」，由应用去执行；写操作与高危操作一律需要人工审批，全程留审计日志。
- 发给模型的终端输出经过**脱敏**：密码 / 密钥 / token / 私钥块 / 内网 IP 等模式在出网前被抹掉。

一句话：模型看到的是你的问题，不是你的钥匙。完整信任边界见 [SECURITY.md](SECURITY.md)。

## 系统架构

两个进程层，中间一条窄桥：渲染层零凭证、零联网，一切敏感操作集中在 Rust 核心。

```
渲染层（WebView）  React 19 + TypeScript + Vite
  xterm.js 终端 · 文件/传输/监控/审计面板 · 轻量 store（stores/）· i18n · 主题
  全部 Tauri 调用收口于 src/lib/tauri/ —— 零凭证、不联网
       │  命令 invoke（参数化）
       ▼  ▲  事件流（终端字节 / 传输进度 / AI 流式 token）
Rust 核心（src-tauri/，单进程独占敏感面）
  commands  IPC 入口，前端唯一可调用面
  session · ssh · mosh · telnet · serial   SSH / SFTP / FTP / Telnet / 串口 / Mosh 会话（russh · suppaftp）
  vault     凭证保险库：OS 钥匙串（不可用时回退 AES-256-GCM 加密文件）
  ai · sanitize   编排状态机 · 脱敏器 · 策略引擎 · 审批 · 审计 · 多模型 provider（BYOK）
  files · db · plugin   传输队列与缓存 · SQLite（WAL）+ 迁移 · 会话录制 · 本地插件
```

- **边界是强制的，不是约定俗成的**：除 `src/lib/tauri/` 外任何前端代码引用 Tauri API 都会被 lint 脚本（`scripts/check-ipc-boundary.sh`）拦截在 CI 之前；出网只发生在 Rust 核心的两个口——AI provider 请求与 SSH/FTP 连接本身。
- **AI 一轮请求的通路**（安全模型在架构上的落点）：提问 → 组装上下文（历史输出均已脱敏）→ provider 生成回复/工具调用 → 策略引擎按风险分级（只读 / 写 / 高危）→ 写与高危挂起等待人工审批（可改判）→ 经已建立的会话句柄（`session_id`）执行 → 输出脱敏后回填模型 → 循环直至给出答案。凭证不在这条链的任何一格。

| 层 | 选型 |
|---|---|
| 桌面框架 | Tauri 2（Rust 核心 + 系统 WebView） |
| 前端 | React 19 + TypeScript + Vite |
| 终端 | xterm.js（+ fit / search / web-links / webgl 插件） |
| SSH / FTP | `russh` / `suppaftp` |
| 持久化 | SQLite + OS 钥匙串 |
| AI | 多模型 provider 层 + tool-calling 编排（轻量状态机，未用重型 agent 框架） |

## 功能

- **连接**：SSH / SFTP / FTP / FTPS / Telnet / 串口 / Mosh、本地终端、多标签 + 分屏、断线重连、主机密钥管理、SSH 密钥库（私钥仅存保险库）
- **AI**：经会话执行命令 / 读输出 / 传文件；分级策略（只读 / 写 / 高危）+ 人工审批（可编辑改判）；输出脱敏；全量审计日志；终端内 Agent 模式与侧栏对话双通路
- **文件**：双栏 SFTP/FTP 浏览器、远端编辑「写回前确认」、传输队列与断点续传
- **效率**：命令片段（变量占位）、多机群发、SSH 本地端口转发、会话监控、会话录制回放、加密导入/导出连接、主题、中英双语、插件系统

## 开发

环境要求：**Node.js 20+（pnpm）**、**Rust stable**，以及 Tauri 2 平台依赖（macOS 需 Xcode CLT；Windows 需 WebView2 + C++ Build Tools；Linux 需 `webkit2gtk` 等）。

```bash
pnpm install

pnpm tauri dev       # 开发模式（前端 + Tauri 热重载）
pnpm check           # tsc --noEmit + IPC 边界 lint + 生产构建
pnpm test            # 前端单测（vitest）
cd src-tauri && cargo test   # Rust 单测

pnpm tauri build     # 生产构建（安装包在 src-tauri/target/release/bundle/）
```

## 打包

- **macOS**：`bash scripts/package-macos.sh`——构建并在 `dist-bundle/` 下产出 `.dmg`（直接用 `hdiutil` 自制 dmg；Tauri 默认的 bundle_dmg.sh 在非交互环境不可靠）。
- **Linux**：`bash scripts/package-linux.sh`（deb / rpm / AppImage，按 Tauri 配置）。
- **Windows**：`pwsh scripts/package.ps1`。

发布流程与签名/公证现状见 [RELEASE.md](RELEASE.md)（当前为未签名构建，首次启动需手动放行 Gatekeeper）。

## 平台状态

- **macOS** 为主要平台，已真机验证。
- **Windows / Linux** 可同工具链编译，但尚未真机验证——视为实验性，欢迎报 bug。
- 当前是**早期预览版（0.x）**：暂无预编译包与 CI，请从源码构建。

## 关于注释语言

源码注释与 AI 工具提示词**刻意使用中文**——提示词是给模型看的功能性指令，不是 UI 文案。界面本身是完整国际化的（内置中英）。贡献时如果你顺手把改动到的注释翻成英文，非常欢迎。

## 贡献

开发环境、代码约定与提 PR 前需跑的检查见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 许可证

[MIT](LICENSE)。第三方组件及其许可证见 [THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md)（依赖变更后运行 `python3 scripts/gen-third-party-licenses.py` 重新生成）。
