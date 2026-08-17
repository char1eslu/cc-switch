use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use regex::Regex;
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{backup::Backup, params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::codex_config::get_codex_config_dir;
use crate::session_manager::{SessionMessage, SessionMeta};

use super::utils::{
    extract_text, parse_timestamp_to_ms, path_basename, read_head_tail_lines, truncate_summary,
    TITLE_MAX_CHARS,
};

const PROVIDER_ID: &str = "codex";
const VSCODE_CONTEXT_PREFIX: &str = "# Context from my IDE setup:";
const CODEX_REQUEST_MARKER: &str = "my request for codex";
const BACKUP_MARKER: &str = ".codex-rescue-backup-";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexOperationReport {
    pub success: bool,
    pub session_id: String,
    pub timestamp: String,
    pub backups: Vec<String>,
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed_line_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexBackupFile {
    pub backup_path: String,
    pub original_path: String,
    pub original_name: String,
    pub directory: String,
    pub stamp: String,
    pub kind: String,
    pub size: u64,
    pub original_exists: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexTrashedThread {
    pub thread_id: String,
    pub title: String,
    pub original_path: String,
    pub trash_path: Option<String>,
    pub manifest_path: String,
    pub cwd: String,
    pub trashed_at: String,
    pub size: u64,
    pub original_exists: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
enum SqliteSnapshotValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SqliteRowSnapshot {
    table: String,
    columns: Vec<String>,
    values: Vec<SqliteSnapshotValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SqliteDatabaseSnapshot {
    relative_path: String,
    rows: Vec<SqliteRowSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadProjectState {
    assignment: Option<Value>,
    sidebar_project_id: Option<String>,
    sidebar_position: Option<usize>,
    was_projectless: bool,
    workspace_root_hint: Option<Value>,
    output_directory: Option<Value>,
}

static UUID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
        .unwrap()
});
static PERMISSIONS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)<permissions instructions>.*?</permissions instructions>\s*").unwrap()
});
static AGENTS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)# AGENTS\.md instructions[^\n]*(?:\n|\r\n).*?</INSTRUCTIONS>\s*").unwrap()
});
static EXTRA_BLANK_LINES_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{3,}").unwrap());

pub fn scan_sessions() -> Vec<SessionMeta> {
    let config_dir = get_codex_config_dir();
    let state_db = state_db_path(&config_dir);
    if state_db.exists() {
        match scan_sessions_from_sqlite(&config_dir, &state_db) {
            Ok(sessions) => return sessions,
            Err(error) => log::warn!("Failed to scan Codex SQLite sessions: {error}"),
        }
    }

    let roots = session_roots();
    scan_sessions_in_roots(&roots)
}

pub fn session_roots() -> Vec<PathBuf> {
    let config_dir = get_codex_config_dir();
    vec![
        config_dir.join("sessions"),
        config_dir.join("archived_sessions"),
    ]
}

fn state_db_path(codex_home: &Path) -> PathBuf {
    codex_home.join("sqlite").join("state_5.sqlite")
}

fn session_index_path(codex_home: &Path) -> PathBuf {
    codex_home.join("session_index.jsonl")
}

fn backup_trash_path(codex_home: &Path) -> PathBuf {
    codex_home.join(".codex-wake-trash")
}

fn thread_trash_path(codex_home: &Path) -> PathBuf {
    backup_trash_path(codex_home).join("threads")
}

fn global_state_path(codex_home: &Path) -> PathBuf {
    codex_home.join(".codex-global-state.json")
}

fn scan_sessions_from_sqlite(
    codex_home: &Path,
    state_db: &Path,
) -> Result<Vec<SessionMeta>, String> {
    let index = load_session_index(codex_home).unwrap_or_default();
    let conn = Connection::open(state_db).map_err(|e| {
        format!(
            "Failed to open Codex state database {}: {e}",
            state_db.display()
        )
    })?;
    let subagent_ids = load_subagent_ids(&conn)?;
    let mut stmt = conn
        .prepare(
            "select id, rollout_path, created_at, updated_at, cwd, title, first_user_message, preview, archived, source, thread_source, has_user_event \
             from threads order by updated_at desc",
        )
        .map_err(|e| format!("Failed to prepare Codex session query: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<i64>>(11)?,
            ))
        })
        .map_err(|e| format!("Failed to query Codex sessions: {e}"))?;

    let mut sessions = Vec::new();
    for row in rows.flatten() {
        let (
            session_id,
            source_path,
            created_at,
            updated_at,
            cwd,
            title,
            first_user_message,
            preview,
            archived,
            source,
            thread_source,
            has_user_event,
        ) = row;
        let path = PathBuf::from(&source_path);
        if subagent_ids.contains(&session_id)
            || thread_source
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("subagent"))
            || source.as_deref().is_some_and(is_subagent_source_text)
            || (path.exists() && is_subagent_session_file(&path))
        {
            continue;
        }
        let is_in_session_index = index.contains_key(&session_id);
        let file_exists = path.exists();
        let archived_flag = archived.unwrap_or(0) != 0;
        let needs_repair = !archived_flag
            && file_exists
            && has_user_event.unwrap_or(0) != 0
            && !is_in_session_index;
        let title = index
            .get(&session_id)
            .and_then(|entry| entry.get("thread_name").and_then(Value::as_str))
            .map(|value| value.to_string())
            .or(title)
            .or(first_user_message)
            .or_else(|| path_basename(&cwd).map(|value| value.to_string()));
        let summary = preview.map(|value| truncate_summary(&value, 160));

        let codex_status = if archived_flag {
            "Archived"
        } else if !file_exists {
            "Missing file"
        } else if !is_in_session_index {
            "Not indexed"
        } else {
            "Available"
        };

        sessions.push(SessionMeta {
            provider_id: PROVIDER_ID.to_string(),
            session_id: session_id.clone(),
            title: title.map(|value| truncate_summary(&value, TITLE_MAX_CHARS)),
            summary,
            project_dir: Some(cwd),
            created_at: created_at.map(seconds_to_ms),
            last_active_at: updated_at
                .map(seconds_to_ms)
                .or(created_at.map(seconds_to_ms)),
            source_path: Some(source_path),
            resume_command: Some(format!("codex resume {session_id}")),
            codex_status: Some(codex_status.to_string()),
            is_in_session_index: Some(is_in_session_index),
            file_exists: Some(file_exists),
            archived: Some(archived_flag),
            needs_repair: Some(needs_repair),
        });
    }

    Ok(sessions)
}

