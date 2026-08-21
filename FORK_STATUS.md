# Fork 状态与上游同步基线

自用备忘：下次上游大更新时，先读这份文件再动手，避免重复评估和踩已知的坑。

最后更新：2026-08-18

## 同步基线

| 项 | 值 |
| --- | --- |
| 已完整评估到的上游基线 | `0b5da510`（`v3.20.0`） |
| 当前已验证代码 head | `c664ad90`（`dev`；本机验证、CI 与 macOS Ad Hoc 构建均通过） |
| 最近一轮已适配的上游安全修复 | `6b8f3643`（脚本/文件读/响应体上限）+ `format_headers` 白名单 |

**下次同步从这里开始**：

```bash
git fetch upstream
git log --oneline 0b5da510..upstream/main
```

- 不要用 `dev..upstream/main` 统计差异：选择性同步历史会夸大提交数。
  用 `0b5da510..upstream/main` 才是真实增量。
- ⚠️ 代码块里的基线值和上表第一行必须一起改。2026-08-18 曾发现两处不一致
  （表写 `1f38c838`，审计节已到 `a98829ba`），按表起算会把 35 个已审提交重算一遍。
- 历轮增量范围与结论见下方「同步审计日志」，从新到旧。

## 同步守则（评估上游提交前先读）

1. **搬运前先核实 fork 是否已有该实现，以及上游 bug 在 fork 里是否真的存在。**
   反面案例三起：`9db9c56f` / `eb356e15`（2026-08-08）先判断值得搬，cherry-pick
   时才发现 fork 早有完整实现，白做两次冲突解决；`967daa1a`（2026-08-13）上游
   bug 的前提（信任 DB 缓存哈希）在 fork 不成立，搬过来是无效改动；`fd14f9c4`
   （2026-08-18）修的是 fork 从未搬过的 #5522 重构引入的回归，fork 无此路径。
   判断依据不能只看 commit message，要 grep 核心符号和测试函数名，并读 fork
   对应函数确认缺陷前提成立。
2. **上游修复项的守卫值必须按 fork 自己的历史值写，不能照抄上游。**
   fork 与上游的种子表历史可能不同步（见 2026-08-18 审计里 deepseek-chat /
   reasoner 的例子：照抄上游守卫值永不命中，老库价格永远不更新）。
3. **不直接整提交覆盖 fork**：按三应用边界逐提交核对后手工适配。
   上游的 ja / zh-TW 文案、赞助商预设、release / updater 流程天然不搬。
4. **跳过上游提交前，先 grep 它是否动 `SCHEMA_VERSION` 或迁移链。**
   `40d747c0` 当 vendor 门控跳过时没人发现它带 16→17 迁移，直到真实库被
   fork 的启动守卫拒绝才暴露。vendor 门控的功能代码可以跳，版本号迁移不行。

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
| 本 fork | **17**（2026-08-21 跟进） |
| 上游（`v3.20.0`） | **17**（`40d747c0` pi 会话统计引入） |

**v17 跟进记录（2026-08-21）。** 真实库因跑过上游构建已落盘 v17，fork 的启动守卫
拒绝打开。上游 v16→v17 迁移只新建 `session_usage_dedup` 去重账本表、不动现有表，
故 fork 照搬该迁移并同步提升 SCHEMA_VERSION：SQL 与上游逐字一致，fork 业务代码
不读写这张表（pi 已裁），但版本号对齐后未来 v18+ 才能干净叠加。新增测试：
`upstream_v17_database_is_accepted`（现场场景：v17 库原样接受）、
`migration_v16_to_v17_creates_session_usage_dedup_ledger`。

验证（`c664ad90`）：真实库副本干跑确认 v17 + dedup 表已在 + integrity ok，
fork 打开零迁移。CI 32518251141 全绿；Ad Hoc 32518253035 构建通过，
artifact `CC-Switch-macOS-arm64-ad-hoc` 11,459,808 bytes。

