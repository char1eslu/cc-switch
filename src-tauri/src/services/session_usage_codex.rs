//! Codex 会话日志使用追踪
//!
//! 从 ~/.codex/sessions/ 下的 JSONL 会话文件中提取精确 token 使用数据，
//! 替代原有的 state_5.sqlite 估算方案。
//!
//! ## 数据流
//! ```text
//! ~/.codex/sessions/YYYY/MM/DD/*.jsonl → 增量解析 → delta 计算 → 费用计算 → proxy_request_logs 表
//! ```
//!
//! ## 解析的事件类型
//! - `session_meta` → 提取 session_id
//! - `turn_context` → 提取当前 model
//! - `event_msg` (type=token_count) → 提取累计 token 用量，计算 delta

use crate::codex_config::get_codex_config_dir;
use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::proxy::usage::calculator::{CostCalculator, ModelPricing};
use crate::proxy::usage::parser::TokenUsage;
use crate::services::session_usage::{
    get_sync_state, metadata_modified_nanos, update_sync_state, SessionSyncResult,
};
use crate::services::usage_stats::{find_model_pricing, should_skip_session_insert, DedupKey};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// 累计 token 用量（跟踪 total_token_usage 字段）
#[derive(Debug, Clone, Default)]
struct CumulativeTokens {
    input: u64,
    cached_input: u64,
    output: u64,
}

/// 单次 API 调用的 token 增量
#[derive(Debug)]
struct DeltaTokens {
    input: u32,
    cached_input: u32,
    output: u32,
}

impl DeltaTokens {
    fn is_zero(&self) -> bool {
        self.input == 0 && self.cached_input == 0 && self.output == 0
    }
}

/// 单个 token 计数快照的签名（字段级 Option，缺字段与 0 值可区分）
#[derive(Debug, Clone, PartialEq, Eq)]
struct TokenCountersSignature {
    input: Option<u64>,
    cached_input: Option<u64>,
    output: Option<u64>,
    reasoning_output: Option<u64>,
    total: Option<u64>,
}

/// 一条 token_count 事件的签名（total + last 两份快照）
#[derive(Debug, Clone, PartialEq, Eq)]
struct TokenUsageSignature {
    total: Option<TokenCountersSignature>,
    last: Option<TokenCountersSignature>,
}

fn parse_signature_counters(value: Option<&serde_json::Value>) -> Option<TokenCountersSignature> {
    let value = value?.as_object()?;
    Some(TokenCountersSignature {
        input: value
            .get("input_tokens")
            .and_then(serde_json::Value::as_u64),
        cached_input: value
            .get("cached_input_tokens")
            .or_else(|| value.get("cache_read_input_tokens"))
            .and_then(serde_json::Value::as_u64),
        output: value
            .get("output_tokens")
            .and_then(serde_json::Value::as_u64),
        reasoning_output: value
            .get("reasoning_output_tokens")
            .and_then(serde_json::Value::as_u64),
        total: value
            .get("total_tokens")
            .and_then(serde_json::Value::as_u64),
    })
}

fn parse_token_signature(info: &serde_json::Value) -> Option<TokenUsageSignature> {
    let total = parse_signature_counters(info.get("total_token_usage"));
    let last = parse_signature_counters(info.get("last_token_usage"));
    (total.is_some() || last.is_some()).then_some(TokenUsageSignature { total, last })
}

