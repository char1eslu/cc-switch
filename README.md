<div align="center">

# CC Switch Fork

### 基于上游 `farion1231/cc-switch` 的个人改动分支

[![Fork branch](https://img.shields.io/badge/fork-dev-blue)](https://github.com/char1eslu/cc-switch/tree/dev)
[![Upstream](https://img.shields.io/badge/upstream-farion1231%2Fcc--switch-lightgrey)](https://github.com/farion1231/cc-switch)
[![CI](https://github.com/char1eslu/cc-switch/actions/workflows/ci.yml/badge.svg?branch=dev)](https://github.com/char1eslu/cc-switch/actions/workflows/ci.yml)
[![macOS arm64 ad hoc](https://github.com/char1eslu/cc-switch/actions/workflows/build-macos-ad-hoc.yml/badge.svg?branch=dev)](https://github.com/char1eslu/cc-switch/actions/workflows/build-macos-ad-hoc.yml)

</div>

> 这是 `farion1231/cc-switch` 的个人 fork，不是上游官方发布页。
> 这个分支主要记录相对上游的自用改动，稳定跨平台版本请优先看上游项目。

> 同步上游前先读 [FORK_STATUS.md](FORK_STATUS.md)：记录了同步基线、裁剪边界、
> 数据库版本已与上游对齐；fork 私有迁移独立记账。更多维护边界和已踩过的坑见下文。

## 和上游的主要区别

| 方向 | 这个 fork 的改动 |
| --- | --- |
| 应用范围 | 聚焦 Claude Code、Claude Desktop、Codex，清理了一批当前不维护的旧工具入口 |
| Codex 会话管理 | Session Manager 增加 Codex 会话扫描、状态识别、Repair、Move、Trim、Branch、Trash、Restore、Backup 等维护操作 |
| 会话浏览 | Codex 对话支持 Markdown/GFM 渲染，目录和标题会过滤 AGENTS、环境上下文、工具 schema 等注入噪音 |
| 搜索和批量操作 | 增加项目过滤、状态计数、JSONL Deep Search、多选批量 Repair/Move/Trash |
| 路径操作 | 会话详情支持 Reveal / Copy Path，Move dialog 对长路径和候选目录做了可用性处理 |
| 技能更新 | 修正本地哈希与 GitHub tree 的排序口径，消除反复提示更新；大型仓库下载超时放宽，结构化错误显示可读文案 |
| Codex 上游协议 | 支持只提供原生 Anthropic Messages（`/v1/messages`）的网关，由本地代理做 Responses ⇄ Anthropic 双向转换 |
| MCP 覆盖 | Claude Code 与 Codex 两端；Claude Desktop 因 gateway 接管无法支持，与上游一致 |
| 数据库 | 与上游 v3.19.1 共用 `user_version=16`；仅保留已裁剪应用的空兼容字段，fork 私有迁移独立记账 |
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

## Codex ↔ Anthropic 协议桥

有些网关只暴露原生 Anthropic Messages 协议（`/v1/messages`），Codex 本身只会说 Responses。
开启后由本地代理做双向转换，Codex 侧无感。

- 在 Codex 供应商表单里把「上游协议」选成 `Anthropic Messages`（需开启路由接管）。
- 鉴权字段可选 `ANTHROPIC_AUTH_TOKEN`（发 `Authorization: Bearer`）或 `ANTHROPIC_API_KEY`（发 `x-api-key`），两者只发其一。
- 「模拟 Claude Code 客户端」**默认关闭**，与上游一致。仅在网关限制只能通过 Claude Code 使用时开启：会伪装 User-Agent、`anthropic-beta`、`x-app`，并在系统提示首行注入 Claude Code 身份。
- 输出上限可按供应商覆盖；未设置时回退到保守的 `max_tokens=8192`，思考预算会钳到上限的一半。
- 请求侧会剥离 Codex/OpenAI 的指纹头，`Accept` 归一化为 `application/json`，避免严格网关返回 406。
- 支持流式与非流式；上游返回 2xx 错误信封时仍可触发故障转移。

> 转换后的请求已对真实 Anthropic 网关（智谱 `glm-5.2`）验证过 7 个场景：非流式、
> 流式 SSE、工具调用、`[1m]` 标记剥离、空 text block 过滤、tool_result 多轮回传、
> `cache_control` 注入，全部返回 200 且响应可正确解析。
>
> 但该验证是按转换层逻辑复现 payload 后直接发网关，**没有走应用内的 Rust 代码路径**。
> Rust 实现与复现之间若有偏差不会被这个测试发现——首次在应用里启用时留意首轮请求。

## MCP / Skills 覆盖范围

| 客户端 | MCP | Skills |
| --- | --- | --- |
| Claude Code | 支持 | 支持 |
| Codex | 支持 | 支持 |
| Claude Desktop | 不支持（见下） | 不适用 |

Claude Desktop 由 cc-switch 以独立的 3P 实例接管，profile 里写的是
`inferenceProvider: "gateway"`。gateway 模式下 Desktop 从 managed config 读取 MCP，
`claude_desktop_config.json` 里的 `mcpServers` 会被忽略并记为
`Skipped invalid MCP server config entries`。

这是 gateway 接管的固有代价，上游 cc-switch 同样不支持 3P Desktop 的本地 MCP。
需要在 Desktop 里用 MCP 时，在 Desktop 自己的界面添加，或使用未被接管的原版实例。

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
| Anthropic 协议桥实测 | 转换 payload 已对真实网关验证 7 个场景，但未走过应用内的 Rust 链路 |

## 仍保留的 CC Switch 能力

- Claude Code、Claude Desktop、Codex provider 管理。
- 官方登录 / 第三方 relay 切换、tray quick switch。
- Unified MCP、Prompts、Skills 面板；MCP 同步到 Claude Code 与 Codex。
- Local proxy、failover、usage dashboard、model test。
- WebDAV / S3 config sync。
- Deep Link import。
- 简体中文 / English UI、深浅色主题和 Tauri 桌面壳。

## 构建和下载

当前 fork 的 bundle 版本仍为 `3.16.3`。这里的手动 macOS arm64 ad-hoc
产物不是上游 `v3.19.1` 的官方发布包，也不包含上游已裁剪的应用面。

### GitHub Actions 自用构建

- Workflow: [Build macOS Ad Hoc](https://github.com/char1eslu/cc-switch/actions/workflows/build-macos-ad-hoc.yml)
- 目标架构：`aarch64-apple-darwin`
- 产物名：`CC-Switch-macOS-arm64-ad-hoc`
- 产物内容：ad-hoc signed `CC Switch.app` zip
- 当前已验证代码 head：[`ba9e37c2`](https://github.com/char1eslu/cc-switch/commit/ba9e37c243b4b844ef74e0673b7b631a251e72aa)
- 最终验证：[CI 30911059851](https://github.com/char1eslu/cc-switch/actions/runs/30911059851) / [Build 30911416458](https://github.com/char1eslu/cc-switch/actions/runs/30911416458)

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

- 从本 fork 旧版（数据库 v17–19）首次升级前，先备份整个 `~/.cc-switch`。新版会把
  历史 fork 数据库事务化规范为官方兼容 v16，并在
  `settings.fork_schema_version` 记录 fork 私有迁移版本。迁移不会恢复 Gemini、
  GrokBuild、OpenCode 或 Hermes 的 UI/业务功能。
- 不要只手工修改 `PRAGMA user_version`。数据库兼容还依赖 MCP、Skills、profiles
  和 proxy_config 的结构；直接改版本号可能令官方版或 fork 在启动时拒绝数据库。
- Codex 会话维护会修改 `~/.codex/sqlite/state_5.sqlite`、`session_index.jsonl` 和 `sessions/**/*.jsonl`。
- Move、Repair、Branch、Trash、Restore 等写操作会尽量先创建 `.codex-rescue-backup-*` 备份。
- 操作同一个会话前建议关闭正在运行的 Codex 进程，避免 SQLite/WAL 或 JSONL 同时写入。
- 这个 fork 是自用分支；需要稳定跨平台发布包时，优先看上游 [farion1231/cc-switch](https://github.com/farion1231/cc-switch)。

## 致谢

- 上游项目：[farion1231/cc-switch](https://github.com/farion1231/cc-switch)
- 相关项目：[nClear/codex-wake](https://github.com/nClear/codex-wake)

## License

MIT. Upstream copyright belongs to the original CC Switch authors.