fn load_subagent_ids(conn: &Connection) -> Result<HashSet<String>, String> {
    let has_edges = conn
        .query_row(
            "select exists(select 1 from sqlite_master where type = 'table' and name = 'thread_spawn_edges')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|e| format!("Failed to inspect Codex spawn edges: {e}"))?
        != 0;
    if !has_edges {
        return Ok(HashSet::new());
    }
    let mut stmt = conn
        .prepare("select child_thread_id from thread_spawn_edges")
        .map_err(|e| format!("Failed to prepare Codex spawn edge query: {e}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| format!("Failed to read Codex spawn edges: {e}"))?;
    Ok(rows.flatten().collect())
}

fn is_subagent_source_text(source: &str) -> bool {
    serde_json::from_str::<Value>(source)
        .ok()
        .as_ref()
        .is_some_and(|value| is_subagent_source(Some(value)))
}

fn seconds_to_ms(value: i64) -> i64 {
    value.saturating_mul(1000)
}

fn scan_sessions_in_roots(roots: &[PathBuf]) -> Vec<SessionMeta> {
    let mut files = Vec::new();
    for root in roots {
        collect_jsonl_files(root, &mut files);
    }

    let mut sessions = Vec::new();
    for path in files {
        if let Some(meta) = parse_session(&path) {
            sessions.push(meta);
        }
    }

    sessions
}

pub fn load_messages(path: &Path) -> Result<Vec<SessionMessage>, String> {
    let file = File::open(path).map_err(|e| format!("Failed to open session file: {e}"))?;
    let reader = BufReader::new(file);
    let mut messages = Vec::new();
    let mut current_turn_start_line: Option<usize> = None;
    let mut has_visible_user_message_in_turn = false;
    let mut visible_user_message_count = 0usize;

    for (index, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(value) => value,
            Err(_) => continue,
        };
        let value: Value = match serde_json::from_str(&line) {
            Ok(parsed) => parsed,
            Err(_) => continue,
        };
        let line_number = index + 1;

        if value.get("type").and_then(Value::as_str) == Some("event_msg") {
            let event_type = value
                .get("payload")
                .and_then(|payload| payload.get("type"))
                .and_then(Value::as_str);
            match event_type {
                Some("task_started") => {
                    current_turn_start_line = Some(line_number);
                    has_visible_user_message_in_turn = false;
                }
                Some("task_complete") => {
                    current_turn_start_line = None;
                    has_visible_user_message_in_turn = false;
                }
                _ => {}
            }
            continue;
        }

        if value.get("type").and_then(Value::as_str) != Some("response_item") {
            continue;
        }

        let payload = match value.get("payload") {
            Some(payload) => payload,
            None => continue,
        };

        let payload_type = payload.get("type").and_then(Value::as_str).unwrap_or("");

        // Codex uses separate payload types for tool interactions
        let (role, content) = match payload_type {
            "message" => {
                let role = payload
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                if role.trim().eq_ignore_ascii_case("developer") {
                    continue;
                }
                let content = payload
                    .get("content")
                    .map(extract_text)
                    .map(|text| clean_codex_message_text(&text))
                    .unwrap_or_default();
                (role, content)
            }
            "function_call" => {
                let name = payload
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                ("assistant".to_string(), format!("[Tool: {name}]"))
            }
            "function_call_output" => {
                let output = payload
                    .get("output")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                ("tool".to_string(), output)
            }
            _ => continue,
        };

        if content.trim().is_empty() {
            continue;
        }

        let role_is_user = role.trim().eq_ignore_ascii_case("user");
        let is_first_visible_user_message = role_is_user && visible_user_message_count == 0;
        let is_turn_start_message =
            role_is_user && current_turn_start_line.is_some() && !has_visible_user_message_in_turn;
        let branch_line_number = if is_turn_start_message {
            current_turn_start_line
        } else {
            None
        };
        let ts = value.get("timestamp").and_then(parse_timestamp_to_ms);
        let can_trim = role_is_user && !is_first_visible_user_message && line_number > 2;
        let can_branch = role_is_user
            && !is_first_visible_user_message
            && branch_line_number.is_some_and(|line| line > 2);

        messages.push(SessionMessage {
            role,
            content,
            ts,
            line_number: Some(line_number),
            branch_line_number,
            can_trim: Some(can_trim),
            can_branch: Some(can_branch),
        });

        if role_is_user {
            visible_user_message_count += 1;
            has_visible_user_message_in_turn = true;
        }
    }

    Ok(messages)
}

pub fn delete_session(root: &Path, path: &Path, session_id: &str) -> Result<bool, String> {
    let meta = parse_session(path)
        .ok_or_else(|| format!("Failed to parse Codex session metadata: {}", path.display()))?;

    if meta.session_id != session_id {
        return Err(format!(
            "Codex session ID mismatch: expected {session_id}, found {}",
            meta.session_id
        ));
    }

    let codex_home = codex_home_for_session_root(root);
    let state_db = state_db_path(&codex_home);
    let (sqlite_record, related_rows) = if state_db.exists() {
        let conn = Connection::open(&state_db).map_err(|e| {
            format!(
                "Failed to open Codex state database {}: {e}",
                state_db.display()
            )
        })?;
        let record = load_sqlite_record(&conn, session_id)?;
        if sqlite_record_is_subagent(&record) {
            return Err(
                "Subagent sessions follow their parent and cannot be deleted independently"
                    .to_string(),
            );
        }
        (record, snapshot_reference_rows(&conn, session_id, true)?)
    } else {
        (Value::Null, Vec::new())
    };
    let external_databases = snapshot_external_databases(&codex_home, session_id)?;
    let global_state = global_state_path(&codex_home);
    let original_global_state = fs::read(&global_state).map_err(|e| {
        format!(
            "Failed to read Codex global project metadata {}: {e}",
            global_state.display()
        )
    })?;
    let (removed_global_state, _) =
        global_state_removing_thread(&original_global_state, session_id)?;
    let session_index = session_index_path(&codex_home);
    let original_session_index = if session_index.exists() {
        Some(fs::read(&session_index).map_err(|e| {
            format!(
                "Failed to read Codex session index {}: {e}",
                session_index.display()
            )
        })?)
    } else {
        None
    };

    let stamp = format!("{}-delete", Utc::now().format("%Y%m%d-%H%M%S"));
    backup_state_files(&codex_home, &stamp)?;
    backup_file(&global_state, &stamp)?;
    backup_file(path, &stamp)?;
    if session_index.exists() {
        backup_file(&session_index, &stamp)?;
    }
    for snapshot in &external_databases {
        if !snapshot.rows.is_empty() {
            backup_sqlite_database(
                &external_database_path(&codex_home, &snapshot.relative_path)?,
                &stamp,
            )?;
        }
    }

    let result = (|| {
        crate::config::atomic_write(&global_state, &removed_global_state)
            .map_err(|e| e.to_string())?;
        if state_db.exists() {
            let mut conn = Connection::open(&state_db).map_err(|e| {
                format!(
                    "Failed to open Codex state database {}: {e}",
                    state_db.display()
                )
            })?;
            delete_reference_rows(&mut conn, session_id, true)?;
        }
        delete_external_database_rows(&codex_home, &external_databases, session_id)?;
        remove_session_index_entry(&session_index, session_id)?;
        fs::remove_file(path).map_err(|e| {
            format!(
                "Failed to delete Codex session file {}: {e}",
                path.display()
            )
        })
    })();
    if let Err(error) = result {
        if state_db.exists() {
            if let Ok(mut conn) = Connection::open(&state_db) {
                if let Some(record) = sqlite_record.as_object() {
                    let _ = insert_sqlite_record(&conn, record);
                }
                let _ = restore_snapshot_rows(&mut conn, &related_rows);
            }
        }
        let _ = restore_external_database_rows(&codex_home, &external_databases);
        let _ = crate::config::atomic_write(&global_state, &original_global_state);
        if let Some(index) = original_session_index {
            let _ = crate::config::atomic_write(&session_index, &index);
        }
        return Err(error);
    }

    Ok(true)
}

pub fn move_session(
    root: &Path,
    path: &Path,
    session_id: &str,
    target_project_dir: &str,
) -> Result<bool, String> {
    let codex_home = codex_home_for_session_root(root);
    move_session_with_home(&codex_home, path, session_id, target_project_dir)
}

fn move_session_with_home(
    codex_home: &Path,
    path: &Path,
    session_id: &str,
    target_project_dir: &str,
) -> Result<bool, String> {
    let meta = parse_session(path)
        .ok_or_else(|| format!("Failed to parse Codex session metadata: {}", path.display()))?;

    if meta.session_id != session_id {
        return Err(format!(
            "Codex session ID mismatch: expected {session_id}, found {}",
            meta.session_id
        ));
    }

    let target_project_dir = target_project_dir.trim();
    if target_project_dir.is_empty() {
        return Err("Target project directory is required".to_string());
    }

    let state_db = codex_home.join("sqlite").join("state_5.sqlite");
    let global_state = global_state_path(codex_home);
    if !global_state.exists() {
        return Err(format!(
            "Codex global project metadata is missing: {}",
            global_state.display()
        ));
    }
    let original_global_state = fs::read(&global_state).map_err(|e| {
        format!(
            "Failed to read Codex global project metadata {}: {e}",
            global_state.display()
        )
    })?;
    let moved_global_state =
        global_state_moving(&original_global_state, session_id, target_project_dir)?;
    let original_session = fs::read(path)
        .map_err(|e| format!("Failed to read Codex session file {}: {e}", path.display()))?;

    let stamp = format!("{}-move", Utc::now().format("%Y%m%d-%H%M%S"));
    backup_state_files(codex_home, &stamp)?;
    backup_file(&global_state, &stamp)?;
    backup_file(path, &stamp)?;

    let result = (|| {
        crate::config::atomic_write(&global_state, &moved_global_state)
            .map_err(|e| e.to_string())?;
        update_session_project(path, target_project_dir)?;
        if state_db.exists() {
            update_sqlite_project(&state_db, session_id, target_project_dir)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let _ = crate::config::atomic_write(&global_state, &original_global_state);
        let _ = crate::config::atomic_write(path, &original_session);
        if state_db.exists() {
            let original_cwd = meta.project_dir.as_deref().unwrap_or_default();
            let _ = update_sqlite_project(&state_db, session_id, original_cwd);
        }
        return Err(error);
    }

    Ok(true)
}

pub fn repair_session(path: &Path, session_id: &str) -> Result<CodexOperationReport, String> {
    let codex_home = get_codex_config_dir();
    let meta = parse_session(path)
        .ok_or_else(|| format!("Failed to parse Codex session metadata: {}", path.display()))?;
    if meta.session_id != session_id {
        return Err(format!(
            "Codex session ID mismatch: expected {session_id}, found {}",
            meta.session_id
        ));
    }

    let stamp = format!("{}-wake", Utc::now().format("%Y%m%d-%H%M%S"));
    let mut backups = backup_state_files(&codex_home, &stamp)?;
    let session_index = session_index_path(&codex_home);
    if session_index.exists() {
        backups.push(backup_file(&session_index, &stamp)?);
    }
    backups.push(backup_file(path, &stamp)?);

    let now = Utc::now();
    let updated_at = now.timestamp();
    let updated_at_ms = now.timestamp_millis();
    let now_jsonl = iso_jsonl(now);
    let now_index = iso_index(now);
    let title = meta
        .title
        .clone()
        .or_else(|| meta.project_dir.as_deref().and_then(path_basename))
        .unwrap_or_else(|| session_id.to_string());

    let state_db = state_db_path(&codex_home);
    if state_db.exists() {
        let conn = Connection::open(&state_db).map_err(|e| {
            format!(
                "Failed to open Codex state database {}: {e}",
                state_db.display()
            )
        })?;
        conn.execute(
            "update threads set thread_source = 'user', updated_at = ?1, updated_at_ms = ?2 where id = ?3",
            params![updated_at, updated_at_ms, session_id],
        )
        .map_err(|e| format!("Failed to repair Codex state database: {e}"))?;
    }
    upsert_session_index(&session_index, session_id, &title, &now_index)?;
    update_session_meta_timestamp(path, &now_jsonl)?;

    Ok(CodexOperationReport {
        success: true,
        session_id: session_id.to_string(),
        timestamp: stamp,
        backups: backups_to_strings(backups),
        changed_files: vec![
            state_db.to_string_lossy().to_string(),
            session_index.to_string_lossy().to_string(),
            path.to_string_lossy().to_string(),
        ],
        new_session_id: None,
        new_source_path: None,
        removed_line_count: None,
    })
}

pub fn trim_session(
    path: &Path,
    session_id: &str,
    line_number: usize,
) -> Result<CodexOperationReport, String> {
    let meta = parse_session(path)
        .ok_or_else(|| format!("Failed to parse Codex session metadata: {}", path.display()))?;
    if meta.session_id != session_id {
        return Err(format!(
            "Codex session ID mismatch: expected {session_id}, found {}",
            meta.session_id
        ));
    }
    if line_number <= 1 {
        return Err("Cannot trim before the first JSONL line".to_string());
    }

    let stamp = format!("{}-trim", Utc::now().format("%Y%m%d-%H%M%S"));
    let backup = backup_file(path, &stamp)?;
    let mut lines = read_jsonl_lines(path)?;
    if line_number > lines.len() {
        return Err("Trim line is outside the chat file".to_string());
    }
    let removed_line_count = lines.len().saturating_sub(line_number - 1);
    lines.truncate(line_number - 1);
    crate::config::atomic_write(path, format!("{}\n", lines.join("\n")).as_bytes())
        .map_err(|e| e.to_string())?;

    Ok(CodexOperationReport {
        success: true,
        session_id: session_id.to_string(),
        timestamp: stamp,
        backups: vec![backup.to_string_lossy().to_string()],
        changed_files: vec![path.to_string_lossy().to_string()],
        new_session_id: None,
        new_source_path: None,
        removed_line_count: Some(removed_line_count),
    })
}

pub fn branch_session(
    path: &Path,
    session_id: &str,
    line_number: usize,
) -> Result<CodexOperationReport, String> {
    let codex_home = get_codex_config_dir();
    let meta = parse_session(path)
        .ok_or_else(|| format!("Failed to parse Codex session metadata: {}", path.display()))?;
    if meta.session_id != session_id {
        return Err(format!(
            "Codex session ID mismatch: expected {session_id}, found {}",
            meta.session_id
        ));
    }
    if line_number <= 1 {
        return Err("Cannot branch before the first JSONL line".to_string());
    }

    let lines = read_jsonl_lines(path)?;
    if line_number > lines.len() {
        return Err("Branch line is outside the chat file".to_string());
    }
    let kept_lines = lines.into_iter().take(line_number - 1).collect::<Vec<_>>();
    if kept_lines.is_empty() {
        return Err("Branch would create an empty chat".to_string());
    }

    let now = Utc::now();
    let stamp = format!("{}-branch", now.format("%Y%m%d-%H%M%S"));
    let mut backups = backup_state_files(&codex_home, &stamp)?;
    let session_index = session_index_path(&codex_home);
    if session_index.exists() {
        backups.push(backup_file(&session_index, &stamp)?);
    }

    let new_session_id = Uuid::new_v4().to_string();
    let project_dir = meta.project_dir.clone().unwrap_or_default();
    let base_title = meta
        .title
        .clone()
        .or_else(|| path_basename(&project_dir))
        .unwrap_or_else(|| session_id.to_string());
    let title = format!("Branch: {base_title}");
    let new_path = codex_home
        .join("sessions")
        .join(now.format("%Y/%m/%d").to_string())
        .join(format!(
            "rollout-{}-{new_session_id}.jsonl",
            now.format("%Y-%m-%dT%H-%M-%S")
        ));
    let branched = branch_content(&kept_lines, &new_session_id, &iso_jsonl(now), &project_dir)?;
    if let Some(parent) = new_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create branch directory {}: {e}",
                parent.display()
            )
        })?;
    }
    crate::config::atomic_write(&new_path, branched.as_bytes()).map_err(|e| e.to_string())?;

    let state_db = state_db_path(&codex_home);
    let mut inserted_sqlite_row = false;
    if state_db.exists() {
        if let Err(error) = insert_branched_sqlite_row(
            &state_db,
            session_id,
            &new_session_id,
            &new_path,
            &truncate_summary(&title, 240),
            now,
        ) {
            let _ = fs::remove_file(&new_path);
            return Err(error);
        }
        inserted_sqlite_row = true;
    }
    if let Err(error) = append_session_index(
        &session_index,
        &new_session_id,
        &truncate_summary(&title, 240),
        &iso_index(now),
    ) {
        if inserted_sqlite_row {
            let _ = delete_sqlite_thread(&state_db, &new_session_id);
        }
        let _ = fs::remove_file(&new_path);
        return Err(error);
    }

    Ok(CodexOperationReport {
        success: true,
        session_id: session_id.to_string(),
        timestamp: stamp,
        backups: backups_to_strings(backups),
        changed_files: vec![
            new_path.to_string_lossy().to_string(),
            state_db.to_string_lossy().to_string(),
            session_index.to_string_lossy().to_string(),
        ],
        new_session_id: Some(new_session_id),
        new_source_path: Some(new_path.to_string_lossy().to_string()),
        removed_line_count: None,
    })
}

pub fn trash_session(path: &Path, session_id: &str) -> Result<CodexOperationReport, String> {
    let codex_home = get_codex_config_dir();
    trash_session_with_home(&codex_home, path, session_id)
}

fn trash_session_with_home(
    codex_home: &Path,
    path: &Path,
    session_id: &str,
) -> Result<CodexOperationReport, String> {
    let sessions_root = codex_home.join("sessions");
    let file_exists = path.exists();
    if file_exists {
        let canonical_path = path
            .canonicalize()
            .map_err(|e| format!("Failed to resolve session path {}: {e}", path.display()))?;
        let canonical_root = sessions_root.canonicalize().map_err(|e| {
            format!(
                "Failed to resolve Codex sessions root {}: {e}",
                sessions_root.display()
            )
        })?;
        if !canonical_path.starts_with(&canonical_root) {
            return Err("Refusing to trash a Codex chat outside the sessions root".to_string());
        }
    } else if !path.is_absolute() || !path.starts_with(&sessions_root) {
        return Err(
            "Refusing to trash missing Codex metadata outside ~/.codex/sessions".to_string(),
        );
    }

    let state_db = state_db_path(codex_home);
    let (sqlite_record, related_rows) = if state_db.exists() {
        let conn = Connection::open(&state_db).map_err(|e| {
            format!(
                "Failed to open Codex state database {}: {e}",
                state_db.display()
            )
        })?;
        let record = load_sqlite_record(&conn, session_id)?;
        if sqlite_record_is_subagent(&record) {
            return Err(
                "Subagent sessions follow their parent and cannot be trashed independently"
                    .to_string(),
            );
        }
        (record, snapshot_reference_rows(&conn, session_id, true)?)
    } else {
        (Value::Null, Vec::new())
    };
    let external_databases = snapshot_external_databases(codex_home, session_id)?;
    let global_state = global_state_path(codex_home);
    let original_global_state = fs::read(&global_state).map_err(|e| {
        format!(
            "Failed to read Codex global project metadata {}: {e}",
            global_state.display()
        )
    })?;
    let (removed_global_state, project_state) =
        global_state_removing_thread(&original_global_state, session_id)?;

    let stamp = format!("{}-trash-thread", Utc::now().format("%Y%m%d-%H%M%S"));
    let mut backups = backup_state_files(codex_home, &stamp)?;
    backups.push(backup_file(&global_state, &stamp)?);
    for snapshot in &external_databases {
        if !snapshot.rows.is_empty() {
            backups.push(backup_sqlite_database(
                &external_database_path(codex_home, &snapshot.relative_path)?,
                &stamp,
            )?);
        }
    }
    let session_index = session_index_path(codex_home);
    let original_session_index = if session_index.exists() {
        Some(fs::read(&session_index).map_err(|e| {
            format!(
                "Failed to read Codex session index {}: {e}",
                session_index.display()
            )
        })?)
    } else {
        None
    };
    if session_index.exists() {
        backups.push(backup_file(&session_index, &stamp)?);
    }
    let session_index_entry = load_session_index(codex_home)
        .unwrap_or_default()
        .get(session_id)
        .cloned();
    let meta = if file_exists {
        let meta = parse_session(path)
            .ok_or_else(|| format!("Failed to parse Codex session metadata: {}", path.display()))?;
        if meta.session_id != session_id {
            return Err(format!(
                "Codex session ID mismatch: expected {session_id}, found {}",
                meta.session_id
            ));
        }
        meta
    } else {
        session_meta_from_sqlite_record(session_id, path, &sqlite_record)?
    };

    let trash_dir = thread_trash_path(codex_home).join(session_id);
    fs::create_dir_all(&trash_dir).map_err(|e| {
        format!(
            "Failed to create trash directory {}: {e}",
            trash_dir.display()
        )
    })?;
    let trash_path = if file_exists {
        let file_name = path
            .file_name()
            .ok_or_else(|| format!("Invalid Codex session path: {}", path.display()))?;
        let trash_path = unique_path(&trash_dir.join(file_name));
        fs::rename(path, &trash_path).map_err(|e| {
            format!(
                "Failed to move Codex session to trash {} -> {}: {e}",
                path.display(),
                trash_path.display()
            )
        })?;
        Some(trash_path)
    } else {
        None
    };

    let manifest_title = meta.title.clone().unwrap_or_else(|| session_id.to_string());
    let manifest_cwd = meta.project_dir.clone().unwrap_or_default();
    let manifest = serde_json::json!({
        "version": 2,
        "threadID": session_id,
        "title": manifest_title,
        "originalPath": path.to_string_lossy(),
        "trashPath": trash_path.as_ref().map(|path| path.to_string_lossy().to_string()),
        "cwd": manifest_cwd,
        "trashedAt": iso_jsonl(Utc::now()),
        "sqliteRecord": sqlite_record,
        "sessionIndexEntry": session_index_entry,
        "relatedRows": related_rows,
        "externalDatabases": external_databases,
        "projectState": project_state,
    });
    let result = (|| {
        crate::config::atomic_write(
            &trash_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest)
                .map_err(|e| format!("Failed to encode trash manifest: {e}"))?
                .as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        crate::config::atomic_write(&global_state, &removed_global_state)
            .map_err(|e| e.to_string())?;
        if state_db.exists() {
            let mut conn = Connection::open(&state_db).map_err(|e| {
                format!(
                    "Failed to open Codex state database {}: {e}",
                    state_db.display()
                )
            })?;
            delete_reference_rows(&mut conn, session_id, true)?;
        }
        delete_external_database_rows(codex_home, &external_databases, session_id)?;
        remove_session_index_entry(&session_index, session_id)
    })();
    if let Err(error) = result {
        if state_db.exists() {
            if let Ok(mut conn) = Connection::open(&state_db) {
                if let Some(record) = sqlite_record.as_object() {
                    let _ = insert_sqlite_record(&conn, record);
                }
                let _ = restore_snapshot_rows(&mut conn, &related_rows);
            }
        }
        let _ = restore_external_database_rows(codex_home, &external_databases);
        let _ = crate::config::atomic_write(&global_state, &original_global_state);
        if let Some(index) = original_session_index {
            let _ = crate::config::atomic_write(&session_index, &index);
        }
        if let Some(trash_path) = &trash_path {
            if !path.exists() {
                let _ = fs::rename(trash_path, path);
            }
        }
        let _ = fs::remove_dir_all(&trash_dir);
        return Err(error);
    }

    Ok(CodexOperationReport {
        success: true,
        session_id: session_id.to_string(),
        timestamp: stamp,
        backups: backups_to_strings(backups),
        changed_files: vec![
            path.to_string_lossy().to_string(),
            state_db.to_string_lossy().to_string(),
            session_index.to_string_lossy().to_string(),
            global_state.to_string_lossy().to_string(),
        ],
        new_session_id: None,
        new_source_path: trash_path.map(|path| path.to_string_lossy().to_string()),
        removed_line_count: None,
    })
}

