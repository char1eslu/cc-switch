#![allow(non_snake_case)]

use crate::session_manager;

#[tauri::command]
pub async fn list_sessions() -> Result<Vec<session_manager::SessionMeta>, String> {
    let sessions = tauri::async_runtime::spawn_blocking(session_manager::scan_sessions)
        .await
        .map_err(|e| format!("Failed to scan sessions: {e}"))?;
    Ok(sessions)
}

#[tauri::command]
pub async fn get_session_messages(
    providerId: String,
    sourcePath: String,
) -> Result<Vec<session_manager::SessionMessage>, String> {
    let provider_id = providerId.clone();
    let source_path = sourcePath.clone();
    tauri::async_runtime::spawn_blocking(move || {
        session_manager::load_messages(&provider_id, &source_path)
    })
    .await
    .map_err(|e| format!("Failed to load session messages: {e}"))?
}

#[tauri::command]
pub async fn launch_session_terminal(
    command: String,
    cwd: Option<String>,
    custom_config: Option<String>,
) -> Result<bool, String> {
    let command = command.clone();
    let cwd = cwd.clone();
    let custom_config = custom_config.clone();

    // Read preferred terminal from global settings
    let preferred = crate::settings::get_preferred_terminal();
    // Map global setting terminal names to session terminal names
    // Global uses "iterm2", session terminal uses "iterm"
    let target = match preferred.as_deref() {
        Some("iterm2") => "iterm".to_string(),
        Some(t) => t.to_string(),
        None => "terminal".to_string(), // Default to Terminal.app on macOS
    };

    tauri::async_runtime::spawn_blocking(move || {
        session_manager::terminal::launch_terminal(
            &target,
            &command,
            cwd.as_deref(),
            custom_config.as_deref(),
        )
    })
    .await
    .map_err(|e| format!("Failed to launch terminal: {e}"))??;

    Ok(true)
}

#[tauri::command]
pub async fn delete_session(
    providerId: String,
    sessionId: String,
    sourcePath: String,
) -> Result<bool, String> {
    let provider_id = providerId.clone();
    let session_id = sessionId.clone();
    let source_path = sourcePath.clone();

    tauri::async_runtime::spawn_blocking(move || {
        session_manager::delete_session(&provider_id, &session_id, &source_path)
    })
    .await
    .map_err(|e| format!("Failed to delete session: {e}"))?
}

#[tauri::command]
pub async fn delete_sessions(
    items: Vec<session_manager::DeleteSessionRequest>,
) -> Result<Vec<session_manager::DeleteSessionOutcome>, String> {
    tauri::async_runtime::spawn_blocking(move || session_manager::delete_sessions(&items))
        .await
        .map_err(|e| format!("Failed to delete sessions: {e}"))
}

#[tauri::command]
pub async fn move_session(
    providerId: String,
    sessionId: String,
    sourcePath: String,
    targetProjectDir: String,
) -> Result<bool, String> {
    let provider_id = providerId.clone();
    let session_id = sessionId.clone();
    let source_path = sourcePath.clone();
    let target_project_dir = targetProjectDir.clone();

    tauri::async_runtime::spawn_blocking(move || {
        session_manager::move_session(
            &provider_id,
            &session_id,
            &source_path,
            &target_project_dir,
        )
    })
    .await
    .map_err(|e| format!("Failed to move session: {e}"))?
}

#[tauri::command]
pub async fn repair_session(
    providerId: String,
    sessionId: String,
    sourcePath: String,
) -> Result<session_manager::providers::codex::CodexOperationReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        session_manager::repair_session(&providerId, &sessionId, &sourcePath)
    })
    .await
    .map_err(|e| format!("Failed to repair session: {e}"))?
}

#[tauri::command]
pub async fn trim_session(
    providerId: String,
    sessionId: String,
    sourcePath: String,
    lineNumber: usize,
) -> Result<session_manager::providers::codex::CodexOperationReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        session_manager::trim_session(&providerId, &sessionId, &sourcePath, lineNumber)
    })
    .await
    .map_err(|e| format!("Failed to trim session: {e}"))?
}

#[tauri::command]
pub async fn branch_session(
    providerId: String,
    sessionId: String,
    sourcePath: String,
    lineNumber: usize,
) -> Result<session_manager::providers::codex::CodexOperationReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        session_manager::branch_session(&providerId, &sessionId, &sourcePath, lineNumber)
    })
    .await
    .map_err(|e| format!("Failed to branch session: {e}"))?
}

#[tauri::command]
pub async fn trash_session(
    providerId: String,
    sessionId: String,
    sourcePath: String,
) -> Result<session_manager::providers::codex::CodexOperationReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        session_manager::trash_session(&providerId, &sessionId, &sourcePath)
    })
    .await
    .map_err(|e| format!("Failed to move session to trash: {e}"))?
}

#[tauri::command]
pub async fn list_codex_backups(
    includeTrash: bool,
) -> Result<Vec<session_manager::providers::codex::CodexBackupFile>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        session_manager::providers::codex::list_backups(includeTrash)
    })
    .await
    .map_err(|e| format!("Failed to list Codex backups: {e}"))?
}

#[tauri::command]
pub async fn restore_codex_backup(backupPath: String, originalPath: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        session_manager::providers::codex::restore_backup(
            std::path::Path::new(&backupPath),
            std::path::Path::new(&originalPath),
        )
    })
    .await
    .map_err(|e| format!("Failed to restore Codex backup: {e}"))?
}

#[tauri::command]
pub async fn move_codex_backup_to_trash(backupPath: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        session_manager::providers::codex::move_backup_to_trash(std::path::Path::new(&backupPath))
    })
    .await
    .map_err(|e| format!("Failed to move Codex backup to trash: {e}"))?
}

#[tauri::command]
pub async fn list_codex_trashed_threads(
) -> Result<Vec<session_manager::providers::codex::CodexTrashedThread>, String> {
    tauri::async_runtime::spawn_blocking(session_manager::providers::codex::list_trashed_threads)
        .await
        .map_err(|e| format!("Failed to list Codex trash: {e}"))?
}

#[tauri::command]
pub async fn restore_codex_trashed_thread(manifestPath: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        session_manager::providers::codex::restore_trashed_thread(std::path::Path::new(
            &manifestPath,
        ))
    })
    .await
    .map_err(|e| format!("Failed to restore Codex trashed thread: {e}"))?
}

#[tauri::command]
pub async fn delete_codex_trashed_thread(manifestPath: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        session_manager::providers::codex::delete_trashed_thread(std::path::Path::new(
            &manifestPath,
        ))
    })
    .await
    .map_err(|e| format!("Failed to delete Codex trashed thread: {e}"))?
}

#[tauri::command]
pub async fn empty_codex_thread_trash() -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(session_manager::providers::codex::empty_thread_trash)
        .await
        .map_err(|e| format!("Failed to empty Codex thread trash: {e}"))?
}

#[tauri::command]
pub async fn empty_codex_backup_trash() -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(session_manager::providers::codex::empty_backup_trash)
        .await
        .map_err(|e| format!("Failed to empty Codex backup trash: {e}"))?
}
