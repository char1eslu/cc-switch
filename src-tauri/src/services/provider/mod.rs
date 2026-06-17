//! Provider service module
//!
//! Handles provider CRUD operations, switching, and configuration management.

mod endpoints;
mod live;
mod usage;

use indexmap::IndexMap;
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

use crate::app_config::AppType;
use crate::database::{validate_cost_multiplier, validate_pricing_source};
use crate::error::AppError;
use crate::provider::{Provider, UsageResult};
use crate::services::mcp::McpService;
use crate::settings::CustomEndpoint;
use crate::store::AppState;

// Re-export sub-module functions for external access
pub use live::{
    import_default_config, read_live_settings, should_import_default_config_on_startup,
    sync_current_to_live,
};

// Internal re-exports (pub(crate))
pub(crate) use live::sanitize_claude_settings_for_live;
pub(crate) use live::{
    build_effective_settings_with_common_config, normalize_provider_common_config_for_storage,
    provider_exists_in_live_config, strip_common_config_from_live_settings,
    sync_current_provider_for_app_to_live, write_live_with_common_config,
};

// Internal re-exports
use usage::validate_usage_script;

/// 统一会话开关变更后，立即按新开关状态重写当前官方 Codex 供应商的
/// live 配置，使开关即时生效（无需等下一次切换）。
/// 当前供应商非官方（或不存在）时为 no-op：注入只作用于官方配置，
/// 第三方 live 配置不受开关影响。
pub fn reapply_current_codex_official_live(state: &AppState) -> Result<bool, AppError> {
    let current_id = ProviderService::current(state, AppType::Codex)?;
    if current_id.is_empty() {
        return Ok(false);
    }
    let providers = state.db.get_all_providers(AppType::Codex.as_str())?;
    let Some(provider) = providers.get(&current_id) else {
        return Ok(false);
    };
    if provider.category.as_deref() != Some("official") {
        return Ok(false);
    }

    // 代理接管期间 live 归代理所有（开启代理时官方供应商只警告不拦截，
    // 二者可以共存）。与切换/保存路径一致：以 backup/占位符为所有权信号，
    // 只更新备份，注入后的配置由接管释放时的恢复路径落盘。
    let has_live_backup =
        futures::executor::block_on(state.db.get_live_backup(AppType::Codex.as_str()))
            .ok()
            .flatten()
            .is_some();
    let live_taken_over = state
        .proxy_service
        .detect_takeover_in_live_config_for_app(&AppType::Codex);
    if has_live_backup || live_taken_over {
        futures::executor::block_on(
            state
                .proxy_service
                .update_live_backup_from_provider(AppType::Codex.as_str(), provider),
        )
        .map_err(|e| AppError::Message(format!("更新 Live 备份失败: {e}")))?;
        return Ok(true);
    }

    live::write_live_with_common_config(&state.db, &AppType::Codex, provider)?;
    Ok(true)
}

/// Provider business logic service
pub struct ProviderService;