pub fn search_raw_session_ids(
    query: &str,
    project_dir: Option<&str>,
) -> Result<Vec<String>, String> {
    let query = query.trim().to_lowercase();
    if query.len() < 3 {
        return Ok(Vec::new());
    }

    let project_dir = project_dir.map(str::trim).filter(|value| !value.is_empty());
    let mut matched_ids = Vec::new();

    for session in scan_sessions() {
        if let Some(project_dir) = project_dir {
            if session.project_dir.as_deref() != Some(project_dir) {
                continue;
            }
        }

        let Some(source_path) = session.source_path.as_deref() else {
            continue;
        };
        if raw_file_contains(Path::new(source_path), &query)? {
            matched_ids.push(session.session_id);
        }
    }

    Ok(matched_ids)
}

pub fn list_backups(include_trash: bool) -> Result<Vec<CodexBackupFile>, String> {
    let codex_home = get_codex_config_dir();
    let root = if include_trash {
        backup_trash_path(&codex_home)
    } else {
        codex_home.clone()
    };
    let mut backups = Vec::new();
    collect_backup_files(&codex_home, &root, include_trash, &mut backups)?;
    backups.sort_by(|a, b| b.stamp.cmp(&a.stamp));
    Ok(backups)
}

pub fn restore_backup(backup_path: &Path, original_path: &Path) -> Result<bool, String> {
    let codex_home = get_codex_config_dir();
    let backup_path = backup_path.canonicalize().map_err(|e| {
        format!(
            "Failed to resolve backup path {}: {e}",
            backup_path.display()
        )
    })?;
    let codex_home_canonical = codex_home
        .canonicalize()
        .map_err(|e| format!("Failed to resolve Codex home {}: {e}", codex_home.display()))?;
    if !backup_path.starts_with(&codex_home_canonical)
        || !backup_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(BACKUP_MARKER))
    {
        return Err("Refusing to restore a non-Codex backup file".to_string());
    }

    let original_path = original_path.to_path_buf();
    if let Some(parent) = original_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create restore directory {}: {e}",
                parent.display()
            )
        })?;
    }
    if original_path.exists() {
        backup_file(
            &original_path,
            &format!("{}-before-restore", Utc::now().format("%Y%m%d-%H%M%S")),
        )?;
    }
    fs::copy(&backup_path, &original_path).map_err(|e| {
        format!(
            "Failed to restore backup {} to {}: {e}",
            backup_path.display(),
            original_path.display()
        )
    })?;
    Ok(true)
}

pub fn move_backup_to_trash(backup_path: &Path) -> Result<bool, String> {
    let codex_home = get_codex_config_dir();
    let backup_path = backup_path.canonicalize().map_err(|e| {
        format!(
            "Failed to resolve backup path {}: {e}",
            backup_path.display()
        )
    })?;
    let codex_home_canonical = codex_home
        .canonicalize()
        .map_err(|e| format!("Failed to resolve Codex home {}: {e}", codex_home.display()))?;
    if !backup_path.starts_with(&codex_home_canonical)
        || !backup_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(BACKUP_MARKER))
    {
        return Err("Refusing to trash a non-Codex backup file".to_string());
    }

    let relative_dir = backup_path
        .parent()
        .and_then(|parent| parent.strip_prefix(&codex_home_canonical).ok())
        .unwrap_or_else(|| Path::new(""));
    let destination_dir = backup_trash_path(&codex_home).join(relative_dir);
    fs::create_dir_all(&destination_dir).map_err(|e| {
        format!(
            "Failed to create backup trash directory {}: {e}",
            destination_dir.display()
        )
    })?;
    let file_name = backup_path
        .file_name()
        .ok_or_else(|| format!("Invalid backup path: {}", backup_path.display()))?;
    let destination = unique_path(&destination_dir.join(file_name));
    fs::rename(&backup_path, &destination).map_err(|e| {
        format!(
            "Failed to move backup to trash {} -> {}: {e}",
            backup_path.display(),
            destination.display()
        )
    })?;
    Ok(true)
}

pub fn list_trashed_threads() -> Result<Vec<CodexTrashedThread>, String> {
    let codex_home = get_codex_config_dir();
    let root = thread_trash_path(&codex_home);
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut threads = Vec::new();
    collect_trashed_threads(&root, &mut threads)?;
    threads.sort_by(|a, b| b.trashed_at.cmp(&a.trashed_at));
    Ok(threads)
}

pub fn restore_trashed_thread(manifest_path: &Path) -> Result<bool, String> {
    let codex_home = get_codex_config_dir();
    restore_trashed_thread_with_home(&codex_home, manifest_path)
}

fn restore_trashed_thread_with_home(
    codex_home: &Path,
    manifest_path: &Path,
) -> Result<bool, String> {
    let manifest_path = manifest_path.canonicalize().map_err(|e| {
        format!(
            "Failed to resolve trash manifest {}: {e}",
            manifest_path.display()
        )
    })?;
    let thread_trash = thread_trash_path(codex_home)
        .canonicalize()
        .map_err(|e| format!("Failed to resolve thread trash: {e}"))?;
    if !manifest_path.starts_with(&thread_trash) {
        return Err("Refusing to restore a chat outside Codex trash".to_string());
    }
    let manifest: Value =
        serde_json::from_str(&fs::read_to_string(&manifest_path).map_err(|e| {
            format!(
                "Failed to read trash manifest {}: {e}",
                manifest_path.display()
            )
        })?)
        .map_err(|e| format!("Failed to parse trash manifest: {e}"))?;
    let original_path = manifest
        .get("originalPath")
        .and_then(Value::as_str)
        .ok_or_else(|| "Trash manifest is missing originalPath".to_string())?;
    let trash_path = manifest.get("trashPath").and_then(Value::as_str);
    let original_path = PathBuf::from(original_path);
    let sessions_root = codex_home.join("sessions");
    if !original_path.is_absolute() || !original_path.starts_with(&sessions_root) {
        return Err("Refusing to restore a chat outside ~/.codex/sessions".to_string());
    }
    if original_path.exists() {
        return Err("Original chat file already exists".to_string());
    }
    let backup_suffix = format!(
        "{}-before-trash-restore",
        Utc::now().format("%Y%m%d-%H%M%S")
    );
    backup_state_files(codex_home, &backup_suffix)?;
    let related_rows = manifest
        .get("relatedRows")
        .cloned()
        .map(serde_json::from_value::<Vec<SqliteRowSnapshot>>)
        .transpose()
        .map_err(|e| format!("Failed to decode Codex related metadata: {e}"))?
        .unwrap_or_default();
    let external_databases = manifest
        .get("externalDatabases")
        .cloned()
        .map(serde_json::from_value::<Vec<SqliteDatabaseSnapshot>>)
        .transpose()
        .map_err(|e| format!("Failed to decode Codex database snapshots: {e}"))?
        .unwrap_or_default();
    for snapshot in &external_databases {
        let path = external_database_path(codex_home, &snapshot.relative_path)?;
        if path.exists() && !snapshot.rows.is_empty() {
            backup_sqlite_database(&path, &backup_suffix)?;
        }
    }
    let global_state = global_state_path(codex_home);
    let current_global_state = fs::read(&global_state).map_err(|e| {
        format!(
            "Failed to read Codex global project metadata {}: {e}",
            global_state.display()
        )
    })?;
    backup_file(&global_state, &backup_suffix)?;
    let project_state = manifest
        .get("projectState")
        .cloned()
        .map(serde_json::from_value::<ThreadProjectState>)
        .transpose()
        .map_err(|e| format!("Failed to decode Codex project metadata: {e}"))?;
    let restored_global_state = project_state
        .as_ref()
        .map(|state| {
            global_state_restoring_thread(
                &current_global_state,
                manifest["threadID"].as_str().unwrap_or_default(),
                state,
            )
        })
        .transpose()?;
    let session_index = session_index_path(codex_home);
    if session_index.exists() {
        backup_file(&session_index, &backup_suffix)?;
    }
    if let Some(parent) = original_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create restore directory {}: {e}",
                parent.display()
            )
        })?;
    }
    if let Some(trash_path) = trash_path {
        let trash_path = PathBuf::from(trash_path)
            .canonicalize()
            .map_err(|e| format!("Failed to resolve trashed chat file: {e}"))?;
        if !trash_path.starts_with(&thread_trash) {
            return Err("Refusing to restore a chat file outside Codex trash".to_string());
        }
        fs::copy(&trash_path, &original_path).map_err(|e| {
            format!(
                "Failed to restore trashed chat {} to {}: {e}",
                trash_path.display(),
                original_path.display()
            )
        })?;
    }

    let session_id = manifest
        .get("threadID")
        .and_then(Value::as_str)
        .ok_or_else(|| "Trash manifest is missing threadID".to_string())?;
    let state_db = state_db_path(codex_home);
    let result = (|| {
        if state_db.exists() {
            let mut conn = Connection::open(&state_db).map_err(|e| {
                format!(
                    "Failed to open Codex state database {}: {e}",
                    state_db.display()
                )
            })?;
            if let Some(record) = manifest.get("sqliteRecord").and_then(Value::as_object) {
                insert_sqlite_record(&conn, record)?;
            }
            restore_snapshot_rows(&mut conn, &related_rows)?;
        }
        restore_external_database_rows(codex_home, &external_databases)?;
        if let Some(entry) = manifest
            .get("sessionIndexEntry")
            .filter(|value| !value.is_null())
        {
            append_raw_session_index_entry(&session_index, entry)?;
        }
        if let Some(global_state_data) = &restored_global_state {
            crate::config::atomic_write(&global_state, global_state_data)
                .map_err(|e| e.to_string())?;
        }
        delete_trash_directory(&manifest_path)
    })();
    if let Err(error) = result {
        if state_db.exists() {
            if let Ok(mut conn) = Connection::open(&state_db) {
                let _ = delete_reference_rows(&mut conn, session_id, true);
            }
        }
        let _ = delete_external_database_rows(codex_home, &external_databases, session_id);
        let _ = crate::config::atomic_write(&global_state, &current_global_state);
        if original_path.exists() {
            let _ = fs::remove_file(&original_path);
        }
        return Err(error);
    }
    Ok(true)
}

pub fn delete_trashed_thread(manifest_path: &Path) -> Result<bool, String> {
    let codex_home = get_codex_config_dir();
    let manifest_path = manifest_path.canonicalize().map_err(|e| {
        format!(
            "Failed to resolve trash manifest {}: {e}",
            manifest_path.display()
        )
    })?;
    let thread_trash = thread_trash_path(&codex_home)
        .canonicalize()
        .map_err(|e| format!("Failed to resolve thread trash: {e}"))?;
    if !manifest_path.starts_with(&thread_trash) {
        return Err("Refusing to delete a chat outside Codex trash".to_string());
    }
    delete_trash_directory(&manifest_path)?;
    Ok(true)
}

pub fn empty_thread_trash() -> Result<usize, String> {
    let codex_home = get_codex_config_dir();
    let trash = thread_trash_path(&codex_home);
    if !trash.exists() {
        return Ok(0);
    }
    let trash = trash
        .canonicalize()
        .map_err(|e| format!("Failed to resolve thread trash: {e}"))?;
    let mut removed = 0;
    for entry in fs::read_dir(&trash)
        .map_err(|e| format!("Failed to read thread trash {}: {e}", trash.display()))?
        .flatten()
    {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let path = match path.canonicalize() {
            Ok(path) => path,
            Err(_) => continue,
        };
        if !path.starts_with(&trash) {
            continue;
        }
        fs::remove_dir_all(&path)
            .map_err(|e| format!("Failed to remove trashed thread {}: {e}", path.display()))?;
        removed += 1;
    }
    Ok(removed)
}

pub fn empty_backup_trash() -> Result<usize, String> {
    let codex_home = get_codex_config_dir();
    let trash = backup_trash_path(&codex_home);
    if !trash.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in fs::read_dir(&trash)
        .map_err(|e| format!("Failed to read backup trash {}: {e}", trash.display()))?
        .flatten()
    {
        let path = entry.path();
        if path == thread_trash_path(&codex_home) {
            continue;
        }
        if path.is_dir() {
            fs::remove_dir_all(&path)
                .map_err(|e| format!("Failed to remove trash directory {}: {e}", path.display()))?;
        } else {
            fs::remove_file(&path)
                .map_err(|e| format!("Failed to remove trash file {}: {e}", path.display()))?;
        }
        removed += 1;
    }
    Ok(removed)
}