/// 快照来源：rate_limits.limit_id。限流刷新会在不同 limit_id 下重发同值
/// token 信息，去重必须分通道进行（见 parse 循环内注释）。
fn token_snapshot_source(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("rate_limits")
        .and_then(|rate_limits| rate_limits.get("limit_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn update_high_water(high_water: &mut CumulativeTokens, current: &CumulativeTokens) {
    high_water.input = high_water.input.max(current.input);
    high_water.cached_input = high_water.cached_input.max(current.cached_input);
    high_water.output = high_water.output.max(current.output);
}

/// 单文件解析时的运行状态
struct FileParseState {
    session_id: Option<String>,
    current_model: String,
    // `total_token_usage` 是会话累计值，跨模型与限流通道变化。分叉快照靠
    // 优先取精确 `last_token_usage` 处理，而不是拆累计基线。
    total_high_water: Option<CumulativeTokens>,
    // 限流刷新会在另一个 limit_id 下重发未变化的 token 信息。同通道重复用
    // 该通道最新完整快照识别；跨通道重复必须匹配紧邻的上一条 token 事件，
    // 不与其他通道的旧快照比较（计数器重置后旧签名可能合法复现）。
    last_signature_by_source: HashMap<Option<String>, TokenUsageSignature>,
    previous_token_signature: Option<TokenUsageSignature>,
    event_index: u32,
}

/// 归一化 Codex 模型名
///
/// 处理规则（按顺序）：
/// 1. 转小写：`GLM-4.6` → `glm-4.6`
/// 2. 剥离 provider 前缀：`openai/gpt-5.4` → `gpt-5.4`
/// 3. 剥离 ISO 日期后缀：`gpt-5.4-2026-03-05` → `gpt-5.4`
/// 4. 剥离紧凑日期后缀：`gpt-5.4-20260305` → `gpt-5.4`
fn normalize_codex_model(raw: &str) -> String {
    // Step 1: 小写
    let mut name = raw.to_lowercase();

    // Step 2: 剥离 "provider/" 前缀（如 openai/, azure/）
    if let Some(pos) = name.rfind('/') {
        name = name[pos + 1..].to_string();
    }

    // Step 3: 剥离 ISO 日期后缀 -YYYY-MM-DD（正好 11 字符）
    if name.len() > 11 && name.is_char_boundary(name.len() - 11) {
        let suffix = &name[name.len() - 11..];
        if suffix.is_ascii()
            && suffix.as_bytes()[0] == b'-'
            && suffix[1..5].chars().all(|c| c.is_ascii_digit())
            && suffix.as_bytes()[5] == b'-'
            && suffix[6..8].chars().all(|c| c.is_ascii_digit())
            && suffix.as_bytes()[8] == b'-'
            && suffix[9..11].chars().all(|c| c.is_ascii_digit())
        {
            name.truncate(name.len() - 11);
        }
    }

    // Step 4: 剥离紧凑日期后缀 -YYYYMMDD（正好 9 字符）
    if name.len() > 9 {
        let parts: Vec<&str> = name.rsplitn(2, '-').collect();
        if parts.len() == 2 {
            if let Some(suffix) = parts.first() {
                if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
                    name = parts[1].to_string();
                }
            }
        }
    }

    name
}

/// 计算两次累计值之间的 delta
fn compute_delta(prev: &Option<CumulativeTokens>, current: &CumulativeTokens) -> DeltaTokens {
    match prev {
        None => DeltaTokens {
            input: current.input as u32,
            cached_input: current.cached_input as u32,
            output: current.output as u32,
        },
        Some(p) => DeltaTokens {
            input: current.input.saturating_sub(p.input) as u32,
            cached_input: current.cached_input.saturating_sub(p.cached_input) as u32,
            output: current.output.saturating_sub(p.output) as u32,
        },
    }
}

/// 从 JSON Value 中提取累计 token 用量
fn parse_cumulative_tokens(total_usage: &serde_json::Value) -> Option<CumulativeTokens> {
    let fields = total_usage.as_object()?;
    if ![
        "input_tokens",
        "cached_input_tokens",
        "cache_read_input_tokens",
        "output_tokens",
        "reasoning_output_tokens",
        "total_tokens",
    ]
    .iter()
    .any(|field| fields.contains_key(*field))
    {
        return None;
    }
    Some(CumulativeTokens {
        input: total_usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cached_input: total_usage
            .get("cached_input_tokens")
            .or_else(|| total_usage.get("cache_read_input_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        output: total_usage
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    })
}

/// 同步 Codex 使用数据（从 JSONL 会话日志）
pub fn sync_codex_usage(db: &Database) -> Result<SessionSyncResult, AppError> {
    let codex_dir = get_codex_config_dir();

    let files = collect_codex_session_files(&codex_dir);

    let mut result = SessionSyncResult {
        imported: 0,
        skipped: 0,
        files_scanned: files.len() as u32,
        deferred_files: 0,
        errors: vec![],
    };

    if files.is_empty() {
        return Ok(result);
    }

    for file_path in &files {
        match sync_single_codex_file(db, file_path) {
            Ok((imported, skipped)) => {
                result.imported += imported;
                result.skipped += skipped;
            }
            Err(e) => {
                let msg = format!("Codex 会话文件解析失败 {}: {e}", file_path.display());
                log::warn!("[CODEX-SYNC] {msg}");
                result.errors.push(msg);
            }
        }
    }

    if result.imported > 0 {
        log::info!(
            "[CODEX-SYNC] 同步完成: 导入 {} 条, 跳过 {} 条, 扫描 {} 个文件",
            result.imported,
            result.skipped,
            result.files_scanned
        );
    }

    Ok(result)
}

/// 收集所有 Codex 会话 JSONL 文件
fn collect_codex_session_files(codex_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    // 1. 扫描 sessions/YYYY/MM/DD/*.jsonl（日期分区目录）
    let sessions_dir = codex_dir.join("sessions");
    if sessions_dir.is_dir() {
        collect_jsonl_recursive(&sessions_dir, &mut files, 0, 3);
    }

    // 2. 扫描 archived_sessions/*.jsonl（扁平归档目录）
    let archived_dir = codex_dir.join("archived_sessions");
    if archived_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&archived_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    files.push(path);
                }
            }
        }
    }

    files
}