/// Result of a provider switch operation, including any non-fatal warnings
#[derive(Debug, serde::Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SwitchResult {
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(target_os = "macos", windows))]
    use crate::claude_desktop_config::PROFILE_ID;
    use crate::config::{get_claude_settings_path, read_json_file, write_json_file};
    use crate::database::Database;
    use crate::provider::ProviderMeta;
    #[cfg(any(target_os = "macos", windows))]
    use crate::provider::{ClaudeDesktopMode, ClaudeDesktopModelRoute};
    use crate::proxy::types::ProxyConfig;
    use crate::store::AppState;
    use serde_json::json;
    use serial_test::serial;
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, OnceLock};
    use tempfile::TempDir;

    struct TempHome {
        #[allow(dead_code)]
        dir: TempDir,
        original_home: Option<String>,
        #[cfg(windows)]
        original_local_app_data: Option<String>,
        original_userprofile: Option<String>,
        original_test_home: Option<String>,
    }

    impl TempHome {
        fn new() -> Self {
            let dir = TempDir::new().expect("failed to create temp home");
            let original_home = env::var("HOME").ok();
            #[cfg(windows)]
            let original_local_app_data = env::var("LOCALAPPDATA").ok();
            let original_userprofile = env::var("USERPROFILE").ok();
            let original_test_home = env::var("CC_SWITCH_TEST_HOME").ok();

            env::set_var("HOME", dir.path());
            #[cfg(windows)]
            env::set_var("LOCALAPPDATA", dir.path().join("AppData").join("Local"));
            env::set_var("USERPROFILE", dir.path());
            env::set_var("CC_SWITCH_TEST_HOME", dir.path());

            Self {
                dir,
                original_home,
                #[cfg(windows)]
                original_local_app_data,
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

            #[cfg(windows)]
            {
                match &self.original_local_app_data {
                    Some(value) => env::set_var("LOCALAPPDATA", value),
                    None => env::remove_var("LOCALAPPDATA"),
                }
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

    #[cfg(windows)]
    fn claude_desktop_profile_path(home: &Path) -> PathBuf {
        home.join("AppData")
            .join("Local")
            .join("Claude-3p")
            .join("configLibrary")
            .join(format!("{PROFILE_ID}.json"))
    }

    #[cfg(target_os = "macos")]
    fn claude_desktop_profile_path(home: &Path) -> PathBuf {
        home.join("Library")
            .join("Application Support")
            .join("Claude-3p")
            .join("configLibrary")
            .join(format!("{PROFILE_ID}.json"))
    }

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

    fn with_test_home<T>(test: impl FnOnce(&AppState, &Path) -> T) -> T {
        let _guard = test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        let old_test_home = std::env::var_os("CC_SWITCH_TEST_HOME");
        let old_home = std::env::var_os("HOME");
        std::env::set_var("CC_SWITCH_TEST_HOME", temp.path());
        std::env::set_var("HOME", temp.path());

        let db = Arc::new(Database::memory().expect("in-memory database"));
        let state = AppState::new(db);
        let result = test(&state, temp.path());

        match old_test_home {
            Some(value) => std::env::set_var("CC_SWITCH_TEST_HOME", value),
            None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
        }
        match old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }

        result
    }

    #[test]
    fn validate_provider_settings_rejects_missing_auth() {
        let provider = Provider::with_id(
            "codex".into(),
            "Codex".into(),
            json!({ "config": "base_url = \"https://example.com\"" }),
            None,
        );
        let err = ProviderService::validate_provider_settings(&AppType::Codex, &provider)
            .expect_err("missing auth should be rejected");
        assert!(
            err.to_string().contains("auth"),
            "expected auth error, got {err:?}"
        );
    }

    #[test]
    fn validate_provider_settings_rejects_negative_cost_multiplier() {
        let mut provider = Provider::with_id(
            "claude".into(),
            "Claude".into(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "token",
                    "ANTHROPIC_BASE_URL": "https://claude.example"
                }
            }),
            None,
        );
        provider.meta = Some(ProviderMeta {
            cost_multiplier: Some("-1".to_string()),
            ..ProviderMeta::default()
        });

        let err = ProviderService::validate_provider_settings(&AppType::Claude, &provider)
            .expect_err("negative multiplier should be rejected");
        assert!(matches!(
            err,
            AppError::Localized {
                key: "error.invalidMultiplier",
                ..
            }
        ));
    }

    #[test]
    fn extract_credentials_returns_expected_values() {
        let provider = Provider::with_id(
            "claude".into(),
            "Claude".into(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "token",
                    "ANTHROPIC_BASE_URL": "https://claude.example"
                }
            }),
            None,
        );
        let (api_key, base_url) =
            ProviderService::extract_credentials(&provider, &AppType::Claude).unwrap();
        assert_eq!(api_key, "token");
        assert_eq!(base_url, "https://claude.example");
    }

    #[test]
    fn extract_codex_common_config_preserves_mcp_servers_base_url() {
        let config_toml = r#"model_provider = "azure"
model = "gpt-4"
disable_response_storage = true

[model_providers.azure]
name = "Azure OpenAI"
base_url = "https://azure.example/v1"
wire_api = "responses"

[mcp_servers.my_server]
base_url = "http://localhost:8080"
"#;

        let settings = json!({ "config": config_toml });
        let extracted = ProviderService::extract_codex_common_config(&settings)
            .expect("extract_codex_common_config should succeed");

        assert!(
            !extracted
                .lines()
                .any(|line| line.trim_start().starts_with("model_provider")),
            "should remove top-level model_provider"
        );
        assert!(
            !extracted
                .lines()
                .any(|line| line.trim_start().starts_with("model =")),
            "should remove top-level model"
        );
        assert!(
            !extracted.contains("[model_providers"),
            "should remove entire model_providers table"
        );
        assert!(
            extracted.contains("http://localhost:8080"),
            "should keep mcp_servers.* base_url"
        );
    }

    #[tokio::test]
    #[serial]
    async fn update_current_claude_provider_syncs_live_when_proxy_takeover_detected_without_backup()
    {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let state = AppState::new(db.clone());

        let original = Provider::with_id(
            "p1".into(),
            "Claude A".into(),
            json!({
                "env": {
                    "ANTHROPIC_API_KEY": "token-a",
                    "ANTHROPIC_BASE_URL": "https://api.a.example",
                    "ANTHROPIC_MODEL": "model-a"
                },
                "permissions": { "allow": ["Bash"] }
            }),
            None,
        );
        db.save_provider("claude", &original)
            .expect("save provider");
        db.set_current_provider("claude", "p1")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Claude, Some("p1"))
            .expect("set local current provider");

        db.update_proxy_config(ProxyConfig {
            live_takeover_active: true,
            ..Default::default()
        })
        .await
        .expect("update proxy config");
        {
            let mut config = db
                .get_proxy_config_for_app("claude")
                .await
                .expect("get app proxy config");
            config.enabled = true;
            db.update_proxy_config_for_app(config)
                .await
                .expect("update app proxy config");
        }

        write_json_file(
            &get_claude_settings_path(),
            &json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "http://127.0.0.1:15721",
                    "ANTHROPIC_API_KEY": "PROXY_MANAGED",
                    "ANTHROPIC_MODEL": "stale-model"
                },
                "permissions": { "allow": ["Bash"] }
            }),
        )
        .expect("seed taken-over live file");

        state
            .proxy_service
            .start()
            .await
            .expect("start proxy service");

        let updated = Provider::with_id(
            "p1".into(),
            "Claude A".into(),
            json!({
                "env": {
                    "ANTHROPIC_API_KEY": "token-updated",
                    "ANTHROPIC_BASE_URL": "https://api.updated.example",
                    "ANTHROPIC_MODEL": "model-updated"
                },
                "permissions": { "allow": ["Read"] }
            }),
            None,
        );

        ProviderService::update(&state, AppType::Claude, None, updated.clone())
            .expect("update current provider");

        let backup = db
            .get_live_backup("claude")
            .await
            .expect("get live backup")
            .expect("backup exists");
        let stored_provider = db
            .get_provider_by_id("p1", "claude")
            .expect("get stored provider")
            .expect("stored provider exists");
        let expected_backup =
            serde_json::to_string(&stored_provider.settings_config).expect("serialize");
        assert_eq!(backup.original_config, expected_backup);

        let live: Value = read_json_file(&get_claude_settings_path()).expect("read live");
        assert_eq!(
            live.get("permissions"),
            updated.settings_config.get("permissions"),
            "provider edits should propagate into Claude live config during takeover"
        );
        assert_eq!(
            live.get("env")
                .and_then(|env| env.get("ANTHROPIC_API_KEY"))
                .and_then(|v| v.as_str()),
            Some("PROXY_MANAGED"),
            "takeover placeholder should stay intact"
        );
        assert_eq!(
            live.get("env")
                .and_then(|env| env.get("ANTHROPIC_BASE_URL"))
                .and_then(|v| v.as_str()),
            Some("http://127.0.0.1:15721"),
            "proxy base URL should stay intact"
        );
        assert!(
            live.get("env")
                .and_then(|env| env.get("ANTHROPIC_MODEL"))
                .is_none(),
            "model override should be removed in takeover live config"
        );
    }

    #[cfg(any(target_os = "macos", windows))]
    #[tokio::test]
    #[serial]
    async fn update_current_claude_desktop_provider_syncs_profile_when_proxy_takeover_is_active() {
        let home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let state = AppState::new(db.clone());

        let mut original = Provider::with_id(
            "p1".into(),
            "Desktop A".into(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "token-a",
                    "ANTHROPIC_BASE_URL": "https://provider.example.com/zen/go"
                }
            }),
            None,
        );
        original.meta = Some(ProviderMeta {
            api_format: Some("openai_chat".into()),
            claude_desktop_mode: Some(ClaudeDesktopMode::Proxy),
            claude_desktop_model_routes: std::collections::HashMap::from([(
                "claude-sonnet-4-6".into(),
                ClaudeDesktopModelRoute {
                    model: "deepseek-v4-flash".into(),
                    label_override: Some("DeepSeek V4 Flash".into()),
                    supports_1m: None,
                },
            )]),
            ..Default::default()
        });
        db.save_provider("claude-desktop", &original)
            .expect("save provider");
        db.set_current_provider("claude-desktop", "p1")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::ClaudeDesktop, Some("p1"))
            .expect("set local current provider");

        // Claude Desktop keeps backup state from takeover startup; this sentinel only
        // marks takeover as active so provider updates rewrite the 3P profile.
        db.save_live_backup("claude-desktop", "{}")
            .await
            .expect("seed live backup");
        {
            let mut config = db
                .get_proxy_config_for_app("claude-desktop")
                .await
                .expect("get app proxy config");
            config.enabled = true;
            db.update_proxy_config_for_app(config)
                .await
                .expect("update app proxy config");
        }

        state
            .proxy_service
            .start()
            .await
            .expect("start proxy service");

        let mut updated = Provider::with_id(
            "p1".into(),
            "Desktop A".into(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "token-updated",
                    "ANTHROPIC_BASE_URL": "https://provider.example.com/zen/go"
                }
            }),
            None,
        );
        updated.meta = Some(ProviderMeta {
            api_format: Some("openai_chat".into()),
            claude_desktop_mode: Some(ClaudeDesktopMode::Proxy),
            claude_desktop_model_routes: std::collections::HashMap::from([(
                "claude-sonnet-4-6".into(),
                ClaudeDesktopModelRoute {
                    model: "deepseek-v4-flash".into(),
                    label_override: Some("DeepSeek V4 Flash Updated".into()),
                    supports_1m: Some(true),
                },
            )]),
            ..Default::default()
        });

        ProviderService::update(&state, AppType::ClaudeDesktop, None, updated.clone())
            .expect("update current provider");

        let backup = db
            .get_live_backup("claude-desktop")
            .await
            .expect("get live backup")
            .expect("backup exists");
        assert_eq!(
            backup.original_config, "{}",
            "Claude Desktop provider edits should not rewrite takeover backup"
        );

        let profile_path = claude_desktop_profile_path(home.dir.path());
        let profile: Value = read_json_file(&profile_path).expect("read desktop profile");
        assert_eq!(
            profile["inferenceGatewayBaseUrl"],
            json!("http://127.0.0.1:15721/claude-desktop"),
            "desktop profile should stay pointed at the local gateway during takeover"
        );
        assert_eq!(profile["inferenceGatewayAuthScheme"], json!("bearer"));
        assert_eq!(
            profile["inferenceModels"],
            json!([{ "name": "claude-sonnet-4-6", "labelOverride": "DeepSeek V4 Flash Updated", "supports1m": true }]),
            "provider edits should propagate into the Claude Desktop 3P profile during takeover"
        );
    }

    #[test]
    #[serial]
    fn rename_rejects_missing_original_provider() {
        with_test_home(|state, _| {
            let original = Provider::with_id(
                "claude-a".into(),
                "Claude A".into(),
                json!({
                    "env": {
                        "ANTHROPIC_AUTH_TOKEN": "token",
                        "ANTHROPIC_BASE_URL": "https://claude.example"
                    }
                }),
                None,
            );
            ProviderService::add(state, AppType::Claude, original.clone(), false)
                .expect("seed provider");

            let mut renamed = original.clone();
            renamed.id = "claude-copy".to_string();

            let err =
                ProviderService::update(state, AppType::Claude, Some("missing-provider"), renamed)
                    .expect_err("stale originalId should be rejected");

            assert!(
                err.to_string().contains("Original provider"),
                "expected missing original provider error, got {err:?}"
            );
            assert!(
                state
                    .db
                    .get_provider_by_id("claude-copy", AppType::Claude.as_str())
                    .expect("query renamed provider")
                    .is_none(),
                "rename must not create a new row when originalId is stale"
            );
        });
    }
}