fn parse_session(path: &Path) -> Option<SessionMeta> {
    let (head, tail) = read_head_tail_lines(path, 10, 30).ok()?;

    let mut session_id: Option<String> = None;
    let mut project_dir: Option<String> = None;
    let mut created_at: Option<i64> = None;
    let mut first_user_message: Option<String> = None;

    // Extract metadata and first user message from head lines
    for line in &head {
        let value: Value = match serde_json::from_str(line) {
            Ok(parsed) => parsed,
            Err(_) => continue,
        };
        if created_at.is_none() {
            created_at = value.get("timestamp").and_then(parse_timestamp_to_ms);
        }
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            if let Some(payload) = value.get("payload") {
                if is_subagent_source(payload.get("source")) {
                    return None;
                }
                if session_id.is_none() {
                    session_id = payload
                        .get("id")
                        .and_then(Value::as_str)
                        .map(|s| s.to_string());
                }
                if project_dir.is_none() {
                    project_dir = payload
                        .get("cwd")
                        .and_then(Value::as_str)
                        .map(|s| s.to_string());
                }
                if let Some(ts) = payload.get("timestamp").and_then(parse_timestamp_to_ms) {
                    created_at.get_or_insert(ts);
                }
            }
        }
        // Extract first user message as title candidate
        if first_user_message.is_none()
            && value.get("type").and_then(Value::as_str) == Some("response_item")
        {
            if let Some(payload) = value.get("payload") {
                if payload.get("type").and_then(Value::as_str) == Some("message")
                    && payload.get("role").and_then(Value::as_str) == Some("user")
                {
                    let text = payload.get("content").map(extract_text).unwrap_or_default();
                    if let Some(title) = title_candidate_from_user_message(&text) {
                        first_user_message = Some(title);
                    }
                }
            }
        }
        if session_id.is_some()
            && project_dir.is_some()
            && created_at.is_some()
            && first_user_message.is_some()
        {
            break;
        }
    }

    // Extract last_active_at and summary from tail lines (reverse order)
    let mut last_active_at: Option<i64> = None;
    let mut summary: Option<String> = None;

    for line in tail.iter().rev() {
        let value: Value = match serde_json::from_str(line) {
            Ok(parsed) => parsed,
            Err(_) => continue,
        };
        if last_active_at.is_none() {
            last_active_at = value.get("timestamp").and_then(parse_timestamp_to_ms);
        }
        if summary.is_none() && value.get("type").and_then(Value::as_str) == Some("response_item") {
            if let Some(payload) = value.get("payload") {
                if payload.get("type").and_then(Value::as_str) == Some("message") {
                    let text = payload.get("content").map(extract_text).unwrap_or_default();
                    if !text.trim().is_empty() {
                        summary = Some(text);
                    }
                }
            }
        }
        if last_active_at.is_some() && summary.is_some() {
            break;
        }
    }

    let session_id = session_id.or_else(|| infer_session_id_from_filename(path));
    let session_id = session_id?;

    let title = first_user_message
        .map(|t| truncate_summary(&t, TITLE_MAX_CHARS))
        .or_else(|| {
            project_dir
                .as_deref()
                .and_then(path_basename)
                .map(|v| v.to_string())
        });

    let summary = summary.map(|text| truncate_summary(&text, 160));

    Some(SessionMeta {
        provider_id: PROVIDER_ID.to_string(),
        session_id: session_id.clone(),
        title,
        summary,
        project_dir,
        created_at,
        last_active_at,
        source_path: Some(path.to_string_lossy().to_string()),
        resume_command: Some(format!("codex resume {session_id}")),
        codex_status: None,
        is_in_session_index: None,
        file_exists: Some(path.exists()),
        archived: None,
        needs_repair: None,
    })
}

fn is_subagent_source(source: Option<&Value>) -> bool {
    source
        .and_then(|value| value.as_object())
        .map(|source| source.contains_key("subagent"))
        .unwrap_or(false)
}

fn is_subagent_session_file(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let reader = BufReader::new(file);
    for line in reader.lines().take(10).flatten() {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        return value
            .get("payload")
            .map(|payload| is_subagent_source(payload.get("source")))
            .unwrap_or(false);
    }
    false
}

fn load_session_index(codex_home: &Path) -> Result<HashMap<String, Value>, String> {
    let path = session_index_path(codex_home);
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let text = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read session index {}: {e}", path.display()))?;
    let mut result = HashMap::new();
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(id) = value.get("id").and_then(Value::as_str) {
            result.insert(id.to_string(), value);
        }
    }
    Ok(result)
}

fn title_candidate_from_user_message(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty()
        || trimmed.starts_with("# AGENTS.md")
        || trimmed.starts_with("<environment_context>")
    {
        return None;
    }

    if trimmed.starts_with(VSCODE_CONTEXT_PREFIX) {
        return extract_codex_prompt_from_ide_context(trimmed);
    }

    Some(trimmed.to_string())
}

fn extract_codex_prompt_from_ide_context(text: &str) -> Option<String> {
    let normalized = text.replace("\r\n", "\n");
    let lines = normalized.lines().collect::<Vec<_>>();

    // VS Code injects the real prompt as the LAST "## My request for Codex:"
    // section, so keep the final matching heading. Earlier matches can be
    // headings that live inside the active selection / open file content.
    // Trade-off: if the request body itself repeats the heading, the title
    // truncates to its trailing part (rare; covered by tests below).
    let mut prompt: Option<String> = None;
    for (index, line) in lines.iter().enumerate() {
        let Some(inline_prompt) = codex_request_heading_payload(line) else {
            continue;
        };

        if !inline_prompt.is_empty() {
            prompt = Some(inline_prompt.to_string());
            continue;
        }

        let following_prompt = lines[index + 1..].join("\n").trim().to_string();
        prompt = (!following_prompt.is_empty()).then_some(following_prompt);
    }

    prompt
}

fn codex_request_heading_payload(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if !trimmed.starts_with('#') {
        return None;
    }

    let heading = trimmed.trim_start_matches('#').trim_start();
    let lowered = heading.to_ascii_lowercase();
    if !lowered.starts_with(CODEX_REQUEST_MARKER) {
        return None;
    }

    let suffix = heading[CODEX_REQUEST_MARKER.len()..].trim_start();
    if suffix.is_empty() {
        return Some("");
    }

    let Some(separator) = suffix.chars().next() else {
        return Some("");
    };
    if !matches!(separator, ':' | '：' | '-' | '—') {
        return None;
    }

    Some(
        suffix
            .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '：' | '-' | '—'))
            .trim(),
    )
}

fn infer_session_id_from_filename(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_string_lossy();
    UUID_RE.find(&file_name).map(|mat| mat.as_str().to_string())
}

fn clean_codex_message_text(text: &str) -> String {
    let without_permissions = PERMISSIONS_RE.replace_all(text, "");
    let without_agents = AGENTS_RE.replace_all(&without_permissions, "");
    EXTRA_BLANK_LINES_RE
        .replace_all(&without_agents, "\n\n")
        .trim()
        .to_string()
}

fn collect_jsonl_files(root: &Path, files: &mut Vec<PathBuf>) {
    if !root.exists() {
        return;
    }

    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, files);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
}

fn codex_home_for_session_root(root: &Path) -> PathBuf {
    match root.file_name().and_then(|name| name.to_str()) {
        Some("sessions" | "archived_sessions") => root
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(get_codex_config_dir),
        _ => get_codex_config_dir(),
    }
}

fn backup_state_files(codex_home: &Path, suffix: &str) -> Result<Vec<PathBuf>, String> {
    let state_db = codex_home.join("sqlite").join("state_5.sqlite");
    if !state_db.exists() {
        return Ok(Vec::new());
    }
    Ok(vec![backup_sqlite_database(&state_db, suffix)?])
}

fn backup_file(path: &Path, suffix: &str) -> Result<PathBuf, String> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Invalid backup source path: {}", path.display()))?;
    let destination = path.with_file_name(format!("{file_name}.codex-rescue-backup-{suffix}"));
    if destination.exists() {
        fs::remove_file(&destination).map_err(|e| {
            format!(
                "Failed to replace existing backup {}: {e}",
                destination.display()
            )
        })?;
    }
    fs::copy(path, &destination).map_err(|e| {
        format!(
            "Failed to back up Codex file {} to {}: {e}",
            path.display(),
            destination.display()
        )
    })?;
    Ok(destination)
}

fn global_state_moving(
    data: &[u8],
    session_id: &str,
    target_project_dir: &str,
) -> Result<Vec<u8>, String> {
    let mut state: Value = serde_json::from_slice(data)
        .map_err(|e| format!("Failed to decode Codex global project metadata: {e}"))?;
    let state_object = state
        .as_object_mut()
        .ok_or_else(|| "Codex global project metadata is not an object".to_string())?;
    let project_id = state_object
        .get("local-projects")
        .and_then(Value::as_object)
        .and_then(|projects| {
            projects.iter().find_map(|(project_id, project)| {
                project
                    .get("rootPaths")
                    .and_then(Value::as_array)
                    .filter(|roots| {
                        roots
                            .iter()
                            .any(|root| root.as_str() == Some(target_project_dir))
                    })
                    .map(|_| project_id.clone())
            })
        })
        .ok_or_else(|| {
            format!("Target project is not registered in Codex: {target_project_dir}")
        })?;

    let assignments = state_object
        .entry("thread-project-assignments")
        .or_insert_with(|| Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| "Codex thread project assignments are invalid".to_string())?;
    assignments.insert(
        session_id.to_string(),
        serde_json::json!({"projectKind": "local", "projectId": project_id.clone()}),
    );

    let orders = state_object
        .entry("sidebar-project-thread-orders")
        .or_insert_with(|| Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| "Codex sidebar project ordering is invalid".to_string())?;
    for order in orders.values_mut() {
        if let Some(ids) = order.get_mut("threadIds").and_then(Value::as_array_mut) {
            ids.retain(|value| value.as_str() != Some(session_id));
        }
    }
    let destination = orders
        .entry(project_id)
        .or_insert_with(|| serde_json::json!({"threadIds": []}));
    let ids = destination
        .get_mut("threadIds")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "Codex destination sidebar ordering is invalid".to_string())?;
    ids.insert(0, Value::String(session_id.to_string()));

    remove_projectless_thread_state(state_object, session_id);
    serde_json::to_vec_pretty(&state)
        .map_err(|e| format!("Failed to encode Codex global project metadata: {e}"))
}

fn remove_projectless_thread_state(state: &mut serde_json::Map<String, Value>, session_id: &str) {
    if let Some(ids) = state
        .get_mut("projectless-thread-ids")
        .and_then(Value::as_array_mut)
    {
        ids.retain(|value| value.as_str() != Some(session_id));
    }
    for key in [
        "thread-workspace-root-hints",
        "thread-projectless-output-directories",
    ] {
        if let Some(values) = state.get_mut(key).and_then(Value::as_object_mut) {
            values.remove(session_id);
        }
    }
}

fn thread_project_state(state: &Value, session_id: &str) -> ThreadProjectState {
    let assignment = state
        .get("thread-project-assignments")
        .and_then(Value::as_object)
        .and_then(|assignments| assignments.get(session_id))
        .cloned();
    let mut sidebar_project_id = None;
    let mut sidebar_position = None;
    if let Some(orders) = state
        .get("sidebar-project-thread-orders")
        .and_then(Value::as_object)
    {
        for (project_id, order) in orders {
            if let Some(position) =
                order
                    .get("threadIds")
                    .and_then(Value::as_array)
                    .and_then(|ids| {
                        ids.iter()
                            .position(|value| value.as_str() == Some(session_id))
                    })
            {
                sidebar_project_id = Some(project_id.clone());
                sidebar_position = Some(position);
                break;
            }
        }
    }
    ThreadProjectState {
        assignment,
        sidebar_project_id,
        sidebar_position,
        was_projectless: state
            .get("projectless-thread-ids")
            .and_then(Value::as_array)
            .is_some_and(|ids| ids.iter().any(|value| value.as_str() == Some(session_id))),
        workspace_root_hint: state
            .get("thread-workspace-root-hints")
            .and_then(Value::as_object)
            .and_then(|values| values.get(session_id))
            .cloned(),
        output_directory: state
            .get("thread-projectless-output-directories")
            .and_then(Value::as_object)
            .and_then(|values| values.get(session_id))
            .cloned(),
    }
}

fn global_state_removing_thread(
    data: &[u8],
    session_id: &str,
) -> Result<(Vec<u8>, ThreadProjectState), String> {
    let state: Value = serde_json::from_slice(data)
        .map_err(|e| format!("Failed to decode Codex global project metadata: {e}"))?;
    let project_state = thread_project_state(&state, session_id);
    let cleaned = remove_thread_references(&state, session_id).unwrap_or(Value::Null);
    let data = serde_json::to_vec_pretty(&cleaned)
        .map_err(|e| format!("Failed to encode Codex global project metadata: {e}"))?;
    Ok((data, project_state))
}

fn remove_thread_references(value: &Value, session_id: &str) -> Option<Value> {
    match value {
        Value::String(text) if text == session_id => None,
        Value::Array(values) => Some(Value::Array(
            values
                .iter()
                .filter_map(|value| remove_thread_references(value, session_id))
                .collect(),
        )),
        Value::Object(values) => {
            let mut cleaned = serde_json::Map::new();
            for (key, value) in values {
                if key == session_id || value.as_str().is_some_and(|text| text == session_id) {
                    continue;
                }
                if let Some(value) = remove_thread_references(value, session_id) {
                    cleaned.insert(key.clone(), value);
                }
            }
            Some(Value::Object(cleaned))
        }
        _ => Some(value.clone()),
    }
}

