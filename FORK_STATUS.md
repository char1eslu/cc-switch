# Fork 状态与上游同步基线

自用备忘：下次上游大更新时，先读这份文件再动手，避免重复评估和踩已知的坑。

最后更新：2026-07-26

## 同步基线

| 项 | 值 |
| --- | --- |
| 已搬运到的上游提交 | `878c26f3` (`feat(proxy): extend tool-result media handling to all conversion bridges`) |
| 评估过但决定跳过的上游 head | `934a2d03` |
| 本 fork 分支 | `dev`，当时 head `9ac11113` |

**下次同步从这里开始**：

```bash
git fetch upstream
git log --oneline 878c26f3..upstream/main
```

`878c26f3..934a2d03` 之间的 7 个提交已逐个看过，结论见下方「已评估并跳过」，不必重复。
若上游 head 已超过 `934a2d03`，只需看 `934a2d03..upstream/main`。

## 裁剪边界（决定哪些上游提交天然不用看）

`AppType` 只保留三个（权威定义在 `src-tauri/src/app_config.rs`）：

- `Claude`（Claude Code）
- `ClaudeDesktop`
- `Codex`

**已裁掉**：Gemini、Grok、GrokBuild、OpenCode、OpenClaw、Hermes。

对应地，上游这些文件在 fork 里不存在（举例，非穷举）：
`proxy/providers/gemini.rs`、`proxy/providers/transform_gemini.rs`、
`src/config/openclawProviderPresets.ts`。

凡是只动这些应用的上游提交，一律跳过。赞助商预设、推荐链接、域名刷新同理
（上游商业合作内容，与自用无关）。

## ⚠️ 数据库版本已领先上游，迁移号会撞车

| | SCHEMA_VERSION |
| --- | --- |
| 本 fork | **18** |
| 上游 (`934a2d03`) | 16 |

fork 独有的迁移：

- **v16 → v17**：`mcp_servers` 加 `enabled_claude_desktop` 列（MCP 支持 Claude Desktop）
- **v17 → v18**：清除上游版本留在库里的已裁剪应用遗留物
  - 删列：`mcp_servers` / `skills` 的 `enabled_gemini`、`enabled_opencode`、`enabled_hermes`、`enabled_grokbuild`
  - 删行：`providers` / `proxy_config` 中 `app_type` 属于已裁剪应用的行

**这是最容易踩的坑**：上游若也推进到 17/18，迁移编号会与 fork 的冲突。
届时必须人工处理——把上游的迁移重编号到 19+，或合并进同一个版本步，
不能直接 cherry-pick。合并前先确认 `set_user_version` 的目标值没有重复。

另注：迁移循环是 `while version < SCHEMA_VERSION`。新列**必须**写进版本化迁移分支，
只写在 `migrate_v0_to_v1` 里对已有库无效（那条只对 v0/全新库执行）——
这个坑已经踩过一次，表现为「列没建出来，DAO 的 SELECT 整体失败，界面数据像是全丢了」。

## fork 独有的实现（上游没有，或与上游不同）

### 协议桥

以下文件上游同名存在，但 fork 版本是**取上游 main 现版后按 fork 结构适配**的，
不是逐 commit cherry-pick 的结果——重新同步时不要指望能干净 rebase：

- `proxy/providers/transform_codex_anthropic.rs`
- `proxy/providers/streaming_codex_anthropic.rs`
- `proxy/providers/codex_responses_sse.rs`
- `proxy/providers/reasoning_bridge.rs`

搬运时删掉了上游的 `CodexCatalogToolProfile` 枚举及其 resolver，以及
`codex_responses_sse.rs` 里 3 个 fork 用不到的 SSE 构造器（`reasoning_item`、
`reasoning_close`、`custom_tool_call_input_delta`）。按 fork 风格删除而非
`#[allow(dead_code)]` 掩盖——上游若再动这些符号，注意 fork 里已经没有了。

### Claude Desktop MCP

`src-tauri/src/mcp/claude_desktop.rs` 是**上游没有的新文件**。
写入 cc-switch 管理的 3P 实例配置（`Claude-3p/claude_desktop_config.json`），
读-改-写只动 `mcpServers` 段，其余键（`deploymentMode`、`enterpriseConfig` 等）原样保留。
非 macOS / Windows 平台静默跳过。

## 已知的隐性约束（改动前先看这里）