教训（已入守则）：跳过上游提交前先 grep 是否动 `SCHEMA_VERSION` / 迁移链——
`40d747c0` 当 vendor 门控跳过时没人发现它带版本号迁移，直到真实库被拒才暴露。

fork 曾经占用 `PRAGMA user_version` 17–19；现在已停止这种做法。启动时若检测到
历史 fork v17–19 数据库，会在事务内补回官方 v16 所需的兼容列、`profiles` 表
及 `proxy_config` 约束，然后把 `user_version` 规范化为 16。识别逻辑同时覆盖
早期转换留下的“proxy_config 已兼容、MCP/Skills 仍为双应用列”的部分规范化 v19。
fork 自身版本写在 `settings.fork_schema_version`，不再与上游迁移号冲突。

兼容列和空表不表示恢复了对应功能：fork 仍只维护三个应用；
Gemini/GrokBuild/OpenCode/Hermes 的列默认值为 0，业务代码不读取。

跟进上游 v17 时按正常上游迁移号适配；fork 独有结构只递增
`fork_schema_version`。不要再次为 fork 私有改动提升 `PRAGMA user_version`，
也不要用手工 `PRAGMA user_version=16` 代替结构迁移。

结构性约束（2026-08-04 改造时踩过）：

- 官方共享结构走 `while version < SCHEMA_VERSION`；fork 兼容列必须由幂等的
  `ensure_upstream_schema_compatibility` 补齐，不能只写在 `migrate_v0_to_v1` 里
  （那条只对 v0/全新库执行），否则 DAO 的 SELECT 会因缺列整体失败，界面数据
  看起来像丢了。
- 该轮验证：真实库副本隔离迁移 `19 -> 16`、`fork_schema_version=1`、
  `integrity_check=ok`，providers / MCP / skills / request logs / rollups 计数
  迁移前后一致。核心提交 `4553aaa3` / `309cbb51` / `3a9c2a73` / `ba9e37c2`。

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

7. **`pricing_fixes` 的条目顺序就是迁移顺序，新条目追加在数组末尾。**
   早期条目先把历史形态收敛到同一旧值，末尾新条目才能单守卫命中；挪到前面
   会让老库停在中间价位。有测试锁住顺序（两跳断言），详见 schema.rs 内注释。

## 同步审计日志

> 2026-08-18 起只保留最新一轮 CI / 构建 run，旧轮链接已随 run 删除失效，
> run ID 留作文字记录。

### 2026-08-18（`a98829ba..0b5da510`，11 个）

上游 `v3.20.0` 发布轮。5 个按裁剪边界天然跳过（release / changelog / PPIO 赞助 /
OpenCode），余下 6 个逐一核对。

已搬运：

| 上游提交 | fork 提交 | 处理 |
| --- | --- | --- |
| `bad9c151`（DeepSeek 部分） | `f5207382` | V4 全系峰谷双档调价：种子表 5 个模型 + `pricing_fixes` 末尾追加 5 条。Gemini 3.7 Flash 部分不适用（fork 无 gemini 行）。chat/reasoner 守卫值按 fork 历史单跳（`0.27/1.10/0.07`、`0.55/2.19/0.14`），不照抄上游两跳值 |
| `897ca892`（前端部分） | `f92a6a56` | `useCodexOauthQuotaByAccountId` 接入 `autoQueryIntervalMinutes`；footer 默认 5 而非 0（改动前是无条件 5 分钟轮询，默认 0 会静默关掉现有行为）。tray.rs 部分不适用（fork 无 managed OAuth） |
| `6e424fd3` + `d1c550ba` | `d2152723` | 恢复 1M 上下文开关（纯解注释）；删 Goal mode（codex-cli 已默认开 goals，取消勾选删行回落到 on 反而误导）。前端测试 322 → 318 |

不适用：

