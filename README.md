<div align="center">

# CC Switch Fork

### 基于上游 `farion1231/cc-switch` 的个人改动分支

[![Fork branch](https://img.shields.io/badge/fork-dev-blue)](https://github.com/char1eslu/cc-switch/tree/dev)
[![Upstream](https://img.shields.io/badge/upstream-farion1231%2Fcc--switch-lightgrey)](https://github.com/farion1231/cc-switch)
[![macOS arm64 ad hoc](https://github.com/char1eslu/cc-switch/actions/workflows/build-macos-ad-hoc.yml/badge.svg?branch=dev)](https://github.com/char1eslu/cc-switch/actions/workflows/build-macos-ad-hoc.yml)

</div>

> 这是 `farion1231/cc-switch` 的个人 fork，不是上游官方发布页。
> 这个分支主要记录相对上游的自用改动，稳定跨平台版本请优先看上游项目。

## 和上游的主要区别

| 方向 | 这个 fork 的改动 |
| --- | --- |
| 应用范围 | 聚焦 Claude Code、Claude Desktop、Codex，清理了一批当前不维护的旧工具入口 |
| Codex 会话管理 | Session Manager 增加 Codex 会话扫描、状态识别、Repair、Move、Trim、Branch、Trash、Restore、Backup 等维护操作 |
| 会话浏览 | Codex 对话支持 Markdown/GFM 渲染，目录和标题会过滤 AGENTS、环境上下文、工具 schema 等注入噪音 |
| 搜索和批量操作 | 增加项目过滤、状态计数、JSONL Deep Search、多选批量 Repair/Move/Trash |
| 路径操作 | 会话详情支持 Reveal / Copy Path，Move dialog 对长路径和候选目录做了可用性处理 |
| 技能更新 | 大型技能仓库下载超时放宽，结构化错误会显示成人可读文案，不直接把 JSON 打到 toast |
| 应用自更新 | 屏蔽 Tauri updater、自更新 endpoint 和 updater artifact，避免应用内检查上游更新 |
| 构建方式 | 保留 macOS Apple Silicon ad-hoc GitHub Actions 构建，当前不做 DMG、公证或自动更新包 |

## Codex 会话相关改动

- 读取 `~/.codex/sqlite/state_5.sqlite`、`session_index.jsonl` 和 `sessions` / `archived_sessions` JSONL。
- 区分 indexed、not indexed、missing file、archived、needs repair 等状态。
- Repair Index 会修复 Codex index 缺失或状态不一致，操作前备份 state、session_index 和 JSONL。
- Move Session 会移动会话到新项目目录，并更新 SQLite `threads.cwd` 和 JSONL `session_meta.payload.cwd`。
- Trim from here 会从指定用户轮次后截断 JSONL，并保留 `.codex-rescue-backup-*` 备份。
- Branch from here 会从指定轮次派生新会话，生成新 UUID、新 JSONL，并写入 SQLite / session_index。
- Trash / Restore / Permanent delete 支持把会话移入回收区、恢复或永久删除。
- Backup Manager 支持列出、恢复、移入 Trash、清空维护备份。
- 项目过滤按 Codex 项目目录聚合会话，显示项目会话数和待修复数。
- Deep Search 可以扫描 Codex JSONL 原文，找隐藏在长对话里的内容。
- 批量模式支持多选 Codex 会话后批量 Repair、Move、Trash，并汇总失败项。

## UI 和可用性改动

- 对话正文使用 `react-markdown` + `remark-gfm` 渲染，支持标题、列表、表格、引用和代码块。
- 搜索高亮仍走纯文本路径，避免 Markdown AST 和关键词高亮互相打架。
- 目录栏跳过 Codex 注入上下文，只保留真实用户请求作为可跳转节点。
- `Trim` 和 `Branch` 只挂在可操作的用户轮次上，避免误切系统上下文或工具输出。
- Move dialog 对长路径做折行和候选目录选择，降低手输路径出错概率。
- 详情页增加 Reveal / Copy Path，便于从 UI 跳到原始会话文件继续人工检查。
- Skills 更新失败时复用结构化错误格式化，避免直接显示后端 JSON。

## 没有做的部分

| 项目 | 当前边界 |
| --- | --- |
| 正式分发 | 目前只有 arm64 ad-hoc 构建，没有 DMG、Developer ID 签名、公证或自动更新包 |
| 后台自动维护 | 没做自动 watcher 或自动修复，所有写操作都需要用户手动触发 |
| 跨工具会话写操作 | Repair / Move / Trim / Branch / Trash 只对 Codex 会话开放 |
| 上游完整工具面 | 清理了当前不维护的旧工具入口，不保证覆盖上游全部 provider / 页面 |
| 应用自更新 | 不接上游 updater，不在应用内检查或安装新版本 |

## 仍保留的 CC Switch 能力

- Claude Code、Claude Desktop、Codex provider 管理。
- 官方登录 / 第三方 relay 切换、tray quick switch。
- Unified MCP、Prompts、Skills 面板。
- Local proxy、failover、usage dashboard、model test。
- WebDAV / S3 config sync。
- Deep Link import。
- 简体中文 / English UI、深浅色主题和 Tauri 桌面壳。

## 构建和下载

### GitHub Actions 自用构建

- Workflow: [Build macOS Ad Hoc](https://github.com/char1eslu/cc-switch/actions/workflows/build-macos-ad-hoc.yml)
- 目标架构：`aarch64-apple-darwin`
- 产物名：`CC-Switch-macOS-arm64-ad-hoc`
- 产物内容：ad-hoc signed `CC Switch.app` zip

如果 macOS 拦截，可以右键打开，或清理 quarantine：

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

Rust/Tauri 完整打包需要本机有 Rust toolchain 和 Tauri 依赖。

## 使用注意

- Codex 会话维护会修改 `~/.codex/sqlite/state_5.sqlite`、`session_index.jsonl` 和 `sessions/**/*.jsonl`。
- Move、Repair、Branch、Trash、Restore 等写操作会尽量先创建 `.codex-rescue-backup-*` 备份。
- 操作同一个会话前建议关闭正在运行的 Codex 进程，避免 SQLite/WAL 或 JSONL 同时写入。
- 这个 fork 是自用分支；需要稳定跨平台发布包时，优先看上游 [farion1231/cc-switch](https://github.com/farion1231/cc-switch)。

## 致谢

- 上游项目：[farion1231/cc-switch](https://github.com/farion1231/cc-switch)
- 相关项目：[nClear/codex-wake](https://github.com/nClear/codex-wake)

## License

MIT. Upstream copyright belongs to the original CC Switch authors.