fn global_state_restoring_thread(
    data: &[u8],
    session_id: &str,
    project_state: &ThreadProjectState,
) -> Result<Vec<u8>, String> {
    let mut state: Value = serde_json::from_slice(data)
        .map_err(|e| format!("Failed to decode Codex global project metadata: {e}"))?;
    let state = state
        .as_object_mut()
        .ok_or_else(|| "Codex global project metadata is not an object".to_string())?;
    if let Some(assignment) = &project_state.assignment {
        state
            .entry("thread-project-assignments")
            .or_insert_with(|| Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .ok_or_else(|| "Codex thread project assignments are invalid".to_string())?
            .insert(session_id.to_string(), assignment.clone());
    }
    if let Some(project_id) = &project_state.sidebar_project_id {
        let orders = state
            .entry("sidebar-project-thread-orders")
            .or_insert_with(|| Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .ok_or_else(|| "Codex sidebar project ordering is invalid".to_string())?;
        for order in orders.values_mut() {
            if let Some(ids) = order.get_mut("threadIds").and_then(Value::as_array_mut) {
                ids.retain(|value| value.as_str() != Some(session_id));
            }
        }
        let destination = orders
            .entry(project_id.clone())
            .or_insert_with(|| serde_json::json!({"threadIds": []}));
        let ids = destination
            .get_mut("threadIds")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| "Codex destination sidebar ordering is invalid".to_string())?;
        let position = project_state.sidebar_position.unwrap_or(0).min(ids.len());
        ids.insert(position, Value::String(session_id.to_string()));
    }
    if project_state.was_projectless {
        let ids = state
            .entry("projectless-thread-ids")
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| "Codex projectless thread list is invalid".to_string())?;
        if !ids.iter().any(|value| value.as_str() == Some(session_id)) {
            ids.push(Value::String(session_id.to_string()));
        }
    }
    for (key, saved) in [
        (
            "thread-workspace-root-hints",
            &project_state.workspace_root_hint,
        ),
        (
            "thread-projectless-output-directories",
            &project_state.output_directory,
        ),
    ] {
        if let Some(saved) = saved {
            state
                .entry(key)
                .or_insert_with(|| Value::Object(serde_json::Map::new()))
                .as_object_mut()
                .ok_or_else(|| format!("Codex {key} metadata is invalid"))?
                .insert(session_id.to_string(), saved.clone());
        }
    }
    serde_json::to_vec_pretty(&state)
        .map_err(|e| format!("Failed to encode Codex global project metadata: {e}"))
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn sqlite_tables(conn: &Connection) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare("select name from sqlite_master where type = 'table' and name not like 'sqlite_%'")
        .map_err(|e| format!("Failed to inspect Codex database tables: {e}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| format!("Failed to read Codex database tables: {e}"))?;
    Ok(rows.flatten().collect())
}

fn sqlite_columns(conn: &Connection, table: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(&format!("pragma table_info({})", quote_identifier(table)))
        .map_err(|e| format!("Failed to inspect Codex table {table}: {e}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| format!("Failed to read Codex table {table}: {e}"))?;
    Ok(rows.flatten().collect())
}

fn snapshot_reference_rows(
    conn: &Connection,
    session_id: &str,
    skip_threads: bool,
) -> Result<Vec<SqliteRowSnapshot>, String> {
    let mut snapshots = Vec::new();
    for table in sqlite_tables(conn)? {
        if skip_threads && table == "threads" {
            continue;
        }
        let columns = sqlite_columns(conn, &table)?;
        let reference_columns = ["thread_id", "parent_thread_id", "child_thread_id"]
            .into_iter()
            .filter(|column| columns.iter().any(|candidate| candidate == column))
            .collect::<Vec<_>>();
        if reference_columns.is_empty() {
            continue;
        }
        let where_clause = reference_columns
            .iter()
            .map(|column| format!("{} = ?1", quote_identifier(column)))
            .collect::<Vec<_>>()
            .join(" or ");
        let sql = format!(
            "select * from {} where {where_clause}",
            quote_identifier(&table)
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare Codex {table} snapshot: {e}"))?;
        let selected_columns: Vec<String> = stmt
            .column_names()
            .iter()
            .map(|name| name.to_string())
            .collect();
        let column_count = stmt.column_count();
        let mut rows = stmt
            .query(params![session_id])
            .map_err(|e| format!("Failed to query Codex {table} snapshot: {e}"))?;
        while let Some(row) = rows
            .next()
            .map_err(|e| format!("Failed to read Codex {table} snapshot: {e}"))?
        {
            let values = (0..column_count)
                .map(|index| {
                    row.get_ref(index)
                        .map(sql_value_ref_to_snapshot)
                        .map_err(|e| format!("Failed to read Codex {table} row: {e}"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            snapshots.push(SqliteRowSnapshot {
                table: table.clone(),
                columns: selected_columns.clone(),
                values,
            });
        }
    }
    Ok(snapshots)
}

fn snapshot_external_databases(
    codex_home: &Path,
    session_id: &str,
) -> Result<Vec<SqliteDatabaseSnapshot>, String> {
    let mut snapshots = Vec::new();
    for relative_path in [
        "sqlite/codex-dev.db",
        "sqlite/codex-history-snapshots-dev.db",
    ] {
        let path = codex_home.join(relative_path);
        if !path.exists() {
            continue;
        }
        let conn = Connection::open(&path)
            .map_err(|e| format!("Failed to open Codex database {}: {e}", path.display()))?;
        snapshots.push(SqliteDatabaseSnapshot {
            relative_path: relative_path.to_string(),
            rows: snapshot_reference_rows(&conn, session_id, false)?,
        });
    }
    Ok(snapshots)
}

fn delete_reference_rows(
    conn: &mut Connection,
    session_id: &str,
    delete_thread: bool,
) -> Result<(), String> {
    let tables = sqlite_tables(conn)?;
    let schemas = tables
        .iter()
        .map(|table| Ok((table.clone(), sqlite_columns(conn, table)?)))
        .collect::<Result<Vec<_>, String>>()?;
    let tx = conn
        .transaction()
        .map_err(|e| format!("Failed to start Codex metadata deletion: {e}"))?;
    for (table, columns) in schemas {
        if table == "threads" {
            continue;
        }
        for column in ["thread_id", "parent_thread_id", "child_thread_id"] {
            if columns.iter().any(|candidate| candidate == column) {
                tx.execute(
                    &format!(
                        "delete from {} where {} = ?1",
                        quote_identifier(&table),
                        quote_identifier(column)
                    ),
                    params![session_id],
                )
                .map_err(|e| format!("Failed to clean Codex {table}.{column}: {e}"))?;
            }
        }
        if columns.iter().any(|column| column == "assigned_thread_id") {
            tx.execute(
                &format!(
                    "update {} set assigned_thread_id = null where assigned_thread_id = ?1",
                    quote_identifier(&table)
                ),
                params![session_id],
            )
            .map_err(|e| format!("Failed to clear Codex {table}.assigned_thread_id: {e}"))?;
        }
    }
    if delete_thread && tables.iter().any(|table| table == "threads") {
        let changed = tx
            .execute("delete from threads where id = ?1", params![session_id])
            .map_err(|e| format!("Failed to remove Codex state row: {e}"))?;
        if changed != 1 {
            return Err("Codex state row was not deleted".to_string());
        }
    }
    if tables
        .iter()
        .any(|table| table == "local_thread_catalog_metadata")
    {
        tx.execute(
            "update local_thread_catalog_metadata set catalog_revision = catalog_revision + 1 where id = 1",
            [],
        )
        .map_err(|e| format!("Failed to refresh the Codex chat catalog: {e}"))?;
    }
    tx.commit()
        .map_err(|e| format!("Failed to commit Codex metadata deletion: {e}"))
}

fn restore_snapshot_rows(
    conn: &mut Connection,
    snapshots: &[SqliteRowSnapshot],
) -> Result<(), String> {
    let has_catalog_rows = snapshots
        .iter()
        .any(|snapshot| snapshot.table == "local_thread_catalog");
    let tx = conn
        .transaction()
        .map_err(|e| format!("Failed to start Codex metadata restore: {e}"))?;
    for snapshot in snapshots {
        let columns = snapshot
            .columns
            .iter()
            .map(|column| quote_identifier(column))
            .collect::<Vec<_>>()
            .join(", ");
        let placeholders = (1..=snapshot.values.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "insert or replace into {} ({columns}) values ({placeholders})",
            quote_identifier(&snapshot.table)
        );
        let values = snapshot
            .values
            .iter()
            .map(snapshot_value_to_sql)
            .collect::<Vec<_>>();
        tx.execute(&sql, params_from_iter(values))
            .map_err(|e| format!("Failed to restore Codex {} row: {e}", snapshot.table))?;
    }
    if has_catalog_rows {
        tx.execute(
            "update local_thread_catalog_metadata set catalog_revision = catalog_revision + 1 where id = 1",
            [],
        )
        .map_err(|e| format!("Failed to refresh the Codex chat catalog: {e}"))?;
    }
    tx.commit()
        .map_err(|e| format!("Failed to commit Codex metadata restore: {e}"))
}

fn external_database_path(codex_home: &Path, relative_path: &str) -> Result<PathBuf, String> {
    match relative_path {
        "sqlite/codex-dev.db" | "sqlite/codex-history-snapshots-dev.db" => {
            Ok(codex_home.join(relative_path))
        }
        _ => Err(format!(
            "Unsupported Codex database snapshot: {relative_path}"
        )),
    }
}

fn delete_external_database_rows(
    codex_home: &Path,
    snapshots: &[SqliteDatabaseSnapshot],
    session_id: &str,
) -> Result<(), String> {
    for snapshot in snapshots {
        let path = external_database_path(codex_home, &snapshot.relative_path)?;
        if !path.exists() {
            continue;
        }
        let mut conn = Connection::open(&path)
            .map_err(|e| format!("Failed to open Codex database {}: {e}", path.display()))?;
        delete_reference_rows(&mut conn, session_id, false)?;
    }
    Ok(())
}

fn restore_external_database_rows(
    codex_home: &Path,
    snapshots: &[SqliteDatabaseSnapshot],
) -> Result<(), String> {
    for snapshot in snapshots {
        let path = external_database_path(codex_home, &snapshot.relative_path)?;
        if !path.exists() || snapshot.rows.is_empty() {
            continue;
        }
        let mut conn = Connection::open(&path)
            .map_err(|e| format!("Failed to open Codex database {}: {e}", path.display()))?;
        restore_snapshot_rows(&mut conn, &snapshot.rows)?;
    }
    Ok(())
}

fn backup_sqlite_database(path: &Path, suffix: &str) -> Result<PathBuf, String> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Invalid Codex database path: {}", path.display()))?;
    let destination = path.with_file_name(format!("{file_name}.codex-rescue-backup-{suffix}"));
    if destination.exists() {
        fs::remove_file(&destination).map_err(|e| {
            format!(
                "Failed to replace Codex database backup {}: {e}",
                destination.display()
            )
        })?;
    }
    let source = Connection::open(path)
        .map_err(|e| format!("Failed to open Codex database {}: {e}", path.display()))?;
    let mut target = Connection::open(&destination).map_err(|e| {
        format!(
            "Failed to create Codex database backup {}: {e}",
            destination.display()
        )
    })?;
    let backup = Backup::new(&source, &mut target)
        .map_err(|e| format!("Failed to initialize Codex database backup: {e}"))?;
    backup
        .run_to_completion(5, Duration::from_millis(25), None)
        .map_err(|e| format!("Failed to write Codex database backup: {e}"))?;
    Ok(destination)
}

fn sql_value_ref_to_snapshot(value: ValueRef<'_>) -> SqliteSnapshotValue {
    match value {
        ValueRef::Null => SqliteSnapshotValue::Null,
        ValueRef::Integer(value) => SqliteSnapshotValue::Integer(value),
        ValueRef::Real(value) => SqliteSnapshotValue::Real(value),
        ValueRef::Text(value) => {
            SqliteSnapshotValue::Text(String::from_utf8_lossy(value).to_string())
        }
        ValueRef::Blob(value) => SqliteSnapshotValue::Blob(value.to_vec()),
    }
}

fn snapshot_value_to_sql(value: &SqliteSnapshotValue) -> SqlValue {
    match value {
        SqliteSnapshotValue::Null => SqlValue::Null,
        SqliteSnapshotValue::Integer(value) => SqlValue::Integer(*value),
        SqliteSnapshotValue::Real(value) => SqlValue::Real(*value),
        SqliteSnapshotValue::Text(value) => SqlValue::Text(value.clone()),
        SqliteSnapshotValue::Blob(value) => SqlValue::Blob(value.clone()),
    }
}

fn update_sqlite_project(
    state_db: &Path,
    session_id: &str,
    target_project_dir: &str,
) -> Result<(), String> {
    let conn = Connection::open(state_db).map_err(|e| {
        format!(
            "Failed to open Codex state database {}: {e}",
            state_db.display()
        )
    })?;
    conn.execute(
        "update threads set cwd = ?1 where id = ?2",
        params![target_project_dir, session_id],
    )
    .map_err(|e| {
        format!(
            "Failed to update Codex state database {}: {e}",
            state_db.display()
        )
    })?;
    Ok(())
}

fn update_session_project(path: &Path, target_project_dir: &str) -> Result<(), String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read Codex session file {}: {e}", path.display()))?;
    let had_trailing_newline = text.ends_with('\n');
    let mut lines = Vec::new();
    let mut updated_meta = false;

    for line in text.lines() {
        let mut value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => {
                lines.push(line.to_string());
                continue;
            }
        };

        match value.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                let payload = value
                    .get_mut("payload")
                    .and_then(Value::as_object_mut)
                    .ok_or_else(|| {
                        "Codex session metadata is missing payload object".to_string()
                    })?;
                payload.insert(
                    "cwd".to_string(),
                    Value::String(target_project_dir.to_string()),
                );
                updated_meta = true;
                lines.push(
                    serde_json::to_string(&value)
                        .map_err(|e| format!("Failed to encode Codex session metadata: {e}"))?,
                );
            }
            Some("turn_context") => {
                if let Some(payload) = value.get_mut("payload").and_then(Value::as_object_mut) {
                    payload.insert(
                        "cwd".to_string(),
                        Value::String(target_project_dir.to_string()),
                    );
                    lines.push(
                        serde_json::to_string(&value)
                            .map_err(|e| format!("Failed to encode Codex turn context: {e}"))?,
                    );
                } else {
                    lines.push(line.to_string());
                }
            }
            _ => lines.push(line.to_string()),
        }
    }

    if !updated_meta {
        return Err("Codex session metadata is missing session_meta line".to_string());
    }

    let mut updated = lines.join("\n");
    if had_trailing_newline || !updated.is_empty() {
        updated.push('\n');
    }
    crate::config::atomic_write(path, updated.as_bytes()).map_err(|e| e.to_string())
}

fn read_jsonl_lines(path: &Path) -> Result<Vec<String>, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read Codex session file {}: {e}", path.display()))?;
    let mut lines = content
        .split('\n')
        .map(str::to_string)
        .collect::<Vec<String>>();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    Ok(lines)
}

fn raw_file_contains(path: &Path, lower_query: &str) -> Result<bool, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "Failed to open Codex session file {}: {error}",
                path.display()
            ))
        }
    };
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => continue,
        };
        if line.to_lowercase().contains(lower_query) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn backups_to_strings(backups: Vec<PathBuf>) -> Vec<String> {
    backups
        .into_iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect()
}

fn iso_jsonl(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn iso_index(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn update_session_meta_timestamp(path: &Path, timestamp: &str) -> Result<(), String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read Codex session file {}: {e}", path.display()))?;
    let (first_line, rest) = text
        .split_once('\n')
        .map(|(first, rest)| (first, Some(rest)))
        .unwrap_or((text.as_str(), None));

    let mut obj: Value = serde_json::from_str(first_line)
        .map_err(|e| format!("Failed to decode Codex session metadata: {e}"))?;
    obj["timestamp"] = Value::String(timestamp.to_string());
    if let Some(payload) = obj.get_mut("payload").and_then(Value::as_object_mut) {
        payload.insert(
            "timestamp".to_string(),
            Value::String(timestamp.to_string()),
        );
    }

    let first_line = serde_json::to_string(&obj)
        .map_err(|e| format!("Failed to encode Codex session metadata: {e}"))?;
    let updated = match rest {
        Some(rest) => format!("{first_line}\n{rest}"),
        None => format!("{first_line}\n"),
    };
    crate::config::atomic_write(path, updated.as_bytes()).map_err(|e| e.to_string())
}

fn branch_content(
    lines: &[String],
    new_session_id: &str,
    timestamp: &str,
    cwd: &str,
) -> Result<String, String> {
    let first = lines
        .first()
        .ok_or_else(|| "Cannot branch an empty chat".to_string())?;
    let mut obj: Value = serde_json::from_str(first)
        .map_err(|e| format!("Failed to decode Codex session metadata: {e}"))?;
    obj["timestamp"] = Value::String(timestamp.to_string());
    let payload = obj
        .get_mut("payload")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "Codex session metadata is missing payload object".to_string())?;
    payload.insert("id".to_string(), Value::String(new_session_id.to_string()));
    payload.insert(
        "timestamp".to_string(),
        Value::String(timestamp.to_string()),
    );
    payload.insert("cwd".to_string(), Value::String(cwd.to_string()));
    payload.insert(
        "thread_source".to_string(),
        Value::String("user".to_string()),
    );

    let first_line = serde_json::to_string(&obj)
        .map_err(|e| format!("Failed to encode Codex branch metadata: {e}"))?;
    let mut result = vec![first_line];
    result.extend(lines.iter().skip(1).cloned());
    Ok(format!("{}\n", result.join("\n")))
}