impl ProviderService {
    fn normalize_provider_if_claude(app_type: &AppType, provider: &mut Provider) {
        if matches!(app_type, AppType::Claude) {
            let mut v = provider.settings_config.clone();
            if normalize_claude_models_in_value(&mut v) {
                provider.settings_config = v;
            }
        }
    }

    /// Check whether a provider exists in live config, tolerating parse errors
    /// only for providers that are explicitly marked as DB-only.
    #[allow(dead_code)]
    fn check_live_config_exists(
        app_type: &AppType,
        provider_id: &str,
        live_config_managed: Option<bool>,
    ) -> Result<bool, AppError> {
        if live_config_managed == Some(false) {
            Ok(provider_exists_in_live_config(app_type, provider_id).unwrap_or(false))
        } else {
            provider_exists_in_live_config(app_type, provider_id)
        }
    }

    #[allow(dead_code)]
    fn provider_live_config_managed(provider: &Provider) -> Option<bool> {
        provider
            .meta
            .as_ref()
            .and_then(|meta| meta.live_config_managed)
    }

    #[allow(dead_code)]
    fn set_provider_live_config_managed(provider: &mut Provider, managed: bool) {
        provider
            .meta
            .get_or_insert_with(Default::default)
            .live_config_managed = Some(managed);
    }

    /// List all providers for an app type
    pub fn list(
        state: &AppState,
        app_type: AppType,
    ) -> Result<IndexMap<String, Provider>, AppError> {
        state.db.get_all_providers(app_type.as_str())
    }

    /// Get current provider ID
    ///
    /// 使用有效的当前供应商 ID（验证过存在性）。
    /// 优先从本地 settings 读取，验证后 fallback 到数据库的 is_current 字段。
    /// 这确保了云同步场景下多设备可以独立选择供应商，且返回的 ID 一定有效。
    ///
    pub fn current(state: &AppState, app_type: AppType) -> Result<String, AppError> {
        crate::settings::get_effective_current_provider(&state.db, &app_type)
            .map(|opt| opt.unwrap_or_default())
    }