pub(crate) fn reset_codex_usage_on_conn(
    conn: &rusqlite::Connection,
    codex_dir: &Path,
) -> Result<(), AppError> {
    if Database::table_exists(conn, "proxy_request_logs")?
        && Database::has_column(conn, "proxy_request_logs", "data_source")?
    {
        conn.execute(
            "DELETE FROM proxy_request_logs WHERE data_source = 'codex_session'",
            [],
        )
        .map_err(|e| AppError::Database(format!("清理 Codex 会话明细失败: {e}")))?;
    }
    if Database::table_exists(conn, "usage_daily_rollups")?
        && Database::has_column(conn, "usage_daily_rollups", "provider_id")?
    {
        conn.execute(
            "DELETE FROM usage_daily_rollups WHERE provider_id = '_codex_session'",
            [],
        )
        .map_err(|e| AppError::Database(format!("清理 Codex 用量汇总失败: {e}")))?;
    }
    if Database::table_exists(conn, "session_log_sync")?
        && Database::has_column(conn, "session_log_sync", "file_path")?
    {
        let mut stmt = conn
            .prepare("SELECT file_path FROM session_log_sync")
            .map_err(|e| AppError::Database(format!("读取会话同步 cursor 失败: {e}")))?;
        let paths = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| AppError::Database(format!("查询会话同步 cursor 失败: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(format!("解析会话同步 cursor 失败: {e}")))?;
        for file_path in paths.into_iter().filter(|file_path| {
            let normalized = file_path.replace('\\', "/");
            let file_name = normalized.rsplit('/').next().unwrap_or_default();
            let is_rollout = file_name.starts_with("rollout-")
                && file_name.ends_with(".jsonl")
                && file_name
                    .trim_end_matches(".jsonl")
                    .get(
                        file_name
                            .trim_end_matches(".jsonl")
                            .len()
                            .saturating_sub(36)..,
                    )
                    .is_some_and(|candidate| uuid::Uuid::parse_str(candidate).is_ok());
            is_rollout
                && (Path::new(file_path).starts_with(codex_dir.join("sessions"))
                    || Path::new(file_path).starts_with(codex_dir.join("archived_sessions"))
                    || normalized
                        .split('/')
                        .any(|part| matches!(part, "sessions" | "archived_sessions")))
        }) {
            conn.execute(
                "DELETE FROM session_log_sync WHERE file_path = ?1",
                [file_path],
            )
            .map_err(|e| AppError::Database(format!("清理 Codex 同步 cursor 失败: {e}")))?;
        }
    }
    Ok(())
}

/// 递归扫描目录下的 .jsonl 文件（限制最大深度）
fn collect_jsonl_recursive(dir: &Path, files: &mut Vec<PathBuf>, depth: u32, max_depth: u32) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && depth < max_depth {
            collect_jsonl_recursive(&path, files, depth + 1, max_depth);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
}

fn rollout_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let candidate = stem.get(stem.len().checked_sub(36)?..)?;
    uuid::Uuid::parse_str(candidate)
        .ok()
        .map(|value| value.hyphenated().to_string())
}

/// 同步单个 Codex JSONL 文件，返回 (imported, skipped)
fn sync_single_codex_file(db: &Database, file_path: &Path) -> Result<(u32, u32), AppError> {
    let file_path_str = file_path.to_string_lossy().to_string();

    // 获取文件元数据
    let metadata = fs::metadata(file_path)
        .map_err(|e| AppError::Config(format!("无法读取文件元数据: {e}")))?;
    let file_modified = metadata_modified_nanos(&metadata);

    // 检查同步状态
    let (last_modified, last_offset) = get_sync_state(db, &file_path_str)?;

    // 文件未变化则跳过
    if file_modified <= last_modified {
        return Ok((0, 0));
    }

    // 打开文件逐行解析
    let file =
        fs::File::open(file_path).map_err(|e| AppError::Config(format!("无法打开文件: {e}")))?;
    let reader = BufReader::new(file);

    let mut state = FileParseState {
        session_id: None,
        current_model: "unknown".to_string(),
        total_high_water: None,
        last_signature_by_source: HashMap::new(),
        previous_token_signature: None,
        event_index: 0,
    };

    let mut line_offset: i64 = 0;
    let mut imported: u32 = 0;
    let mut skipped: u32 = 0;

    for line_result in reader.lines() {
        line_offset += 1;

        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue, // 容忍不完整的最后一行
        };

        if line.trim().is_empty() {
            continue;
        }

        // 快速过滤：在 JSON 反序列化前跳过无关行
        let is_event_msg = line.contains("\"event_msg\"");
        let is_turn_context = line.contains("\"turn_context\"");
        let is_session_meta = line.contains("\"session_meta\"");

        if !is_event_msg && !is_turn_context && !is_session_meta {
            continue;
        }
        if is_event_msg && !line.contains("\"token_count\"") {
            continue;
        }

        let value: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = match value.get("type").and_then(|t| t.as_str()) {
            Some(t) => t,
            None => continue,
        };

        match event_type {
            "session_meta" if state.session_id.is_none() => {
                let payload = value.get("payload");
                state.session_id = payload
                    .and_then(|p| {
                        p.get("session_id")
                            .or_else(|| p.get("sessionId"))
                            .or_else(|| p.get("id"))
                    })
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }
            "turn_context" => {
                if let Some(payload) = value.get("payload") {
                    // model 可能在 payload.model 或 payload.info.model
                    if let Some(model) = payload
                        .get("model")
                        .or_else(|| payload.get("info").and_then(|info| info.get("model")))
                        .and_then(|v| v.as_str())
                    {
                        state.current_model = normalize_codex_model(model);
                    }
                }
            }
            "event_msg" => {
                let payload = match value.get("payload") {
                    Some(p) => p,
                    None => continue,
                };

                // 只处理 token_count 类型
                if payload.get("type").and_then(|t| t.as_str()) != Some("token_count") {
                    continue;
                }

                let info = match payload.get("info") {
                    Some(i) if !i.is_null() => i,
                    _ => continue, // 跳过 info 为 null 的首个事件
                };

                // 提取模型（token_count 事件也可能携带 model）
                if let Some(model) = info
                    .get("model")
                    .or_else(|| info.get("model_name"))
                    .or_else(|| payload.get("model"))
                    .and_then(|v| v.as_str())
                {
                    state.current_model = normalize_codex_model(model);
                }

                let signature = match parse_token_signature(info) {
                    Some(s) => s,
                    None => continue,
                };

                // 优先取精确的 last_token_usage（单次调用真实用量）；
                // 只有缺失时才退回 total 快照差分。
                let snapshot_source = token_snapshot_source(payload);
                let total = info
                    .get("total_token_usage")
                    .and_then(parse_cumulative_tokens);
                let last = info
                    .get("last_token_usage")
                    .and_then(parse_cumulative_tokens);
                if total.is_none() && last.is_none() {
                    continue;
                }
                let has_total_snapshot = total.is_some();
                let duplicate_snapshot = has_total_snapshot
                    && (state.last_signature_by_source.get(&snapshot_source) == Some(&signature)
                        || state.previous_token_signature.as_ref() == Some(&signature));
                if has_total_snapshot {
                    state
                        .last_signature_by_source
                        .insert(snapshot_source, signature.clone());
                }
                state.previous_token_signature = Some(signature.clone());

                let delta = if duplicate_snapshot {
                    // 重放的同值快照：零增量，防止限流刷新导致的重复计费
                    DeltaTokens {
                        input: 0,
                        cached_input: 0,
                        output: 0,
                    }
                } else if let Some(last) = last {
                    DeltaTokens {
                        input: last.input as u32,
                        cached_input: last.cached_input as u32,
                        output: last.output as u32,
                    }
                } else if let Some(total) = total.as_ref() {
                    compute_delta(&state.total_high_water, total)
                } else {
                    continue;
                };
                if let Some(total) = total {
                    match state.total_high_water.as_mut() {
                        Some(high_water) => update_high_water(high_water, &total),
                        None => state.total_high_water = Some(total),
                    }
                }

                // 钳制：cached 不应超过 input（防护异常数据）
                let delta = DeltaTokens {
                    cached_input: delta.cached_input.min(delta.input),
                    ..delta
                };

                if delta.is_zero() {
                    continue; // 跳过 task 边界的零 delta 事件
                }

                state.event_index += 1;

                // 跳过已处理的行（但仍需解析以恢复状态）
                if line_offset <= last_offset {
                    continue;
                }

                // 生成唯一 request_id
                let request_id_namespace = rollout_id_from_filename(file_path)
                    .as_deref()
                    .or(state.session_id.as_deref())
                    .unwrap_or("unknown")
                    .to_string();
                let request_id = format!(
                    "codex_session:{}:{}",
                    request_id_namespace, state.event_index
                );

                // 提取时间戳
                let timestamp = value
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                match insert_codex_session_entry(
                    db,
                    &request_id,
                    &delta,
                    &state.current_model,
                    state.session_id.as_deref(),
                    timestamp.as_deref(),
                ) {
                    Ok(true) => imported += 1,
                    Ok(false) => skipped += 1,
                    Err(e) => {
                        log::warn!("[CODEX-SYNC] 插入失败 ({}): {e}", request_id);
                        skipped += 1;
                    }
                }
            }
            _ => {}
        }
    }

    // 更新同步状态
    update_sync_state(db, &file_path_str, file_modified, line_offset)?;

    Ok((imported, skipped))
}

