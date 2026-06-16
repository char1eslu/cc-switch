<div align="center">

# CC Switch - Codex Wake Fork

### Claude / Codex 桌面管理器，带 Codex Wake 会话维护能力

[![Fork branch](https://img.shields.io/badge/fork-dev-blue)](https://github.com/char1eslu/cc-switch/tree/dev)
[![Upstream](https://img.shields.io/badge/upstream-farion1231%2Fcc--switch-lightgrey)](https://github.com/farion1231/cc-switch)
[![macOS arm64 ad hoc](https://github.com/char1eslu/cc-switch/actions/workflows/build-macos-ad-hoc.yml/badge.svg?branch=dev)](https://github.com/char1eslu/cc-switch/actions/workflows/build-macos-ad-hoc.yml)

</div>

> 这是 `farion1231/cc-switch` 的个人 fork，不是上游官方发布页。
> `dev` 分支现在聚焦 Claude Code、Claude Desktop 和 Codex，并把一部分 Codex Wake 的会话维护能力接进 CC Switch 的 Session Manager。

## 这个 fork 不一样的地方

这个分支不再做“大而全”的多工具入口，已经删掉当前维护范围外的配置、服务、页面入口、测试入口和图标资源。保留下来的主线是：

| 方向       | 变化                                                                                                    |
| ---------- | ------------------------------------------------------------------------------------------------------- |
| 应用范围   | 聚焦 Claude Code、Claude Desktop、Codex                                                                 |
| Codex Wake | 把 repair、move、trim、branch、trash、restore、backup、项目过滤、深度搜索和批量维护接进 Session Manager |
| 会话预览   | Codex 对话支持 Markdown/GFM 渲染，并过滤环境注入噪音                                                    |
| 入口清理   | 移除旧工具的 provider、MCP、Settings、Usage、Session、安装入口                                          |
| 构建定位   | 保留自用 macOS Apple Silicon ad-hoc workflow                                                            |

## 已搬过来的 Codex Wake 功能

这些功能在 `Sessions` 页面里使用，危险操作主要只对 Codex 会话开放。

| 功能               | 状态   | 说明                                                                                                   |
| ------------------ | ------ | ------------------------------------------------------------------------------------------------------ |
| Codex 会话扫描     | 已实现 | 读取 `~/.codex/sqlite/state_5.sqlite`、`session_index.jsonl` 和 `sessions` / `archived_sessions` JSONL |
| 状态识别           | 已实现 | 区分 indexed、not indexed、missing file、archived、needs repair 等状态                                 |
| Repair Index       | 已实现 | 修复 Codex index 缺失或状态不一致，操作前备份 state、session_index 和 JSONL                            |
| Move Session       | 已实现 | 移动会话到新的项目目录，并更新 SQLite `threads.cwd` 和 JSONL `session_meta.payload.cwd`                |
| Trim from here     | 已实现 | 从指定用户轮次后截断 JSONL，并保留 `.codex-rescue-backup-*` 备份                                       |
| Branch from here   | 已实现 | 从指定轮次派生新会话，生成新 UUID、新 JSONL，并写入 SQLite/session_index                               |
| Move to Trash      | 已实现 | 将会话移入 `~/.codex/.codex-wake-trash/threads/`，并从 active index 移除                               |
| Restore Trash      | 已实现 | 从 Trash manifest 恢复会话文件、SQLite row 和 session_index entry                                      |
| Permanent delete   | 已实现 | 删除 Trash 里的会话目录，带路径边界校验                                                                |
| Backup Manager     | 已实现 | 列出、恢复、移入 Trash、清空 Codex Wake/Rescue 备份                                                    |
| 项目过滤           | 已实现 | 按 Codex 项目目录聚合会话，显示项目会话数和待修复数                                                    |
| Deep Search        | 已实现 | 除元数据搜索外，可直接扫描 Codex JSONL 原文命中隐藏在长对话里的内容                                    |
| 批量维护           | 已实现 | 多选 Codex 会话后批量 Repair、Move、Trash，逐个执行并汇总失败                                          |
| Reveal / Copy Path | 已实现 | 会话详情可复制路径或在 Finder/文件管理器中定位原始 JSONL                                               |
| Prompt/title 清理  | 已实现 | 避免把 AGENTS、`<environment_context>`、VS Code context 当成真实标题                                   |

## 对话管理已经细化的部分

- 对话正文用 `react-markdown` + `remark-gfm` 渲染，支持标题、列表、表格、引用和代码块。
- 搜索高亮仍走纯文本路径，避免 Markdown AST 和关键词高亮互相打架。
- 目录栏会跳过 Codex 注入上下文，只保留真实用户请求作为可跳转节点。
- `Trim` 和 `Branch` 只挂在可操作的用户轮次上，避免误切系统上下文或工具输出。
- Move dialog 对长路径做了折行和候选目录选择，降低手输路径出错概率。
- Trash、Restore、Backup 操作统一走 Codex Wake 风格的 manifest 和备份路径。
- 左侧列表支持 Codex 项目过滤和状态计数，能直接按项目收拢会话。
- 搜索支持两层：默认查标题/摘要/路径，Deep Search 再扫 JSONL 原文。
- 批量模式不只做永久删除，也能批量修复索引、移动项目、移入 Trash。
- 详情页增加 Reveal，便于从 UI 跳到原始会话文件继续人工检查。

## 还可以继续细化的对话管理

这些还没做完，后续可以继续往 Codex Wake 方向补：

| 方向           | 待做内容                                                                              |
| -------------- | ------------------------------------------------------------------------------------- |
| 操作前预览     | Trim/Branch/Move/Repair 前展示将修改的 JSONL 行号、SQLite row、session_index entry    |
| 会话导出       | 导出单个会话为 Markdown、JSONL、HTML，保留角色、时间、工具调用和项目路径              |
| 会话合并       | 支持把同项目的短会话合并成一个新会话，并生成可回滚备份                                |
| 状态批量清理   | 在现有批量 Repair/Move/Trash 基础上，增加按 missing/index/archived 状态一键筛选和清理 |
| 断点定位       | 从目录或搜索结果跳转时高亮当前轮次，并支持复制该轮次的 resume/branch 信息             |
| 更细的噪音过滤 | 把 app context、permissions、tool schema、长环境块做成可切换显示的折叠块              |
| 跨机器迁移     | 导出会话、index、cwd 映射和备份 manifest，辅助迁移到另一台机器                        |
| 自动健康检查   | 只读扫描 Codex index/JSONL 不一致项，给出修复建议，不自动写入                         |

## 没有实现或不准备搬的部分

| 项目                   | 当前边界                                                                  |
| ---------------------- | ------------------------------------------------------------------------- |
| 完整原生 Codex Wake UI | 没搬 Swift/AppKit 原生应用，只接入 Tauri/React Session Manager            |
| 后台 watcher           | 没做自动监控或自动修复，所有写操作都需要用户手动触发                      |
| 跨工具 Wake 写操作     | Codex Wake 风格的 Repair/Move/Trim/Branch/Trash 只对 Codex 开放           |
| 正式分发               | 目前只有 arm64 ad-hoc 构建，没有 DMG、Developer ID 签名、公证或自动更新包 |
| 旧多工具入口           | 已删除旧工具入口，这个 fork 不再维护这些 provider 和配置路径              |

## 仍保留的 CC Switch 能力

- Claude Code、Claude Desktop、Codex provider 管理。
- 官方登录/第三方 relay 切换、tray quick switch。
- Unified MCP、Prompts、Skills 面板。
- Local proxy、failover、usage dashboard、model test。
- WebDAV / S3 config sync。
- Deep Link import。
- 多语言 UI、深浅色主题和 Tauri 桌面壳。

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

## License

MIT. Upstream copyright belongs to the original CC Switch authors.