fn upsert_session_index(
    session_index: &Path,
    session_id: &str,
    title: &str,
    updated_at: &str,
) -> Result<(), String> {
    if !session_index.exists() {
        return Ok(());
    }
    let text = fs::read_to_string(session_index).map_err(|e| {
        format!(
            "Failed to read session index {}: {e}",
            session_index.display()
        )
    })?;
    let mut lines = Vec::new();
    let mut updated = false;
    for line in text.lines() {
        let Ok(mut value) = serde_json::from_str::<Value>(line) else {
            lines.push(line.to_string());
            continue;
        };
        if value.get("id").and_then(Value::as_str) == Some(session_id) {
            value["updated_at"] = Value::String(updated_at.to_string());
            if value
                .get("thread_name")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
            {
                value["thread_name"] = Value::String(title.to_string());
            }
            lines.push(serde_json::to_string(&value).map_err(|e| e.to_string())?);
            updated = true;
        } else {
            lines.push(line.to_string());
        }
    }
    if !updated {
        lines.push(
            serde_json::to_string(&serde_json::json!({
                "id": session_id,
                "thread_name": title,
                "updated_at": updated_at,
            }))
            .map_err(|e| e.to_string())?,
        );
    }
    crate::config::atomic_write(session_index, format!("{}\n", lines.join("\n")).as_bytes())
        .map_err(|e| e.to_string())
}

fn append_session_index(
    session_index: &Path,
    session_id: &str,
    title: &str,
    updated_at: &str,
) -> Result<(), String> {
    if !session_index.exists() {
        return Ok(());
    }
    let mut text = fs::read_to_string(session_index).map_err(|e| {
        format!(
            "Failed to read session index {}: {e}",
            session_index.display()
        )
    })?;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(
        &serde_json::to_string(&serde_json::json!({
            "id": session_id,
            "thread_name": title,
            "updated_at": updated_at,
        }))
        .map_err(|e| e.to_string())?,
    );
    text.push('\n');
    crate::config::atomic_write(session_index, text.as_bytes()).map_err(|e| e.to_string())
}

fn remove_session_index_entry(session_index: &Path, session_id: &str) -> Result<(), String> {
    if !session_index.exists() {
        return Ok(());
    }
    let text = fs::read_to_string(session_index).map_err(|e| {
        format!(
            "Failed to read session index {}: {e}",
            session_index.display()
        )
    })?;
    let lines = text
        .lines()
        .filter(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|value| value.get("id").and_then(Value::as_str).map(str::to_string))
                .as_deref()
                != Some(session_id)
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    crate::config::atomic_write(session_index, format!("{}\n", lines.join("\n")).as_bytes())
        .map_err(|e| e.to_string())
}

fn insert_branched_sqlite_row(
    state_db: &Path,
    source_session_id: &str,
    new_session_id: &str,
    rollout_path: &Path,
    title: &str,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let conn = Connection::open(state_db).map_err(|e| {
        format!(
            "Failed to open Codex state database {}: {e}",
            state_db.display()
        )
    })?;
    let inserted = conn
        .execute(
            "insert into threads (
            id, rollout_path, created_at, updated_at, source, model_provider, cwd, title,
            sandbox_policy, approval_mode, tokens_used, has_user_event, archived, archived_at,
            git_sha, git_branch, git_origin_url, cli_version, first_user_message,
            agent_nickname, agent_role, memory_mode, model, reasoning_effort, agent_path,
            created_at_ms, updated_at_ms, thread_source, preview
        )
        select
            ?1, ?2, ?3, ?3, source, model_provider, cwd, ?4,
            sandbox_policy, approval_mode, 0, has_user_event, 0, NULL,
            git_sha, git_branch, git_origin_url, cli_version, first_user_message,
            agent_nickname, agent_role, memory_mode, model, reasoning_effort, agent_path,
            ?5, ?5, 'user', preview
        from threads
        where id = ?6",
            params![
                new_session_id,
                rollout_path.to_string_lossy().to_string(),
                now.timestamp(),
                title,
                now.timestamp_millis(),
                source_session_id
            ],
        )
        .map_err(|e| format!("Failed to insert Codex branch row: {e}"))?;
    if inserted != 1 {
        return Err("Branch was not registered in the Codex state database".to_string());
    }
    Ok(())
}

fn delete_sqlite_thread(state_db: &Path, session_id: &str) -> Result<(), String> {
    let conn = Connection::open(state_db).map_err(|e| {
        format!(
            "Failed to open Codex state database {}: {e}",
            state_db.display()
        )
    })?;
    conn.execute("delete from threads where id = ?1", params![session_id])
        .map_err(|e| format!("Failed to roll back Codex branch row: {e}"))?;
    Ok(())
}

fn load_sqlite_record(conn: &Connection, session_id: &str) -> Result<Value, String> {
    let mut stmt = conn
        .prepare("select * from threads where id = ?1")
        .map_err(|e| format!("Failed to prepare Codex state row query: {e}"))?;
    let column_names = stmt
        .column_names()
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    stmt.query_row(params![session_id], |row| {
        let mut map = serde_json::Map::new();
        for (index, name) in column_names.iter().enumerate() {
            map.insert(name.clone(), sql_value_ref_to_json(row.get_ref(index)?));
        }
        Ok(Value::Object(map))
    })
    .optional()
    .map_err(|e| format!("Failed to load Codex state row: {e}"))?
    .ok_or_else(|| "Codex state row not found".to_string())
}

fn sqlite_record_is_subagent(record: &Value) -> bool {
    record
        .get("thread_source")
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case("subagent"))
        || record
            .get("source")
            .and_then(Value::as_str)
            .is_some_and(is_subagent_source_text)
}

fn session_meta_from_sqlite_record(
    session_id: &str,
    path: &Path,
    record: &Value,
) -> Result<SessionMeta, String> {
    let Some(record) = record.as_object() else {
        return Err("Codex state row not found".to_string());
    };

    let title = record
        .get("title")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            record
                .get("first_user_message")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
        })
        .or_else(|| {
            record
                .get("preview")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
        })
        .map(|value| truncate_summary(value, TITLE_MAX_CHARS));
    let cwd = record
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let created_at = record
        .get("created_at_ms")
        .and_then(Value::as_i64)
        .or_else(|| {
            record
                .get("created_at")
                .and_then(Value::as_i64)
                .map(seconds_to_ms)
        });
    let last_active_at = record
        .get("updated_at_ms")
        .and_then(Value::as_i64)
        .or_else(|| {
            record
                .get("updated_at")
                .and_then(Value::as_i64)
                .map(seconds_to_ms)
        });

    Ok(SessionMeta {
        provider_id: PROVIDER_ID.to_string(),
        session_id: session_id.to_string(),
        title,
        summary: record
            .get("preview")
            .and_then(Value::as_str)
            .map(|value| truncate_summary(value, 160)),
        project_dir: cwd,
        created_at,
        last_active_at,
        source_path: Some(path.to_string_lossy().to_string()),
        resume_command: Some(format!("codex resume {session_id}")),
        codex_status: Some("Missing file".to_string()),
        is_in_session_index: None,
        file_exists: Some(false),
        archived: record
            .get("archived")
            .and_then(Value::as_i64)
            .map(|value| value != 0),
        needs_repair: Some(false),
    })
}

fn sql_value_ref_to_json(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => Value::Number(value.into()),
        ValueRef::Real(value) => serde_json::Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        ValueRef::Text(value) => Value::String(String::from_utf8_lossy(value).to_string()),
        ValueRef::Blob(_) => Value::Null,
    }
}

fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let mut counter = 2;
    loop {
        let candidate = PathBuf::from(format!("{}.{}", path.display(), counter));
        if !candidate.exists() {
            return candidate;
        }
        counter += 1;
    }
}

fn collect_backup_files(
    codex_home: &Path,
    root: &Path,
    include_trash: bool,
    backups: &mut Vec<CodexBackupFile>,
) -> Result<(), String> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root)
        .map_err(|e| format!("Failed to read backup directory {}: {e}", root.display()))?
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            collect_backup_files(codex_home, &path, include_trash, backups)?;
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some((original_name, stamp)) = file_name.split_once(BACKUP_MARKER) else {
            continue;
        };
        if original_name.is_empty() || stamp.is_empty() {
            continue;
        }
        if !include_trash && path.starts_with(backup_trash_path(codex_home)) {
            continue;
        }
        let directory = path
            .parent()
            .map(|parent| parent.to_string_lossy().to_string())
            .unwrap_or_default();
        let original_path = path.with_file_name(original_name);
        let metadata = fs::metadata(&path)
            .map_err(|e| format!("Failed to read backup metadata {}: {e}", path.display()))?;
        let kind = backup_kind(original_name);
        backups.push(CodexBackupFile {
            backup_path: path.to_string_lossy().to_string(),
            original_path: original_path.to_string_lossy().to_string(),
            original_name: original_name.to_string(),
            directory,
            stamp: stamp.to_string(),
            kind: kind.to_string(),
            size: metadata.len(),
            original_exists: original_path.exists(),
            reason: backup_reason(stamp, kind),
        });
    }
    Ok(())
}

fn backup_kind(original_name: &str) -> &'static str {
    if original_name == "state_5.sqlite" || original_name.starts_with("state_5.sqlite-") {
        "stateDatabase"
    } else if original_name == "session_index.jsonl" {
        "sessionIndex"
    } else if original_name.ends_with(".jsonl") {
        "chatFile"
    } else {
        "other"
    }
}

fn backup_reason(stamp: &str, kind: &str) -> String {
    if stamp.contains("-trim") {
        "Created before Trim from here"
    } else if stamp.contains("-before-restore") {
        "Created before Restore"
    } else if stamp.contains("-wake") {
        "Created before Repair Index"
    } else if stamp.contains("-trash-thread") {
        "Created before Move to Trash"
    } else if stamp.contains("-move") {
        "Created before Move"
    } else if kind == "chatFile" {
        "Created before a chat change"
    } else {
        "Created by Codex Wake"
    }
    .to_string()
}

fn collect_trashed_threads(
    root: &Path,
    threads: &mut Vec<CodexTrashedThread>,
) -> Result<(), String> {
    for entry in fs::read_dir(root)
        .map_err(|e| format!("Failed to read thread trash {}: {e}", root.display()))?
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            let manifest_path = path.join("manifest.json");
            if manifest_path.exists() {
                let manifest: Value =
                    serde_json::from_str(&fs::read_to_string(&manifest_path).map_err(|e| {
                        format!(
                            "Failed to read trash manifest {}: {e}",
                            manifest_path.display()
                        )
                    })?)
                    .map_err(|e| format!("Failed to parse trash manifest: {e}"))?;
                let trash_path = manifest
                    .get("trashPath")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let size = trash_path
                    .as_deref()
                    .and_then(|path| fs::metadata(path).ok())
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                let original_path = manifest
                    .get("originalPath")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                threads.push(CodexTrashedThread {
                    thread_id: manifest
                        .get("threadID")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    title: manifest
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    original_path: original_path.clone(),
                    trash_path,
                    manifest_path: manifest_path.to_string_lossy().to_string(),
                    cwd: manifest
                        .get("cwd")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    trashed_at: manifest
                        .get("trashedAt")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    size,
                    original_exists: Path::new(&original_path).exists(),
                });
            } else {
                collect_trashed_threads(&path, threads)?;
            }
        }
    }
    Ok(())
}

fn insert_sqlite_record(
    conn: &Connection,
    record: &serde_json::Map<String, Value>,
) -> Result<(), String> {
    let columns = record.keys().cloned().collect::<Vec<_>>();
    let placeholders = (1..=columns.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>();
    let sql = format!(
        "insert or replace into threads ({}) values ({})",
        columns.join(", "),
        placeholders.join(", ")
    );
    let values = columns
        .iter()
        .map(|column| json_to_sql_value(record.get(column).unwrap_or(&Value::Null)))
        .collect::<Vec<_>>();
    conn.execute(&sql, params_from_iter(values))
        .map_err(|e| format!("Failed to restore Codex state row: {e}"))?;
    Ok(())
}

fn json_to_sql_value(value: &Value) -> SqlValue {
    match value {
        Value::Null => SqlValue::Null,
        Value::Bool(value) => SqlValue::Integer(if *value { 1 } else { 0 }),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                SqlValue::Integer(value)
            } else if let Some(value) = value.as_f64() {
                SqlValue::Real(value)
            } else {
                SqlValue::Null
            }
        }
        Value::String(value) => SqlValue::Text(value.clone()),
        Value::Array(_) | Value::Object(_) => SqlValue::Text(value.to_string()),
    }
}

fn append_raw_session_index_entry(session_index: &Path, entry: &Value) -> Result<(), String> {
    if !session_index.exists() {
        return Ok(());
    }
    let Some(id) = entry.get("id").and_then(Value::as_str) else {
        return Ok(());
    };
    let existing = fs::read_to_string(session_index).map_err(|e| {
        format!(
            "Failed to read session index {}: {e}",
            session_index.display()
        )
    })?;
    for line in existing.lines() {
        if serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|value| value.get("id").and_then(Value::as_str).map(str::to_string))
            .as_deref()
            == Some(id)
        {
            return Ok(());
        }
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&serde_json::to_string(entry).map_err(|e| e.to_string())?);
    text.push('\n');
    crate::config::atomic_write(session_index, text.as_bytes()).map_err(|e| e.to_string())
}