/// 插入单条 Codex 会话记录到 proxy_request_logs
fn insert_codex_session_entry(
    db: &Database,
    request_id: &str,
    delta: &DeltaTokens,
    model: &str,
    session_id: Option<&str>,
    timestamp: Option<&str>,
) -> Result<bool, AppError> {
    let conn = lock_conn!(db.conn);

    let created_at = timestamp
        .and_then(|ts| {
            chrono::DateTime::parse_from_rfc3339(ts)
                .ok()
                .map(|dt| dt.timestamp())
        })
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        });

    let dedup_key = DedupKey {
        app_type: "codex",
        model,
        input_tokens: delta.input,
        output_tokens: delta.output,
        cache_read_tokens: delta.cached_input,
        cache_creation_tokens: 0,
        created_at,
    };
    if should_skip_session_insert(&conn, request_id, &dedup_key)? {
        return Ok(false);
    }

    // 计算费用
    let usage = TokenUsage {
        input_tokens: delta.input,
        output_tokens: delta.output,
        cache_read_tokens: delta.cached_input,
        cache_creation_tokens: 0,
        model: Some(model.to_string()),
        message_id: None,
    };

    let pricing = find_codex_pricing(&conn, model);
    let multiplier = Decimal::from(1);
    let (input_cost, output_cost, cache_read_cost, cache_creation_cost, total_cost) = match pricing
    {
        Some(p) => {
            let cost = CostCalculator::calculate_for_app("codex", &usage, &p, multiplier);
            (
                cost.input_cost.to_string(),
                cost.output_cost.to_string(),
                cost.cache_read_cost.to_string(),
                cost.cache_creation_cost.to_string(),
                cost.total_cost.to_string(),
            )
        }
        None => (
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
        ),
    };

    let inserted_rows = conn
        .execute(
            "INSERT OR IGNORE INTO proxy_request_logs (
            request_id, provider_id, app_type, model, request_model,
            input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
            input_cost_usd, output_cost_usd, cache_read_cost_usd, cache_creation_cost_usd, total_cost_usd,
            latency_ms, first_token_ms, status_code, error_message, session_id,
            provider_type, is_streaming, cost_multiplier, created_at, data_source
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
            rusqlite::params![
                request_id,
                "_codex_session",    // provider_id
                "codex",             // app_type
                model,
                model,               // request_model = model
                delta.input,
                delta.output,
                delta.cached_input,
                0i64,                // cache_creation_tokens: Codex 日志无此数据
                input_cost,
                output_cost,
                cache_read_cost,
                cache_creation_cost,
                total_cost,
                0i64,                // latency_ms
                Option::<i64>::None, // first_token_ms
                200i64,              // status_code
                Option::<String>::None, // error_message
                session_id.map(|s| s.to_string()),
                Some("codex_session"), // provider_type
                1i64,                // is_streaming
                "1.0",               // cost_multiplier
                created_at,
                "codex_session",     // data_source
            ],
        )
        .map_err(|e| AppError::Database(format!("插入 Codex 会话日志失败: {e}")))?;

    if inserted_rows > 0 {
        crate::usage_events::notify_log_recorded();
    }

    Ok(true)
}