    /// Add a new provider
    pub fn add(
        state: &AppState,
        app_type: AppType,
        provider: Provider,
        add_to_live: bool,
    ) -> Result<bool, AppError> {
        let mut provider = provider;
        // Normalize Claude model keys
        Self::normalize_provider_if_claude(&app_type, &mut provider);
        Self::validate_provider_settings(&app_type, &provider)?;
        normalize_provider_common_config_for_storage(state.db.as_ref(), &app_type, &mut provider)?;
        let _ = add_to_live;

        // Save to database
        state.db.save_provider(app_type.as_str(), &provider)?;

        // Check if sync is needed (if this is current provider, or no current provider)
        let current = state.db.get_current_provider(app_type.as_str())?;
        if current.is_none() {
            // No current provider, set as current and sync
            state
                .db
                .set_current_provider(app_type.as_str(), &provider.id)?;
            write_live_with_common_config(state.db.as_ref(), &app_type, &provider)?;
        }

        Ok(true)
    }

    /// Update a provider
    pub fn update(
        state: &AppState,
        app_type: AppType,
        original_id: Option<&str>,
        provider: Provider,
    ) -> Result<bool, AppError> {
        let mut provider = provider;
        let original_id = original_id.unwrap_or(provider.id.as_str()).to_string();
        let provider_id_changed = original_id != provider.id;
        // Normalize Claude model keys
        Self::normalize_provider_if_claude(&app_type, &mut provider);
        Self::validate_provider_settings(&app_type, &provider)?;
        normalize_provider_common_config_for_storage(state.db.as_ref(), &app_type, &mut provider)?;

        if provider_id_changed {
            return Err(AppError::Message(
                "Provider key cannot be changed for this app".to_string(),
            ));
        }

        // Save to database
        state.db.save_provider(app_type.as_str(), &provider)?;

        // For other apps: Check if this is current provider (use effective current, not just DB)
        let effective_current =
            crate::settings::get_effective_current_provider(&state.db, &app_type)?;
        let is_current = effective_current.as_deref() == Some(provider.id.as_str());

        if is_current {
            // 如果 Claude 代理接管处于激活状态，并且代理服务正在运行：
            // - 不直接走普通 Live 写入逻辑
            // - 改为更新 Live 备份，并在 Claude 下同步代理安全的 Live 配置
            let has_live_backup =
                futures::executor::block_on(state.db.get_live_backup(app_type.as_str()))
                    .ok()
                    .flatten()
                    .is_some();
            let live_taken_over = state
                .proxy_service
                .detect_takeover_in_live_config_for_app(&app_type);
            // Backup or live placeholders mean the live file is currently owned
            // by proxy takeover, including the short activation window before
            // proxy_config.enabled is committed.
            let should_sync_via_proxy = has_live_backup || live_taken_over;

            if should_sync_via_proxy {
                if matches!(app_type, AppType::ClaudeDesktop) {
                    write_live_with_common_config(state.db.as_ref(), &app_type, &provider)?;
                } else {
                    futures::executor::block_on(
                        state
                            .proxy_service
                            .update_live_backup_from_provider(app_type.as_str(), &provider),
                    )
                    .map_err(|e| AppError::Message(format!("更新 Live 备份失败: {e}")))?;
                }

                if matches!(app_type, AppType::Claude)
                    && futures::executor::block_on(state.proxy_service.is_running())
                {
                    futures::executor::block_on(
                        state
                            .proxy_service
                            .sync_claude_live_from_provider_while_proxy_active(&provider),
                    )
                    .map_err(|e| AppError::Message(format!("同步 Claude Live 配置失败: {e}")))?;
                }
            } else {
                write_live_with_common_config(state.db.as_ref(), &app_type, &provider)?;
                // Sync MCP
                McpService::sync_all_enabled(state)?;
            }
        }

        Ok(true)
    }

    /// Delete a provider
    ///
    /// 同时检查本地 settings 和数据库的当前供应商，防止删除任一端正在使用的供应商。
    pub fn delete(state: &AppState, app_type: AppType, id: &str) -> Result<(), AppError> {
        // Check both local settings and database
        let local_current = crate::settings::get_current_provider(&app_type);
        let db_current = state.db.get_current_provider(app_type.as_str())?;

        if local_current.as_deref() == Some(id) || db_current.as_deref() == Some(id) {
            return Err(AppError::Message(
                "无法删除当前正在使用的供应商".to_string(),
            ));
        }

        state.db.delete_provider(app_type.as_str(), id)
    }

    pub fn remove_from_live_config(
        _state: &AppState,
        app_type: AppType,
        id: &str,
    ) -> Result<(), AppError> {
        let _ = id;
        Err(AppError::Message(format!(
            "App {} does not support remove from live config",
            app_type.as_str()
        )))
    }

    /// Switch to a provider
    ///
    /// Switch flow:
    /// 1. Validate target provider exists
    /// 2. Check if proxy takeover mode is active AND proxy server is running
    /// 3. If takeover mode active: hot-switch proxy target and refresh proxy-safe Live labels
    /// 4. If normal mode:
    ///    a. **Backfill mechanism**: Backfill current live config to current provider
    ///    b. Update local settings current_provider_xxx (device-level)
    ///    c. Update database is_current (as default for new devices)
    ///    d. Write target provider config to live files
    ///    e. Sync MCP configuration
    pub fn switch(state: &AppState, app_type: AppType, id: &str) -> Result<SwitchResult, AppError> {
        // Check if provider exists
        let providers = state.db.get_all_providers(app_type.as_str())?;
        let _provider = providers
            .get(id)
            .ok_or_else(|| AppError::Message(format!("供应商 {id} 不存在")))?;

        if matches!(app_type, AppType::ClaudeDesktop) {
            return Self::switch_normal(state, app_type, id, &providers);
        }

        // Provider switches and takeover toggles both mutate live config and the
        // restore backup. Serialize them per app, then decide from the locked
        // current state so a just-started takeover cannot be overwritten by a
        // normal live write.
        let _switch_guard = if matches!(app_type, AppType::Claude | AppType::Codex) {
            Some(futures::executor::block_on(
                state.proxy_service.lock_switch_for_app(app_type.as_str()),
            ))
        } else {
            None
        };

        // Backup or live placeholders mean the live file is owned by proxy
        // takeover, even if the proxy server is temporarily stopped or is in the
        // activation window before enabled=true is committed.
        let is_app_taken_over =
            futures::executor::block_on(state.db.get_live_backup(app_type.as_str()))
                .ok()
                .flatten()
                .is_some();
        let live_taken_over = state
            .proxy_service
            .detect_takeover_in_live_config_for_app(&app_type);

        let should_hot_switch = is_app_taken_over || live_taken_over;

        // Block switching to official providers when proxy takeover is active.
        // Using a proxy with official APIs (Anthropic/OpenAI/Google) may cause account bans.
        if should_hot_switch && _provider.category.as_deref() == Some("official") {
            return Err(AppError::localized(
                "switch.official_blocked_by_proxy",
                "代理接管模式下不能切换到官方供应商，使用代理访问官方 API 可能导致账号被封禁。请先关闭代理接管，或选择第三方供应商。",
                "Cannot switch to official provider while proxy takeover is active. Using proxy with official APIs may cause account bans.",
            ));
        }

        if should_hot_switch {
            // Proxy takeover mode: hot-switch without restoring upstream Live config.
            // The proxy layer may still refresh proxy-safe Live fields so client labels
            // follow the selected provider while endpoints remain local.
            log::info!(
                "代理接管模式：热切换 {} 的目标供应商为 {}",
                app_type.as_str(),
                id
            );

            futures::executor::block_on(
                state
                    .proxy_service
                    .hot_switch_provider_inner(app_type.as_str(), id),
            )
            .map_err(|e| AppError::Message(format!("热切换失败: {e}")))?;

            // The proxy server will route requests to the new provider via is_current.
            // MCP sync is intentionally skipped while Live config is owned by takeover.
            return Ok(SwitchResult::default());
        }

        // Normal mode: full switch with Live config write
        Self::switch_normal(state, app_type, id, &providers)
    }