fn delete_trash_directory(manifest_path: &Path) -> Result<(), String> {
    let Some(directory) = manifest_path.parent() else {
        return Err("Invalid trash manifest path".to_string());
    };
    fs::remove_dir_all(directory).map_err(|e| {
        format!(
            "Failed to delete trash directory {}: {e}",
            directory.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_codex_session(path: &Path, session_id: &str, message: &str) {
        std::fs::write(
            path,
            format!(
                "{{\"timestamp\":\"2026-03-06T21:50:12Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{session_id}\",\"cwd\":\"/tmp/project\"}}}}\n\
                 {{\"timestamp\":\"2026-03-06T21:50:13Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":\"{message}\"}}}}\n",
            ),
        )
        .expect("write session");
    }

    #[test]
    fn scan_sessions_in_roots_includes_active_and_archived_files() {
        let temp = tempdir().expect("tempdir");
        let active = temp.path().join("sessions");
        let archived = temp.path().join("archived_sessions");
        std::fs::create_dir_all(&active).expect("active dir");
        std::fs::create_dir_all(&archived).expect("archived dir");

        write_codex_session(&active.join("active.jsonl"), "active-id", "Active session");
        write_codex_session(
            &archived.join("archived.jsonl"),
            "archived-id",
            "Archived session",
        );

        let sessions = scan_sessions_in_roots(&[active, archived]);
        let ids = sessions
            .into_iter()
            .map(|session| session.session_id)
            .collect::<Vec<_>>();

        assert!(ids.contains(&"active-id".to_string()));
        assert!(ids.contains(&"archived-id".to_string()));
    }

    #[test]
    fn delete_session_removes_jsonl_file() {
        let temp = tempdir().expect("tempdir");
        let sessions_dir = temp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("sessions dir");
        std::fs::write(
            temp.path().join(".codex-global-state.json"),
            serde_json::json!({
                "local-projects": {},
                "thread-project-assignments": {},
                "sidebar-project-thread-orders": {},
            })
            .to_string(),
        )
        .expect("write global state");
        let path = sessions_dir
            .join("rollout-2026-03-06T21-50-12-019cc369-bd7c-7891-b371-7b20b4fe0b18.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-03-06T21:50:12Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"019cc369-bd7c-7891-b371-7b20b4fe0b18\",\"cwd\":\"/tmp/project\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:13Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":\"hello\"}}\n"
            ),
        )
        .expect("write session");

        delete_session(&sessions_dir, &path, "019cc369-bd7c-7891-b371-7b20b4fe0b18")
            .expect("delete session");

        assert!(!path.exists());
    }

    #[test]
    fn move_session_updates_sqlite_jsonl_and_creates_backups() {
        let temp = tempdir().expect("tempdir");
        let sqlite_dir = temp.path().join("sqlite");
        std::fs::create_dir_all(&sqlite_dir).expect("sqlite dir");
        let state_db = sqlite_dir.join("state_5.sqlite");
        {
            let conn = Connection::open(&state_db).expect("open sqlite");
            conn.execute("create table threads (id text primary key, cwd text)", [])
                .expect("create threads table");
            conn.execute(
                "insert into threads (id, cwd) values (?1, ?2)",
                params!["move-id", "/old/project"],
            )
            .expect("insert row");
        }

        let sessions_dir = temp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("sessions dir");
        std::fs::write(
            temp.path().join(".codex-global-state.json"),
            serde_json::json!({
                "local-projects": {
                    "local-destination": {
                        "id": "local-destination",
                        "name": "Destination",
                        "rootPaths": ["/new/project"],
                    },
                },
                "thread-project-assignments": {},
                "sidebar-project-thread-orders": {
                    "local-destination": {"threadIds": []},
                },
                "projectless-thread-ids": ["move-id"],
            })
            .to_string(),
        )
        .expect("write global state");
        let session_path = sessions_dir.join("session.jsonl");
        std::fs::write(
            &session_path,
            concat!(
                "{\"timestamp\":\"2026-03-06T21:50:12Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"move-id\",\"cwd\":\"/old/project\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:12Z\",\"type\":\"turn_context\",\"payload\":{\"cwd\":\"/old/project\",\"model\":\"gpt-5\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:13Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":\"move me\"}}\n"
            ),
        )
        .expect("write session");

        move_session_with_home(temp.path(), &session_path, "move-id", "/new/project")
            .expect("move session");

        let first_line = std::fs::read_to_string(&session_path)
            .expect("read session")
            .lines()
            .next()
            .unwrap()
            .to_string();
        let value: Value = serde_json::from_str(&first_line).expect("json");
        assert_eq!(
            value
                .get("payload")
                .and_then(|payload| payload.get("cwd"))
                .and_then(Value::as_str),
            Some("/new/project")
        );
        let second_line = std::fs::read_to_string(&session_path)
            .expect("read session")
            .lines()
            .nth(1)
            .unwrap()
            .to_string();
        let value: Value = serde_json::from_str(&second_line).expect("json");
        assert_eq!(
            value
                .get("payload")
                .and_then(|payload| payload.get("cwd"))
                .and_then(Value::as_str),
            Some("/new/project")
        );

        let conn = Connection::open(&state_db).expect("open sqlite");
        let cwd: String = conn
            .query_row("select cwd from threads where id = 'move-id'", [], |row| {
                row.get(0)
            })
            .expect("select cwd");
        assert_eq!(cwd, "/new/project");

        let state_backups = std::fs::read_dir(&sqlite_dir)
            .expect("read sqlite dir")
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".codex-rescue-backup-")
            })
            .count();
        assert!(state_backups >= 1);

        let session_backups = std::fs::read_dir(&sessions_dir)
            .expect("read sessions dir")
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".codex-rescue-backup-")
            })
            .count();
        assert_eq!(session_backups, 1);
    }

    #[test]
    fn move_session_rejects_session_id_mismatch() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-03-06T21:50:12Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"actual-id\",\"cwd\":\"/tmp/project\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:13Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":\"hello\"}}\n"
            ),
        )
        .expect("write session");

        let err = move_session_with_home(temp.path(), &path, "expected-id", "/new/project")
            .expect_err("should reject mismatch");

        assert!(err.contains("Codex session ID mismatch"));
    }

    #[test]
    fn parse_session_uses_first_user_message_as_title() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-03-06T21:50:12Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"test-id\",\"cwd\":\"/tmp/project\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:13Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":\"How do I deploy?\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:14Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":\"Here is how...\"}}\n"
            ),
        )
        .expect("write");

        let meta = parse_session(&path).unwrap();
        assert_eq!(meta.title.as_deref(), Some("How do I deploy?"));
    }

    #[test]
    fn parse_session_skips_agents_md_injection() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-03-06T21:50:12Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"test-id\",\"cwd\":\"/tmp/project\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:13Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"developer\",\"content\":\"<permissions>\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:13Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":\"# AGENTS.md instructions for /tmp/project\\n<INSTRUCTIONS>Do stuff</INSTRUCTIONS>\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:14Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":\"Fix the login bug\"}}\n"
            ),
        )
        .expect("write");

        let meta = parse_session(&path).unwrap();
        // Should skip AGENTS.md injection and use the real user message
        assert_eq!(meta.title.as_deref(), Some("Fix the login bug"));
    }

    #[test]
    fn parse_session_skips_subagent_sessions() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-04-28T10:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"subagent-id\",\"cwd\":\"/tmp/project\",\"originator\":\"codex-tui\",\"source\":{\"subagent\":{\"thread_spawn\":{\"parent_thread_id\":\"parent-id\",\"depth\":1,\"agent_role\":\"explorer\"}}}}}\n",
                "{\"timestamp\":\"2026-04-28T10:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":\"Inspect the project\"}}\n"
            ),
        )
        .expect("write");

        assert!(parse_session(&path).is_none());
    }

    #[test]
    fn parse_session_skips_environment_context_injection() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-03-06T21:50:12Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"test-id\",\"cwd\":\"/tmp/project\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:13Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":\"<environment_context>\\n  <cwd>/tmp/project</cwd>\\n</environment_context>\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:14Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":\"Fix the login bug\"}}\n"
            ),
        )
        .expect("write");

        let meta = parse_session(&path).unwrap();
        // Should skip environment_context injection and use the real user message
        assert_eq!(meta.title.as_deref(), Some("Fix the login bug"));
    }

    #[test]
    fn parse_session_extracts_vscode_ide_request_as_title() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-03-06T21:50:12Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"test-id\",\"cwd\":\"/tmp/project\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:13Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":\"# Context from my IDE setup:\\n\\n## Active file: src/main.ts\\n\\n## My request for Codex:\\nFix the session title preview\"}}\n"
            ),
        )
        .expect("write");

        let meta = parse_session(&path).unwrap();
        assert_eq!(meta.title.as_deref(), Some("Fix the session title preview"));
    }

    #[test]
    fn parse_session_extracts_inline_vscode_ide_request_as_title() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-03-06T21:50:12Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"test-id\",\"cwd\":\"/tmp/project\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:13Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":\"# Context from my IDE setup:\\n\\n## My request for Codex: Fix the TOC preview\"}}\n"
            ),
        )
        .expect("write");

        let meta = parse_session(&path).unwrap();
        assert_eq!(meta.title.as_deref(), Some("Fix the TOC preview"));
    }

    #[test]
    fn parse_session_ignores_marker_mentions_before_request_heading() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-03-06T21:50:12Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"test-id\",\"cwd\":\"/tmp/project\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:13Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":\"# Context from my IDE setup:\\n\\n## Active selection:\\nMy request for Codex: not the prompt\\n\\n## My request for Codex:\\nUse the real request heading\"}}\n"
            ),
        )
        .expect("write");

        let meta = parse_session(&path).unwrap();
        assert_eq!(meta.title.as_deref(), Some("Use the real request heading"));
    }

    #[test]
    fn parse_session_uses_last_request_heading_when_selection_has_one() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-03-06T21:50:12Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"test-id\",\"cwd\":\"/tmp/project\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:13Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":\"# Context from my IDE setup:\\n\\n## Active selection: docs/codex-format.md\\n## My request for Codex:\\nselected document content, not the real request\\n\\n## My request for Codex:\\nUse the last request heading\"}}\n"
            ),
        )
        .expect("write");

        let meta = parse_session(&path).unwrap();
        assert_eq!(meta.title.as_deref(), Some("Use the last request heading"));
    }

    // Known limitation: the IDE marker is matched purely by text, so a
    // "## My request for Codex:" line inside the real request body is treated as
    // a new boundary and only the trailing part is kept. This pins the
    // best-effort behavior; fully fixing it needs structured IDE section data
    // that the Codex VS Code context does not provide.
    #[test]
    fn parse_session_keeps_trailing_part_when_request_body_repeats_heading() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-03-06T21:50:12Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"test-id\",\"cwd\":\"/tmp/project\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:13Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":\"# Context from my IDE setup:\\n\\n## Active file: foo.ts\\n\\n## My request for Codex:\\nDocument the format, for example:\\n## My request for Codex:\\nand the rest follows.\"}}\n"
            ),
        )
        .expect("write");

        let meta = parse_session(&path).unwrap();
        assert_eq!(meta.title.as_deref(), Some("and the rest follows."));
    }

    #[test]
    fn parse_session_skips_vscode_ide_context_without_request() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-03-06T21:50:12Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"test-id\",\"cwd\":\"/tmp/project\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:13Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":\"# Context from my IDE setup:\\n\\n## Active file: src/main.ts\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:14Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":\"Fix the login bug\"}}\n"
            ),
        )
        .expect("write");

        let meta = parse_session(&path).unwrap();
        assert_eq!(meta.title.as_deref(), Some("Fix the login bug"));
    }

    #[test]
    fn parse_session_falls_back_to_dir_basename() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-03-06T21:50:12Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"test-id\",\"cwd\":\"/tmp/my-project\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:13Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":\"Hello\"}}\n"
            ),
        )
        .expect("write");

        let meta = parse_session(&path).unwrap();
        // No user message → falls back to dir basename
        assert_eq!(meta.title.as_deref(), Some("my-project"));
    }

    #[test]
    fn parse_session_truncates_long_title() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("session.jsonl");
        let long_msg = "a".repeat(200);
        std::fs::write(
            &path,
            format!(
                "{{\"timestamp\":\"2026-03-06T21:50:12Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"test-id\",\"cwd\":\"/tmp/p\"}}}}\n\
                 {{\"timestamp\":\"2026-03-06T21:50:13Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":\"{long_msg}\"}}}}\n",
            ),
        )
        .expect("write");

        let meta = parse_session(&path).unwrap();
        let title = meta.title.unwrap();
        assert!(title.len() <= TITLE_MAX_CHARS + 3); // +3 for "..."
        assert!(title.ends_with("..."));
    }

    #[test]
    fn load_messages_includes_function_call_and_output() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-03-06T21:50:12Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"test-id\",\"cwd\":\"/tmp\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:13Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":\"list files\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:14Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"shell\",\"arguments\":\"{\\\"cmd\\\":[\\\"ls\\\"]}\",\"call_id\":\"call_1\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:15Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"function_call_output\",\"call_id\":\"call_1\",\"output\":\"file1.txt\\nfile2.txt\"}}\n",
                "{\"timestamp\":\"2026-03-06T21:50:16Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Done.\"}]}}\n",
            ),
        )
        .expect("write");

        let msgs = load_messages(&path).expect("load");
        assert_eq!(msgs.len(), 4);

        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].content, "list files");

        assert_eq!(msgs[1].role, "assistant");
        assert!(msgs[1].content.contains("[Tool: shell]"));

        assert_eq!(msgs[2].role, "tool");
        assert!(msgs[2].content.contains("file1.txt"));

        assert_eq!(msgs[3].role, "assistant");
        assert_eq!(msgs[3].content, "Done.");
    }

    // Compatibility fixtures mirroring the Codex Keeper (codex-wake) suite for
    // the current Codex Desktop storage schema.
    struct CodexCompatFixture {
        _temp: tempfile::TempDir,
        home: PathBuf,
        root_id: &'static str,
        child_id: &'static str,
        source_project_id: &'static str,
        destination_project_id: &'static str,
        source_path: String,
        destination_path: String,
        root_rollout: PathBuf,
        child_rollout: PathBuf,
        state_db: PathBuf,
    }

    impl CodexCompatFixture {
        fn new() -> Self {
            let temp = tempdir().expect("tempdir");
            let home = temp.path().to_path_buf();
            let root_id = "00000000-0000-7000-8000-000000000001";
            let child_id = "00000000-0000-7000-8000-000000000002";
            let source_project_id = "local-source";
            let destination_project_id = "local-destination";
            let source_path = home.join("projects/source").to_string_lossy().to_string();
            let destination_path = home
                .join("projects/destination")
                .to_string_lossy()
                .to_string();
            let root_rollout = home.join("sessions/2026/08/17/root.jsonl");
            let child_rollout = home.join("sessions/2026/08/17/child.jsonl");
            let state_db = home.join("sqlite/state_5.sqlite");

            for dir in [
                home.join("sqlite"),
                home.join("sessions/2026/08/17"),
                home.join("projects/source"),
                home.join("projects/destination"),
            ] {
                std::fs::create_dir_all(&dir).expect("create fixture dir");
            }

            let fixture = Self {
                _temp: temp,
                home,
                root_id,
                child_id,
                source_project_id,
                destination_project_id,
                source_path,
                destination_path,
                root_rollout,
                child_rollout,
                state_db,
            };
            fixture.create_databases();
            fixture.create_rollouts();
            fixture.create_metadata();
            fixture
        }

        fn create_databases(&self) {
            let conn = Connection::open(&self.state_db).expect("open state db");
            conn.execute_batch(
                "create table threads (
                    id text primary key, rollout_path text not null, created_at integer not null,
                    updated_at integer not null, source text not null, model_provider text not null,
                    cwd text not null, title text not null, sandbox_policy text not null,
                    approval_mode text not null, tokens_used integer not null default 0,
                    has_user_event integer not null default 0, archived integer not null default 0,
                    archived_at integer, git_sha text, git_branch text, git_origin_url text,
                    cli_version text not null default '', first_user_message text not null default '',
                    agent_nickname text, agent_role text, memory_mode text not null default 'enabled',
                    model text, reasoning_effort text, agent_path text, created_at_ms integer,
                    updated_at_ms integer, thread_source text, preview text not null default '',
                    recency_at integer not null default 0, recency_at_ms integer not null default 0,
                    history_mode text not null default 'legacy', name text, is_pinned integer not null default 0,
                    thread_section_id text, section_position integer, section_entered_at_ms integer
                );
                create table thread_spawn_edges (
                    parent_thread_id text not null, child_thread_id text not null primary key, status text not null
                );
                create table thread_dynamic_tools (
                    thread_id text not null, position integer not null, name text not null,
                    description text not null, input_schema text not null,
                    defer_loading integer not null default 0, namespace text,
                    primary key(thread_id, position)
                );",
            )
            .expect("create state schema");
            conn.execute(
                "insert into threads values (?1, ?2, 100, 200, 'vscode', 'openai', ?3, 'Root', '{}', 'never', 10, 1, 0, null, null, null, null, '1.0', 'Root message', null, null, 'enabled', 'gpt-test', 'high', null, 100000, 200000, 'user', 'Root preview', 150, 150000, 'legacy', 'Pinned root', 1, 'section-1', 2, 160000)",
                params![
                    self.root_id,
                    self.root_rollout.to_string_lossy(),
                    self.source_path,
                ],
            )
            .expect("insert root thread");
            let child_source = serde_json::json!({
                "subagent": {"thread_spawn": {"parent_thread_id": self.root_id, "depth": 1}}
            })
            .to_string();
            conn.execute(
                "insert into threads values (?1, ?2, 110, 190, ?3, 'openai', ?4, 'Child', '{}', 'never', 5, 0, 0, null, null, null, null, '1.0', '', 'Scout', 'research-worker', 'enabled', 'gpt-test', 'low', '/root/scout', 110000, 190000, 'subagent', '', 140, 140000, 'legacy', null, 0, null, null, null)",
                params![
                    self.child_id,
                    self.child_rollout.to_string_lossy(),
                    child_source,
                    self.source_path,
                ],
            )
            .expect("insert child thread");
            conn.execute(
                "insert into thread_spawn_edges values (?1, ?2, 'open')",
                params![self.root_id, self.child_id],
            )
            .expect("insert spawn edge");
            conn.execute(
                "insert into thread_dynamic_tools values (?1, 0, 'fixture', 'Fixture tool', '{}', 0, 'tests')",
                params![self.root_id],
            )
            .expect("insert dynamic tool");

            let catalog =
                Connection::open(self.home.join("sqlite/codex-dev.db")).expect("open catalog db");
            catalog
                .execute_batch(
                    "create table local_thread_catalog (thread_id text primary key, display_title text);
                     create table local_thread_catalog_metadata (id integer primary key, catalog_revision integer);
                     insert into local_thread_catalog_metadata values (1, 4);",
                )
                .expect("create catalog schema");
            catalog
                .execute(
                    "insert into local_thread_catalog values (?1, 'Catalog root')",
                    params![self.root_id],
                )
                .expect("insert catalog row");

            let history = Connection::open(self.home.join("sqlite/codex-history-snapshots-dev.db"))
                .expect("open history db");
            history
                .execute_batch(
                    "create table app_server_history_snapshots (thread_id text primary key, payload_json text);",
                )
                .expect("create history schema");
            history
                .execute(
                    "insert into app_server_history_snapshots values (?1, '{\"fixture\":true}')",
                    params![self.root_id],
                )
                .expect("insert history snapshot");
        }

        fn create_rollouts(&self) {
            let root_meta = serde_json::json!({
                "type": "session_meta",
                "timestamp": "2026-08-17T00:00:00Z",
                "payload": {"id": self.root_id, "cwd": self.source_path, "thread_source": "user"},
            });
            let child_source = serde_json::json!({
                "subagent": {"thread_spawn": {"parent_thread_id": self.root_id, "depth": 1}}
            });
            let child_meta = serde_json::json!({
                "type": "session_meta",
                "timestamp": "2026-08-17T00:00:00Z",
                "payload": {
                    "id": self.child_id,
                    "cwd": self.source_path,
                    "thread_source": "subagent",
                    "source": child_source,
                },
            });
            std::fs::write(
                &self.root_rollout,
                format!("{}\n", serde_json::to_string(&root_meta).unwrap()),
            )
            .expect("write root rollout");
            std::fs::write(
                &self.child_rollout,
                format!("{}\n", serde_json::to_string(&child_meta).unwrap()),
            )
            .expect("write child rollout");
        }

        fn create_metadata(&self) {
            let index_entry = serde_json::json!({
                "id": self.root_id,
                "thread_name": "Root",
                "updated_at": "2026-08-17T00:00:00Z",
            });
            std::fs::write(
                self.home.join("session_index.jsonl"),
                format!("{}\n", serde_json::to_string(&index_entry).unwrap()),
            )
            .expect("write session index");
            let state = serde_json::json!({
                "local-projects": {
                    self.source_project_id: {
                        "id": self.source_project_id,
                        "name": "Source",
                        "rootPaths": [self.source_path],
                    },
                    self.destination_project_id: {
                        "id": self.destination_project_id,
                        "name": "Destination",
                        "rootPaths": [self.destination_path],
                    },
                },
                "thread-project-assignments": {
                    self.root_id: {"projectKind": "local", "projectId": self.source_project_id},
                },
                "sidebar-project-thread-orders": {
                    self.source_project_id: {"threadIds": [self.root_id]},
                    self.destination_project_id: {"threadIds": []},
                },
                "projectless-thread-ids": [],
                "thread-workspace-root-hints": {},
                "thread-projectless-output-directories": {},
            });
            std::fs::write(
                self.home.join(".codex-global-state.json"),
                serde_json::to_string_pretty(&state).unwrap(),
            )
            .expect("write global state");
        }

        fn query_text(&self, sql: &str) -> String {
            let conn = Connection::open(&self.state_db).expect("open state db");
            conn.query_row(sql, [], |row| {
                row.get::<_, Option<String>>(0)
                    .map(|value| value.unwrap_or_default())
            })
            .expect("query state db")
        }

        fn query_count(&self, sql: &str) -> i64 {
            let conn = Connection::open(&self.state_db).expect("open state db");
            conn.query_row(sql, [], |row| row.get::<_, i64>(0))
                .expect("query state db count")
        }

        fn query_external_on(&self, db_name: &str, sql: &str) -> String {
            let conn =
                Connection::open(self.home.join("sqlite").join(db_name)).expect("open external db");
            conn.query_row(sql, [], |row| match row.get_ref(0)? {
                ValueRef::Null => Ok(String::new()),
                ValueRef::Integer(value) => Ok(value.to_string()),
                ValueRef::Real(value) => Ok(value.to_string()),
                ValueRef::Text(value) => Ok(String::from_utf8_lossy(value).to_string()),
                ValueRef::Blob(value) => Ok(String::from_utf8_lossy(value).to_string()),
            })
            .expect("query external db")
        }

        fn read_global_state(&self) -> Value {
            serde_json::from_str(
                &std::fs::read_to_string(self.home.join(".codex-global-state.json"))
                    .expect("read global state"),
            )
            .expect("parse global state")
        }
    }

    #[test]
    fn scan_folds_subagents_into_parent() {
        let fixture = CodexCompatFixture::new();
        let sessions =
            scan_sessions_from_sqlite(&fixture.home, &fixture.state_db).expect("scan sessions");

        let ids: Vec<&str> = sessions
            .iter()
            .map(|session| session.session_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![fixture.root_id],
            "Only the parent should be listed"
        );
        assert_eq!(
            sessions[0].is_in_session_index,
            Some(true),
            "Indexed parent must be indexed"
        );
        assert_eq!(
            sessions[0].needs_repair,
            Some(false),
            "Indexed parent must not need repair"
        );
    }

    #[test]
    fn move_updates_sqlite_rollout_and_native_project_metadata() {
        let fixture = CodexCompatFixture::new();
        move_session_with_home(
            &fixture.home,
            &fixture.root_rollout,
            fixture.root_id,
            &fixture.destination_path,
        )
        .expect("move session");

        let cwd = {
            let conn = Connection::open(&fixture.state_db).expect("open state db");
            conn.query_row(
                "select cwd from threads where id = ?1",
                params![fixture.root_id],
                |row| row.get::<_, String>(0),
            )
            .expect("select cwd")
        };
        assert_eq!(cwd, fixture.destination_path, "SQLite cwd was not moved");

        let rollout: Value = serde_json::from_str(
            std::fs::read_to_string(&fixture.root_rollout)
                .expect("read rollout")
                .lines()
                .next()
                .expect("rollout line"),
        )
        .expect("parse rollout");
        assert_eq!(
            rollout["payload"]["cwd"].as_str(),
            Some(fixture.destination_path.as_str()),
            "Rollout cwd was not moved"
        );

        let state = fixture.read_global_state();
        let assignment = &state["thread-project-assignments"][fixture.root_id];
        assert_eq!(
            assignment["projectId"].as_str(),
            Some(fixture.destination_project_id),
            "Native project assignment was not moved"
        );
        let orders = &state["sidebar-project-thread-orders"];
        let source_ids = orders[fixture.source_project_id]["threadIds"]
            .as_array()
            .expect("source order");
        assert!(
            !source_ids
                .iter()
                .any(|value| value.as_str() == Some(fixture.root_id)),
            "Source sidebar still contains the moved thread"
        );
        let destination_ids = orders[fixture.destination_project_id]["threadIds"]
            .as_array()
            .expect("destination order");
        assert_eq!(
            destination_ids.first().and_then(Value::as_str),
            Some(fixture.root_id),
            "Destination sidebar does not contain the moved thread"
        );
    }

    #[test]
    fn trash_and_restore_preserve_current_metadata_and_relations() {
        let fixture = CodexCompatFixture::new();
        trash_session_with_home(&fixture.home, &fixture.root_rollout, fixture.root_id)
            .expect("trash session");

        assert_eq!(
            fixture.query_count(&format!(
                "select count(*) from threads where id = '{}'",
                fixture.root_id
            )),
            0,
            "Thread row remains after trash"
        );
        assert_eq!(
            fixture.query_count(&format!(
                "select count(*) from thread_dynamic_tools where thread_id = '{}'",
                fixture.root_id
            )),
            0,
            "Dynamic tools remain after trash"
        );
        assert_eq!(
            fixture.query_count(&format!(
                "select count(*) from thread_spawn_edges where parent_thread_id = '{}'",
                fixture.root_id
            )),
            0,
            "Spawn edges remain after trash"
        );
        assert_eq!(
            fixture.query_external_on(
                "codex-dev.db",
                &format!(
                    "select count(*) from local_thread_catalog where thread_id = '{}'",
                    fixture.root_id
                )
            ),
            "0",
            "Catalog row remains after trash"
        );
        assert_eq!(
            fixture.query_external_on(
                "codex-dev.db",
                "select catalog_revision from local_thread_catalog_metadata where id = 1"
            ),
            "5",
            "Catalog revision was not advanced after trash"
        );
        assert_eq!(
            fixture.query_external_on(
                "codex-history-snapshots-dev.db",
                &format!(
                    "select count(*) from app_server_history_snapshots where thread_id = '{}'",
                    fixture.root_id
                )
            ),
            "0",
            "History snapshot remains after trash"
        );
        assert!(
            !fixture.root_rollout.exists(),
            "Rollout remains after trash"
        );
        assert!(
            fixture.read_global_state()["thread-project-assignments"][fixture.root_id].is_null(),
            "Global assignment remains after trash"
        );

        let manifest = thread_trash_path(&fixture.home)
            .join(fixture.root_id)
            .join("manifest.json");
        restore_trashed_thread_with_home(&fixture.home, &manifest).expect("restore session");

        assert!(fixture.root_rollout.exists(), "Rollout was not restored");
        assert_eq!(
            fixture.query_text(&format!(
                "select name || '|' || is_pinned || '|' || history_mode from threads where id = '{}'",
                fixture.root_id
            )),
            "Pinned root|1|legacy",
            "Current thread metadata was not restored"
        );
        assert_eq!(
            fixture.query_count(&format!(
                "select count(*) from thread_dynamic_tools where thread_id = '{}'",
                fixture.root_id
            )),
            1,
            "Dynamic tools were not restored"
        );
        assert_eq!(
            fixture.query_text(&format!(
                "select status from thread_spawn_edges where parent_thread_id = '{}'",
                fixture.root_id
            )),
            "open",
            "Spawn edge was not restored"
        );
        assert_eq!(
            fixture.query_external_on(
                "codex-dev.db",
                &format!(
                    "select display_title from local_thread_catalog where thread_id = '{}'",
                    fixture.root_id
                )
            ),
            "Catalog root",
            "Catalog row was not restored"
        );
        assert_eq!(
            fixture.query_external_on(
                "codex-dev.db",
                "select catalog_revision from local_thread_catalog_metadata where id = 1"
            ),
            "6",
            "Catalog revision was not advanced after restore"
        );
        assert_eq!(
            fixture.query_external_on(
                "codex-history-snapshots-dev.db",
                &format!(
                    "select payload_json from app_server_history_snapshots where thread_id = '{}'",
                    fixture.root_id
                )
            ),
            "{\"fixture\":true}",
            "History snapshot was not restored"
        );
        let state = fixture.read_global_state();
        assert_eq!(
            state["thread-project-assignments"][fixture.root_id]["projectId"].as_str(),
            Some(fixture.source_project_id),
            "Native project assignment was not restored"
        );

        let sessions = scan_sessions_from_sqlite(&fixture.home, &fixture.state_db)
            .expect("scan after restore");
        let ids: Vec<&str> = sessions
            .iter()
            .map(|session| session.session_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![fixture.root_id],
            "Restored parent must stay without independent subagent entries"
        );
    }
}