| 上游提交 | 核实结论 |
| --- | --- |
| `fd14f9c4` | 修上游 #5522 重构引入的 preflight 挂死。fork 的 `resolve_path_default` 仍是重构前形态，`wait_child_output` / `CommandDeadline` / `isolate_child_process_group` / `terminate_child_tree` 全仓零命中，无 SIGTTIN 自停路径，搬过来是无效改动 |
| `0455a92c` | 依赖 `src/utils/providerCapabilities.ts`（fork 无此文件）与已延期的 `a2e22f33` managed OAuth |

验证：CI 32158109397 全绿（前端 318 tests；Rust 1542 passed / 2 ignored，
含定价两跳断言）；Ad Hoc 32158596850 构建通过。代码 head：`e90fe008`（后
`3f67786e` 为补记文档）。

### 2026-08-17（`1f38c838..a98829ba`，35 个）

按三应用边界逐提交核对：搬 8 个、已有实现 3 个、跳过 22 个、延期 2 个。

已搬运：

| 上游提交 | fork 提交 | 处理 |
| --- | --- | --- |
| `dfb2e523` | `26aee87a` | SQL fidelity 与恢复安全：暂存库 schema/事务校验、原子备份发布、序列/REAL/TEXT 保真、备份目录锁与恢复保护 |
| `c9fe340b` | `ef713e45` | 同步一致性：live/Prompt/Skill 后置同步错误汇总、保留表从 live DB 读取、恢复时序与会话游标保护 |
| `d9d4a660` | `2194e2f0` | 新增 IME-safe input；裁掉的 Hermes/OpenClaw/OpenCode 表单不回引 |
| `a98829ba` | `f411001c` | 将 IME-safe input 接入 fork 仍保留的 Basic provider fields，并补 blur/composition 回归测试 |
| `46f19a15` | `4eb61771` | DeepSeek chat usage 的 `prompt_cache_hit_tokens` 兜底 |
| `c8262476` | `02a627eb` | Kimi/Moonshot 不再注入 thinking/reasoning_content；保留 fork 的转换结构 |
| `3d126f45` | `4340e032` | 多年 usage trend tooltip 与点位对齐，并补组件测试 |
| `f62c854a` | `07eb86d5` | Codex Device Code login epoch，`clear_auth` 后拒绝过期流程重新登记 |

已有对应实现，无需搬运：`d2b070c9`（已由 `02a6bda4` 完成）、`7dc0a725`（已由
`81d1a596` 完成，`grok-4.5` 主档保持 `$0.50`）、`40cac1a6`（已由 `9029c0ab`
完成 reasoning levels 闭环）。

跳过 / 延期：22 个供应商 / 预设 / 赞助商 / 已裁应用提交（`5602324b`、
`5f6072ce`、`c99550e0`、`a7f073e9`、`5b8bf1fe`、`eb69e492`、`6a7da87c`、
`f748f3ac`、`84e75ad2`、`40d747c0`⚠️、`e163a671`、`c6247d13`、`1435223b`、
`3f75bbdf`、`e12fc623`、`9dcd3486`、`af06356d`、`4080a8e9`、`d01eab97`、
`de9af49a`、`d4fefefc`、`b109dcd3`）；`a2e22f33`（managed OAuth 含 Copilot，
约 11.6K 行，延期）；`bdeaac75`（Alpha Search / hosted WebSearch，约 10K 行，
streaming_responses 已裁剪，延期）。

⚠️ `40d747c0` 当时按 pi vendor 门控跳过，未意识到它同时把 SCHEMA_VERSION
升到 17。功能部分跳过正确，但版本号分叉 2026-08-18 复核时才被发现，
已记入「数据库版本与上游对齐」。教训：跳过提交前 grep 一下是否动
`SCHEMA_VERSION` / 迁移链。

验证：CI 32049925321 全绿；Ad Hoc 32050196805 通过（run 已删，ID 记录）。
Rust 1542 passed / 2 ignored；前端 52 files / 322 tests。

### 2026-08-13（`c39c9032..1f38c838`，24 个）