    /// Normal switch flow (non-proxy mode)
    fn switch_normal(
        state: &AppState,
        app_type: AppType,
        id: &str,
        providers: &indexmap::IndexMap<String, Provider>,
    ) -> Result<SwitchResult, AppError> {
        let provider = providers
            .get(id)
            .ok_or_else(|| AppError::Message(format!("供应商 {id} 不存在")))?;

        let mut result = SwitchResult::default();

        // Backfill: Backfill current live config to current provider
        // Use effective current provider (validated existence) to ensure backfill targets valid provider
        let current_id = crate::settings::get_effective_current_provider(&state.db, &app_type)?;

        if let Some(current_id) = current_id {
            if current_id != id {
                // Only backfill when switching to a different provider
                if let Ok(live_config) = read_live_settings(app_type.clone()) {
                    if let Some(mut current_provider) = providers.get(&current_id).cloned() {
                        current_provider.settings_config = strip_common_config_from_live_settings(
                            state.db.as_ref(),
                            &app_type,
                            &current_provider,
                            live_config,
                        );
                        if let Err(e) = state.db.save_provider(app_type.as_str(), &current_provider)
                        {
                            log::warn!("Backfill failed: {e}");
                            result
                                .warnings
                                .push(format!("backfill_failed:{current_id}"));
                        }
                    }
                }
            }
        }

        crate::settings::set_current_provider(&app_type, Some(id))?;
        state.db.set_current_provider(app_type.as_str(), id)?;

        // Sync to live.
        write_live_with_common_config(state.db.as_ref(), &app_type, provider)?;

        // Sync MCP
        McpService::sync_all_enabled(state)?;

        Ok(result)
    }

    /// Sync current provider to live configuration (re-export)
    pub fn sync_current_to_live(state: &AppState) -> Result<(), AppError> {
        sync_current_to_live(state)
    }

    pub fn sync_current_provider_for_app(
        state: &AppState,
        app_type: AppType,
    ) -> Result<(), AppError> {
        if app_type.is_additive_mode() {
            return sync_current_provider_for_app_to_live(state, &app_type);
        }

        let current_id =
            match crate::settings::get_effective_current_provider(&state.db, &app_type)? {
                Some(id) => id,
                None => return Ok(()),
            };

        let providers = state.db.get_all_providers(app_type.as_str())?;
        let Some(provider) = providers.get(&current_id) else {
            return Ok(());
        };

        let has_live_backup =
            futures::executor::block_on(state.db.get_live_backup(app_type.as_str()))
                .ok()
                .flatten()
                .is_some();

        let live_taken_over = state
            .proxy_service
            .detect_takeover_in_live_config_for_app(&app_type);

        // See the save path above: backup/placeholders are the ownership signal
        // here, not just proxy_config.enabled.
        if has_live_backup || live_taken_over {
            if matches!(app_type, AppType::ClaudeDesktop) {
                write_live_with_common_config(state.db.as_ref(), &app_type, provider)?;
                return Ok(());
            }

            futures::executor::block_on(
                state
                    .proxy_service
                    .update_live_backup_from_provider(app_type.as_str(), provider),
            )
            .map_err(|e| AppError::Message(format!("更新 Live 备份失败: {e}")))?;
            return Ok(());
        }

        sync_current_provider_for_app_to_live(state, &app_type)
    }

    pub fn migrate_legacy_common_config_usage(
        state: &AppState,
        app_type: AppType,
        legacy_snippet: &str,
    ) -> Result<(), AppError> {
        if app_type.is_additive_mode() || legacy_snippet.trim().is_empty() {
            return Ok(());
        }

        let providers = state.db.get_all_providers(app_type.as_str())?;

        for provider in providers.values() {
            if provider
                .meta
                .as_ref()
                .and_then(|meta| meta.common_config_enabled)
                .is_some()
            {
                continue;
            }

            if !live::provider_uses_common_config(&app_type, provider, Some(legacy_snippet)) {
                continue;
            }

            let mut updated_provider = provider.clone();
            updated_provider
                .meta
                .get_or_insert_with(Default::default)
                .common_config_enabled = Some(true);

            match live::remove_common_config_from_settings(
                &app_type,
                &updated_provider.settings_config,
                legacy_snippet,
            ) {
                Ok(settings) => updated_provider.settings_config = settings,
                Err(err) => {
                    log::warn!(
                        "Failed to normalize legacy common config for {} provider '{}': {err}",
                        app_type.as_str(),
                        updated_provider.id
                    );
                }
            }

            state
                .db
                .save_provider(app_type.as_str(), &updated_provider)?;
        }

        Ok(())
    }