/// 查找 Codex 模型定价（带归一化）
fn find_codex_pricing(conn: &rusqlite::Connection, model_id: &str) -> Option<ModelPricing> {
    find_model_pricing(conn, &normalize_codex_model(model_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_delta_first_event() {
        let prev = None;
        let current = CumulativeTokens {
            input: 17934,
            cached_input: 9600,
            output: 454,
        };
        let delta = compute_delta(&prev, &current);
        assert_eq!(delta.input, 17934);
        assert_eq!(delta.cached_input, 9600);
        assert_eq!(delta.output, 454);
        assert!(!delta.is_zero());
    }

    #[test]
    fn test_delta_subsequent_event() {
        let prev = Some(CumulativeTokens {
            input: 17934,
            cached_input: 9600,
            output: 454,
        });
        let current = CumulativeTokens {
            input: 36722,
            cached_input: 27904,
            output: 804,
        };
        let delta = compute_delta(&prev, &current);
        assert_eq!(delta.input, 36722 - 17934);
        assert_eq!(delta.cached_input, 27904 - 9600);
        assert_eq!(delta.output, 804 - 454);
    }

    #[test]
    fn test_delta_zero_at_task_boundary() {
        let prev = Some(CumulativeTokens {
            input: 58346,
            cached_input: 46976,
            output: 1045,
        });
        // task 边界：相同的累计值
        let current = CumulativeTokens {
            input: 58346,
            cached_input: 46976,
            output: 1045,
        };
        let delta = compute_delta(&prev, &current);
        assert!(delta.is_zero());
    }

    #[test]
    fn test_delta_saturating_sub() {
        // 异常情况：当前值小于前值（不应发生，但需防护）
        let prev = Some(CumulativeTokens {
            input: 100,
            cached_input: 50,
            output: 30,
        });
        let current = CumulativeTokens {
            input: 80,
            cached_input: 40,
            output: 20,
        };
        let delta = compute_delta(&prev, &current);
        assert_eq!(delta.input, 0);
        assert_eq!(delta.cached_input, 0);
        assert_eq!(delta.output, 0);
        assert!(delta.is_zero());
    }

    #[test]
    fn test_parse_cumulative_tokens_valid() {
        let json: serde_json::Value = serde_json::json!({
            "input_tokens": 17934,
            "cached_input_tokens": 9600,
            "output_tokens": 454,
            "reasoning_output_tokens": 233,
            "total_tokens": 18388
        });
        let tokens = parse_cumulative_tokens(&json).unwrap();
        assert_eq!(tokens.input, 17934);
        assert_eq!(tokens.cached_input, 9600);
        assert_eq!(tokens.output, 454);
    }

    #[test]
    fn test_parse_cumulative_tokens_null() {
        let json = serde_json::Value::Null;
        assert!(parse_cumulative_tokens(&json).is_none());
    }

    #[test]
    fn test_parse_cumulative_tokens_alt_field_names() {
        // 某些版本可能使用 cache_read_input_tokens 而非 cached_input_tokens
        let json: serde_json::Value = serde_json::json!({
            "input_tokens": 1000,
            "cache_read_input_tokens": 500,
            "output_tokens": 200
        });
        let tokens = parse_cumulative_tokens(&json).unwrap();
        assert_eq!(tokens.cached_input, 500);
    }

    #[test]
    fn test_collect_codex_session_files_nonexistent() {
        let files = collect_codex_session_files(Path::new("/nonexistent/path"));
        assert!(files.is_empty());
    }

    /// 交错限流通道 + 跨通道重放快照的回归测试（上游 59a2bd10）。
    ///
    /// 场景：两个 limit_id 交替上报 total 快照，且限流刷新会重发同值快照。
    /// 旧实现（单一 prev_total 基线、优先 total 差分）会因通道 B 的较低累计值
    /// 丢事件、因重放快照重复计费。新实现优先取精确 last_token_usage，
    /// 并按通道 + 紧邻事件去重。
    #[test]
    fn test_interleaved_lanes_and_replayed_snapshots() -> Result<(), AppError> {
        let db = Database::memory()?;
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("session.jsonl");

        let token_count =
            |total: (u64, u64, u64), last: (u64, u64, u64), limit_id: &str, ts: &str| -> String {
                serde_json::json!({
                    "timestamp": ts,
                    "type": "event_msg",
                    "payload": {
                        "type": "token_count",
                        "info": {
                            "total_token_usage": {
                                "input_tokens": total.0,
                                "cached_input_tokens": total.1,
                                "output_tokens": total.2,
                                "reasoning_output_tokens": 0,
                                "total_tokens": total.0 + total.2
                            },
                            "last_token_usage": {
                                "input_tokens": last.0,
                                "cached_input_tokens": last.1,
                                "output_tokens": last.2,
                                "reasoning_output_tokens": 0,
                                "total_tokens": last.0 + last.2
                            }
                        },
                        "rate_limits": { "limit_id": limit_id }
                    }
                })
                .to_string()
            };

        let lines = [
            serde_json::json!({
                "timestamp": "2026-08-21T00:00:00Z",
                "type": "session_meta",
                "payload": { "session_id": "sess-lanes" }
            })
            .to_string(),
            serde_json::json!({
                "timestamp": "2026-08-21T00:00:01Z",
                "type": "turn_context",
                "payload": { "model": "gpt-5.6-sol" }
            })
            .to_string(),
            // 通道 A 正常事件
            token_count(
                (100, 60, 10),
                (100, 60, 10),
                "lane_a",
                "2026-08-21T00:00:02Z",
            ),
            // 通道 B 累计值更低（独立通道）：旧实现差分为 0 丢事件，新实现取 last
            token_count((90, 50, 8), (30, 20, 4), "lane_b", "2026-08-21T00:00:03Z"),
            // 通道 A 推进
            token_count((105, 62, 12), (5, 2, 2), "lane_a", "2026-08-21T00:00:04Z"),
            // 同值快照换到通道 B 重放：必须识别为零增量
            token_count((105, 62, 12), (5, 2, 2), "lane_b", "2026-08-21T00:00:05Z"),
        ];
        fs::write(&file, lines.join("\n") + "\n").expect("write jsonl");

        let (imported, _skipped) = sync_single_codex_file(&db, &file)?;

        // 事件 1/2/3 入库；事件 4 是重放，零增量被跳过
        assert_eq!(imported, 3, "replayed snapshot must not be imported");

        let conn = lock_conn!(db.conn);
        let (total_input, total_cached, total_output): (i64, i64, i64) = conn.query_row(
            "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(cache_read_tokens),0),
                    COALESCE(SUM(output_tokens),0)
             FROM proxy_request_logs WHERE data_source = 'codex_session'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        // 精确 last 之和：100+30+5 / 60+20+2 / 10+4+2
        assert_eq!((total_input, total_cached, total_output), (135, 82, 16));

        Ok(())
    }

    #[test]
    fn test_resumed_rollout_uses_physical_id_for_request_and_meta_id_for_session(
    ) -> Result<(), AppError> {
        let db = Database::memory()?;
        let dir = tempfile::tempdir().expect("tempdir");
        let thread_id = "019d0000-0000-7000-8000-000000000001";
        let rollout_id = "019d0000-0000-7000-8000-000000000002";
        let file = dir.path().join(format!(
            "rollout-2026-09-01T00-00-00-{thread_id}_{rollout_id}.jsonl"
        ));
        let lines = [
            serde_json::json!({
                "timestamp": "2026-09-01T00:00:00Z",
                "type": "session_meta",
                "payload": { "id": thread_id }
            })
            .to_string(),
            serde_json::json!({
                "timestamp": "2026-09-01T00:00:01Z",
                "type": "turn_context",
                "payload": { "model": "gpt-5.6-sol" }
            })
            .to_string(),
            serde_json::json!({
                "timestamp": "2026-09-01T00:00:02Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "last_token_usage": {
                            "input_tokens": 10,
                            "cached_input_tokens": 5,
                            "output_tokens": 2,
                            "total_tokens": 12
                        }
                    }
                }
            })
            .to_string(),
        ];
        fs::write(&file, lines.join("\n") + "\n").expect("write jsonl");

        assert_eq!(sync_single_codex_file(&db, &file)?.0, 1);

        let conn = lock_conn!(db.conn);
        let (request_id, session_id): (String, String) = conn.query_row(
            "SELECT request_id, session_id FROM proxy_request_logs
             WHERE data_source = 'codex_session'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(request_id, format!("codex_session:{rollout_id}:1"));
        assert_eq!(session_id, thread_id);

        Ok(())
    }

    #[test]
    fn test_parse_token_signature_distinguishes_fields() {
        let info: serde_json::Value = serde_json::json!({
            "total_token_usage": { "input_tokens": 10, "output_tokens": 2 },
            "last_token_usage": { "input_tokens": 10, "output_tokens": 2 }
        });
        let sig = parse_token_signature(&info).expect("signature");
        let total = sig.total.clone().expect("total signature");
        assert_eq!(total.input, Some(10));
        assert_eq!(total.cached_input, None, "缺失字段与 0 值必须可区分");
        assert!(sig.last.is_some());

        // 同值不同通道的签名相等（去重依据）
        let info2: serde_json::Value = serde_json::json!({
            "total_token_usage": { "input_tokens": 10, "output_tokens": 2 },
            "last_token_usage": { "input_tokens": 10, "output_tokens": 2 }
        });
        assert_eq!(sig, parse_token_signature(&info2).unwrap());

        // 值变化 → 签名不同
        let info3: serde_json::Value = serde_json::json!({
            "total_token_usage": { "input_tokens": 11, "output_tokens": 2 },
            "last_token_usage": { "input_tokens": 1, "output_tokens": 0 }
        });
        assert_ne!(sig, parse_token_signature(&info3).unwrap());

        // 两份快照都缺 → 无签名
        assert!(parse_token_signature(&serde_json::json!({})).is_none());
    }

    #[test]
    fn test_insert_codex_session_skips_matching_proxy_log() -> Result<(), AppError> {
        let db = Database::memory()?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "INSERT INTO proxy_request_logs (
                    request_id, provider_id, app_type, model, request_model,
                    input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                    total_cost_usd, latency_ms, status_code, created_at, data_source
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    "codex-proxy",
                    "openai",
                    "codex",
                    "gpt-5.4",
                    "gpt-5.4",
                    10,
                    2,
                    1,
                    7,
                    "0.01",
                    100,
                    200,
                    1000,
                    "proxy"
                ],
            )?;
        }

        let delta = DeltaTokens {
            input: 10,
            cached_input: 1,
            output: 2,
        };
        let inserted = insert_codex_session_entry(
            &db,
            "codex-session-dup",
            &delta,
            "gpt-5.4",
            Some("session-1"),
            Some("1970-01-01T00:16:45Z"),
        )?;
        assert!(!inserted);

        let conn = lock_conn!(db.conn);
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM proxy_request_logs", [], |row| {
            row.get(0)
        })?;
        assert_eq!(count, 1);

        Ok(())
    }

    // ── 模型名归一化测试 ──

    #[test]
    fn test_normalize_codex_model_lowercase() {
        assert_eq!(normalize_codex_model("GLM-4.6"), "glm-4.6");
        assert_eq!(normalize_codex_model("DeepSeek-Chat"), "deepseek-chat");
        assert_eq!(normalize_codex_model("GPT-5.4"), "gpt-5.4");
    }

    #[test]
    fn test_normalize_codex_model_strip_prefix() {
        assert_eq!(normalize_codex_model("openai/gpt-5.4"), "gpt-5.4");
        assert_eq!(
            normalize_codex_model("azure/gpt-5.2-codex"),
            "gpt-5.2-codex"
        );
        assert_eq!(normalize_codex_model("OPENAI/GPT-5.4"), "gpt-5.4");
    }

    #[test]
    fn test_normalize_codex_model_strip_iso_date() {
        assert_eq!(normalize_codex_model("gpt-5.4-2026-03-05"), "gpt-5.4");
        assert_eq!(
            normalize_codex_model("gpt-5.4-pro-2026-03-05"),
            "gpt-5.4-pro"
        );
    }

    #[test]
    fn test_normalize_codex_model_strip_compact_date() {
        assert_eq!(normalize_codex_model("gpt-5.4-20260305"), "gpt-5.4");
        assert_eq!(
            normalize_codex_model("claude-opus-4-6-20260206"),
            "claude-opus-4-6"
        );
    }

    #[test]
    fn test_normalize_codex_model_no_change() {
        assert_eq!(normalize_codex_model("gpt-5.4"), "gpt-5.4");
        assert_eq!(normalize_codex_model("gpt-5.2-codex"), "gpt-5.2-codex");
        assert_eq!(normalize_codex_model("o3"), "o3");
        assert_eq!(normalize_codex_model("deepseek-chat"), "deepseek-chat");
    }

    #[test]
    fn test_normalize_codex_model_combined() {
        // prefix + uppercase + ISO date
        assert_eq!(
            normalize_codex_model("openai/GPT-5.4-2026-03-05"),
            "gpt-5.4"
        );
        // prefix + compact date
        assert_eq!(normalize_codex_model("openai/gpt-5.4-20260305"), "gpt-5.4");
    }

    #[test]
    fn test_cached_clamped_to_input() {
        // cached > input 的异常场景应被 min() 钳制
        let prev = Some(CumulativeTokens {
            input: 100,
            cached_input: 0,
            output: 50,
        });
        let current = CumulativeTokens {
            input: 110,       // delta = 10
            cached_input: 80, // delta = 80（异常：大于 input delta）
            output: 60,
        };
        let delta = compute_delta(&prev, &current);
        // 钳制前：cached_input = 80, input = 10
        assert_eq!(delta.cached_input, 80);
        assert_eq!(delta.input, 10);
        // 实际钳制在调用侧：delta.cached_input.min(delta.input)
        let clamped = delta.cached_input.min(delta.input);
        assert_eq!(clamped, 10);
    }
}