已搬运：`1f38c838` → 智谱配额条目类型 `TOKENS_LIMIT` 改名 `CREDIT_LIMIT`，
原判断只认前者导致用量面板整体空白（用户在用智谱网关，真实线上故障）。改为
两个名字都认，fork 补了上游没有的测试 `zhipu_accepts_credit_limit_type`。

已核实上游 bug 在 fork 中不存在：`967daa1a`（fork 的
`compute_local_git_tree_hash` 每次实地重算、从不读缓存 `content_hash`；
`install` 阶段已把 `directory` 规范成单段名）。

明确跳过（17 个）：Hermes / OpenCode / OpenClaw / GrokBuild 表单（已裁）；
JieKou AI / PPIO 预设（赞助商）；Claude Desktop 模型配置文案澄清（675 行 +
四语言，纯措辞）；表单边框与高级选项对齐、checkbox 样式、代理路由激活动画、
模型映射模糊搜索（纯视觉 / 非需求）；Windows WiX 注册表转义与 WSL2 CI（不用
Windows）；上游 CI 分区跳过与 i18n labeler（`.github/labeler.yml` 不存在）。

同轮完成 fork 侧 Skill 备份保留策略改造（见「fork 侧改造记录」）。

### 2026-08-10（`413c09e0..c39c9032`，1 个）

`c39c9032`（Windows WSL 拒绝原子替换时的回退）：不用 Windows + 裁剪边界外，跳过。
无搬运。同轮完成 fork 侧定价补缺（见「fork 侧改造记录」）。

### 2026-08-08（`28529620..413c09e0`，27 个）

已搬运：

| 上游提交 | fork 提交 | 处理 |
| --- | --- | --- |
| `6b8f3643` | `053859b9` | 4 个安全上限：usage_script 加 5s 中断 / 16 MiB 内存 / 256 KiB 栈；`model_catalog_json` 路径限制在预期目录内 + 32 MiB 上限；proxy 响应体与解压 128 MiB 上限（压缩炸弹）；deeplink 导入前展示 usageAccessToken / usageUserId。剔除 `session_usage_grokbuild.rs`，i18n 只取 en/zh |
| `413c09e0` | `1722c1a8` | 生成 catalog 时尊重用户手写的 `model_catalog_json` |
| `40b6376b` | `1cf8517a` | skill `readme_url` 从解析后的源目录构建，修 nested path 404 |

搬运过程中发现并修复的 fork 自身问题：

| fork 提交 | 问题 |
| --- | --- |
| `abf1f953` | 解 `forwarder.rs` 冲突时误带入 `validate_responses_success_response` / `validate_responses_stream_start`，依赖 fork 从未搬过的地基且无调用方 → 7 个编译错误。核实后删掉 109 行死代码，而非为死代码补地基 |
| `6394ebed` | **凭证泄漏**：搬 `6b8f3643` 时带了 `format_headers` 的回归测试却漏了实现，暴露出 fork 无条件输出所有响应头值——`set-cookie`、`authorization` 明文进日志。改为白名单（content-type / content-encoding / content-length / retry-after / cf-ray / request-id 系 + ratelimit 前缀）+ 160 字符截断 |

已核实 fork 已有实现：`9db9c56f`（Chat tool call 报错，`upstream_tool_call_dropped`
在 `streaming_codex_chat.rs:659`，7 个测试都在）、`eb356e15`（SKILL.md 锚点，
`resolve_skill_source_dir` 已是三步结构）。

明确跳过（22 个）：Codex usage 计费大改（`59a2bd10` / `baf07a27`，fork 的 usage
实现差异大，此前已延期）；backup 性能改造（`668bbda9`，1121 行纯性能）；搜索
列表 + 批量应用开关（`9f19d8fd`，5284 行新功能）；AppSwitcher UI（已按三应用改过）；
Copilot 兼容（`13ea497a`，fork 完全没有 Copilot）；废弃死代码清理（`3c1154be`，
9 个待删文件里 8 个 fork 早不存在）；翻译 key（`a354f08a`，GrokBuild 专用 +
四语言）；OpenCode / OMO / Hermes（已裁）；赞助商 / 版本号 / 发行说明。