    pub fn migrate_legacy_common_config_usage_if_needed(
        state: &AppState,
        app_type: AppType,
    ) -> Result<(), AppError> {
        if app_type.is_additive_mode() {
            return Ok(());
        }

        let Some(snippet) = state.db.get_config_snippet(app_type.as_str())? else {
            return Ok(());
        };

        if snippet.trim().is_empty() {
            return Ok(());
        }

        Self::migrate_legacy_common_config_usage(state, app_type, &snippet)
    }

    /// Extract common config snippet from current provider
    ///
    /// Extracts the current provider's configuration and removes provider-specific fields
    /// (API keys, model settings, endpoints) to create a reusable common config snippet.
    pub fn extract_common_config_snippet(
        state: &AppState,
        app_type: AppType,
    ) -> Result<String, AppError> {
        // Get current provider
        let current_id = Self::current(state, app_type.clone())?;
        if current_id.is_empty() {
            return Err(AppError::Message("No current provider".to_string()));
        }

        let providers = state.db.get_all_providers(app_type.as_str())?;
        let provider = providers
            .get(&current_id)
            .ok_or_else(|| AppError::Message(format!("Provider {current_id} not found")))?;

        match app_type {
            AppType::Claude => Self::extract_claude_common_config(&provider.settings_config),
            AppType::ClaudeDesktop => Ok(String::new()),
            AppType::Codex => Self::extract_codex_common_config(&provider.settings_config),
        }
    }

    /// Extract common config snippet from a config value (e.g. editor content).
    pub fn extract_common_config_snippet_from_settings(
        app_type: AppType,
        settings_config: &Value,
    ) -> Result<String, AppError> {
        match app_type {
            AppType::Claude => Self::extract_claude_common_config(settings_config),
            AppType::ClaudeDesktop => Ok(String::new()),
            AppType::Codex => Self::extract_codex_common_config(settings_config),
        }
    }

    /// Extract common config for Claude (JSON format)
    fn extract_claude_common_config(settings: &Value) -> Result<String, AppError> {
        let mut config = settings.clone();

        // Fields to exclude from common config
        const ENV_EXCLUDES: &[&str] = &[
            // Auth
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            // Models and Claude Code model-menu display names
            "ANTHROPIC_MODEL",
            "ANTHROPIC_REASONING_MODEL", // legacy: 已废弃，但旧配置可能残留
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
            // Endpoint
            "ANTHROPIC_BASE_URL",
        ];

        const TOP_LEVEL_EXCLUDES: &[&str] = &[
            "apiBaseUrl",
            // Legacy model fields
            "primaryModel",
            "smallFastModel",
        ];

        // Remove env fields
        if let Some(env) = config.get_mut("env").and_then(|v| v.as_object_mut()) {
            for key in ENV_EXCLUDES {
                env.remove(*key);
            }
            // If env is empty after removal, remove the env object itself
            if env.is_empty() {
                config.as_object_mut().map(|obj| obj.remove("env"));
            }
        }

        // Remove top-level fields
        if let Some(obj) = config.as_object_mut() {
            for key in TOP_LEVEL_EXCLUDES {
                obj.remove(*key);
            }
        }

        // Check if result is empty
        if config.as_object().is_none_or(|obj| obj.is_empty()) {
            return Ok("{}".to_string());
        }

        serde_json::to_string_pretty(&config)
            .map_err(|e| AppError::Message(format!("Serialization failed: {e}")))
    }

    /// Extract common config for Codex (TOML format)
    fn extract_codex_common_config(settings: &Value) -> Result<String, AppError> {
        // Codex config is stored as { "auth": {...}, "config": "toml string" }
        let config_toml = settings
            .get("config")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if config_toml.is_empty() {
            return Ok(String::new());
        }

        let mut doc = config_toml
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| AppError::Message(format!("TOML parse error: {e}")))?;

        // Remove provider-specific fields.
        let root = doc.as_table_mut();
        root.remove("model");
        root.remove("model_provider");
        // Legacy/alt formats might use a top-level base_url.
        root.remove("base_url");

        // Remove entire model_providers table (provider-specific configuration)
        root.remove("model_providers");

        // Clean up multiple empty lines (keep at most one blank line).
        let mut cleaned = String::new();
        let mut blank_run = 0usize;
        for line in doc.to_string().lines() {
            if line.trim().is_empty() {
                blank_run += 1;
                if blank_run <= 1 {
                    cleaned.push('\n');
                }
                continue;
            }
            blank_run = 0;
            cleaned.push_str(line);
            cleaned.push('\n');
        }

