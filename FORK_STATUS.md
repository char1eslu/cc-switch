# Fork 状态与上游同步基线

自用备忘：下次上游大更新时，先读这份文件再动手，避免重复评估和踩已知的坑。

最后更新：2026-08-17

## 同步基线

| 项 | 值 |
| --- | --- |
| 已完整评估到的上游基线 | `1f38c838`（`v3.19.2` 之后） |
| 2026-08-13 上游增量审计 | `c39c9032..1f38c838`；24 个提交，搬 1 个、跳过 23 个（其中 1 个已核实上游 bug 在 fork 中不存在；明细见下方审计表） |
| 2026-08-10 上游增量审计 | `413c09e0..c39c9032`；1 个提交（`c39c9032` Windows WSL 原子替换回退），跳过 |
| 2026-08-08 上游增量审计 | `28529620..413c09e0`；27 个提交，搬 3 个、跳过 24 个（明细见下方审计表） |
| 最近一轮已适配的上游安全修复 | `6b8f3643`（脚本/文件读/响应体上限）+ `format_headers` 白名单 |
| 当前已验证代码 head | `0c1b4327`（`dev`；CI [32041716595](https://github.com/char1eslu/cc-switch/actions/runs/32041716595) 全绿、macOS Ad Hoc [32041958536](https://github.com/char1eslu/cc-switch/actions/runs/32041958536) 构建通过） |

**下次同步从这里开始**：

```bash
git fetch upstream
git log --oneline 1f38c838..upstream/main
```

不要用 `dev..upstream/main` 统计差异：选择性同步历史会夸大提交数。
用 `1f38c838..upstream/main` 才是真实增量。

## 2026-08-17 Codex 26.810 会话兼容（fork 侧，非上游提交）

**起因**：Codex Desktop 26.810 改了会话存储——子代理线程在 `threads` 表有独立行
但只靠 `thread_spawn_edges` / `thread_source` / `source` JSON 标记归属；项目归属迁到
`~/.codex/.codex-global-state.json`（原生 assignments + 侧栏排序）；会话列表还出现
只有内部事件、无用户事件的行。fork 的会话管理随之出现：子代理被当独立会话列出、
Move 不生效（只改 DB/JSONL，原生项目状态不动，Codex 桌面端刷新后被弹回）、
删除/恢复遗漏新库与新列。

**来源**：修复先在 Codex Keeper（codex-wake 自 fork，Swift）完成并验证
（`696b59f`，含隔离兼容测试套件），本次按同一契约移植到 cc-switch 的 Rust
会话管理。核心提交：`0c1b4327`（`dev`）。

改动要点（全部在 `src-tauri/src/session_manager/providers/codex.rs`）：

1. **子代理识别四路信号**：`thread_spawn_edges.child_thread_id`、
   `threads.thread_source = 'subagent'`、`threads.source` JSON 里的
   `subagent.thread_spawn`、以及 rollout 文件内容兜底。命中即折叠，
   不独立列出，也拒绝被单独 move/delete/trash（"follow their parent"）。
2. **`needs_repair` 增加 `has_user_event != 0` 前提**：新版给内部线程也建
   `threads` 行，无用户事件的行不再被误报为待修复/待索引。
3. **Move 三端同步**：state DB `threads.cwd` + rollout
   `session_meta.payload.cwd` / `turn_context` + `.codex-global-state.json`
   （写 `thread-project-assignments`、迁 `sidebar-project-thread-orders`、清
   projectless/workspace-hint/output-dir）。目标项目必须已在 Codex 注册
   （预检报错而非半移动），任一步失败整体回滚。
4. **Trash/Restore manifest v2**：`relatedRows` / `externalDatabases` 逐行保存
   **列名 + 任意类型值**（Null/Integer/Real/Text/Blob），Codex 以后加列不用改代码。
   覆盖 `codex-dev.db`（`local_thread_catalog` 行 + `catalog_revision` 递增）和
   `codex-history-snapshots-dev.db`（`app_server_history_snapshots`）。
   恢复时还原原生项目状态（assignment、侧栏原位置、projectless 标记、
   workspace hint、output dir）。
5. **引用行清理/恢复泛化**：运行时探测每张表的
   `thread_id` / `parent_thread_id` / `child_thread_id` / `assigned_thread_id`
   列（`assigned_thread_id` 置 NULL 而非删行），不再写死表名清单。
6. **SQLite 备份改用 rusqlite online Backup API**：单文件一致快照，
   不再裸拷 `state_5.sqlite-wal` / `-shm`（旧法在 WAL 活跃时可能拷出不一致状态）。

新增读写的文件：`~/.codex/.codex-global-state.json`、
`~/.codex/sqlite/codex-dev.db`、`~/.codex/sqlite/codex-history-snapshots-dev.db`。

本轮踩到 / 值得记下的约束：

- **`.codex-global-state.json` 现在是 delete/move/trash 的硬依赖**：文件缺失
  直接拒绝操作（与 Codex Keeper 行为一致）。比这更老的 Codex 版本没有该文件时，
  这些写操作会报错——属于有意为之，避免在状态不同步的情况下盲改。
- **v1 废纸篓 manifest 仍可恢复**：新字段（relatedRows/externalDatabases/
  projectState）缺省按空处理，只是不带新快照能力。
- **测试卫生（已踩）**：delete/move 的测试必须把会话放在
  `<tmp>/sessions/`（或 `archived_sessions/`）下并把**该目录**作为 root 传入；
  否则 `codex_home_for_session_root` 判不出 codex home，回落到真实 `~/.codex`
  （或被设置覆盖的目录），测试会读写真实数据。旧的
  `delete_session_removes_jsonl_file` 就是这样静默读到真实库的
  "Codex state row not found"。
- `local_thread_catalog_metadata.catalog_revision` 的递增逻辑：删除和恢复各 +1
  （触发 Codex 桌面端刷新会话目录）。兼容测试断言 4→5→6。

验证：

- Rust 全量 **1606 测试通过**（含 3 个移植自 Codex Keeper 兼容套件的新测试：
  子代理折叠、移动三端同步含原生侧栏、trash/restore 全链路含 catalog revision）。
  Clippy 零警告，rustfmt 已应用。前端无改动。
- CI [`32041716595`](https://github.com/char1eslu/cc-switch/actions/runs/32041716595) 全绿；
  arm64 Ad Hoc [`32041958536`](https://github.com/char1eslu/cc-switch/actions/runs/32041958536)
  构建通过，artifact `CC-Switch-macOS-arm64-ad-hoc` 11,463,215 bytes。
- 本机验证用 `/private/tmp` 一次性 Rustup 工具链完成，结束后已删除（不污染长期环境）。

## 2026-08-15 Codex 模型与恢复保护

| 提交 | 内容 |
| --- | --- |
| `81d1a596` | 修正 Grok 4.5 缓存定价，并补 Grok 4.6 与 DeepSeek 别名 |
| `3e3d4ef5` | `ultra` 按直连、DeepSeek、low/high 与 OpenRouter 模式分别保留或降级 |
| `02a6bda4` | proxy takeover 恢复时保留官方 ChatGPT 登录，不再被第三方 Codex 配置覆盖 |
| `9029c0ab` | Codex 自定义模型支持逐模型 `reasoningLevels` 与 `defaultReasoningLevel`；model catalog、前端编辑与 camelCase / snake_case 读取形成闭环 |
| `981652fc` | 应用 CI 给出的 3 处 rustfmt 结果；这是本轮经过完整 CI 与 macOS 构建的代码 head |

验证：

- CI [`31893969336`](https://github.com/char1eslu/cc-switch/actions/runs/31893969336)：Rust fmt、Clippy、后端测试、TypeScript、前端格式与 306 个前端测试全部通过。
- macOS arm64 Ad Hoc [`31894246576`](https://github.com/char1eslu/cc-switch/actions/runs/31894246576)：bundle、ad-hoc 签名与 artifact 上传通过。
- Artifact：`CC-Switch-macOS-arm64-ad-hoc`，11,382,379 bytes；核对时未过期。
- `f2165eec` 及后续提交仅更新文档，因此仍以 `981652fc` 作为已验证代码 head。

**搬运前先核实 fork 是否已有该实现，以及上游那个 bug 在 fork 里是否真的存在。**
2026-08-08 那轮 27 个提交里有 2 个（`9db9c56f` Chat tool call 报错、`eb356e15`
SKILL.md 锚点）先判断为"值得搬"，cherry-pick 时才发现 fork 早已有完整实现
（连 7 个测试都在），白做了两次冲突解决。2026-08-13 的 `967daa1a` 则相反：
上游 bug 的前提（信任 DB 缓存哈希）在 fork 里不成立，搬过来是无效改动。
判断依据不能只看 commit message，要 grep 核心符号和测试函数名，并读 fork
对应函数确认缺陷前提成立。

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

## 数据库版本与上游对齐，fork 迁移单独记账

| | SCHEMA_VERSION |
| --- | --- |
| 本 fork | **16** |
| 上游 (`v3.19.2` 之后 `413c09e0`，2026-08-08 复核) | 16 |

fork 曾经占用 `PRAGMA user_version` 17–19；现在已停止这种做法。启动时若检测到
历史 fork v17–19 数据库，会在事务内补回官方 v16 所需的兼容列、`profiles` 表
及 `proxy_config` 约束，然后把 `user_version` 规范化为 16。识别逻辑同时覆盖
早期转换留下的“proxy_config 已兼容、MCP/Skills 仍为双应用列”的部分规范化 v19。
fork 自身版本写在
`settings.fork_schema_version`，不再与上游迁移号冲突。

兼容列和空表不表示恢复了对应功能：fork 仍只维护 Claude Code、Claude Desktop、
Codex；Gemini/GrokBuild/OpenCode/Hermes 的列默认值为 0，业务代码不读取。

上游将来推进到 17+ 时，按正常上游迁移号适配；fork 独有结构只递增
`fork_schema_version`。不要再次为 fork 私有改动提升 `PRAGMA user_version`。
也不要用手工 `PRAGMA user_version=16` 代替结构迁移。

### 2026-08-04 数据库兼容改造验证

- 核心提交：`4553aaa3`（官方 v16 结构）、`309cbb51`（收紧旧 fork 识别）、
  `3a9c2a73`（部分规范化 v19）、`ba9e37c2`（最终 rustfmt）。
- 最终 CI：[`30911059851`](https://github.com/char1eslu/cc-switch/actions/runs/30911059851)，
  前端 typecheck / format / unit tests、Rust fmt / Clippy / tests 全部通过。
- 最终 arm64 Ad Hoc 构建：[`30911416458`](https://github.com/char1eslu/cc-switch/actions/runs/30911416458)，
  thin arm64、Ad Hoc 签名有效。
- 安装前用最终构建二进制在隔离 HOME 中迁移真实数据库副本：
  `user_version 19 -> 16`、`fork_schema_version=1`、`integrity_check=ok`；
  providers / MCP / skills / request logs / rollups 计数迁移前后完全一致。
- 真实库迁移后再次确认完整性、核心表计数和本地代理健康；安装前备份保留在
  `~/.cc-switch/backups/`。
- v15 -> v16 保持官方语义：清理可从 JSONL 重建的 Codex session usage；
  Gemini/GrokBuild 仅保留官方兼容占位行，默认重试值分别为 5/3。

另注：官方共享结构仍走 `while version < SCHEMA_VERSION`；fork 兼容列
则必须由幂等的 `ensure_upstream_schema_compatibility` 补齐，不能只写在
`migrate_v0_to_v1` 里（那条只对 v0/全新库执行）。否则 DAO 的 SELECT 会因缺列
整体失败，界面数据看起来像是丢了。

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

### ⚠️ Claude Desktop MCP：已尝试并回退，不要再做一遍

**结论：gateway 接管模式下做不成，上游也没做。别再尝试。**

cc-switch 用独立 3P 实例接管 Claude Desktop，profile
（`Claude-3p/configLibrary/<uuid>.json`）里写 `inferenceProvider: "gateway"`。
gateway 模式下 Desktop 从 managed config 读 MCP，日志固定输出
`Credentials loaded from managed config { provider: 'gateway', mcpServerCount: 0 }`，
而 `claude_desktop_config.json` 里的 `mcpServers` 被整体忽略并记为
`Skipped invalid MCP server config entries`。

验证过的事实：

- 写 stdio 格式（`npx mcp-remote` 桥）→ 被拒
- 写原生 HTTP 格式（`{type,url,headers}`，与用户原版 Desktop 里能用的完全一致）→ 同样被拒
- 用户原版 Desktop 目录**没有** configLibrary profile，不走 gateway，所以本地
  `mcpServers` 生效——这才是"原版能用、3P 不能用"的真正原因，不是格式问题
- 上游 cc-switch 的 profile 同样只有推理字段（`inferenceGateway*` / `inferenceModels` /
  `inferenceProvider`），**完全没有 MCP 处理**

若真要做，唯一可能的方向是把 MCP 写进 profile 的 managed config，但上游没有先例，
需要逆向 Desktop 的 profile schema。当前判断投入产出不值。

回退提交：`664685b1`（及后续 CI 修复）。历史 DB 列已回退；官方兼容字段与
该不可行功能无关。

## 已知的隐性约束（改动前先看这里）

1. **凡是前端按 `AppId` 索引的 Rust 结构，字段名必须 `#[serde(rename)]` 成连字符。**
   前端一律用 `apps[app]`，其中 `app` 来自 `AppId`（`"claude-desktop"`）；
   Rust 字段是下划线，不加 rename 就序列化成 `claude_desktop`，前端读到
   `undefined`。症状很误导：**图标点不动、编辑框勾不上，但数据其实写进了库**
   ——因为 toggle 命令走 `AppType::from_str`，那条路径认连字符。
   已踩过（在已回退的 Desktop MCP 上）。参照 `AppType` / `McpConfig` /
   `PromptConfig` 的写法：`rename` + `alias = "claudeDesktop"` +
   `alias = "claude_desktop"`。

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

## 2026-07-30 同步审计（`934a2d03..c0ff89b9`）

### 已搬运或按 fork 结构适配

| 上游提交 | fork 提交 | 处理 |
| --- | --- | --- |
| `c98913df` | `2c035726` | SQL 导入拒绝跨文件语句，patch-equivalent |
| `35486afd` | `4ba0f254` | terminal cwd 使用 POSIX 单引号转义，patch-equivalent |
| `cd17912f` | `72ca87b8`, `e7d85ad0` | 防止 common config walker 触碰 `Object.prototype`，并补回冲突遗漏的 Codex model TOML 转义 |
| `6dbb944b` | `7ca36906` | deeplink 风险分级 helper，patch-equivalent |
| `a443eae9` | `064e543b` | 导入确认显示 MCP args/env 并标记风险，按两语言和三应用结构适配 |
| `19bf236e` | `f62f53ad` | URL-safe Base64 解码，patch-equivalent |
| `cfa90f39` | `53615055` | usage scripts 默认禁用并显示代码，按 fork 结构适配 |
| `ff3bc242` | `4f96131d` | 只搬适用的协议桥 panic、Codex MCP 非表 panic、skill zip-slip；跳过已裁剪应用部分 |

### 已评估并跳过或延期

| 上游提交 / 范围 | 结论 |
| --- | --- |
| `708b3879`, `2b2f2cfa`, `414b7150` | 上游正式发布、updater 和 R2 镜像链；fork 只做手动 ad-hoc artifact |
| `12b972a6` | models.dev 自动定价同步涉及 20 个文件和独立持久化架构；非当前需求，延期 |
| `87b0e3fb` | 仅修上游 ZIP 测试的 TMPDIR 并发隔离；fork CI 未出现对应 flaky failure，延期 |
| `56fb46c0` | Codex parent rollout timeline cache 是大型性能改造，fork 的 usage 实现差异很大，延期 |
| `f5f4281d` | 上游为 8 个应用永久改成 icon-only；fork 只有 3 个应用，保留名称并按宽度自动收起更易用 |
| `6b13d018`, `3b9d0593`, `c0ff89b9` | 上游 `v3.19.0` 版本号、CHANGELOG 和发行说明；会误报已裁剪功能，跳过 |
| sponsor / preset / Gemini / Grok Build / OpenClaw-only commits | 超出 fork 的三应用裁剪边界 |

## 2026-08-08 同步审计（`28529620..413c09e0`，27 个）

### 已搬运

| 上游提交 | fork 提交 | 处理 |
| --- | --- | --- |
| `6b8f3643` | `053859b9` | 4 个安全上限：usage_script 加 5s 中断 / 16 MiB 内存 / 256 KiB 栈；`model_catalog_json` 路径限制在预期目录内 + 32 MiB 上限；proxy 响应体与解压 128 MiB 上限（压缩炸弹）；deeplink 导入前展示 usageAccessToken / usageUserId。剔除 `session_usage_grokbuild.rs`，i18n 只取 en/zh。附带 CI/release 的 pnpm 改用 `corepack install` 从 `packageManager` 读版本 |
| `413c09e0` | `1722c1a8` | 生成 catalog 时尊重用户手写的 `model_catalog_json`，patch-equivalent |
| `40b6376b` | `1cf8517a` | skill `readme_url` 从解析后的源目录构建（新增 `choose_doc_path` / `doc_path_for_source`），修 nested path 404 |

### 搬运过程中发现并修复的 fork 自身问题

| fork 提交 | 问题 |
| --- | --- |
| `abf1f953` | 解 `forwarder.rs` 冲突时把上游整块照单保留，误带入 `validate_responses_success_response` / `validate_responses_stream_start`。二者依赖 fork 从未搬过的流式预检地基（`inspect_responses_json_document` / `inspect_responses_start_event` / `responses_error_envelope_message`），且无任何调用方 → 7 个 E0425/E0433。核实后删掉 109 行死代码，而非为死代码补地基 |
| `6394ebed` | **凭证泄漏**：搬 `6b8f3643` 时带进了 `format_headers` 的回归测试却漏了实现，测试因此暴露出 fork 的 `format_headers` 无条件输出所有响应头的值——`set-cookie` 的 session、`authorization` 的 token 明文进日志。改为上游的白名单设计（只有 content-type / content-encoding / content-length / retry-after / cf-ray / x-request-id / request-id / x-correlation-id 及 `x-ratelimit-*` / `ratelimit-*` 前缀输出值），并加 160 字符截断 |

### 已核实 fork 已有实现，无需搬运

| 上游提交 | 核实结论 |
| --- | --- |
| `9db9c56f` | Chat tool call 被丢弃时报 failed 而非假装 completed。核心标识 `upstream_tool_call_dropped` 在 `streaming_codex_chat.rs:659`，7 个测试全部存在，`transform_codex_chat.rs` 的 cherry-pick 暂存 diff 为空 |
| `eb356e15` | 源目录按 SKILL.md 锚点解析。fork 的 `resolve_skill_source_dir` 已是 `direct.is_dir() && direct.join("SKILL.md").is_file()` 三步结构，含两个 ast-grep wrapper 负例测试 |

### 明确跳过

| 上游提交 | 跳过原因 |
| --- | --- |
| `59a2bd10`, `baf07a27` | Codex usage 计费大改（558 / 347 行）；fork 的 usage 实现差异大，此前已延期过 |
| `668bbda9` | backup 性能改造 1121 行，纯性能收益、风险高 |
| `9f19d8fd` | 搜索列表 + 批量应用开关，5284 行新功能 |
| `0cb6e014`, `968794e3`, `492245dc` | UI 改动；`AppSwitcher` 已按三应用改过，冲突面大而收益低 |
| `f38722a4` | Qwen3.8 Max 定价 1 行；自用不关心成本统计 |
| `13ea497a` | GitHub Copilot 兼容现代 Claude Code。**已核实 fork 完全没有 Copilot**（Rust / 前端 0 引用，`copilot_auth.rs`、`copilot_model_map.rs` 均不存在） |
| `3c1154be` | 删除废弃死代码。**已核实 9 个待删文件里 8 个 fork 早已不存在**，唯一残留 `useCustomEndpoints.ts` 已是 0 引用孤儿，不需要靠上游提交来删 |
| `a354f08a` | 补 9 个翻译 key。前 2 个是 GrokBuild 表单专用（已裁）；后 6 个只是把硬编码中文 `defaultValue` 换成正式条目，fork 的 `defaultValue` 兜底已能显示，且改 4 语言文件（fork 无 ja / zh-TW） |
| `0345fad6`, `92ca95ff` | OpenCode / OMO，已裁 |
| `83830767` | Hermes，已裁 |
| `290b65c0`, `5b697abc`, `0e604b75`, `4d3e2c35`, `996d512f`, `ebbf141f` | 赞助商预设 / 推荐链接 / README |
| `43eaf073`, `425e932b`, `fbf52cff`, `a4bba43f` | `v3.19.2` 版本号、发行说明、指南文档；会误报已裁剪功能 |

## 2026-08-10 同步审计（`413c09e0..c39c9032`，1 个）+ 定价补缺

### 上游增量

| 上游提交 | 跳过原因 |
| --- | --- |
| `c39c9032` | fix(windows): WSL 拒绝原子替换时的回退。不用 Windows + 裁剪边界外 |

无搬运。

### 定价补缺（fork 侧，非上游提交）

按实时 usage 库（`~/.cc-switch/cc-switch.db`）核查 `proxy_request_logs`，找出
"有请求、无定价"的型号补进 `seed_model_pricing`：

| model_id | 定价（in/out/cr/cc） | 依据 |
| --- | --- | --- |
| `claude-opus-5` | 5/25/0.50/6.25 | 官方 $5/$25 + Opus 档惯例；历史 1.8 亿 token 此前全 $0 |
| `grok-4.5-build-free` | 2/6/0.30/0 | 对齐上游 `grok-4.5-build` 的 costUsdTicks 实测（build 档 $0.30，非 API 挂牌 $0.50） |
| `xopglm52` | 1.4/4.4/0.26/0 | 用户中转别名 = GLM 5.2，镜像 live 库已学到的 glm-5.2 |

提交：`e9272739`（补 3 价）、`11e74cd1`（rustfmt 修超宽行）、`52719096`
（grok-4.5-build-free 改上游实测 $0.30，撤回误改主档 grok-4.5）。
CI 全绿 + arm64 Ad Hoc 构建通过（runs 31370842112 / 31371115760 / 31373331309 / 31373612158）。

### 核查中确认的非 bug（避免重复踩）

- **Claude 短名（`claude-haiku-4-5` / `claude-sonnet-4-6`）"无定价"不是规范化问题**：
  `find_model_pricing_row` 的前缀匹配（`should_try_pricing_prefix_match` 对
  `claude-` 且 dash≥3 启用）本就能命中带日期条目。那些 0 成本行全是失败请求
  （503/429/502，无 token），`pricing_model` 为空是 token 未采集的连带症状。
- **上游 grok 定价是实测反推、非纯官方文档**：上游读 Grok CLI OAuth 上报的
  `costUsdTicks`（1 tick = 1e-10 USD）反算，build 档实测 cache_read $0.30
  （API 挂牌 $0.50），主档 `grok-4.5` 保留挂牌 $0.50（未实测主档）。fork 对齐
  此分档，不要把主档也改成 $0.30（无实测依据的分叉）。

### 未做（已与用户确认）

- 历史 Opus 5 约 1.8 亿 token（≈ $395）**不回填**：dashboard 成本是写入时
  存死的（`SUM(total_cost_usd)`，不重算），补价只对未来流量生效。回填需一次性
  重算 `proxy_request_logs` + `usage_daily_rollups` 的 cost 列，用户选择不做。

## 2026-08-13 同步审计（`c39c9032..1f38c838`，24 个）

### 已搬运

| 上游提交 | 处理 |
| --- | --- |
| `1f38c838` | 智谱把国内端点的配额条目类型从 `TOKENS_LIMIT` 改名为 `CREDIT_LIMIT`，原判断只认前者 → 所有档位被 `continue` 跳过、用量面板整体空白（上游 issue #6153）。改为两个名字都认（仍大小写不敏感）。用户实际在用智谱网关，属于会真实触发的线上故障。上游未加测试，fork 补 `zhipu_accepts_credit_limit_type`（两种类型名混用 + `unit` 显式分窗） |

### 已核实上游 bug 在 fork 中不存在

| 上游提交 | 核实结论 |
| --- | --- |
| `967daa1a` | 上游 `check_updates` 先信任 DB 缓存的 `content_hash` 再看磁盘，换机恢复库备份后 SSOT 目录已丢而缓存仍在 → 误报「无更新」，缺失被永久掩盖。**fork 无此路径**：`compute_local_git_tree_hash`（`skill.rs:2251`）每次实地重算、从不读 `content_hash`，目录不存在直接返回 `None`，天然进入更新列表。上游同时引入的 `require_valid_directory` 亦非 fork 所需——fork 在 `install` 阶段就把 `directory` 经 `sanitize_skill_source_path` + `sanitize_install_name` 规范成单段名后才写库（`skill.rs:594`、`604`），入库值已不可能含 `..` 或分隔符 |

### 明确跳过

| 上游提交 | 跳过原因 |
| --- | --- |
| `580a4d7b` | Hermes 表单层级，已裁 |
| `ec842156`, `076c2744` | OpenCode / Hermes / OpenClaw 表单；`ProfileSwitcher.tsx` 在 fork 中不存在 |
| `5b77da2b`, `95b95da6` | OpenClaw User-Agent 与模型编辑器，已裁 |
| `7de63227`, `bef46cd5` | GrokBuild，已裁 |
| `16cc0d7f`, `390102a2` | OpenCode Go 预设路由与 DeepSeek contextWindow；OpenCode 已裁 |
| `58d92e56`, `3711e1a0` | JieKou AI / PPIO 供应商预设，赞助商内容 |
| `7e5007d5` | Claude Desktop 模型配置模式文案澄清：675 行表单重排 + 4 语言文案（fork 无 ja / zh-TW），纯 UI 措辞，冲突面大收益低 |
| `619a592c`, `8673e9d8` | Claude / Claude Desktop 表单边框与高级选项对齐，纯样式；fork 表单已按三应用改过 |
| `ccc86298` | 代理路由激活动画，新增 `RoutingActivationBrand.tsx` 164 行装饰性组件 |
| `c0050623` | checkbox 样式统一，纯视觉 |
| `bc7f5f41` | 供应商编辑器留白收紧，涉及 Gemini / GrokBuild 表单（已裁） |
| `7e152d75` | 模型映射下拉模糊搜索，改 4 语言文案，非当前需求 |
| `3c592d93` | Windows WiX 注册表键转义；不用 Windows |
| `ceef0a52` | Windows 后端测试跑在 WSL2 文件系统上（CI + nightly workflow）；不用 Windows，此前同类提交已跳过 |
| `c98cc3a9`, `36ed280d` | 上游 CI 分区跳过与 i18n labeler glob；`.github/labeler.yml` 在 fork 中不存在 |

### Skill 备份保留策略改造（fork 侧，非上游提交）

**动机**：更新 Skill 前会自动备份（`create_uninstall_backup`，`update_skill`
里也调），旧策略是全局「最多 20 个目录」，与体积无关。实测
`~/.cc-switch/skill-backups/` 已 105 MB / 20 个，正好卡在上限——即已在持续
删除，但两个问题：

1. **不分 Skill**：高频更新的小 Skill（`academic-*` 系列各 ~1 MB）挤占额度，
   把低频更新的大 Skill 备份推出去。实测 `nature-figure`（34 MB）
   与 `academic-research-suite`（30 MB）各只剩 1–2 代。
2. **无体积约束**：20 个大备份合计可轻松到数百 MB。`Bizard`（56 MB）
   若进入更新循环，仅它 3 代就 168 MB。

**改法**（`SKILL_BACKUP_RETAIN_PER_SKILL = 3` +
`SKILL_BACKUP_TOTAL_SIZE_LIMIT_BYTES = 1 GiB`）：

- 按 `meta.json` 的 `skill.directory` 分组，每个 Skill 各留最新 3 代；
- 分组裁剪后若总体积仍超 1 GiB，全局按最旧优先删，但**至少留 1 个**
  （单个备份自身超限也不删，否则「更新前已备份」静默失效）；
- 排序键用 `meta.json` 的 `backup_created_at`，不用目录 mtime——
  `copy_dir_recursive` 之后才写 `meta.json`，且恢复/拷贝会重置 mtime，
  按 mtime 判定会删错代；
- `meta.json` 不可读的目录归入同一孤儿组（`SKILL_BACKUP_ORPHAN_GROUP`）。
  这类目录 `list_backups` 会跳过、界面不可见也无法恢复，若按目录名各自成组
  则每组只有 1 个、永远够不到 3 代上限 → 永不回收。

体积统计用 `symlink_metadata` 不解引用符号链接；单项失败只跳过该项，
不让整轮裁剪失败（清理是旁路操作，失败不应阻止后续增长被控制）。
实测全量 stat 4376 个文件耗时 0.07s，每次备份后跑一次可接受。

5 个单测覆盖：分组独立留 3 代、按 `backup_created_at` 而非 mtime 排序、
孤儿组归并、体积上限最旧优先、绝不删最后一个。体积相关两例通过
`cleanup_old_skill_backups_with_limit` 注入小上限，避免造 1 GiB 载荷。

**新策略只经单测验证，未在真实备份目录上跑过。** 清理只在
`create_uninstall_backup` 末尾触发（卸载或更新 Skill 时），装新构建后不会立即
生效。改造时本机现状：20 个目录 / 105 MB，正卡在旧的 20 个上限上。首次更新
任一 Skill 后应观察 `academic-*` 系列是否收敛到各 3 代。

## 未完成 / 待验证

- **Codex ↔ Anthropic 协议桥：转换 payload 已验证，应用内链路仍未跑过。**

  2026-07-27 对真实网关（智谱 `open.bigmodel.cn/api/anthropic`，模型 `glm-5.2`）
  验证了 7 个场景，全部返回 200：非流式、流式 SSE（事件序列完整到
  `message_stop`）、工具调用（`stop_reason: tool_use` 且参数正确）、`[1m]`
  标记剥离、空 text block 过滤、`tool_result` 多轮回传、`cache_control` 注入。

  **但这是用 Python 按 `transform_codex_anthropic.rs` 的逻辑复现 payload 测的**，
  验证的是「转换后的请求能被 Anthropic 网关接受、响应能被正确解析」。
  Rust 实现与复现之间若有偏差，这个测试发现不了。

  仍待验证：在应用里真配一个 `Anthropic Messages` 格式的 Codex 供应商，用
  Codex 实跑一轮，走完整的 Rust 转换链路。

  另：缓存未命中（`cache_creation=0 / cache_read=0`），可能是该中转不支持
  prompt caching，也可能是测试 prompt 未达 Anthropic 的 1024 token 缓存下限。
  这条没验证成功，但只影响成本，不影响功能。
- **Claude Desktop MCP 已回退，不再是待办。**
  详见上方「Claude Desktop MCP：尝试过并已回退」。gateway 接管导致本地
  `mcpServers` 被忽略，与格式无关，上游同样不支持。

## 验证手段

本机默认不保留 Rust toolchain；后端改动以 GitHub CI 为最终验证，若临时在本机
验证，必须使用隔离目录并在完成后删除 Rustup/Cargo/target 缓存：

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