### 2026-07-30（`934a2d03..c0ff89b9`）

已搬运：`c98913df`（SQL 导入拒绝跨文件语句）、`35486afd`（terminal cwd POSIX
单引号转义）、`cd17912f`（common config walker 不碰 `Object.prototype` + 补回
Codex model TOML 转义）、`6dbb944b`（deeplink 风险分级）、`a443eae9`（导入确认
显示 MCP args/env 并标记风险）、`19bf236e`（URL-safe Base64）、`cfa90f39`（usage
scripts 默认禁用并显示代码）、`ff3bc242`（协议桥 panic / Codex MCP 非表 panic /
skill zip-slip，只搬适用部分）。

跳过 / 延期：上游正式发布、updater 和 R2 镜像链；models.dev 自动定价同步
（20 文件，延期）；上游 ZIP 测试 TMPDIR 并发隔离（fork 无对应 flaky）；
Codex parent rollout timeline cache（大型性能改造，延期）；上游 8 应用 icon-only
改版（fork 3 应用保留名称更易用）；`v3.19.0` 版本号 / CHANGELOG；赞助商 / 已裁
应用提交。

### 更早（`878c26f3..934a2d03`，7 个）

全部跳过：内置定价表 Opus 条目（自用不关心成本统计）；默认模型升级（主体是
Gemini / OpenCode / OpenClaw 预设）；OpenClaw base URL 修正（无 OpenClaw）；
AICoding 合作伙伴 / 赞助商域名与推荐链接 / 赞助商列表同步（赞助商内容）；
发布产物镜像到 Cloudflare R2（fork 不做正式分发）。

## fork 侧改造记录（非上游提交）

### Codex 26.810 会话兼容（2026-08-17，核心提交 `0c1b4327`）

**起因**：Codex Desktop 26.810 改了会话存储——子代理线程在 `threads` 表有独立行
但只靠 spawn edges / `thread_source` / `source` JSON 标记归属；项目归属迁到
`~/.codex/.codex-global-state.json`（原生 assignments + 侧栏排序）；会话列表还出现
只有内部事件、无用户事件的行。

**来源**：修复先在 Codex Keeper（codex-wake 自 fork，Swift）完成并验证
（`696b59f`，含隔离兼容测试套件），本次按同一契约移植到 cc-switch 的 Rust
会话管理。改动全部在 `src-tauri/src/session_manager/providers/codex.rs`：

1. **子代理识别四路信号**：`thread_spawn_edges.child_thread_id`、
   `threads.thread_source = 'subagent'`、`threads.source` JSON 里的
   `subagent.thread_spawn`、以及 rollout 文件内容兜底。命中即折叠，
   不独立列出，也拒绝被单独 move/delete/trash（"follow their parent"）。
2. **`needs_repair` 增加 `has_user_event != 0` 前提**：无用户事件的内部线程
   不再被误报为待修复/待索引。
3. **Move 三端同步**：state DB `threads.cwd` + rollout
   `session_meta.payload.cwd` / `turn_context` + `.codex-global-state.json`
   （写 `thread-project-assignments`、迁 `sidebar-project-thread-orders`、清
   projectless/workspace-hint/output-dir）。目标项目必须已在 Codex 注册
   （预检报错而非半移动），任一步失败整体回滚。
4. **Trash/Restore manifest v2**：`relatedRows` / `externalDatabases` 逐行保存
   **列名 + 任意类型值**（Null/Integer/Real/Text/Blob），Codex 以后加列不用改代码。
   覆盖 `codex-dev.db`（`local_thread_catalog` 行 + `catalog_revision` 递增）和
   `codex-history-snapshots-dev.db`（`app_server_history_snapshots`）。
   恢复时还原原生项目状态。
5. **引用行清理/恢复泛化**：运行时探测每张表的
   `thread_id` / `parent_thread_id` / `child_thread_id` / `assigned_thread_id`
   列（`assigned_thread_id` 置 NULL 而非删行），不再写死表名清单。