        Ok(cleaned.trim().to_string())
    }

    /// Import default configuration from live files (re-export)
    ///
    /// Returns `Ok(true)` if imported, `Ok(false)` if skipped.
    pub fn import_default_config(state: &AppState, app_type: AppType) -> Result<bool, AppError> {
        import_default_config(state, app_type)
    }

    pub fn should_import_default_config_on_startup(
        state: &AppState,
        app_type: &AppType,
    ) -> Result<bool, AppError> {
        should_import_default_config_on_startup(state, app_type)
    }

    /// Read current live settings (re-export)
    pub fn read_live_settings(app_type: AppType) -> Result<Value, AppError> {
        read_live_settings(app_type)
    }

    /// Get custom endpoints list (re-export)
    pub fn get_custom_endpoints(
        state: &AppState,
        app_type: AppType,
        provider_id: &str,
    ) -> Result<Vec<CustomEndpoint>, AppError> {
        endpoints::get_custom_endpoints(state, app_type, provider_id)
    }

    /// Add custom endpoint (re-export)
    pub fn add_custom_endpoint(
        state: &AppState,
        app_type: AppType,
        provider_id: &str,
        url: String,
    ) -> Result<(), AppError> {
        endpoints::add_custom_endpoint(state, app_type, provider_id, url)
    }

    /// Remove custom endpoint (re-export)
    pub fn remove_custom_endpoint(
        state: &AppState,
        app_type: AppType,
        provider_id: &str,
        url: String,
    ) -> Result<(), AppError> {
        endpoints::remove_custom_endpoint(state, app_type, provider_id, url)
    }

    /// Update endpoint last used timestamp (re-export)
    pub fn update_endpoint_last_used(
        state: &AppState,
        app_type: AppType,
        provider_id: &str,
        url: String,
    ) -> Result<(), AppError> {
        endpoints::update_endpoint_last_used(state, app_type, provider_id, url)
    }

    /// Update provider sort order
    pub fn update_sort_order(
        state: &AppState,
        app_type: AppType,
        updates: Vec<ProviderSortUpdate>,
    ) -> Result<bool, AppError> {
        let mut providers = state.db.get_all_providers(app_type.as_str())?;

        for update in updates {
            if let Some(provider) = providers.get_mut(&update.id) {
                provider.sort_index = Some(update.sort_index);
                state.db.save_provider(app_type.as_str(), provider)?;
            }
        }

        Ok(true)
    }

    /// Query provider usage (re-export)
    pub async fn query_usage(
        state: &AppState,
        app_type: AppType,
        provider_id: &str,
    ) -> Result<UsageResult, AppError> {
        usage::query_usage(state, app_type, provider_id).await
    }

    /// Test usage script (re-export)
    #[allow(clippy::too_many_arguments)]
    pub async fn test_usage_script(
        state: &AppState,
        app_type: AppType,
        provider_id: &str,
        script_code: &str,
        timeout: u64,
        api_key: Option<&str>,
        base_url: Option<&str>,
        access_token: Option<&str>,
        user_id: Option<&str>,
        template_type: Option<&str>,
    ) -> Result<UsageResult, AppError> {
        usage::test_usage_script(
            state,
            app_type,
            provider_id,
            script_code,
            timeout,
            api_key,
            base_url,
            access_token,
            user_id,
            template_type,
        )
        .await
    }

    fn validate_provider_settings(app_type: &AppType, provider: &Provider) -> Result<(), AppError> {
        match app_type {
            AppType::Claude => {
                if !provider.settings_config.is_object() {
                    return Err(AppError::localized(
                        "provider.claude.settings.not_object",
                        "Claude 配置必须是 JSON 对象",
                        "Claude configuration must be a JSON object",
                    ));
                }
            }
            AppType::ClaudeDesktop => {
                crate::claude_desktop_config::validate_provider(provider)?;
            }
            AppType::Codex => {
                let settings = provider.settings_config.as_object().ok_or_else(|| {
                    AppError::localized(
                        "provider.codex.settings.not_object",
                        "Codex 配置必须是 JSON 对象",
                        "Codex configuration must be a JSON object",
                    )
                })?;

                let auth = settings.get("auth").ok_or_else(|| {
                    AppError::localized(
                        "provider.codex.auth.missing",
                        format!("供应商 {} 缺少 auth 配置", provider.id),
                        format!("Provider {} is missing auth configuration", provider.id),
                    )
                })?;
                if !auth.is_object() {
                    return Err(AppError::localized(
                        "provider.codex.auth.not_object",
                        format!("供应商 {} 的 auth 配置必须是 JSON 对象", provider.id),
                        format!(
                            "Provider {} auth configuration must be a JSON object",
                            provider.id
                        ),
                    ));
                }

                if let Some(config_value) = settings.get("config") {
                    if !(config_value.is_string() || config_value.is_null()) {
                        return Err(AppError::localized(
                            "provider.codex.config.invalid_type",
                            "Codex config 字段必须是字符串",
                            "Codex config field must be a string",
                        ));
                    }
                    if let Some(cfg_text) = config_value.as_str() {
                        crate::codex_config::validate_config_toml(cfg_text)?;
                    }
                }
            }
        }

        // Validate and clean UsageScript configuration (common for all app types)
        if let Some(meta) = &provider.meta {
            if let Some(multiplier) = meta.cost_multiplier.as_deref() {
                validate_cost_multiplier(multiplier)?;
            }
            if let Some(source) = meta.pricing_model_source.as_deref() {
                validate_pricing_source(source)?;
            }
            if let Some(usage_script) = &meta.usage_script {
                validate_usage_script(usage_script)?;
            }
        }

        Ok(())
    }

    #[allow(dead_code)]
    fn extract_credentials(
        provider: &Provider,
        app_type: &AppType,
    ) -> Result<(String, String), AppError> {
        match app_type {
            AppType::Claude => {
                let env = provider
                    .settings_config
                    .get("env")
                    .and_then(|v| v.as_object())
                    .ok_or_else(|| {
                        AppError::localized(
                            "provider.claude.env.missing",
                            "配置格式错误: 缺少 env",
                            "Invalid configuration: missing env section",
                        )
                    })?;

                let api_key = env
                    .get("ANTHROPIC_AUTH_TOKEN")
                    .or_else(|| env.get("ANTHROPIC_API_KEY"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AppError::localized(
                            "provider.claude.api_key.missing",
                            "缺少 API Key",
                            "API key is missing",
                        )
                    })?
                    .to_string();

                let base_url = env
                    .get("ANTHROPIC_BASE_URL")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AppError::localized(
                            "provider.claude.base_url.missing",
                            "缺少 ANTHROPIC_BASE_URL 配置",
                            "Missing ANTHROPIC_BASE_URL configuration",
                        )
                    })?
                    .to_string();

                Ok((api_key, base_url))
            }
            AppType::ClaudeDesktop => {
                let credentials =
                    crate::claude_desktop_config::direct_gateway_credentials(provider)?;
                Ok((credentials.api_key, credentials.base_url))
            }
            AppType::Codex => {
                let _auth = provider
                    .settings_config
                    .get("auth")
                    .and_then(|v| v.as_object())
                    .ok_or_else(|| {
                        AppError::localized(
                            "provider.codex.auth.missing",
                            "配置格式错误: 缺少 auth",
                            "Invalid configuration: missing auth section",
                        )
                    })?;

                let config_toml = provider
                    .settings_config
                    .get("config")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let api_key = crate::codex_config::extract_codex_api_key(
                    provider.settings_config.get("auth"),
                    Some(config_toml),
                )
                .ok_or_else(|| {
                    AppError::localized(
                        "provider.codex.api_key.missing",
                        "缺少 API Key",
                        "API key is missing",
                    )
                })?;

                let base_url = if config_toml.contains("base_url") {
                    let re = Regex::new(r#"base_url\s*=\s*["']([^"']+)["']"#).map_err(|e| {
                        AppError::localized(
                            "provider.regex_init_failed",
                            format!("正则初始化失败: {e}"),
                            format!("Failed to initialize regex: {e}"),
                        )
                    })?;
                    re.captures(config_toml)
                        .and_then(|caps| caps.get(1))
                        .map(|m| m.as_str().to_string())
                        .ok_or_else(|| {
                            AppError::localized(
                                "provider.codex.base_url.invalid",
                                "config.toml 中 base_url 格式错误",
                                "base_url in config.toml has invalid format",
                            )
                        })?
                } else {
                    return Err(AppError::localized(
                        "provider.codex.base_url.missing",
                        "config.toml 中缺少 base_url 配置",
                        "base_url is missing from config.toml",
                    ));
                };

                Ok((api_key, base_url))
            }
        }
    }
}

