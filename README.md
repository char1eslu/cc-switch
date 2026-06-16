<div align="center">

# CC Switch - Codex Keeper Fork

### 一个偏向 Codex 会话维护的 CC Switch 自用分支

[![Fork branch](https://img.shields.io/badge/fork-dev-blue)](https://github.com/char1eslu/cc-switch/tree/dev)
[![Upstream](https://img.shields.io/badge/upstream-farion1231%2Fcc--switch-lightgrey)](https://github.com/farion1231/cc-switch)
[![macOS arm64 ad hoc](https://github.com/char1eslu/cc-switch/actions/workflows/build-macos-ad-hoc.yml/badge.svg?branch=dev)](https://github.com/char1eslu/cc-switch/actions/workflows/build-macos-ad-hoc.yml)

</div>

> 这个仓库是 `farion1231/cc-switch` 的 fork，不是上游官方发布页。
> 当前 `dev` 分支主要改动是把一部分 Codex Wake / Codex Keeper 的会话维护能力移植进 CC Switch 的 Session Manager，并提供自用的 macOS arm64 ad-hoc 构建。

## 这个 fork 改了什么

上游 CC Switch 本来就是一个多 AI 编程工具的桌面管理器，覆盖 Claude Code、Claude Desktop、Codex、Gemini CLI、OpenCode、OpenClaw、Hermes 等工具的 provider、MCP、Prompts、Skills、Proxy、Usage 和 Session Manager。

这个 fork 没有重写这些基础能力，而是在上游基础上重点补了四类东西：

| 方向 | 这个 fork 的变化 |
| --- | --- |
| Codex 会话维护 | 把 Codex Keeper 的 repair、move、trim、branch、trash、restore、backup 管理接进 Session Manager |
| Codex 会话预览 | 对话内容改为 Markdown/GFM 渲染，过滤 Codex 注入的环境上下文噪音 |
| macOS 自用构建 | 新增手动 GitHub Actions workflow，只构建 macOS Apple Silicon arm64 `.app` zip |
| README 主页 | 去掉上游 sponsor/营销主页，改成 fork 差异说明和功能对照 |

## 已搬过来的 Codex Keeper 功能

这些功能现在都在 CC Switch 的 `Sessions` 页面中，主要只针对 Codex 会话生效。

| 功能 | 状态 | 说明 |
| --- | --- | --- |
| 扫描 Codex SQLite 会话 | 已实现 | 优先读取 `~/.codex/sqlite/state_5.sqlite` 和 `session_index.jsonl`，再 fallback 到 `sessions` / `archived_sessions` JSONL |
| 会话状态识别 | 已实现 | 显示 available、not indexed、missing file、archived 等状态，并隐藏 Codex subagent 会话 |
| Repair Index | 已实现 | 修复 Codex session index 缺失或状态不一致的问题，操作前备份 state/session_index/JSONL |
| Move Session | 已实现 | 移动会话到新的项目目录，会更新 SQLite `threads.cwd` 和 JSONL `session_meta.payload.cwd` |
| Trim from here | 已实现 | 从指定用户轮次后截断 JSONL，并保留 `.codex-rescue-backup-*` 备份 |
| Branch from here | 已实现 | 从指定轮次创建新 Codex 会话，生成新 UUID、新 JSONL，并写入 SQLite 和 `session_index.jsonl` |
| Move to Trash | 已实现 | 将会话移入 `~/.codex/.codex-wake-trash/threads/`，并从 SQLite/session_index 移除 |
| Restore trashed session | 已实现 | 从 Codex Keeper Trash 恢复会话文件、SQLite row 和 session_index entry |
| Permanently delete trashed session | 已实现 | 删除 Trash manifest 所在目录，带路径边界校验 |
| Empty session trash | 已实现 | 清空已删除会话 Trash |
| Backup list | 已实现 | 列出 Codex Keeper/Rescue 生成的 state、session_index、JSONL 备份 |
| Restore backup | 已实现 | 从备份恢复原文件，恢复前会再次创建 before-restore 备份 |
| Move backup to trash | 已实现 | 把备份移入 `.codex-wake-trash` |
| Empty backup trash | 已实现 | 清空备份 Trash |
| Codex prompt/title 清理 | 已实现 | 避免把 `<environment_context>`、AGENTS 注入、VS Code context 当成真实标题 |

### 会话预览改动

这部分来自对 Codex Keeper / Wake 预览体验的补齐：

- Assistant/User 消息使用 `react-markdown` + `remark-gfm` 渲染。
- 支持标题、列表、引用、表格、行内代码和代码块。
- 搜索高亮时仍保留纯文本路径，避免 Markdown AST 和高亮冲突。
- 隐藏或折叠 Codex 注入上下文，例如：
  - `# AGENTS.md instructions for ...`
  - `<environment_context>`
  - `<permissions instructions>`
  - `<app-context>`
  - 没有真实 `My request for Codex` 的 VS Code context
- 修了 Session Manager 右侧按钮栏和 Move dialog 长路径溢出问题。

## 还没实现的部分

这不是完整复制 Codex Keeper 原生应用，目前还有这些明确限制：

| 未实现项 | 当前状态 |
| --- | --- |
| 完整原生 macOS UI | 没搬 Swift/AppKit 原生界面，只接入了 CC Switch 的 Tauri/React Session Manager |
| 自动后台监控/自动修复 | 没做 watcher，也不会自动改 Codex 状态；所有危险操作都需要用户点按钮 |
| 跨工具 Keeper 操作 | Repair/Move/Trim/Branch/Trash 目前只针对 Codex；Claude/Gemini/OpenCode 仍主要沿用上游浏览/恢复/删除能力 |
| 会话导出/合并向导 | 没做 Markdown/JSONL 导出、会话合并、跨机器迁移向导 |
| 操作前 diff 预览 | 目前显示操作结果和备份路径，不提供 SQLite/JSONL diff 预览 |
| 正式 macOS 分发 | 只有 arm64 ad-hoc zip；没有 x86_64/universal、DMG、Developer ID 签名、公证或自动更新包 |
| MCP 管理重写 | MCP/Skills 仍沿用上游 CC Switch 实现，这个 fork 主要改 Session Manager |

## 构建和下载

### GitHub Actions 自用构建

这个 fork 的 `dev` 分支提供一个手动 workflow：

- Workflow: [Build macOS Ad Hoc](https://github.com/char1eslu/cc-switch/actions/workflows/build-macos-ad-hoc.yml)
- 目标架构：`aarch64-apple-darwin`
- 产物名：`CC-Switch-macOS-arm64-ad-hoc`
- 产物内容：ad-hoc signed `CC Switch.app` zip

它不会构建 x86_64，也不会构建 universal app。

自用安装时，如果 macOS 拦截，可以右键打开，或对解压后的 app 清理 quarantine：

```bash
xattr -dr com.apple.quarantine "CC Switch.app"
```

### 本地开发

```bash
pnpm install
pnpm typecheck
pnpm test:unit
pnpm build:renderer
pnpm tauri build --target aarch64-apple-darwin --bundles app
```

Rust/Tauri 完整打包需要本机有 Rust toolchain 和 Tauri 依赖。没有本机 Rust 时，直接用上面的 GitHub Actions 更省事。

## 使用注意

- Codex 会话维护会修改 `~/.codex/sqlite/state_5.sqlite`、`session_index.jsonl` 和 `sessions/**/*.jsonl`。
- Move、Repair、Branch、Trash、Restore 等操作都会尽量先创建 `.codex-rescue-backup-*` 备份。
- 建议在操作同一个会话前关闭正在运行的 Codex 进程，避免 SQLite/WAL 或 JSONL 正在写入。
- 这个 fork 是自用分支；如果你需要稳定的跨平台发布包，请优先看上游 [farion1231/cc-switch](https://github.com/farion1231/cc-switch)。

## 上游 CC Switch 仍然保留的能力

这个 fork 沿用上游主体功能，包括：

- Claude Code、Claude Desktop、Codex、Gemini CLI、OpenCode、OpenClaw、Hermes 的 provider 管理。
- 50+ provider presets、官方登录/第三方 relay 切换、tray quick switch。
- Unified MCP、Prompts、Skills 面板。
- Local proxy、failover、usage dashboard、model test。
- WebDAV / cloud config sync。
- Deep Link import。
- 多语言 UI 和深浅色主题。

## 主要提交

| Commit | 内容 |
| --- | --- |
| `8572acb7` | Port Codex Keeper session tools |
| `9cf41e68` | Fix Codex Keeper Rust title handling |
| `e7332897` | Build macOS ad hoc app for arm64 only |
| `8eaf412b` | Fix Session Manager preview UI |

## License

MIT. Upstream copyright belongs to the original CC Switch authors.