6. **SQLite 备份改用 rusqlite online Backup API**：单文件一致快照，
   不再裸拷 `state_5.sqlite-wal` / `-shm`（旧法在 WAL 活跃时可能拷出不一致状态）。

值得记下的约束：

- **`.codex-global-state.json` 是 delete/move/trash 的硬依赖**：文件缺失直接拒绝
  操作（与 Codex Keeper 行为一致）。比这更老的 Codex 版本没有该文件时这些写操作
  会报错——有意为之，避免在状态不同步的情况下盲改。
- **v1 废纸篓 manifest 仍可恢复**：新字段缺省按空处理，只是不带新快照能力。
- **测试卫生（已踩）**：delete/move 的测试必须把会话放在
  `<tmp>/sessions/`（或 `archived_sessions/`）下并把**该目录**作为 root 传入；
  否则 `codex_home_for_session_root` 判不出 codex home，回落到真实 `~/.codex`，
  测试会读写真实数据。
- `local_thread_catalog_metadata.catalog_revision`：删除和恢复各 +1（触发 Codex
  桌面端刷新）。兼容测试断言 4→5→6。

验证：Rust 全量 1606 测试通过（含 3 个移植自 Codex Keeper 兼容套件的新测试），
Clippy 零警告。CI 32041716595 / Ad Hoc 32041958536（run 已删，ID 记录）。

### Codex 模型与恢复保护（2026-08-15）

| 提交 | 内容 |
| --- | --- |
| `81d1a596` | 修正 Grok 4.5 缓存定价，并补 Grok 4.6 与 DeepSeek 别名 |
| `3e3d4ef5` | `ultra` 按直连、DeepSeek、low/high 与 OpenRouter 模式分别保留或降级 |
| `02a6bda4` | proxy takeover 恢复时保留官方 ChatGPT 登录，不被第三方 Codex 配置覆盖 |
| `9029c0ab` | Codex 自定义模型支持逐模型 `reasoningLevels` 与 `defaultReasoningLevel`；model catalog、前端编辑与 camelCase / snake_case 读取形成闭环 |
| `981652fc` | 应用 CI 给出的 rustfmt 结果；本轮经完整 CI 与 macOS 构建的代码 head |

验证：CI 31893969336 / Ad Hoc 31894246576（run 已删，ID 记录）。

### Skill 备份保留策略（2026-08-13）

**动机**：旧策略是全局「最多 20 个目录」，与体积无关。实测
`~/.cc-switch/skill-backups/` 已 105 MB / 20 个，正卡在上限：高频更新的小 Skill
（`academic-*` 系列各 ~1 MB）挤占额度，把低频更新的大 Skill 备份推出去
（`nature-figure` 34 MB 与 `academic-research-suite` 30 MB 各只剩 1–2 代），
且 20 个大备份合计可到数百 MB。

**改法**（`SKILL_BACKUP_RETAIN_PER_SKILL = 3` + 总体积上限 1 GiB）：

- 按 `meta.json` 的 `skill.directory` 分组，每个 Skill 各留最新 3 代；
- 分组裁剪后若总体积仍超 1 GiB，全局按最旧优先删，但**至少留 1 个**
  （单个备份自身超限也不删，否则「更新前已备份」静默失效）；
- 排序键用 `meta.json` 的 `backup_created_at` 而非目录 mtime——恢复/拷贝会重置
  mtime，按 mtime 判定会删错代；
- `meta.json` 不可读的目录归入同一孤儿组（否则 `list_backups` 跳过的这类目录
  每组只有 1 个、永远够不到上限 → 永不回收）。

体积统计用 `symlink_metadata` 不解引用符号链接；单项失败只跳过，不让整轮裁剪
失败。5 个单测覆盖：分组独立留 3 代、按 `backup_created_at` 排序、孤儿组归并、
体积上限最旧优先、绝不删最后一个。

