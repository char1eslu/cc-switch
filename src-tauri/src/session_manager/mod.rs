pub mod providers;
pub mod terminal;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use providers::{claude, codex};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMeta {
    pub provider_id: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_active_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_in_session_index: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_exists: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_repair: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_number: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_line_number: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_trim: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_branch: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSessionRequest {
    pub provider_id: String,
    pub session_id: String,
    pub source_path: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSessionOutcome {
    pub provider_id: String,
    pub session_id: String,
    pub source_path: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn scan_sessions() -> Vec<SessionMeta> {
    let (r1, r2) = std::thread::scope(|s| {
        let h1 = s.spawn(codex::scan_sessions);
        let h2 = s.spawn(claude::scan_sessions);
        (h1.join().unwrap_or_default(), h2.join().unwrap_or_default())
    });

    let mut sessions = Vec::new();
    sessions.extend(r1);
    sessions.extend(r2);

    sessions.sort_by(|a, b| {
        let a_ts = a.last_active_at.or(a.created_at).unwrap_or(0);
        let b_ts = b.last_active_at.or(b.created_at).unwrap_or(0);
        b_ts.cmp(&a_ts)
    });

    sessions
}

pub fn load_messages(provider_id: &str, source_path: &str) -> Result<Vec<SessionMessage>, String> {
    let path = Path::new(source_path);
    match provider_id {
        "codex" => codex::load_messages(path),
        "claude" => claude::load_messages(path),
        _ => Err(format!("Unsupported provider: {provider_id}")),
    }
}

pub fn delete_session(
    provider_id: &str,
    session_id: &str,
    source_path: &str,
) -> Result<bool, String> {
    let roots = provider_roots(provider_id)?;
    delete_session_with_roots(provider_id, session_id, Path::new(source_path), &roots)
}

pub fn delete_sessions(requests: &[DeleteSessionRequest]) -> Vec<DeleteSessionOutcome> {
    collect_delete_session_outcomes(requests, |request| {
        delete_session(
            &request.provider_id,
            &request.session_id,
            &request.source_path,
        )
    })
}

pub fn move_session(
    provider_id: &str,
    session_id: &str,
    source_path: &str,
    target_project_dir: &str,
) -> Result<bool, String> {
    if provider_id != "codex" {
        return Err(format!(
            "Moving sessions is currently supported only for Codex, got {provider_id}"
        ));
    }

    validate_target_project_dir(target_project_dir)?;
    let roots = provider_roots(provider_id)?;
    move_session_with_roots(
        provider_id,
        session_id,
        Path::new(source_path),
        target_project_dir,
        &roots,
    )
}

pub fn repair_session(
    provider_id: &str,
    session_id: &str,
    source_path: &str,
) -> Result<codex::CodexOperationReport, String> {
    if provider_id != "codex" {
        return Err(format!(
            "Repair Index is currently supported only for Codex, got {provider_id}"
        ));
    }
    codex::repair_session(Path::new(source_path), session_id)
}

pub fn trim_session(
    provider_id: &str,
    session_id: &str,
    source_path: &str,
    line_number: usize,
) -> Result<codex::CodexOperationReport, String> {
    if provider_id != "codex" {
        return Err(format!(
            "Trim is currently supported only for Codex, got {provider_id}"
        ));
    }
    codex::trim_session(Path::new(source_path), session_id, line_number)
}

pub fn branch_session(
    provider_id: &str,
    session_id: &str,
    source_path: &str,
    line_number: usize,
) -> Result<codex::CodexOperationReport, String> {
    if provider_id != "codex" {
        return Err(format!(
            "Branch is currently supported only for Codex, got {provider_id}"
        ));
    }
    codex::branch_session(Path::new(source_path), session_id, line_number)
}

pub fn trash_session(
    provider_id: &str,
    session_id: &str,
    source_path: &str,
) -> Result<codex::CodexOperationReport, String> {
    if provider_id != "codex" {
        return Err(format!(
            "Codex Wake Trash is currently supported only for Codex, got {provider_id}"
        ));
    }
    codex::trash_session(Path::new(source_path), session_id)
}

fn delete_session_with_roots(
    provider_id: &str,
    session_id: &str,
    source_path: &Path,
    roots: &[PathBuf],
) -> Result<bool, String> {
    let validated_source = canonicalize_existing_path(source_path, "session source")?;

    let mut saw_existing_root = false;
    for root in roots {
        if !root.exists() {
            continue;
        }

        saw_existing_root = true;
        let validated_root = canonicalize_existing_path(root, "session root")?;
        if validated_source.starts_with(&validated_root) {
            return match provider_id {
                "codex" => codex::delete_session(&validated_root, &validated_source, session_id),
                "claude" => claude::delete_session(&validated_root, &validated_source, session_id),
                _ => Err(format!("Unsupported provider: {provider_id}")),
            };
        }
    }

    if !saw_existing_root {
        return Err(format!(
            "Session root not found for provider {provider_id}: {}",
            roots
                .first()
                .map(|root| root.display().to_string())
                .unwrap_or_else(|| "<none>".to_string())
        ));
    }

    Err(format!(
        "Session source path is outside provider roots: {}",
        source_path.display()
    ))
}

fn move_session_with_roots(
    provider_id: &str,
    session_id: &str,
    source_path: &Path,
    target_project_dir: &str,
    roots: &[PathBuf],
) -> Result<bool, String> {
    let validated_source = canonicalize_existing_path(source_path, "session source")?;

    let mut saw_existing_root = false;
    for root in roots {
        if !root.exists() {
            continue;
        }

        saw_existing_root = true;
        let validated_root = canonicalize_existing_path(root, "session root")?;
        if validated_source.starts_with(&validated_root) {
            return match provider_id {
                "codex" => codex::move_session(
                    &validated_root,
                    &validated_source,
                    session_id,
                    target_project_dir,
                ),
                _ => Err(format!(
                    "Moving sessions is currently supported only for Codex, got {provider_id}"
                )),
            };
        }
    }

    if !saw_existing_root {
        return Err(format!(
            "Session root not found for provider {provider_id}: {}",
            roots
                .first()
                .map(|root| root.display().to_string())
                .unwrap_or_else(|| "<none>".to_string())
        ));
    }

    Err(format!(
        "Session source path is outside provider roots: {}",
        source_path.display()
    ))
}

fn provider_roots(provider_id: &str) -> Result<Vec<PathBuf>, String> {
    let roots = match provider_id {
        "codex" => codex::session_roots(),
        "claude" => vec![crate::config::get_claude_config_dir().join("projects")],
        _ => return Err(format!("Unsupported provider: {provider_id}")),
    };

    Ok(roots)
}

fn validate_target_project_dir(target_project_dir: &str) -> Result<(), String> {
    let trimmed = target_project_dir.trim();
    if trimmed.is_empty() {
        return Err("Target project directory is required".to_string());
    }

    if !Path::new(trimmed).is_absolute() {
        return Err("Target project directory must be an absolute path".to_string());
    }

    Ok(())
}

fn canonicalize_existing_path(path: &Path, label: &str) -> Result<PathBuf, String> {
    if !path.exists() {
        return Err(format!("{label} not found: {}", path.display()));
    }

    path.canonicalize()
        .map_err(|e| format!("Failed to resolve {label} {}: {e}", path.display()))
}

fn collect_delete_session_outcomes<F>(
    requests: &[DeleteSessionRequest],
    mut deleter: F,
) -> Vec<DeleteSessionOutcome>
where
    F: FnMut(&DeleteSessionRequest) -> Result<bool, String>,
{
    requests
        .iter()
        .map(|request| match deleter(request) {
            Ok(true) => DeleteSessionOutcome {
                provider_id: request.provider_id.clone(),
                session_id: request.session_id.clone(),
                source_path: request.source_path.clone(),
                success: true,
                error: None,
            },
            Ok(false) => DeleteSessionOutcome {
                provider_id: request.provider_id.clone(),
                session_id: request.session_id.clone(),
                source_path: request.source_path.clone(),
                success: false,
                error: Some("Session was not deleted".to_string()),
            },
            Err(error) => DeleteSessionOutcome {
                provider_id: request.provider_id.clone(),
                session_id: request.session_id.clone(),
                source_path: request.source_path.clone(),
                success: false,
                error: Some(error),
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_codex_session(path: &Path, session_id: &str) {
        std::fs::write(
            path,
            format!(
                "{{\"timestamp\":\"2026-03-06T21:50:12Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{session_id}\",\"cwd\":\"/tmp/project\"}}}}\n\
                 {{\"timestamp\":\"2026-03-06T21:50:13Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":\"hello\"}}}}\n",
            ),
        )
        .expect("write source");
    }

    #[test]
    fn moves_codex_source_path_under_allowed_root() {
        let codex_home = tempdir().expect("tempdir");
        let root = codex_home.path().join("sessions");
        std::fs::create_dir_all(&root).expect("sessions dir");
        let source = root.join("session.jsonl");
        write_codex_session(&source, "move-session");

        let moved = move_session_with_roots(
            "codex",
            "move-session",
            &source,
            "/tmp/new-project",
            &[root],
        )
        .expect("move session");

        assert!(moved);

        let first_line = std::fs::read_to_string(&source)
            .expect("read source")
            .lines()
            .next()
            .unwrap()
            .to_string();
        let value: serde_json::Value = serde_json::from_str(&first_line).expect("json");
        assert_eq!(
            value
                .get("payload")
                .and_then(|payload| payload.get("cwd"))
                .and_then(serde_json::Value::as_str),
            Some("/tmp/new-project")
        );
    }

    #[test]
    fn move_rejects_source_path_outside_provider_root() {
        let root = tempdir().expect("tempdir");
        let outside = tempdir().expect("tempdir");
        let source = outside.path().join("session.jsonl");
        write_codex_session(&source, "session-1");

        let err = move_session_with_roots(
            "codex",
            "session-1",
            &source,
            "/tmp/new-project",
            &[root.path().to_path_buf()],
        )
        .expect_err("expected outside-root path to be rejected");

        assert!(err.contains("outside provider roots"));
    }

    #[test]
    fn move_rejects_relative_target_project_dir() {
        let err = validate_target_project_dir("relative/project")
            .expect_err("expected relative target path to fail");

        assert!(err.contains("absolute path"));
    }

    #[test]
    fn accepts_source_path_under_any_allowed_provider_root() {
        let active_root = tempdir().expect("active root");
        let archived_root = tempdir().expect("archived root");
        let source = archived_root.path().join("session.jsonl");
        write_codex_session(&source, "archived-session");

        let deleted = delete_session_with_roots(
            "codex",
            "archived-session",
            &source,
            &[
                active_root.path().to_path_buf(),
                archived_root.path().to_path_buf(),
            ],
        )
        .expect("delete archived session");

        assert!(deleted);
        assert!(!source.exists());
    }

    #[test]
    fn rejects_source_path_outside_provider_root() {
        let root = tempdir().expect("tempdir");
        let outside = tempdir().expect("tempdir");
        let source = outside.path().join("session.jsonl");
        std::fs::write(&source, "{}").expect("write source");

        let err =
            delete_session_with_roots("codex", "session-1", &source, &[root.path().to_path_buf()])
                .expect_err("expected outside-root path to be rejected");

        assert!(err.contains("outside provider roots"));
    }

    #[test]
    fn rejects_missing_source_path() {
        let root = tempdir().expect("tempdir");
        let missing = root.path().join("missing.jsonl");

        let err =
            delete_session_with_roots("codex", "session-1", &missing, &[root.path().to_path_buf()])
                .expect_err("expected missing source path to fail");

        assert!(err.contains("session source not found"));
    }

    #[test]
    fn batch_delete_collects_successes_and_failures_in_order() {
        let requests = vec![
            DeleteSessionRequest {
                provider_id: "codex".to_string(),
                session_id: "s1".to_string(),
                source_path: "/tmp/s1".to_string(),
            },
            DeleteSessionRequest {
                provider_id: "claude".to_string(),
                session_id: "s2".to_string(),
                source_path: "/tmp/s2".to_string(),
            },
        ];

        let outcomes = collect_delete_session_outcomes(&requests, |request| {
            match request.session_id.as_str() {
                "s1" => Ok(true),
                "s2" => Err("boom".to_string()),
                _ => Ok(false),
            }
        });

        assert_eq!(outcomes.len(), 3);
        assert!(outcomes[0].success);
        assert_eq!(outcomes[0].error, None);
        assert!(!outcomes[1].success);
        assert_eq!(outcomes[1].error.as_deref(), Some("boom"));
        assert!(!outcomes[2].success);
        assert_eq!(
            outcomes[2].error.as_deref(),
            Some("Session was not deleted")
        );
    }
}