/// Normalize Claude model keys in a JSON value
///
/// Reads old key (ANTHROPIC_SMALL_FAST_MODEL), writes new keys (DEFAULT_*), and deletes old key.
pub(crate) fn normalize_claude_models_in_value(settings: &mut Value) -> bool {
    let mut changed = false;
    let env = match settings.get_mut("env").and_then(|v| v.as_object_mut()) {
        Some(obj) => obj,
        None => return changed,
    };

    let model = env
        .get("ANTHROPIC_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let small_fast = env
        .get("ANTHROPIC_SMALL_FAST_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let current_haiku = env
        .get("ANTHROPIC_DEFAULT_HAIKU_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let current_sonnet = env
        .get("ANTHROPIC_DEFAULT_SONNET_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let current_opus = env
        .get("ANTHROPIC_DEFAULT_OPUS_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let target_haiku = current_haiku
        .or_else(|| small_fast.clone())
        .or_else(|| model.clone());
    let target_sonnet = current_sonnet
        .or_else(|| model.clone())
        .or_else(|| small_fast.clone());
    let target_opus = current_opus
        .or_else(|| model.clone())
        .or_else(|| small_fast.clone());

    if env.get("ANTHROPIC_DEFAULT_HAIKU_MODEL").is_none() {
        if let Some(v) = target_haiku {
            env.insert(
                "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
                Value::String(v),
            );
            changed = true;
        }
    }
    if env.get("ANTHROPIC_DEFAULT_SONNET_MODEL").is_none() {
        if let Some(v) = target_sonnet {
            env.insert(
                "ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(),
                Value::String(v),
            );
            changed = true;
        }
    }
    if env.get("ANTHROPIC_DEFAULT_OPUS_MODEL").is_none() {
        if let Some(v) = target_opus {
            env.insert("ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(), Value::String(v));
            changed = true;
        }
    }

    if env.remove("ANTHROPIC_SMALL_FAST_MODEL").is_some() {
        changed = true;
    }

    changed
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderSortUpdate {
    pub id: String,
    #[serde(rename = "sortIndex")]
    pub sort_index: usize,
}

// ============================================================================
// 统一供应商（Universal Provider）服务方法
// ============================================================================

use crate::provider::UniversalProvider;
use std::collections::HashMap;

impl ProviderService {
    /// 获取所有统一供应商
    pub fn list_universal(
        state: &AppState,
    ) -> Result<HashMap<String, UniversalProvider>, AppError> {
        state.db.get_all_universal_providers()
    }

    /// 获取单个统一供应商
    pub fn get_universal(
        state: &AppState,
        id: &str,
    ) -> Result<Option<UniversalProvider>, AppError> {
        state.db.get_universal_provider(id)
    }

    /// 添加或更新统一供应商（不自动同步，需手动调用 sync_universal_to_apps）
    pub fn upsert_universal(
        state: &AppState,
        provider: UniversalProvider,
    ) -> Result<bool, AppError> {
        // 保存统一供应商
        state.db.save_universal_provider(&provider)?;

        Ok(true)
    }

    /// 删除统一供应商
    pub fn delete_universal(state: &AppState, id: &str) -> Result<bool, AppError> {
        // 获取统一供应商（用于删除生成的子供应商）
        let provider = state.db.get_universal_provider(id)?;

        // 删除统一供应商
        state.db.delete_universal_provider(id)?;

        // 删除生成的子供应商
        if let Some(p) = provider {
            if p.apps.claude {
                let claude_id = format!("universal-claude-{id}");
                let _ = state.db.delete_provider("claude", &claude_id);
            }
            if p.apps.codex {
                let codex_id = format!("universal-codex-{id}");
                let _ = state.db.delete_provider("codex", &codex_id);
            }
        }

        Ok(true)
    }

    /// 同步统一供应商到各应用
    pub fn sync_universal_to_apps(state: &AppState, id: &str) -> Result<bool, AppError> {
        let provider = state
            .db
            .get_universal_provider(id)?
            .ok_or_else(|| AppError::Message(format!("统一供应商 {id} 不存在")))?;

        // 同步到 Claude
        if let Some(mut claude_provider) = provider.to_claude_provider() {
            // 合并已有配置
            if let Some(existing) = state.db.get_provider_by_id(&claude_provider.id, "claude")? {
                let mut merged = existing.settings_config.clone();
                Self::merge_json(&mut merged, &claude_provider.settings_config);
                claude_provider.settings_config = merged;
            }
            state.db.save_provider("claude", &claude_provider)?;
        } else {
            // 如果禁用了 Claude，删除对应的子供应商
            let claude_id = format!("universal-claude-{id}");
            let _ = state.db.delete_provider("claude", &claude_id);
        }

        // 同步到 Codex
        if let Some(mut codex_provider) = provider.to_codex_provider() {
            // 合并已有配置
            if let Some(existing) = state.db.get_provider_by_id(&codex_provider.id, "codex")? {
                let mut merged = existing.settings_config.clone();
                Self::merge_json(&mut merged, &codex_provider.settings_config);
                codex_provider.settings_config = merged;
            }
            state.db.save_provider("codex", &codex_provider)?;
        } else {
            let codex_id = format!("universal-codex-{id}");
            let _ = state.db.delete_provider("codex", &codex_id);
        }

        Ok(true)
    }

    /// 递归合并 JSON：base 为底，patch 覆盖同名字段
    fn merge_json(base: &mut serde_json::Value, patch: &serde_json::Value) {
        use serde_json::Value;

        match (base, patch) {
            (Value::Object(base_map), Value::Object(patch_map)) => {
                for (k, v_patch) in patch_map {
                    match base_map.get_mut(k) {
                        Some(v_base) => Self::merge_json(v_base, v_patch),
                        None => {
                            base_map.insert(k.clone(), v_patch.clone());
                        }
                    }
                }
            }
            // 其它类型：直接覆盖
            (base_val, patch_val) => {
                *base_val = patch_val.clone();
            }
        }
    }
}
