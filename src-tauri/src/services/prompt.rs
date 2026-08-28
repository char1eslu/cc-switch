use indexmap::IndexMap;
use std::path::Path;

use crate::app_config::AppType;
use crate::config::write_text_file;
use crate::error::AppError;
use crate::prompt::Prompt;
use crate::prompt_files::prompt_file_path;
use crate::store::AppState;

/// 安全地获取当前 Unix 时间戳
fn get_unix_timestamp() -> Result<i64, AppError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| AppError::Message(format!("Failed to get system time: {e}")))
}

pub struct PromptService;

fn project_prompt_set_to_path(
    prompts: &IndexMap<String, Prompt>,
    target_path: &Path,
) -> Result<Option<String>, AppError> {
    let enabled: Vec<(&String, &Prompt)> = prompts
        .iter()
        .filter(|(_, prompt)| prompt.enabled)
        .collect();

    if let Some((_, prompt)) = enabled.first() {
        write_text_file(target_path, &prompt.content)?;
    }
    // With nothing enabled, leave the target file untouched. This projection
    // only runs after a database restore, and the live file is not part of
    // the sync payload — clearing it here would wipe local content the
    // restored snapshot never contained. Disabling the last prompt from the
    // UI still clears the file via `PromptService::upsert_prompt`.

    if enabled.len() <= 1 {
        return Ok(None);
    }

    let ids = enabled
        .iter()
        .map(|(id, _)| id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Ok(Some(format!(
        "多个 Prompt 同时启用，已按稳定顺序投影第一个；enabled IDs: {ids}"
    )))
}

impl PromptService {
    pub fn get_prompts(
        state: &AppState,
        app: AppType,
    ) -> Result<IndexMap<String, Prompt>, AppError> {
        Self::sync_live_file_to_db(state, app.clone())?;
        state.db.get_prompts(app.as_str())
    }

    fn sync_live_file_to_db(state: &AppState, app: AppType) -> Result<(), AppError> {
        let target_path = prompt_file_path(&app)?;
        if !target_path.exists() {
            Self::disable_enabled_prompts(state, &app)?;
            return Ok(());
        }

        let live_content =
            std::fs::read_to_string(&target_path).map_err(|e| AppError::io(&target_path, e))?;
        if live_content.trim().is_empty() {
            Self::disable_enabled_prompts(state, &app)?;
            return Ok(());
        }

        let mut prompts = state.db.get_prompts(app.as_str())?;
        if let Some((matching_enabled_id, _)) = prompts
            .iter()
            .find(|(_, prompt)| prompt.enabled && prompt.content == live_content)
        {
            Self::disable_other_enabled_prompts(state, &app, matching_enabled_id)?;
            return Ok(());
        }

        let timestamp = get_unix_timestamp()?;
        if let Some((enabled_id, enabled_prompt)) = prompts
            .iter_mut()
            .find(|(_, prompt)| prompt.enabled)
            .map(|(id, prompt)| (id.clone(), prompt))
        {
            enabled_prompt.content = live_content;
            enabled_prompt.updated_at = Some(timestamp);
            state.db.save_prompt(app.as_str(), enabled_prompt)?;
            Self::disable_other_enabled_prompts(state, &app, &enabled_id)?;
            log::info!("同步 live 提示词内容到已启用项: {enabled_id}");
            return Ok(());
        }

        if let Some((matching_id, matching_prompt)) = prompts
            .iter_mut()
            .find(|(_, prompt)| prompt.content.trim() == live_content.trim())
            .map(|(id, prompt)| (id.clone(), prompt))
        {
            matching_prompt.enabled = true;
            matching_prompt.updated_at = Some(timestamp);
            state.db.save_prompt(app.as_str(), matching_prompt)?;
            Self::disable_other_enabled_prompts(state, &app, &matching_id)?;
            log::info!("同步 live 提示词内容，启用已有项: {matching_id}");
            return Ok(());
        }

        let id = format!("auto-imported-{timestamp}");
        let prompt = Prompt {
            id: id.clone(),
            name: format!(
                "Auto-imported Prompt {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M")
            ),
            content: live_content,
            description: Some("Automatically imported from live prompt file".to_string()),
            enabled: true,
            created_at: Some(timestamp),
            updated_at: Some(timestamp),
        };
        state.db.save_prompt(app.as_str(), &prompt)?;
        Self::disable_other_enabled_prompts(state, &app, &id)?;
        log::info!("同步 live 提示词内容，创建已启用项: {id}");

        Ok(())
    }

    fn disable_other_enabled_prompts(
        state: &AppState,
        app: &AppType,
        enabled_id: &str,
    ) -> Result<(), AppError> {
        Self::disable_enabled_prompts_except(state, app, Some(enabled_id))
    }

    fn disable_enabled_prompts(state: &AppState, app: &AppType) -> Result<(), AppError> {
        Self::disable_enabled_prompts_except(state, app, None)
    }

    fn disable_enabled_prompts_except(
        state: &AppState,
        app: &AppType,
        enabled_id: Option<&str>,
    ) -> Result<(), AppError> {
        let prompts = state.db.get_prompts(app.as_str())?;
        for mut prompt in prompts.into_values() {
            if enabled_id != Some(prompt.id.as_str()) && prompt.enabled {
                prompt.enabled = false;
                state.db.save_prompt(app.as_str(), &prompt)?;
            }
        }

        Ok(())
    }

    pub fn upsert_prompt(
        state: &AppState,
        app: AppType,
        id: &str,
        mut prompt: Prompt,
    ) -> Result<(), AppError> {
        prompt.id = id.to_string();

        let prompts_to_disable: Vec<Prompt> = if prompt.enabled {
            state
                .db
                .get_prompts(app.as_str())?
                .into_values()
                .filter(|existing| existing.id != prompt.id && existing.enabled)
                .map(|mut existing| {
                    existing.enabled = false;
                    existing
                })
                .collect()
        } else {
            Vec::new()
        };

        let is_enabled = prompt.enabled;

        state.db.save_prompt(app.as_str(), &prompt)?;

        for prompt_to_disable in prompts_to_disable {
            state.db.save_prompt(app.as_str(), &prompt_to_disable)?;
        }

        if is_enabled {
            // 启用提示词：写入内容到文件
            let target_path = prompt_file_path(&app)?;
            write_text_file(&target_path, &prompt.content)?;
        } else {
            // 禁用提示词：检查是否还有其他已启用的提示词
            let prompts = state.db.get_prompts(app.as_str())?;
            let any_enabled = prompts.values().any(|p| p.enabled);

            if !any_enabled {
                // 所有提示词都已禁用，清空文件
                let target_path = prompt_file_path(&app)?;
                if target_path.exists() {
                    write_text_file(&target_path, "")?;
                }
            }
        }

        Ok(())
    }

    pub fn delete_prompt(state: &AppState, app: AppType, id: &str) -> Result<(), AppError> {
        let prompts = state.db.get_prompts(app.as_str())?;

        if let Some(prompt) = prompts.get(id) {
            if prompt.enabled {
                return Err(AppError::InvalidInput("无法删除已启用的提示词".to_string()));
            }
        }

        state.db.delete_prompt(app.as_str(), id)?;
        Ok(())
    }

    pub fn enable_prompt(state: &AppState, app: AppType, id: &str) -> Result<(), AppError> {
        // 回填当前 live 文件内容到已启用的提示词，或创建备份
        let target_path = prompt_file_path(&app)?;
        if target_path.exists() {
            if let Ok(live_content) = std::fs::read_to_string(&target_path) {
                if !live_content.trim().is_empty() {
                    let mut prompts = state.db.get_prompts(app.as_str())?;

                    // 尝试回填到当前已启用的提示词
                    if let Some((enabled_id, enabled_prompt)) = prompts
                        .iter_mut()
                        .find(|(_, p)| p.enabled)
                        .map(|(id, p)| (id.clone(), p))
                    {
                        let timestamp = get_unix_timestamp()?;
                        enabled_prompt.content = live_content.clone();
                        enabled_prompt.updated_at = Some(timestamp);
                        log::info!("回填 live 提示词内容到已启用项: {enabled_id}");
                        state.db.save_prompt(app.as_str(), enabled_prompt)?;
                    } else {
                        // 没有已启用的提示词，则创建一次备份（避免重复备份）
                        let content_exists = prompts
                            .values()
                            .any(|p| p.content.trim() == live_content.trim());
                        if !content_exists {
                            let timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs() as i64;
                            let backup_id = format!("backup-{timestamp}");
                            let backup_prompt = Prompt {
                                id: backup_id.clone(),
                                name: format!(
                                    "原始提示词 {}",
                                    chrono::Local::now().format("%Y-%m-%d %H:%M")
                                ),
                                content: live_content,
                                description: Some("自动备份的原始提示词".to_string()),
                                enabled: false,
                                created_at: Some(timestamp),
                                updated_at: Some(timestamp),
                            };
                            log::info!("回填 live 提示词内容，创建备份: {backup_id}");
                            state.db.save_prompt(app.as_str(), &backup_prompt)?;
                        }
                    }
                }
            }
        }

        // 启用目标提示词并写入文件
        let mut prompts = state.db.get_prompts(app.as_str())?;

        for prompt in prompts.values_mut() {
            prompt.enabled = false;
        }

        if let Some(prompt) = prompts.get_mut(id) {
            prompt.enabled = true;
            write_text_file(&target_path, &prompt.content)?; // 原子写入
            state.db.save_prompt(app.as_str(), prompt)?;
        } else {
            return Err(AppError::InvalidInput(format!("提示词 {id} 不存在")));
        }

        // Save all prompts to disable others
        for (_, prompt) in prompts.iter() {
            state.db.save_prompt(app.as_str(), prompt)?;
        }

        Ok(())
    }

    pub fn import_from_file(state: &AppState, app: AppType) -> Result<String, AppError> {
        let file_path = prompt_file_path(&app)?;

        if !file_path.exists() {
            return Err(AppError::Message("提示词文件不存在".to_string()));
        }

        let content =
            std::fs::read_to_string(&file_path).map_err(|e| AppError::io(&file_path, e))?;
        let timestamp = get_unix_timestamp()?;

        let id = format!("imported-{timestamp}");
        let prompt = Prompt {
            id: id.clone(),
            name: format!(
                "导入的提示词 {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M")
            ),
            content,
            description: Some("从现有配置文件导入".to_string()),
            enabled: false,
            created_at: Some(timestamp),
            updated_at: Some(timestamp),
        };

        Self::upsert_prompt(state, app, &id, prompt)?;
        Ok(id)
    }

    pub fn get_current_file_content(app: AppType) -> Result<Option<String>, AppError> {
        let file_path = prompt_file_path(&app)?;
        if !file_path.exists() {
            return Ok(None);
        }
        let content =
            std::fs::read_to_string(&file_path).map_err(|e| AppError::io(&file_path, e))?;
        Ok(Some(content))
    }

    /// Project the database SSOT to one application's managed prompt file.
    ///
    /// This deliberately does not call `enable_prompt`: restore paths must not
    /// read stale live content and write it back into the freshly imported DB.
    pub fn sync_to_live(state: &AppState, app: AppType) -> Result<(), AppError> {
        if matches!(app, AppType::ClaudeDesktop) {
            return Ok(());
        }

        let prompts = state.db.get_prompts(app.as_str())?;
        let target_path = prompt_file_path(&app)?;
        if let Some(warning) = project_prompt_set_to_path(&prompts, &target_path)? {
            return Err(AppError::Message(warning));
        }
        Ok(())
    }

    /// Best-effort projection for every Prompt-capable application.
    pub fn sync_all_to_live(state: &AppState) -> Result<(), AppError> {
        let mut failures = Vec::new();
        for app in AppType::all() {
            if matches!(app, AppType::ClaudeDesktop) {
                continue;
            }
            if let Err(error) = Self::sync_to_live(state, app.clone()) {
                log::warn!("同步 Prompt 到 {app:?} 失败: {error}");
                failures.push(format!("{}: {error}", app.as_str()));
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(AppError::Message(format!(
                "部分应用 Prompt 同步失败: {}",
                failures.join("; ")
            )))
        }
    }

    /// 首次启动时从现有提示词文件自动导入（如果存在）
    /// 返回导入的数量
    pub fn import_from_file_on_first_launch(
        state: &AppState,
        app: AppType,
    ) -> Result<usize, AppError> {
        // 幂等性保护：该应用已有提示词则跳过
        let existing = state.db.get_prompts(app.as_str())?;
        if !existing.is_empty() {
            return Ok(0);
        }

        let file_path = prompt_file_path(&app)?;

        // 检查文件是否存在
        if !file_path.exists() {
            return Ok(0);
        }

        // 读取文件内容
        let content = match std::fs::read_to_string(&file_path) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("读取提示词文件失败: {file_path:?}, 错误: {e}");
                return Ok(0);
            }
        };

        // 检查内容是否为空
        if content.trim().is_empty() {
            return Ok(0);
        }

        log::info!("发现提示词文件，自动导入: {file_path:?}");

        // 创建提示词对象
        let timestamp = get_unix_timestamp()?;
        let id = format!("auto-imported-{timestamp}");
        let prompt = Prompt {
            id: id.clone(),
            name: format!(
                "Auto-imported Prompt {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M")
            ),
            content,
            description: Some("Automatically imported on first launch".to_string()),
            enabled: true, // 首次导入时自动启用
            created_at: Some(timestamp),
            updated_at: Some(timestamp),
        };

        // 保存到数据库
        state.db.save_prompt(app.as_str(), &prompt)?;

        log::info!("自动导入完成: {}", app.as_str());
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use crate::store::AppState;
    use serial_test::serial;
    use std::env;
    use std::fs;
    use std::sync::Arc;
    use tempfile::TempDir;

    struct TempHome {
        #[allow(dead_code)]
        dir: TempDir,
        original_home: Option<String>,
        original_userprofile: Option<String>,
        original_test_home: Option<String>,
    }

    impl TempHome {
        fn new() -> Self {
            let dir = TempDir::new().expect("create temp home");
            let original_home = env::var("HOME").ok();
            let original_userprofile = env::var("USERPROFILE").ok();
            let original_test_home = env::var("CC_SWITCH_TEST_HOME").ok();

            env::set_var("HOME", dir.path());
            env::set_var("USERPROFILE", dir.path());
            env::set_var("CC_SWITCH_TEST_HOME", dir.path());

            Self {
                dir,
                original_home,
                original_userprofile,
                original_test_home,
            }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match &self.original_home {
                Some(value) => env::set_var("HOME", value),
                None => env::remove_var("HOME"),
            }
            match &self.original_userprofile {
                Some(value) => env::set_var("USERPROFILE", value),
                None => env::remove_var("USERPROFILE"),
            }
            match &self.original_test_home {
                Some(value) => env::set_var("CC_SWITCH_TEST_HOME", value),
                None => env::remove_var("CC_SWITCH_TEST_HOME"),
            }
        }
    }

    fn test_state() -> AppState {
        let db = Arc::new(Database::init().expect("init database"));
        AppState::new(db)
    }

    fn prompt(id: &str, content: &str, enabled: bool) -> Prompt {
        Prompt {
            id: id.to_string(),
            name: id.to_string(),
            content: content.to_string(),
            description: None,
            enabled,
            created_at: Some(1),
            updated_at: Some(1),
        }
    }

    fn write_live_prompt(app: &AppType, content: &str) {
        let path = prompt_file_path(app).expect("prompt path");
        fs::create_dir_all(path.parent().expect("prompt parent")).expect("create prompt parent");
        fs::write(path, content).expect("write live prompt");
    }

    #[test]
    #[serial]
    fn upsert_enabled_prompt_disables_other_enabled_prompts() {
        let _home = TempHome::new();
        let state = test_state();

        PromptService::upsert_prompt(&state, AppType::Codex, "one", prompt("one", "one", true))
            .expect("insert first enabled prompt");
        PromptService::upsert_prompt(&state, AppType::Codex, "two", prompt("two", "two", true))
            .expect("insert second enabled prompt");

        let prompts = state
            .db
            .get_prompts(AppType::Codex.as_str())
            .expect("get prompts");

        assert_eq!(prompts.values().filter(|prompt| prompt.enabled).count(), 1);
        assert!(prompts.get("two").expect("second prompt").enabled);
    }

    #[test]
    #[serial]
    fn get_prompts_syncs_live_file_into_existing_enabled_prompt() {
        let _home = TempHome::new();
        let state = test_state();

        state
            .db
            .save_prompt(AppType::Codex.as_str(), &prompt("active", "old", true))
            .expect("seed prompt");
        write_live_prompt(&AppType::Codex, "live content");

        let prompts =
            PromptService::get_prompts(&state, AppType::Codex).expect("get synced prompts");
        let active = prompts.get("active").expect("active prompt");

        assert!(active.enabled);
        assert_eq!(active.content, "live content");
    }

    #[test]
    #[serial]
    fn get_prompts_imports_live_file_when_db_has_no_prompt() {
        let _home = TempHome::new();
        let state = test_state();

        write_live_prompt(&AppType::Claude, "claude live content");

        let prompts =
            PromptService::get_prompts(&state, AppType::Claude).expect("get synced prompts");

        assert_eq!(prompts.len(), 1);
        let prompt = prompts.values().next().expect("imported prompt");
        assert!(prompt.enabled);
        assert_eq!(prompt.content, "claude live content");
    }

    #[test]
    #[serial]
    fn get_prompts_disables_db_when_live_file_is_empty() {
        let _home = TempHome::new();
        let state = test_state();

        state
            .db
            .save_prompt(AppType::Claude.as_str(), &prompt("active", "old", true))
            .expect("seed prompt");
        write_live_prompt(&AppType::Claude, "  \n");

        let prompts =
            PromptService::get_prompts(&state, AppType::Claude).expect("get synced prompts");

        assert!(prompts.values().all(|prompt| !prompt.enabled));
    }

    #[test]
    fn restored_prompt_projection_preserves_the_live_file_when_none_are_enabled() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("AGENTS.md");
        std::fs::write(&path, "local content").expect("seed live prompt file");
        let mut prompts = IndexMap::new();
        prompts.insert("off".to_string(), prompt("off", "managed", false));

        let warning = project_prompt_set_to_path(&prompts, &path).expect("project prompt");
        assert!(warning.is_none());
        // The live file is not part of the sync payload, so a restore with no
        // enabled prompt must not wipe local content it never contained.
        assert_eq!(
            std::fs::read_to_string(path).expect("read prompt"),
            "local content"
        );
    }
}