**新策略只经单测验证，未在真实备份目录上跑过。** 清理只在
`create_uninstall_backup` 末尾触发，装新构建后首次更新任一 Skill 时应观察
`academic-*` 系列是否收敛到各 3 代。

### 定价补缺（2026-08-10）

按实时 usage 库核查 `proxy_request_logs`，找出"有请求、无定价"的型号补进
`seed_model_pricing`：

| model_id | 定价（in/out/cr/cc） | 依据 |
| --- | --- | --- |
| `claude-opus-5` | 5/25/0.50/6.25 | 官方 $5/$25 + Opus 档惯例；历史 1.8 亿 token 此前全 $0 |
| `grok-4.5-build-free` | 2/6/0.30/0 | 对齐上游 `grok-4.5-build` 的 costUsdTicks 实测（build 档 $0.30，非 API 挂牌 $0.50） |
| `xopglm52` | 1.4/4.4/0.26/0 | 用户中转别名 = GLM 5.2，镜像 live 库已学到的 glm-5.2 |

提交：`e9272739`（补 3 价）、`11e74cd1`（rustfmt）、`52719096`
（grok-4.5-build-free 改上游实测 $0.30，撤回误改主档 grok-4.5）。

核查中确认的非 bug（避免重复踩）：

- Claude 短名"无定价"不是规范化问题：`find_model_pricing_row` 的前缀匹配本就能
  命中带日期条目；那些 0 成本行全是失败请求（503/429/502，无 token）。
- 上游 grok 定价是实测反推（读 Grok CLI OAuth 的 `costUsdTicks`），build 档实测
  cache_read $0.30，主档 `grok-4.5` 保留挂牌 $0.50。fork 对齐此分档，不要把
  主档也改成 $0.30。

未做（已确认）：历史 Opus 5 约 1.8 亿 token（≈ $395）不回填——dashboard 成本是
写入时存死的，补价只对未来流量生效；回填需一次性重算 cost 列，用户选择不做。

## 未完成 / 待验证

- **Codex ↔ Anthropic 协议桥：转换 payload 已验证，应用内链路仍未跑过。**

  2026-07-27 对真实网关（智谱 `open.bigmodel.cn/api/anthropic`，模型 `glm-5.2`）
  验证了 7 个场景，全部返回 200：非流式、流式 SSE、工具调用、`[1m]` 标记剥离、
  空 text block 过滤、tool_result 多轮回传、`cache_control` 注入。

  **但这是用 Python 按 `transform_codex_anthropic.rs` 的逻辑复现 payload 测的**。
  Rust 实现与复现之间若有偏差，这个测试发现不了。仍待验证：在应用里真配一个
  `Anthropic Messages` 格式的 Codex 供应商，用 Codex 实跑一轮完整 Rust 链路。

  另：缓存未命中可能是该中转不支持 prompt caching，也可能是测试 prompt 未达
  1024 token 下限。只影响成本，不影响功能。
- **Skill 备份新策略未在真实备份目录上观察过收敛**（见「fork 侧改造记录」）。

## 验证手段

本机默认不保留 Rust toolchain，也没有 pnpm；后端改动以 GitHub CI 为最终验证。
本机跑前端用 `node_modules/.bin` 下的项目锁定版二进制（tsc / vitest / prettier），
不要用 `npx prettier`——会拉最新版，与 CI 的 `format:check` 结果不一致。
另注意 `format:check` 只覆盖 `src/**`，改了 `tests/**` 要单独跑一次 prettier。

```bash
gh workflow run "CI" --ref dev -R char1eslu/cc-switch
gh workflow run build-macos-ad-hoc.yml --ref dev -R char1eslu/cc-switch
```

注意 `gh` 可能解析到 upstream remote，命令要显式带 `-R char1eslu/cc-switch`。

若临时在本机验证 Rust，必须使用隔离目录并在完成后删除 Rustup/Cargo/target 缓存
（2026-08-17 那轮用 `/private/tmp` 一次性工具链，结束即删）。

数据库迁移改动，建议先用真实库的副本干跑验证，确认列/行变化与数据无损。