1. **`McpApps` 的 serde 键名必须是连字符。**
   `claude_desktop` 字段需要 `#[serde(rename = "claude-desktop")]` +
   `alias = "claudeDesktop"` / `alias = "claude_desktop"`。
   前端一律按 `AppId`（连字符）索引 `apps[app]`；少了 rename 就读到 `undefined`，
   表现为「图标点不动、编辑框勾不上，但数据其实写进了库」——因为 toggle 命令
   走 `AppType::from_str`，那条路径认连字符。已踩过。

2. **Skills 本地哈希的排序必须与 GitHub tree API 同口径（相对路径字节序）。**
   不能用 `Vec<PathBuf>::sort()`——`PathBuf: Ord` 是逐路径组件比较，
   遇到「目录名是另一文件名前缀」的情况（如 `academic-paper-reviewer/` vs
   `academic-paper/`）与字节序结果不同，导致本地哈希永远对不上远程、
   界面反复提示有更新且无法收敛。已踩过，见 `compute_dir_git_tree_hash`。

3. **写入侧与检测侧的哈希算法必须同一个函数。**
   `compute_dir_hash` 已委托给 `compute_dir_git_tree_hash`。两者一旦分叉，
   同样造成反复提示更新。有单测守着（`install_hash_matches_update_check_hash`）。

4. **指纹伪装默认关闭**（`impersonateClaudeCode`），与上游一致。
   仅在网关限制只能通过 Claude Code 使用时由用户显式开启。

5. **Tauri updater 已屏蔽**：不接上游 updater endpoint，不生成 updater artifact。
   上游动 release / 自更新流程的提交一律跳过。

6. **i18n 只有 `en` / `zh`**。上游是 en/ja/zh/zh-TW 四语言，
   搬运涉及文案的提交时注意 fork 少两个文件。

## 已评估并跳过（`878c26f3..934a2d03`，7 个）

| 提交 | 内容 | 跳过原因 |
| --- | --- | --- |
| `9cf4ae41` | 内置定价表加 Opus 4.8 / 4.7 | 自用不关心成本统计 |
| `b972f0a3` | 默认模型升级 Opus 5 / GPT-5.6 / Gemini 3.6 | 28 文件 460 行，主体是 Gemini/OpenCode/OpenClaw 预设与四语言文案 |
| `bc7c8222` | OpenClaw Kimi base URL 修正 | fork 无 OpenClaw |
| `876e9f89` | 恢复 AICoding 合作伙伴（七种应用） | 赞助商内容 |
| `b0482320` | 刷新赞助商域名与推荐链接 | 赞助商内容 |
| `934a2d03` | 同步赞助商列表到各应用与 README | 赞助商内容 |
| `414b7150` | 发布产物镜像到 Cloudflare R2 | fork 不做正式分发 |

## 未完成 / 待验证

- **Codex ↔ Anthropic 协议桥未做真实端到端验证。**
  已过编译 / clippy / 单测，但从未跑通过一次真实请求。
  验证方式：新建 Codex 供应商 → 上游协议选 `Anthropic Messages` → 填可用网关与模型名 → 用 Codex 实跑。
- **Claude Desktop MCP 已定位根因并修复。**
  根因：Claude Desktop 的 `claude_desktop_config.json` 只接受 stdio 服务器
  （校验 schema `gD` 要求 `command` 字段），HTTP/SSE 远程服务器会被跳过并报
  "not valid MCP server configurations"。修复：`claude_desktop.rs` 同步时把
  HTTP/SSE 服务器包装成 `npx mcp-remote` stdio 桥，headers 以 `--header` 传递。
  待验证：重启 3P Desktop 后远程工具是否出现（依赖本机有 Node.js / npx）。

## 验证手段

本机无 Rust toolchain，后端改动**只能靠 CI 验证**：

```bash
gh workflow run "CI" --ref dev -R char1eslu/cc-switch
gh workflow run build-macos-ad-hoc.yml --ref dev -R char1eslu/cc-switch
```

注意 `gh` 可能解析到 upstream remote，命令要显式带 `-R char1eslu/cc-switch`。

前端格式化必须用项目锁定版本，不要用 `npx prettier`（会拉最新版，
格式化结果与 CI 的 `pnpm format:check` 不一致）：

```bash
pnpm install && pnpm format
```

数据库迁移改动，建议先用真实库的副本干跑验证，确认列/行变化与数据无损。
