//! Provider service module
//!
//! Handles provider CRUD operations, switching, and configuration management.

pub(crate) mod claude_direct;
mod claude_editor;
pub(crate) mod codex_direct;
mod codex_editor;
mod codex_login;
mod editor_toml;
mod endpoints;
mod live;
mod usage;

use indexmap::IndexMap;
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

use crate::app_config::AppType;
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

pub use claude_editor::{EditorSave, EditorView};

// Internal re-exports (pub(crate))
pub(crate) use live::{provider_exists_in_live_config, write_live_for_state, LiveSyncOutcome};

use usage::validate_usage_script;

/// Codex official providers are safe to select during takeover: Codex keeps
/// ownership of the active ChatGPT login and the proxy only forwards the
/// authenticated request. Other apps' official providers retain the block.
pub fn official_provider_supports_proxy_takeover(app_type: &AppType, provider: &Provider) -> bool {
    matches!(app_type, AppType::Codex)
        && crate::proxy::providers::is_codex_official_provider(provider)
}

/// 统一会话开关变更后，立即按新开关状态重写当前官方 Codex 供应商的选路（关键字段），
/// 使开关即时生效，无需等下一次切换。当前供应商非官方（或不存在）时为 no-op：开关只
/// 影响官方直连的选路。代理模式下 live 是代理契约，不受这个开关影响。
pub fn reapply_current_codex_official_live(state: &AppState) -> Result<bool, AppError> {
    let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &AppType::Codex)?;
    let current_id = ProviderService::current(state, AppType::Codex)?;
    if current_id.is_empty() {
        return Ok(false);
    }
    let providers = state.db.get_all_providers(AppType::Codex.as_str())?;
    let Some(provider) = providers.get(&current_id) else {
        return Ok(false);
    };
    if !codex_direct::is_official(provider) {
        return Ok(false);
    }
    live::sync_live_for_provider_respecting_mode(state, &AppType::Codex, provider, None)?;
    Ok(true)
}

/// 新版不再读通用配置片段，但旧设备经云同步拿到新建的行时仍按这个标记合并片段；不写的
/// 话，旧版切到它会把 hooks、MCP 等共享设置整份抹掉。
fn keep_common_config_for_old_versions(provider: &mut Provider) {
    provider
        .meta
        .get_or_insert_with(Default::default)
        .common_config_enabled = Some(true);
}

/// 编辑器保存的是新增的供应商，还是已有的。
#[derive(Clone, Copy, PartialEq, Eq)]
enum EditorSaveKind {
    Add,
    Update,
}

impl EditorSaveKind {
    /// 这次保存要不要把关键字段也换进 live：直连模式下编辑的是当前供应商，或者新增的是
    /// 第一个供应商。代理模式下 live 的关键字段是代理契约，只写全局改动。
    fn writes_key_fields(
        self,
        state: &AppState,
        app_type: &AppType,
        mode: &crate::mode::state::ModeState,
        id: &str,
    ) -> Result<bool, AppError> {
        if mode.is_proxy() {
            return Ok(false);
        }
        let direct = crate::mode::current::provider_for(
            &state.db,
            app_type,
            crate::mode::current::Purpose::Direct,
        )?;
        Ok(match self {
            Self::Add => direct.is_none(),
            Self::Update => direct.as_deref() == Some(id),
        })
    }
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
    #[cfg(any(target_os = "macos", windows, target_os = "linux"))]
    use crate::claude_desktop_config::PROFILE_ID;
    use crate::config::{get_claude_settings_path, read_json_file, write_json_file};
    use crate::database::Database;
    use crate::provider::{
        AuthBinding, AuthBindingSource, ClaudeModelConfig, ProviderMeta, UniversalProvider,
        UsageScript,
    };
    #[cfg(any(target_os = "macos", windows, target_os = "linux"))]
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
        #[cfg(target_os = "linux")]
        original_xdg_config_home: Option<std::ffi::OsString>,
    }

    impl TempHome {
        fn new() -> Self {
            let dir = TempDir::new().expect("failed to create temp home");
            let original_home = env::var("HOME").ok();
            #[cfg(windows)]
            let original_local_app_data = env::var("LOCALAPPDATA").ok();
            let original_userprofile = env::var("USERPROFILE").ok();
            let original_test_home = env::var("CC_SWITCH_TEST_HOME").ok();
            #[cfg(target_os = "linux")]
            let original_xdg_config_home = env::var_os("XDG_CONFIG_HOME");

            env::set_var("HOME", dir.path());
            #[cfg(windows)]
            env::set_var("LOCALAPPDATA", dir.path().join("AppData").join("Local"));
            env::set_var("USERPROFILE", dir.path());
            env::set_var("CC_SWITCH_TEST_HOME", dir.path());
            // Claude Desktop Linux paths follow XDG_CONFIG_HOME; pin them under the temp home.
            #[cfg(target_os = "linux")]
            env::remove_var("XDG_CONFIG_HOME");

            Self {
                dir,
                original_home,
                #[cfg(windows)]
                original_local_app_data,
                original_userprofile,
                original_test_home,
                #[cfg(target_os = "linux")]
                original_xdg_config_home,
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

            #[cfg(target_os = "linux")]
            {
                match &self.original_xdg_config_home {
                    Some(value) => env::set_var("XDG_CONFIG_HOME", value),
                    None => env::remove_var("XDG_CONFIG_HOME"),
                }
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

    #[cfg(target_os = "linux")]
    fn claude_desktop_profile_path(home: &Path) -> PathBuf {
        home.join(".config")
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

    fn codex_settings(base_url: &str, api_key: &str) -> Value {
        json!({
            "auth": {
                "OPENAI_API_KEY": api_key
            },
            "config": format!(
                "model_provider = \"custom\"\n\
                 [model_providers.custom]\n\
                 name = \"custom\"\n\
                 base_url = \"{base_url}\"\n\
                 wire_api = \"chat\"\n"
            )
        })
    }

    fn usage_script_with_credentials(
        api_key: Option<&str>,
        base_url: Option<&str>,
        template_type: Option<&str>,
    ) -> UsageScript {
        UsageScript {
            enabled: true,
            language: "javascript".to_string(),
            code: "return { remaining: 1, unit: 'USD' };".to_string(),
            timeout: Some(10),
            api_key: api_key.map(str::to_string),
            base_url: base_url.map(str::to_string),
            access_token: None,
            user_id: None,
            template_type: template_type.map(str::to_string),
            auto_query_interval: None,
            coding_plan_provider: None,
            access_key_id: Some("ak-test".to_string()),
            secret_access_key: Some("sk-test".to_string()),
            team_organization_id: None,
            team_project_id: None,
        }
    }

    fn codex_provider_with_usage(
        id: &str,
        base_url: &str,
        api_key: &str,
        usage_api_key: Option<&str>,
        usage_base_url: Option<&str>,
        template_type: Option<&str>,
    ) -> Provider {
        let mut provider = Provider::with_id(
            id.to_string(),
            format!("Provider {id}"),
            codex_settings(base_url, api_key),
            None,
        );
        provider.meta = Some(ProviderMeta {
            usage_script: Some(usage_script_with_credentials(
                usage_api_key,
                usage_base_url,
                template_type,
            )),
            ..Default::default()
        });
        provider
    }

    fn managed_codex_provider(id: &str, account_id: &str) -> Provider {
        let mut provider = Provider::with_id(
            id.to_string(),
            format!("Managed {id}"),
            json!({
                "auth": {},
                "config": ""
            }),
            None,
        );
        provider.category = Some("official".to_string());
        provider.meta = Some(ProviderMeta {
            auth_binding: Some(AuthBinding {
                source: AuthBindingSource::ManagedAccount,
                auth_provider: Some("codex_oauth".to_string()),
                account_id: Some(account_id.to_string()),
            }),
            ..Default::default()
        });
        provider
    }

    #[test]
    #[serial]
    fn add_clears_usage_credentials_that_match_provider_config() {
        with_test_home(|state, _| {
            let provider = codex_provider_with_usage(
                "codex-a",
                "https://api.a.example/v1/",
                "sk-a",
                Some(" sk-a "),
                Some(" https://api.a.example/v1/ "),
                None,
            );

            ProviderService::add(state, AppType::Codex, provider, false).expect("add provider");

            let saved = state
                .db
                .get_provider_by_id("codex-a", AppType::Codex.as_str())
                .expect("query saved provider")
                .expect("saved provider should exist");
            let script = saved
                .meta
                .as_ref()
                .and_then(|meta| meta.usage_script.as_ref())
                .expect("usage script should remain");

            assert_eq!(script.api_key, None);
            assert_eq!(script.base_url, None);
        });
    }

    #[test]
    #[serial]
    fn update_preserves_usage_credentials_that_only_match_previous_config() {
        with_test_home(|state, _| {
            let provider = codex_provider_with_usage(
                "codex-usage-old",
                "https://api.a.example/v1/",
                "sk-a",
                Some("sk-a"),
                Some("https://api.a.example/v1/"),
                None,
            );
            state
                .db
                .save_provider(AppType::Codex.as_str(), &provider)
                .expect("seed provider with explicit usage credentials");

            let mut updated = provider.clone();
            updated.settings_config = codex_settings("https://api.b.example/v1/", "sk-b");

            ProviderService::update(state, AppType::Codex, None, updated)
                .expect("update provider main credentials");

            let saved = state
                .db
                .get_provider_by_id("codex-usage-old", AppType::Codex.as_str())
                .expect("query updated provider")
                .expect("updated provider should exist");
            let script = saved
                .meta
                .as_ref()
                .and_then(|meta| meta.usage_script.as_ref())
                .expect("usage script should remain");

            assert_eq!(script.api_key.as_deref(), Some("sk-a"));
            assert_eq!(
                script.base_url.as_deref(),
                Some("https://api.a.example/v1/")
            );
            assert_eq!(
                saved.resolve_usage_credentials(&AppType::Codex),
                ("https://api.b.example/v1".to_string(), "sk-b".to_string())
            );
        });
    }

    #[test]
    #[serial]
    fn copied_provider_uses_edited_credentials_after_add_clears_mirrored_usage_credentials() {
        with_test_home(|state, _| {
            let copied_provider = codex_provider_with_usage(
                "codex-copy",
                "https://api.a.example/v1/",
                "sk-a",
                Some("sk-a"),
                Some("https://api.a.example/v1/"),
                None,
            );

            ProviderService::add(state, AppType::Codex, copied_provider, false)
                .expect("add copied provider");

            let saved_after_add = state
                .db
                .get_provider_by_id("codex-copy", AppType::Codex.as_str())
                .expect("query copied provider")
                .expect("copied provider should exist");
            let script_after_add = saved_after_add
                .meta
                .as_ref()
                .and_then(|meta| meta.usage_script.as_ref())
                .expect("usage script should remain");
            assert_eq!(script_after_add.api_key, None);
            assert_eq!(script_after_add.base_url, None);

            let mut edited_provider = saved_after_add.clone();
            edited_provider.settings_config = codex_settings("https://api.b.example/v1/", "sk-b");

            ProviderService::update(state, AppType::Codex, None, edited_provider)
                .expect("edit copied provider credentials");

            let saved_after_update = state
                .db
                .get_provider_by_id("codex-copy", AppType::Codex.as_str())
                .expect("query edited provider")
                .expect("edited provider should exist");
            let script_after_update = saved_after_update
                .meta
                .as_ref()
                .and_then(|meta| meta.usage_script.as_ref())
                .expect("usage script should remain");

            assert_eq!(script_after_update.api_key, None);
            assert_eq!(script_after_update.base_url, None);
            assert_eq!(
                saved_after_update.resolve_usage_credentials(&AppType::Codex),
                ("https://api.b.example/v1".to_string(), "sk-b".to_string())
            );
        });
    }

    #[test]
    #[serial]
    fn update_clears_usage_credentials_that_match_current_config() {
        with_test_home(|state, _| {
            let provider = codex_provider_with_usage(
                "codex-current",
                "https://api.a.example/v1",
                "sk-a",
                Some("sk-usage"),
                Some("https://usage.example/api"),
                None,
            );
            state
                .db
                .save_provider(AppType::Codex.as_str(), &provider)
                .expect("seed provider with distinct usage credentials");

            let mut updated = provider.clone();
            updated.settings_config = codex_settings("https://api.b.example/v1/", "sk-b");
            updated.meta = Some(ProviderMeta {
                usage_script: Some(usage_script_with_credentials(
                    Some(" sk-b "),
                    Some(" https://api.b.example/v1/ "),
                    None,
                )),
                ..Default::default()
            });

            ProviderService::update(state, AppType::Codex, None, updated)
                .expect("update provider with redundant usage credentials");

            let saved = state
                .db
                .get_provider_by_id("codex-current", AppType::Codex.as_str())
                .expect("query updated provider")
                .expect("updated provider should exist");
            let script = saved
                .meta
                .as_ref()
                .and_then(|meta| meta.usage_script.as_ref())
                .expect("usage script should remain");

            assert_eq!(script.api_key, None);
            assert_eq!(script.base_url, None);
        });
    }

    #[test]
    #[serial]
    fn add_preserves_distinct_usage_credentials() {
        with_test_home(|state, _| {
            let provider = codex_provider_with_usage(
                "codex-distinct",
                "https://api.main.example/v1",
                "sk-main",
                Some("sk-usage"),
                Some("https://usage.example/api"),
                None,
            );

            ProviderService::add(state, AppType::Codex, provider, false).expect("add provider");

            let saved = state
                .db
                .get_provider_by_id("codex-distinct", AppType::Codex.as_str())
                .expect("query saved provider")
                .expect("saved provider should exist");
            let script = saved
                .meta
                .as_ref()
                .and_then(|meta| meta.usage_script.as_ref())
                .expect("usage script should remain");

            assert_eq!(script.api_key.as_deref(), Some("sk-usage"));
            assert_eq!(
                script.base_url.as_deref(),
                Some("https://usage.example/api")
            );
        });
    }

    #[test]
    #[serial]
    fn add_does_not_clear_token_plan_credentials() {
        with_test_home(|state, _| {
            let provider = codex_provider_with_usage(
                "codex-token-plan",
                "https://api.plan.example/v1",
                "sk-plan",
                Some("sk-plan"),
                Some("https://api.plan.example/v1"),
                Some("token_plan"),
            );

            ProviderService::add(state, AppType::Codex, provider, false).expect("add provider");

            let saved = state
                .db
                .get_provider_by_id("codex-token-plan", AppType::Codex.as_str())
                .expect("query saved provider")
                .expect("saved provider should exist");
            let script = saved
                .meta
                .as_ref()
                .and_then(|meta| meta.usage_script.as_ref())
                .expect("usage script should remain");

            assert_eq!(script.api_key.as_deref(), Some("sk-plan"));
            assert_eq!(
                script.base_url.as_deref(),
                Some("https://api.plan.example/v1")
            );
            assert_eq!(script.access_key_id.as_deref(), Some("ak-test"));
            assert_eq!(script.secret_access_key.as_deref(), Some("sk-test"));
        });
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
    #[serial]
    fn add_accepts_multiple_unbound_codex_official_cards() {
        with_test_home(|state, _| {
            crate::settings::reload_settings().expect("reload settings");
            state
                .db
                .init_default_official_providers()
                .expect("seed official providers");
            let fixed_id = crate::database::CODEX_OFFICIAL_PROVIDER_ID;
            state
                .db
                .set_current_provider(AppType::Codex.as_str(), fixed_id)
                .expect("set database current");
            crate::settings::set_current_provider(&AppType::Codex, Some(fixed_id))
                .expect("set local current");

            for id in ["follow-login-a", "follow-login-b"] {
                let mut provider = Provider::with_id(
                    id.to_string(),
                    id.to_string(),
                    json!({ "auth": {}, "config": "" }),
                    None,
                );
                provider.category = Some("official".to_string());
                ProviderService::add(state, AppType::Codex, provider, false)
                    .expect("add unbound Official card");
            }

            let providers = state
                .db
                .get_all_providers(AppType::Codex.as_str())
                .expect("read providers");
            assert!(providers.contains_key("follow-login-a"));
            assert!(providers.contains_key("follow-login-b"));
        });
    }

    #[test]
    #[serial]
    fn update_keeps_official_provider_id_when_binding_and_unbinding() {
        with_test_home(|state, _| {
            crate::settings::reload_settings().expect("reload settings");
            tauri::async_runtime::block_on(async {
                state
                    .codex_oauth_manager
                    .add_test_account_with_user_identity(
                        "acct-managed",
                        "managed-access-token",
                        "managed-user",
                    )
                    .await
                    .expect("seed managed account");
            });

            let provider_id = crate::database::CODEX_OFFICIAL_PROVIDER_ID;
            let mut unbound = Provider::with_id(
                provider_id.to_string(),
                "OpenAI Official".to_string(),
                json!({ "auth": {}, "config": "" }),
                None,
            );
            unbound.category = Some("official".to_string());
            state
                .db
                .save_provider(AppType::Codex.as_str(), &unbound)
                .expect("save unbound card");
            state
                .db
                .set_current_provider(AppType::Codex.as_str(), provider_id)
                .expect("set database current");
            crate::settings::set_current_provider(&AppType::Codex, Some(provider_id))
                .expect("set local current");

            let mut bound = managed_codex_provider(provider_id, "acct-managed");
            bound.name = unbound.name.clone();
            ProviderService::update(state, AppType::Codex, Some(provider_id), bound)
                .expect("bind managed account");

            let saved_bound = state
                .db
                .get_provider_by_id(provider_id, AppType::Codex.as_str())
                .expect("query bound card")
                .expect("bound card should keep its ID");
            assert_eq!(
                ProviderService::managed_codex_oauth_account_id(&saved_bound).as_deref(),
                Some("acct-managed")
            );
            assert_eq!(
                state
                    .db
                    .get_current_provider(AppType::Codex.as_str())
                    .expect("read database current")
                    .as_deref(),
                Some(provider_id)
            );
            assert_eq!(
                crate::settings::get_current_provider(&AppType::Codex).as_deref(),
                Some(provider_id)
            );

            // 旧版「统一会话历史」注入进 live、又被回填进行里的形态，保存时剥掉。
            unbound.settings_config["config"] = Value::String("model_provider = \"custom\"\n\n[model_providers.custom]\nname = \"OpenAI\"\nrequires_openai_auth = true\nsupports_websockets = true\nwire_api = \"responses\"\n".to_string());
            ProviderService::update(state, AppType::Codex, Some(provider_id), unbound)
                .expect("unbind managed account");

            let saved_unbound = state
                .db
                .get_provider_by_id(provider_id, AppType::Codex.as_str())
                .expect("query unbound card")
                .expect("unbound card should keep its ID");
            assert!(ProviderService::managed_codex_oauth_account_id(&saved_unbound).is_none());
            assert_eq!(saved_unbound.settings_config["config"], json!(""));
            assert_eq!(
                state
                    .db
                    .get_current_provider(AppType::Codex.as_str())
                    .expect("read database current")
                    .as_deref(),
                Some(provider_id)
            );
            assert_eq!(
                crate::settings::get_current_provider(&AppType::Codex).as_deref(),
                Some(provider_id)
            );
        });
    }

    #[test]
    fn sensitive_key_matcher_covers_common_credential_namings() {
        for key in [
            // 裸 `_KEY`：最常见的写法，却曾被"只枚举 `_API_KEY` 这些子类"漏在外面
            "OPENAI_KEY",
            "GROQ_KEY",
            "XAI_KEY",
            // 不带分隔符的复合写法
            "VOLC_ACCESSKEY",
            "ALIYUN_SECRETKEY",
            "SOME_APITOKEN",
            // personal access token：既不含 TOKEN 也不含 KEY
            "GITHUB_PAT",
            "gitlab_pat",
            // 口令类缩写
            "MYSQL_PWD",
            "DB_PASS",
            "GPG_PASSPHRASE",
            "AWS_CREDS",
            // 发往上游的自定义请求头、Cookie、Authorization
            "ANTHROPIC_CUSTOM_HEADERS",
            "GEMINI_CLI_CUSTOM_HEADERS",
            "headers",
            "UPSTREAM_COOKIE",
            "PROXY_AUTHORIZATION",
        ] {
            assert!(
                ProviderService::is_sensitive_config_key(key),
                "{key} must be treated as a credential"
            );
        }

        // 后缀必须带下划线，不能把正常配置一起卷进来
        for key in [
            "PATH",
            "OLDPWD",
            "GEMINI_COMPAT",
            "SSL_BYPASS",
            "GEMINI_TIMEOUT_MS",
            "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
            // 名字带 HEADER 但不是发往上游的凭据：普通开关、用户自己的遥测端点
            "CLAUDE_CODE_ATTRIBUTION_HEADER",
            "OTEL_EXPORTER_OTLP_HEADERS",
        ] {
            assert!(
                !ProviderService::is_sensitive_config_key(key),
                "{key} is ordinary shareable config and must not be stripped"
            );
        }
    }

    #[tokio::test]
    #[serial]
    async fn update_current_claude_provider_writes_live_when_proxy_never_enabled() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let state = AppState::new(db.clone());
        let original = Provider::with_id(
            "p1".into(),
            "Claude A".into(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "token-a",
                    "ANTHROPIC_BASE_URL": "https://api.old.example"
                }
            }),
            None,
        );
        db.save_provider("claude", &original)
            .expect("save provider");
        db.set_current_provider("claude", "p1")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Claude, Some("p1"))
            .expect("set local current provider");
        write_live_for_state(&state, &AppType::Claude, &original).expect("seed live file");

        let mut updated = original.clone();
        updated.settings_config["env"]["ANTHROPIC_BASE_URL"] =
            Value::String("https://api.new.example".into());
        ProviderService::update(&state, AppType::Claude, None, updated)
            .expect("update current provider");

        let live: Value = read_json_file(&get_claude_settings_path()).expect("read live");
        assert_eq!(
            live["env"]["ANTHROPIC_BASE_URL"].as_str(),
            Some("https://api.new.example")
        );
    }

    /// 编辑当前供应商、去掉它的独有字段：live 里 CC Switch 写进去的那个值随之删掉，
    /// 用户自己的键不动。
    #[tokio::test]
    #[serial]
    async fn update_current_claude_provider_drops_the_compat_switch_it_no_longer_has() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let state = AppState::new(db.clone());
        let original = Provider::with_id(
            "ds".into(),
            "DeepSeek".into(),
            json!({ "env": {
                "ANTHROPIC_BASE_URL": "https://api.deepseek.example/anthropic",
                "CLAUDE_CODE_DISABLE_ARTIFACT": "1"
            }}),
            None,
        );
        db.save_provider("claude", &original)
            .expect("save provider");
        ProviderService::switch(&state, AppType::Claude, "ds").expect("switch");
        let mut live: Value = read_json_file(&get_claude_settings_path()).expect("read live");
        live["env"]["DEBUG"] = json!("1");
        write_json_file(&get_claude_settings_path(), &live).expect("user edit");

        let mut updated = original.clone();
        updated.settings_config["env"]
            .as_object_mut()
            .expect("env")
            .remove("CLAUDE_CODE_DISABLE_ARTIFACT");
        ProviderService::update(&state, AppType::Claude, None, updated).expect("update");

        let live: Value = read_json_file(&get_claude_settings_path()).expect("read live");
        assert_eq!(
            live,
            json!({ "env": {
                "ANTHROPIC_BASE_URL": "https://api.deepseek.example/anthropic",
                "DEBUG": "1"
            }})
        );
    }

    /// 文件已经写成目标供应商、指针还没改时崩溃：下次启动按 pending 补完指针，
    /// 文件和指针重新一致。
    #[tokio::test]
    #[serial]
    async fn claude_switch_interrupted_before_the_pointer_rolls_forward() {
        use crate::mode::operation::failpoint;

        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let state = AppState::new(db.clone());
        for (id, url) in [("a", "https://a.example"), ("b", "https://b.example")] {
            let provider = Provider::with_id(
                id.into(),
                id.into(),
                json!({ "env": { "ANTHROPIC_BASE_URL": url } }),
                None,
            );
            db.save_provider("claude", &provider)
                .expect("save provider");
        }
        ProviderService::switch(&state, AppType::Claude, "a").expect("switch to a");

        failpoint::crash_at(Some("published:0"));
        let result = ProviderService::switch(&state, AppType::Claude, "b");
        failpoint::crash_at(None);
        assert!(result.is_err(), "the injected crash surfaces");

        let live: Value = read_json_file(&get_claude_settings_path()).expect("read live");
        assert_eq!(
            live["env"]["ANTHROPIC_BASE_URL"],
            json!("https://b.example")
        );
        assert_eq!(
            db.get_current_provider("claude")
                .expect("current")
                .as_deref(),
            Some("a"),
            "the pointer has not moved yet"
        );

        crate::mode::operation::recover_on_startup(&db);
        assert_eq!(
            db.get_current_provider("claude")
                .expect("current")
                .as_deref(),
            Some("b")
        );
        assert_eq!(
            crate::settings::get_current_provider(&AppType::Claude).as_deref(),
            Some("b")
        );
    }

    /// A stale backup row must be refreshed but must not divert the live write.
    #[tokio::test]
    #[serial]
    async fn update_current_claude_provider_writes_live_when_backup_row_is_stale() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let state = AppState::new(db.clone());
        let original = Provider::with_id(
            "p1".into(),
            "Claude A".into(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "token-a",
                    "ANTHROPIC_BASE_URL": "https://api.old.example"
                }
            }),
            None,
        );
        db.save_provider("claude", &original)
            .expect("save provider");
        db.set_current_provider("claude", "p1")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Claude, Some("p1"))
            .expect("set local current provider");
        write_live_for_state(&state, &AppType::Claude, &original).expect("seed live file");
        db.save_live_backup(
            "claude",
            &serde_json::to_string(&original.settings_config).expect("serialize backup"),
        )
        .await
        .expect("seed stale backup");
        assert!(!state.proxy_service.is_running().await);

        let mut updated = original.clone();
        updated.settings_config["env"]["ANTHROPIC_BASE_URL"] =
            Value::String("https://api.new.example".into());
        ProviderService::update(&state, AppType::Claude, None, updated)
            .expect("update current provider");

        let live: Value = read_json_file(&get_claude_settings_path()).expect("read live");
        assert_eq!(
            live["env"]["ANTHROPIC_BASE_URL"].as_str(),
            Some("https://api.new.example"),
            "a leftover backup row is not proxy mode: the edit goes to live"
        );
    }

    /// An enabled flag left behind by an interrupted teardown is not enough to
    /// suppress a live write when neither placeholder nor backup evidence exists.
    #[tokio::test]
    #[serial]
    async fn update_current_claude_provider_ignores_enabled_flag_without_evidence() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let state = AppState::new(db.clone());
        let original = Provider::with_id(
            "p1".into(),
            "Claude A".into(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "token-a",
                    "ANTHROPIC_BASE_URL": "https://api.old.example"
                }
            }),
            None,
        );
        db.save_provider("claude", &original)
            .expect("save provider");
        db.set_current_provider("claude", "p1")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Claude, Some("p1"))
            .expect("set local current provider");
        write_live_for_state(&state, &AppType::Claude, &original).expect("seed live file");
        let mut config = db
            .get_proxy_config_for_app("claude")
            .await
            .expect("read proxy config");
        config.enabled = true;
        db.update_proxy_config_for_app(config)
            .await
            .expect("leave enabled flag set");
        assert!(!state.proxy_service.is_running().await);

        let mut updated = original.clone();
        updated.settings_config["env"]["ANTHROPIC_BASE_URL"] =
            Value::String("https://api.new.example".into());
        ProviderService::update(&state, AppType::Claude, None, updated)
            .expect("update current provider");

        let live: Value = read_json_file(&get_claude_settings_path()).expect("read live");
        assert_eq!(
            live["env"]["ANTHROPIC_BASE_URL"].as_str(),
            Some("https://api.new.example")
        );
    }

    #[test]
    fn extract_claude_common_config_strips_all_credentials_keeps_shareable() {
        // env 混入多种凭据（Anthropic/OpenRouter/Google/OpenAI/Gemini + AWS/Vertex）
        // 与可共享配置；顶层混入非标准的 apiKey/api_key 凭据与正常设置。
        let settings = json!({
            "env": {
                "ANTHROPIC_API_KEY": "sk-ant",
                "ANTHROPIC_AUTH_TOKEN": "tok-ant",
                "OPENROUTER_API_KEY": "sk-or",
                "GOOGLE_API_KEY": "g-key",
                "OPENAI_API_KEY": "sk-oai",
                "GEMINI_API_KEY": "g-gem",
                "AWS_ACCESS_KEY_ID": "AKIA",
                "AWS_SECRET_ACCESS_KEY": "secret",
                "AWS_SESSION_TOKEN": "sess",
                "GOOGLE_APPLICATION_CREDENTIALS": "/path/creds.json",
                "AWS_BEARER_TOKEN_BEDROCK": "bedrock-tok",
                "ANTHROPIC_BASE_URL": "https://example.com",
                "ANTHROPIC_MODEL": "claude-x",
                "CLAUDE_CODE_SUBAGENT_MODEL": "gpt-5.4-mini",
                "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "400000",
                "CLAUDE_CODE_AUTO_COMPACT_WINDOW": "400000",
                // 可共享、非机密配置（复数 _TOKENS 不应被误剥）
                "ENABLE_TOOL_SEARCH": "true",
                "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "8192"
            },
            "apiKey": "sk-top",
            "api_key": "sk-top2",
            "theme": "dark",
            "includeCoAuthoredBy": false
        });

        let snippet = ProviderService::extract_claude_common_config(&settings)
            .expect("extract should succeed");
        let value: Value = serde_json::from_str(&snippet).expect("snippet is valid JSON");

        // 所有凭据都不得出现在共享片段里
        let env = value.get("env");
        for leaked in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "OPENROUTER_API_KEY",
            "GOOGLE_API_KEY",
            "OPENAI_API_KEY",
            "GEMINI_API_KEY",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "AWS_BEARER_TOKEN_BEDROCK",
        ] {
            assert!(
                env.and_then(|e| e.get(leaked)).is_none(),
                "credential {leaked} must not leak into common config"
            );
        }
        assert!(
            value.get("apiKey").is_none() && value.get("api_key").is_none(),
            "top-level credentials must be stripped"
        );

        // 端点/模型（provider-specific 非机密）也应剥掉
        assert!(env.and_then(|e| e.get("ANTHROPIC_BASE_URL")).is_none());
        assert!(env.and_then(|e| e.get("ANTHROPIC_MODEL")).is_none());
        assert!(env
            .and_then(|e| e.get("CLAUDE_CODE_SUBAGENT_MODEL"))
            .is_none());
        assert!(env
            .and_then(|e| e.get("CLAUDE_CODE_MAX_CONTEXT_TOKENS"))
            .is_none());
        assert!(env
            .and_then(|e| e.get("CLAUDE_CODE_AUTO_COMPACT_WINDOW"))
            .is_none());

        // 可共享的非机密配置必须保留（含复数 _TOKENS 不被误剥）
        assert_eq!(
            env.and_then(|e| e.get("ENABLE_TOOL_SEARCH"))
                .and_then(|v| v.as_str()),
            Some("true")
        );
        assert_eq!(
            env.and_then(|e| e.get("CLAUDE_CODE_MAX_OUTPUT_TOKENS"))
                .and_then(|v| v.as_str()),
            Some("8192")
        );
        assert_eq!(value.get("theme").and_then(|v| v.as_str()), Some("dark"));
        assert_eq!(value.get("includeCoAuthoredBy"), Some(&json!(false)));
    }

    /// 关键字段（协议选择器、Bedrock/Vertex 区域、`/model` 的选择等）不进共享片段；
    /// 同在 `CLAUDE_CODE_USE_` 前缀下、与供应商无关的开关照常共享。
    #[test]
    fn extract_claude_common_config_keeps_key_fields_per_provider() {
        let settings = json!({
            "env": {
                "CLAUDE_CODE_USE_BEDROCK": "1",
                "CLAUDE_CODE_USE_VERTEX": "1",
                "CLAUDE_CODE_SKIP_BEDROCK_AUTH": "1",
                "AWS_REGION": "us-west-2",
                "AWS_PROFILE": "work",
                "CLOUD_ML_REGION": "us-east5",
                "VERTEX_REGION_CLAUDE_4_0_OPUS": "europe-west1",
                "ANTHROPIC_VERTEX_PROJECT_ID": "my-project",
                "ANTHROPIC_SMALL_FAST_MODEL": "haiku",
                "CLAUDE_CODE_SUBAGENT_MODEL_FORCE": "1",
                "CLAUDE_CODE_USE_POWERSHELL_TOOL": "1",
                "CLAUDE_CODE_ATTRIBUTION_HEADER": "0",
                "DISABLE_TELEMETRY": "1"
            },
            "model": "opus",
            "fallbackModel": "sonnet",
            "apiKeyHelper": "~/bin/key.sh",
            "awsAuthRefresh": "aws sso login",
            "hooks": { "Stop": [] },
            "theme": "dark"
        });

        let snippet = ProviderService::extract_claude_common_config(&settings)
            .expect("extract should succeed");
        let value: Value = serde_json::from_str(&snippet).expect("snippet is valid JSON");

        assert_eq!(
            value,
            json!({
                "env": {
                    "CLAUDE_CODE_USE_POWERSHELL_TOOL": "1",
                    "CLAUDE_CODE_ATTRIBUTION_HEADER": "0",
                    "DISABLE_TELEMETRY": "1"
                },
                "hooks": { "Stop": [] },
                "theme": "dark"
            })
        );
        assert!(
            snippet.find("CLAUDE_CODE_USE_POWERSHELL_TOOL") < snippet.find("DISABLE_TELEMETRY"),
            "removing keys must not reorder the ones that stay: {snippet}"
        );
    }

    /// Regression for issue #4272: Fable tier env keys must not enter the shared
    /// Claude common-config snippet (same class as haiku/sonnet/opus model pins).
    #[test]
    fn extract_claude_common_config_strips_fable_model_env_keys() {
        let settings = json!({
            "env": {
                "ANTHROPIC_DEFAULT_HAIKU_MODEL": "haiku-mapped",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME": "Haiku Mapped",
                "ANTHROPIC_DEFAULT_SONNET_MODEL": "sonnet-mapped[1M]",
                "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME": "Sonnet Mapped",
                "ANTHROPIC_DEFAULT_OPUS_MODEL": "opus-mapped[1M]",
                "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME": "Opus Mapped",
                "ANTHROPIC_DEFAULT_FABLE_MODEL": "deepseek-v4-flash[1M]",
                "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME": "deepseek-v4-flash",
                "ANTHROPIC_MODEL": "default-mapped",
                "ENABLE_TOOL_SEARCH": "true"
            },
            "theme": "dark"
        });

        let snippet = ProviderService::extract_claude_common_config(&settings)
            .expect("extract should succeed");
        let value: Value = serde_json::from_str(&snippet).expect("snippet is valid JSON");
        let env = value.get("env");

        for stripped in [
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
            "ANTHROPIC_DEFAULT_FABLE_MODEL",
            "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME",
            "ANTHROPIC_MODEL",
        ] {
            assert!(
                env.and_then(|e| e.get(stripped)).is_none(),
                "provider-specific model key {stripped} must not enter common config"
            );
        }

        assert_eq!(
            env.and_then(|e| e.get("ENABLE_TOOL_SEARCH"))
                .and_then(|v| v.as_str()),
            Some("true")
        );
        assert_eq!(value.get("theme").and_then(|v| v.as_str()), Some("dark"));
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
    fn extract_codex_common_config_strips_provider_fields_and_injected_artifacts() {
        // 顶层 experimental_bearer_token 模拟无活跃路由时的 fallback 注入；
        // web_search = "disabled" 是 cc-switch 对黑名单网关注入的哨兵；
        // 顶层 wire_api 模拟无 model_provider 时的 fallback 写法；
        // [mcp.servers] 是历史错误格式，sync_all_enabled 清不掉它。
        let config_toml = r#"model_provider = "azure"
model = "gpt-4"
wire_api = "chat"
disable_response_storage = true
model_reasoning_effort = "high"
approval_policy = "on-request"
experimental_bearer_token = "sk-live-secret"
model_catalog_json = "cc-switch-model-catalog.json"
web_search = "disabled"

[agents]
default_subagent_model = "gpt-4-mini"
max_threads = 4

[model_providers.azure]
name = "Azure OpenAI"
base_url = "https://azure.example/v1"
wire_api = "responses"

[mcp_servers.my_server]
base_url = "http://localhost:8080"

[mcp.servers.legacy_server]
command = "legacy-cmd"
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
        // MCP 归 DB mcp_servers 表所有，不得进共享片段（含历史错误格式 [mcp.servers]）
        assert!(
            !extracted.contains("mcp_servers") && !extracted.contains("http://localhost:8080"),
            "should strip mcp_servers from the shared snippet, got: {extracted}"
        );
        assert!(
            !extracted.contains("[mcp") && !extracted.contains("legacy-cmd"),
            "should strip the legacy [mcp.servers] form from the shared snippet, got: {extracted}"
        );
        // 顶层 wire_api 是供应商路由语义（model_providers 整表已剥，
        // 剩余任何 wire_api 都意味着泄漏）
        assert!(
            !extracted.contains("wire_api"),
            "should strip top-level wire_api from the shared snippet, got: {extracted}"
        );
        // 注入产物不得进共享片段（bearer token 泄漏为密钥级问题）
        assert!(
            !extracted.contains("experimental_bearer_token")
                && !extracted.contains("sk-live-secret"),
            "should strip top-level fallback bearer token, got: {extracted}"
        );
        assert!(
            !extracted.contains("model_catalog_json"),
            "should strip catalog projection pointer, got: {extracted}"
        );
        assert!(
            !extracted.contains("web_search"),
            "should strip the cc-switch web_search disabled sentinel, got: {extracted}"
        );
        // 关键字段归供应商（片段已冻结，收进去会从行里剥掉、再也写不回 live）
        for key in [
            "disable_response_storage",
            "model_reasoning_effort",
            "default_subagent_model",
        ] {
            assert!(
                !extracted.contains(key),
                "key field {key} must stay with the provider, got: {extracted}"
            );
        }
        // 真正可共享的键保留
        assert!(
            extracted.contains("approval_policy = \"on-request\"")
                && extracted.contains("max_threads = 4"),
            "shareable keys must survive extraction, got: {extracted}"
        );
    }

    #[test]
    fn extract_codex_common_config_keeps_user_set_web_search() {
        let config_toml = "web_search = \"enabled\"\ndisable_response_storage = true\n";
        let settings = json!({ "config": config_toml });
        let extracted = ProviderService::extract_codex_common_config(&settings)
            .expect("extract should succeed");
        assert!(
            extracted.contains("web_search = \"enabled\""),
            "a user-set web_search value is a shareable preference, got: {extracted}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn editing_the_routed_claude_provider_rewrites_the_proxy_contract() {
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
                }
            }),
            None,
        );
        db.save_provider("claude", &original)
            .expect("save provider");
        db.set_current_provider("claude", "p1")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Claude, Some("p1"))
            .expect("set local current provider");
        write_json_file(
            &get_claude_settings_path(),
            &json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://api.a.example",
                    "ANTHROPIC_API_KEY": "token-a",
                    "ANTHROPIC_MODEL": "model-a"
                },
                "permissions": { "allow": ["Bash"] }
            }),
        )
        .expect("seed live file");

        db.update_proxy_config(ProxyConfig {
            listen_port: 0,
            ..Default::default()
        })
        .await
        .expect("update proxy config");
        crate::mode::controller::enter(&state, &AppType::Claude)
            .await
            .expect("enter routing mode");
        let proxy_url = state
            .proxy_service
            .build_proxy_urls()
            .await
            .expect("proxy url")
            .0;

        let updated = Provider::with_id(
            "p1".into(),
            "Claude A".into(),
            json!({
                "env": {
                    "ANTHROPIC_API_KEY": "token-updated",
                    "ANTHROPIC_BASE_URL": "https://api.updated.example",
                    "ANTHROPIC_MODEL": "model-updated"
                }
            }),
            None,
        );
        ProviderService::update(&state, AppType::Claude, None, updated)
            .expect("update routed provider");

        let live: Value = read_json_file(&get_claude_settings_path()).expect("read live");
        let env = &live["env"];
        assert_eq!(env["ANTHROPIC_API_KEY"], "PROXY_MANAGED");
        assert_eq!(env["ANTHROPIC_BASE_URL"], proxy_url.as_str());
        assert!(env.get("ANTHROPIC_MODEL").is_none(), "{live}");
        assert_eq!(
            env["ANTHROPIC_DEFAULT_SONNET_MODEL_NAME"], "model-updated",
            "the contract's display names follow the edited provider"
        );
        assert_eq!(
            live["permissions"],
            json!({ "allow": ["Bash"] }),
            "user settings in live are left alone"
        );

        state
            .proxy_service
            .stop()
            .await
            .expect("stop proxy service");
    }

    #[tokio::test]
    #[serial]
    async fn update_current_codex_provider_refreshes_and_clears_catalog_during_takeover() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let state = AppState::new(db.clone());

        let mut original = Provider::with_id(
            "p1".into(),
            "Codex A".into(),
            json!({
                "auth": { "OPENAI_API_KEY": "token-a" },
                "config": r#"model_provider = "custom"
model = "old-model"

[model_providers.custom]
name = "Codex A"
base_url = "https://api.a.example/v1"
wire_api = "responses"
requires_openai_auth = true
"#,
                "modelCatalog": {
                    "models": [{ "model": "old-model" }]
                }
            }),
            None,
        );
        original.meta = Some(ProviderMeta {
            api_format: Some("openai_responses".into()),
            ..Default::default()
        });
        db.save_provider("codex", &original).expect("save provider");
        db.set_current_provider("codex", "p1")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Codex, Some("p1"))
            .expect("set local current provider");

        db.update_proxy_config(ProxyConfig {
            listen_port: 0,
            ..Default::default()
        })
        .await
        .expect("update proxy config");
        crate::mode::controller::enter(&state, &AppType::Codex)
            .await
            .expect("enter routing mode");
        assert!(
            state
                .proxy_service
                .live_has_proxy_placeholder(&AppType::Codex),
            "Codex live config should carry the proxy contract"
        );

        let mut updated = original.clone();
        updated.settings_config["config"] = json!(
            r#"model_provider = "custom"
model = "gpt-5.4"

[model_providers.custom]
name = "Codex A"
base_url = "https://api.updated.example/v1"
wire_api = "responses"
requires_openai_auth = true
"#
        );
        updated.settings_config["modelCatalog"] = json!({
            "models": [{ "model": "gpt-5.4", "displayName": "GPT 5.4" }]
        });

        ProviderService::update(&state, AppType::Codex, None, updated.clone())
            .expect("update current Codex provider mapping");

        let catalog_path = crate::codex_config::get_codex_model_catalog_path();
        let catalog: Value = read_json_file(&catalog_path).expect("read generated catalog");
        assert_eq!(catalog["models"][0]["slug"], "gpt-5.4");
        assert_eq!(
            catalog["models"][0]["input_modalities"],
            json!(["text", "image"]),
            "unknown/GPT models must fail open to image input"
        );
        let live_config = fs::read_to_string(crate::codex_config::get_codex_config_path())
            .expect("read Codex config.toml");
        assert!(live_config.contains("model_catalog_json"));

        updated.settings_config["modelCatalog"] = json!({ "models": [] });
        ProviderService::update(&state, AppType::Codex, None, updated)
            .expect("remove current Codex provider mapping");

        let live_config = fs::read_to_string(crate::codex_config::get_codex_config_path())
            .expect("read Codex config.toml after mapping removal");
        assert!(
            !live_config.contains("model_catalog_json"),
            "removing mappings during takeover must clear the stale catalog pointer"
        );

        state
            .proxy_service
            .stop()
            .await
            .expect("stop proxy service");
    }

    #[cfg(any(target_os = "macos", windows, target_os = "linux"))]
    #[tokio::test]
    #[serial]
    async fn update_current_claude_desktop_provider_syncs_profile_when_proxy_takeover_is_active() {
        let home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let state = AppState::new(db.clone());

        // 端口取 0 让内核分配：默认端口 15721 是用户机器上常驻的 CC Switch 自己占着的，
        // 用它会让本机跑测试必然撞上 `Address already in use`。下面的断言改成按实际
        // 端口比对，所以这里不必固定值。
        db.update_proxy_config(ProxyConfig {
            listen_port: 0,
            ..Default::default()
        })
        .await
        .expect("update proxy config");

        let mut original = Provider::with_id(
            "p1".into(),
            "Desktop A".into(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "token-a",
                    "ANTHROPIC_BASE_URL": "https://opencode.ai/zen/go"
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

        let proxy_info = state
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
                    "ANTHROPIC_BASE_URL": "https://opencode.ai/zen/go"
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
            json!(format!(
                "http://127.0.0.1:{}/claude-desktop",
                proxy_info.port
            )),
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
    fn add_first_managed_codex_with_reauth_required_account_is_rejected() {
        with_test_home(|state, _| {
            crate::settings::reload_settings().expect("reload settings");
            tauri::async_runtime::block_on(async {
                state
                    .codex_oauth_manager
                    .add_test_account_with_access_token("acct-legacy", "managed-token", None)
                    .await
                    .expect("seed legacy account without id_token");
            });
            let provider = managed_codex_provider("managed-legacy", "acct-legacy");

            let error = ProviderService::add(state, AppType::Codex, provider.clone(), false)
                .expect_err("reauth-required account must not be written to live auth");
            assert!(
                error.to_string().contains("id_token"),
                "backend should require re-login even if the frontend gate is bypassed: {error}"
            );
            assert!(state
                .db
                .get_provider_by_id(&provider.id, AppType::Codex.as_str())
                .expect("query provider")
                .is_none());
            assert!(!crate::codex_config::get_codex_auth_path().exists());
        });
    }

    #[test]
    #[serial]
    fn switch_from_managed_codex_official_to_unbound_clears_live_without_backfilling_token() {
        with_test_home(|state, _| {
            crate::settings::reload_settings().expect("reload settings");
            tauri::async_runtime::block_on(async {
                state
                    .codex_oauth_manager
                    .add_test_account_with_user_identity(
                        "acct-managed",
                        "managed-token",
                        "managed-user",
                    )
                    .await
                    .expect("seed managed Codex OAuth account");
            });

            let mut managed = Provider::with_id(
                "managed-official".to_string(),
                "Managed Official".to_string(),
                json!({
                    "auth": {},
                    "config": ""
                }),
                None,
            );
            managed.category = Some("official".to_string());
            managed.meta = Some(ProviderMeta {
                auth_binding: Some(AuthBinding {
                    source: AuthBindingSource::ManagedAccount,
                    auth_provider: Some("codex_oauth".to_string()),
                    account_id: Some("acct-managed".to_string()),
                }),
                ..Default::default()
            });

            let mut unbound = Provider::with_id(
                "unbound-official".to_string(),
                "Unbound Official".to_string(),
                json!({
                    "auth": {},
                    "config": ""
                }),
                None,
            );
            unbound.category = Some("official".to_string());

            state
                .db
                .save_provider(AppType::Codex.as_str(), &managed)
                .expect("save managed provider");
            state
                .db
                .save_provider(AppType::Codex.as_str(), &unbound)
                .expect("save unbound provider");

            ProviderService::switch(state, AppType::Codex, "managed-official")
                .expect("switch to managed official");
            let live_auth: Value = read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read managed live auth");
            assert_eq!(
                live_auth
                    .pointer("/tokens/access_token")
                    .and_then(Value::as_str),
                Some("managed-token"),
                "managed switch should write the selected ChatGPT token to live auth"
            );

            // Simulate a bare Codex CLI self-refresh. The app marker still
            // describes the pre-refresh write, while both access and refresh
            // token material on disk have rotated.
            let rotated_id_token = crate::codex_config::test_codex_id_token("managed-user");
            let rotated_live_auth = crate::codex_config::codex_managed_oauth_auth_value(
                "acct-managed",
                "cli-rotated-access",
                Some(&rotated_id_token),
                "cli-rotated-refresh",
                "2099-01-02T03:04:05Z",
            );
            write_json_file(
                &crate::codex_config::get_codex_auth_path(),
                &rotated_live_auth,
            )
            .expect("simulate Codex CLI token rotation");

            ProviderService::switch(state, AppType::Codex, "unbound-official")
                .expect("switch to unbound official");

            assert!(
                !crate::codex_config::get_codex_auth_path().exists(),
                "switching to an unbound official provider should clear the recorded managed live auth"
            );
            assert_eq!(
                tauri::async_runtime::block_on(
                    state
                        .codex_oauth_manager
                        .test_refresh_token_for_account("acct-managed")
                )
                .as_deref(),
                Some("cli-rotated-refresh"),
                "switch-away must adopt the CLI-rotated refresh token before deleting live auth"
            );

            let saved_managed = state
                .db
                .get_provider_by_id("managed-official", AppType::Codex.as_str())
                .expect("query managed provider")
                .expect("managed provider should exist");
            assert_eq!(
                saved_managed.settings_config.get("auth"),
                Some(&json!({})),
                "switch-away backfill must not persist the managed access token into provider storage"
            );
        });
    }

    #[test]
    #[serial]
    fn managed_codex_switch_adopts_outgoing_cli_rotation_before_account_or_key_overwrite() {
        with_test_home(|state, _| {
            crate::settings::reload_settings().expect("reload settings");
            tauri::async_runtime::block_on(async {
                state
                    .codex_oauth_manager
                    .add_test_account_with_user_identity("acct-a", "managed-access-a", "user-a")
                    .await
                    .expect("seed account A");
                state
                    .codex_oauth_manager
                    .add_test_account_with_user_identity("acct-b", "managed-access-b", "user-b")
                    .await
                    .expect("seed account B");
            });

            let provider_a = managed_codex_provider("managed-a", "acct-a");
            let provider_b = managed_codex_provider("managed-b", "acct-b");
            let mut third_party = Provider::with_id(
                "third-party".to_string(),
                "Third Party".to_string(),
                json!({
                    "auth": { "OPENAI_API_KEY": "sk-third-party" },
                    "config": r#"model_provider = "third"
[model_providers.third]
name = "Third"
base_url = "https://third.example/v1"
wire_api = "responses"
"#
                }),
                None,
            );
            third_party.category = Some("custom".to_string());
            for provider in [&provider_a, &provider_b, &third_party] {
                state
                    .db
                    .save_provider(AppType::Codex.as_str(), provider)
                    .expect("save provider");
            }

            ProviderService::switch(state, AppType::Codex, &provider_a.id)
                .expect("activate managed A");
            let id_token_a = crate::codex_config::test_codex_id_token("user-a");
            write_json_file(
                &crate::codex_config::get_codex_auth_path(),
                &crate::codex_config::codex_managed_oauth_auth_value(
                    "acct-a",
                    "cli-access-a1",
                    Some(&id_token_a),
                    "cli-refresh-a1",
                    "2099-01-02T00:00:00Z",
                ),
            )
            .expect("rotate account A live auth");

            ProviderService::switch(state, AppType::Codex, &provider_b.id)
                .expect("switch managed A to managed B");
            assert_eq!(
                tauri::async_runtime::block_on(
                    state
                        .codex_oauth_manager
                        .test_refresh_token_for_account("acct-a")
                )
                .as_deref(),
                Some("cli-refresh-a1"),
                "A's CLI generation must be adopted before B overwrites auth.json"
            );
            let live_b: Value =
                read_json_file(&crate::codex_config::get_codex_auth_path()).expect("read B auth");
            assert_eq!(
                live_b.pointer("/tokens/account_id").and_then(Value::as_str),
                Some("acct-b")
            );

            let id_token_b = crate::codex_config::test_codex_id_token("user-b");
            write_json_file(
                &crate::codex_config::get_codex_auth_path(),
                &crate::codex_config::codex_managed_oauth_auth_value(
                    "acct-b",
                    "cli-access-b1",
                    Some(&id_token_b),
                    "cli-refresh-b1",
                    "2099-01-03T00:00:00Z",
                ),
            )
            .expect("rotate account B live auth");

            ProviderService::switch(state, AppType::Codex, &third_party.id)
                .expect("switch managed B to API-key provider");
            assert_eq!(
                tauri::async_runtime::block_on(
                    state
                        .codex_oauth_manager
                        .test_refresh_token_for_account("acct-b")
                )
                .as_deref(),
                Some("cli-refresh-b1"),
                "B's CLI generation must be adopted before the third-party switch removes auth.json"
            );
            assert!(
                !crate::codex_config::get_codex_auth_path().exists(),
                "third-party switches are config-only: auth.json is removed"
            );
            let live_config = std::fs::read_to_string(crate::codex_config::get_codex_config_path())
                .expect("read third-party config");
            assert!(
                live_config.contains("experimental_bearer_token = \"sk-third-party\""),
                "the third-party key rides in config.toml; got:\n{live_config}"
            );
        });
    }

    #[test]
    #[serial]
    fn managed_codex_direct_update_adopts_outgoing_cli_rotation_and_commits_target_binding() {
        with_test_home(|state, _| {
            crate::settings::reload_settings().expect("reload settings");
            tauri::async_runtime::block_on(async {
                state
                    .codex_oauth_manager
                    .add_test_account_with_user_identity("acct-a", "managed-access-a", "user-a")
                    .await
                    .expect("seed account A");
                state
                    .codex_oauth_manager
                    .add_test_account_with_user_identity("acct-b", "managed-access-b", "user-b")
                    .await
                    .expect("seed account B");
            });

            let provider = managed_codex_provider("managed-official-a", "acct-a");
            state
                .db
                .save_provider(AppType::Codex.as_str(), &provider)
                .expect("save managed official provider");
            ProviderService::switch(state, AppType::Codex, &provider.id)
                .expect("activate managed account A");
            assert!(
                tauri::async_runtime::block_on(state.db.get_live_backup(AppType::Codex.as_str()))
                    .expect("read initial live backup")
                    .is_none(),
                "direct update precondition requires no takeover backup"
            );

            let id_token_a = crate::codex_config::test_codex_id_token("user-a");
            write_json_file(
                &crate::codex_config::get_codex_auth_path(),
                &crate::codex_config::codex_managed_oauth_auth_value(
                    "acct-a",
                    "cli-access-a1",
                    Some(&id_token_a),
                    "cli-refresh-a1",
                    "2099-03-01T00:00:00Z",
                ),
            )
            .expect("simulate account A CLI rotation");

            let mut updated = provider.clone();
            updated.name = "OpenAI Official B".to_string();
            updated
                .meta
                .as_mut()
                .and_then(|meta| meta.auth_binding.as_mut())
                .expect("managed binding")
                .account_id = Some("acct-b".to_string());

            ProviderService::update(state, AppType::Codex, None, updated.clone())
                .expect("directly update managed binding from A to B");

            assert_eq!(
                tauri::async_runtime::block_on(
                    state
                        .codex_oauth_manager
                        .test_refresh_token_for_account("acct-a")
                )
                .as_deref(),
                Some("cli-refresh-a1"),
                "direct update must adopt A's CLI generation before overwriting live auth"
            );
            let saved = state
                .db
                .get_provider_by_id(&provider.id, AppType::Codex.as_str())
                .expect("read updated provider")
                .expect("updated provider exists");
            assert_eq!(saved.name, updated.name);
            assert_eq!(
                saved
                    .meta
                    .as_ref()
                    .and_then(|meta| meta.managed_account_id_for("codex_oauth"))
                    .as_deref(),
                Some("acct-b")
            );

            let live_b: Value = read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read account B live auth");
            assert_eq!(
                live_b.pointer("/tokens/account_id").and_then(Value::as_str),
                Some("acct-b")
            );
            assert!(
                crate::codex_config::codex_auth_matches_recorded_managed_oauth(&live_b, "acct-b")
                    .expect("check account B marker"),
                "clearing outgoing account A must not remove account B's marker"
            );
            assert!(
                tauri::async_runtime::block_on(state.db.get_live_backup(AppType::Codex.as_str()))
                    .expect("read live backup after direct update")
                    .is_none(),
                "direct update must not create a takeover backup"
            );
        });
    }

    #[test]
    #[serial]
    fn same_account_managed_codex_update_rejects_equal_timestamp_refresh_conflict() {
        with_test_home(|state, _| {
            crate::settings::reload_settings().expect("reload settings");
            tauri::async_runtime::block_on(async {
                state
                    .codex_oauth_manager
                    .add_test_account_with_user_identity(
                        "acct-managed",
                        "managed-access",
                        "managed-user",
                    )
                    .await
                    .expect("seed managed account");
            });

            let provider = managed_codex_provider("managed-same-account", "acct-managed");
            state
                .db
                .save_provider(AppType::Codex.as_str(), &provider)
                .expect("save managed provider");
            ProviderService::switch(state, AppType::Codex, &provider.id)
                .expect("activate managed provider");

            // Different refresh material at the exact manager generation
            // timestamp is ambiguous at millisecond precision. A same-account
            // update has no outgoing-account guard, so its managed bundle
            // preflight itself must refuse to overwrite this CLI generation.
            tauri::async_runtime::block_on(
                state
                    .codex_oauth_manager
                    .test_set_token_updated_at_ms("acct-managed", 1_700_000_000_000),
            );
            let id_token = crate::codex_config::test_codex_id_token("managed-user");
            let cli_live_auth = crate::codex_config::codex_managed_oauth_auth_value(
                "acct-managed",
                "cli-access-r1",
                Some(&id_token),
                "cli-refresh-r1",
                "2023-11-14T22:13:20Z",
            );
            write_json_file(&crate::codex_config::get_codex_auth_path(), &cli_live_auth)
                .expect("seed equal-timestamp CLI generation");

            let mut updated = provider.clone();
            updated.name = "Managed updated".to_string();
            let error = ProviderService::update(state, AppType::Codex, None, updated)
                .expect_err("ambiguous same-account generation must block the live write");
            assert!(
                error
                    .to_string()
                    .contains("无法安全判断 refresh token 新旧"),
                "update should explain the safe-write rejection: {error}"
            );

            let live_after: Value = read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read preserved CLI auth");
            assert_eq!(
                live_after, cli_live_auth,
                "same-account managed update must not overwrite ambiguous CLI token material"
            );
            assert_eq!(
                tauri::async_runtime::block_on(
                    state
                        .codex_oauth_manager
                        .test_refresh_token_for_account("acct-managed")
                )
                .as_deref(),
                Some("test-refresh-token"),
                "ambiguous CLI material must not replace the manager generation either"
            );
            assert_eq!(
                state
                    .db
                    .get_provider_by_id(&provider.id, AppType::Codex.as_str())
                    .expect("read provider after rejected update")
                    .expect("provider remains present")
                    .name,
                provider.name,
                "rejected preflight must leave the provider row unchanged"
            );
        });
    }

    #[test]
    #[serial]
    fn switch_away_rejects_legacy_refresh_conflict_on_every_retry() {
        with_test_home(|state, _| {
            crate::settings::reload_settings().expect("reload settings");
            tauri::async_runtime::block_on(async {
                state
                    .codex_oauth_manager
                    .add_test_account_with_user_identity(
                        "acct-legacy",
                        "managed-access",
                        "legacy-user",
                    )
                    .await
                    .expect("seed managed account");
            });

            let managed = managed_codex_provider("managed-legacy", "acct-legacy");
            let mut unbound = Provider::with_id(
                "unbound-official".to_string(),
                "Unbound Official".to_string(),
                json!({
                    "auth": {},
                    "config": ""
                }),
                None,
            );
            unbound.category = Some("official".to_string());
            for provider in [&managed, &unbound] {
                state
                    .db
                    .save_provider(AppType::Codex.as_str(), provider)
                    .expect("save provider");
            }
            ProviderService::switch(state, AppType::Codex, &managed.id)
                .expect("activate managed provider");

            tauri::async_runtime::block_on(
                state
                    .codex_oauth_manager
                    .test_set_token_updated_at_ms("acct-legacy", 0),
            );
            let id_token = crate::codex_config::test_codex_id_token("legacy-user");
            let cli_live_auth = crate::codex_config::codex_managed_oauth_auth_value(
                "acct-legacy",
                "cli-access-r1",
                Some(&id_token),
                "cli-refresh-r1",
                "2023-11-14T22:13:20Z",
            );
            write_json_file(&crate::codex_config::get_codex_auth_path(), &cli_live_auth)
                .expect("seed CLI generation against legacy manager state");

            for attempt in 1..=2 {
                let error = ProviderService::switch(state, AppType::Codex, &unbound.id)
                    .expect_err("legacy conflict must block every switch-away retry");
                assert!(
                    error
                        .to_string()
                        .contains("无法安全判断 refresh token 新旧"),
                    "attempt {attempt} should remain ambiguous: {error}"
                );
                let live_after: Value = read_json_file(&crate::codex_config::get_codex_auth_path())
                    .expect("read preserved CLI auth");
                assert_eq!(
                    live_after, cli_live_auth,
                    "attempt {attempt} must not overwrite or delete the CLI generation"
                );
                assert_eq!(
                    state
                        .db
                        .get_current_provider(AppType::Codex.as_str())
                        .expect("read current provider")
                        .as_deref(),
                    Some(managed.id.as_str()),
                    "attempt {attempt} must not commit the target provider"
                );
            }

            assert_eq!(
                tauri::async_runtime::block_on(
                    state
                        .codex_oauth_manager
                        .test_refresh_token_for_account("acct-legacy")
                )
                .as_deref(),
                Some("test-refresh-token"),
                "ambiguous legacy retries must keep manager material unchanged"
            );
        });
    }

    #[test]
    #[serial]
    fn codex_auth_center_remove_and_logout_clear_live_credentials_and_marker() {
        with_test_home(|state, _| {
            crate::settings::reload_settings().expect("reload settings");
            tauri::async_runtime::block_on(async {
                state
                    .codex_oauth_manager
                    .add_test_account_with_user_identity(
                        "acct-managed",
                        "managed-access",
                        "managed-user",
                    )
                    .await
                    .expect("seed managed account");
            });
            let provider = managed_codex_provider("managed-auth-center", "acct-managed");
            state
                .db
                .save_provider(AppType::Codex.as_str(), &provider)
                .expect("save managed provider");
            ProviderService::switch(state, AppType::Codex, &provider.id)
                .expect("activate managed provider");
            assert!(crate::codex_config::get_codex_auth_path().exists());
            assert!(crate::codex_config::codex_managed_oauth_live_auth_marker_exists());

            tauri::async_runtime::block_on(
                state.codex_oauth_manager.remove_account("acct-managed"),
            )
            .expect("remove managed account");
            assert!(
                !crate::codex_config::get_codex_auth_path().exists(),
                "removing the active account must delete its refreshable live auth"
            );
            assert!(
                !crate::codex_config::codex_managed_oauth_live_auth_marker_exists(),
                "removing the active account must delete its marker"
            );
            assert_eq!(
                state
                    .db
                    .get_provider_by_id(&provider.id, AppType::Codex.as_str())
                    .expect("read provider")
                    .and_then(|provider| provider.meta)
                    .and_then(|meta| meta.managed_account_id_for("codex_oauth"))
                    .as_deref(),
                Some("acct-managed"),
                "the binding is retained so re-login with the same account can recover it"
            );

            tauri::async_runtime::block_on(async {
                state
                    .codex_oauth_manager
                    .add_test_account_with_user_identity(
                        "acct-managed",
                        "managed-access-2",
                        "managed-user",
                    )
                    .await
                    .expect("re-login managed account");
            });
            ProviderService::switch(state, AppType::Codex, &provider.id)
                .expect("reactivate managed provider after re-login");
            assert!(crate::codex_config::get_codex_auth_path().exists());

            tauri::async_runtime::block_on(state.codex_oauth_manager.clear_auth())
                .expect("logout all managed accounts");
            assert!(!crate::codex_config::get_codex_auth_path().exists());
            assert!(!crate::codex_config::codex_managed_oauth_live_auth_marker_exists());
            assert!(
                tauri::async_runtime::block_on(state.codex_oauth_manager.list_accounts())
                    .is_empty()
            );
        });
    }

    #[test]
    #[serial]
    fn deleted_codex_account_can_rebind_or_switch_in_either_mode() {
        for mode in ["direct", "proxy"] {
            for rebind in [true, false] {
                with_test_home(|state, _| {
                    crate::settings::reload_settings().unwrap();
                    let runtime = tauri::async_runtime::handle();
                    let token = crate::codex_config::test_codex_id_token("same-user");
                    runtime
                        .block_on(
                            state
                                .codex_oauth_manager
                                .add_test_account_with_workspace_and_access_token(
                                    "old-local-id",
                                    "workspace",
                                    "old-access",
                                    Some(&token),
                                ),
                        )
                        .unwrap();
                    runtime.block_on(async {
                        let mut config = state.db.get_proxy_config().await.unwrap();
                        config.listen_port = 0;
                        state.db.update_proxy_config(config).await.unwrap();
                    });
                    let current = managed_codex_provider("current", "old-local-id");
                    state.db.save_provider("codex", &current).unwrap();
                    ProviderService::switch(state, AppType::Codex, &current.id).unwrap();
                    if mode == "proxy" {
                        runtime
                            .block_on(crate::mode::controller::enter(state, &AppType::Codex))
                            .unwrap();
                    }
                    runtime
                        .block_on(
                            crate::commands::remove_codex_oauth_account_with_switch_lock(
                                state,
                                "old-local-id",
                            ),
                        )
                        .unwrap();
                    let restarted = (mode == "direct").then(|| AppState::new(state.db.clone()));
                    let state = restarted.as_ref().unwrap_or(state);
                    // Ordinary login creates a new local ID, even for the same user/workspace.
                    runtime
                        .block_on(
                            state
                                .codex_oauth_manager
                                .add_test_account_with_workspace_and_access_token(
                                    "new-local-id",
                                    "workspace",
                                    "new-access",
                                    Some(&token),
                                ),
                        )
                        .unwrap();
                    assert_eq!(
                        ProviderService::managed_codex_oauth_account_id(
                            &state
                                .db
                                .get_provider_by_id("current", "codex")
                                .unwrap()
                                .unwrap()
                        )
                        .as_deref(),
                        Some("old-local-id")
                    );

                    // 绑定已失效：直连下进入路由要报错让用户重新绑定，客户端文件不动。
                    if mode == "direct" {
                        let error = runtime
                            .block_on(crate::mode::controller::enter(state, &AppType::Codex))
                            .unwrap_err();
                        assert!(error.contains("选择账号"), "{error}");
                        assert!(!crate::mode::current::is_proxy(&AppType::Codex));
                        assert!(!state
                            .proxy_service
                            .live_has_proxy_placeholder(&AppType::Codex));
                    }

                    let target = managed_codex_provider(
                        if rebind { "current" } else { "target" },
                        "new-local-id",
                    );
                    if rebind {
                        ProviderService::update(state, AppType::Codex, None, target.clone())
                            .unwrap();
                    } else {
                        state.db.save_provider("codex", &target).unwrap();
                        ProviderService::switch(state, AppType::Codex, &target.id).unwrap();
                    }
                    let auth: Value = read_json_file(&crate::codex_config::get_codex_auth_path())
                        .unwrap_or_else(|error| panic!("{mode}, rebind={rebind}: {error}"));
                    assert_eq!(
                        auth["tokens"]["access_token"], "new-access",
                        "{mode}, rebind={rebind}"
                    );
                    assert_eq!(
                        crate::mode::current::provider_for(
                            &state.db,
                            &AppType::Codex,
                            crate::mode::current::Purpose::InUse,
                        )
                        .unwrap()
                        .as_deref(),
                        Some(target.id.as_str())
                    );

                    if mode == "proxy" && !rebind {
                        // 路由换了，直连指针仍是失效的那家：在路由模式下改它的绑定只存行，
                        // 退出路由时再写回。
                        ProviderService::update(
                            state,
                            AppType::Codex,
                            None,
                            managed_codex_provider("current", "new-local-id"),
                        )
                        .unwrap();
                    }
                    runtime.block_on(async {
                        if mode == "direct" {
                            crate::mode::controller::enter(state, &AppType::Codex)
                                .await
                                .unwrap();
                        }
                        crate::mode::controller::exit(state, &AppType::Codex)
                            .await
                            .unwrap();
                        assert!(!state.proxy_service.is_running().await);
                    });
                    let restored: Value =
                        read_json_file(&crate::codex_config::get_codex_auth_path()).unwrap();
                    assert_eq!(
                        restored["tokens"]["access_token"], "new-access",
                        "leaving routing mode must keep the replacement account ({mode}, rebind={rebind})"
                    );
                });
            }
        }
    }

    #[test]
    #[serial]
    fn deleted_codex_account_recovers_after_persisted_startup() {
        for exit_state in ["detached", "crashed"] {
            let _guard = test_guard();
            let _home = TempHome::new();
            crate::settings::reload_settings().unwrap();
            let runtime = tauri::async_runtime::handle();
            let state = AppState::new(Arc::new(Database::init().unwrap()));
            let token = crate::codex_config::test_codex_id_token("same-user");
            runtime.block_on(async {
                state
                    .codex_oauth_manager
                    .add_test_account_with_workspace_and_access_token(
                        "old-local-id",
                        "workspace",
                        "old-access",
                        Some(&token),
                    )
                    .await
                    .unwrap();
                let mut config = state.db.get_proxy_config().await.unwrap();
                config.listen_port = 0;
                state.db.update_proxy_config(config).await.unwrap();
            });
            let current = managed_codex_provider("current", "old-local-id");
            state.db.save_provider("codex", &current).unwrap();
            ProviderService::switch(&state, AppType::Codex, "current").unwrap();
            runtime.block_on(async {
                crate::mode::controller::enter(&state, &AppType::Codex)
                    .await
                    .unwrap();
                crate::commands::remove_codex_oauth_account_with_switch_lock(
                    &state,
                    "old-local-id",
                )
                .await
                .unwrap();
                state
                    .codex_oauth_manager
                    .add_test_account_with_workspace_and_access_token(
                        "new-local-id",
                        "workspace",
                        "new-access",
                        Some(&token),
                    )
                    .await
                    .unwrap();
            });
            // Model a later CLI login. Startup recovery must leave it intact.
            let native_auth = crate::codex_config::codex_managed_oauth_auth_value(
                "native-workspace",
                "native-access",
                Some(&token),
                "native-refresh",
                "2026-09-14T00:00:00Z",
            );
            write_json_file(&crate::codex_config::get_codex_auth_path(), &native_auth).unwrap();
            runtime.block_on(async {
                if exit_state == "detached" {
                    // 直连供应商的账号没了写不出来：只清掉占位符，登录不动。
                    crate::mode::controller::detach_all(&state).await;
                    assert!(!state
                        .proxy_service
                        .live_has_proxy_placeholder(&AppType::Codex));
                } else {
                    // 崩溃：客户端仍指着代理，只是监听没了。
                    state.proxy_service.stop().await.unwrap();
                    assert!(state
                        .proxy_service
                        .live_has_proxy_placeholder(&AppType::Codex));
                }
                assert!(crate::mode::current::is_proxy(&AppType::Codex));
            });
            drop(state);

            crate::settings::reload_settings().unwrap();
            let restarted = AppState::new(Arc::new(Database::init().unwrap()));
            assert_eq!(
                ProviderService::managed_codex_oauth_account_id(
                    &restarted
                        .db
                        .get_provider_by_id("current", "codex")
                        .unwrap()
                        .unwrap()
                )
                .as_deref(),
                Some("old-local-id"),
            );
            runtime.block_on(async {
                let accounts = restarted.codex_oauth_manager.list_accounts().await;
                assert_eq!(accounts.len(), 1);
                assert_eq!(accounts[0].id, "new-local-id");
                let store_path = crate::config::get_app_config_dir().join("codex_oauth_auth.json");
                let persisted_accounts = fs::read(&store_path).unwrap();
                // Match setup's ordering: extract common config from the direct live
                // file before re-attaching routing mode.
                crate::initialize_common_config_snippets(&restarted);
                crate::mode::controller::startup(&restarted).await;
                // 路由的账号失效，接不上就退回直连。
                assert!(
                    !crate::mode::current::is_proxy(&AppType::Codex),
                    "{exit_state}"
                );
                assert!(
                    !restarted
                        .db
                        .get_proxy_config_for_app("codex")
                        .await
                        .unwrap()
                        .enabled
                );
                assert!(!restarted.proxy_service.is_running().await);
                assert!(!restarted
                    .proxy_service
                    .live_has_proxy_placeholder(&AppType::Codex));
                assert_eq!(fs::read(&store_path).unwrap(), persisted_accounts);
                assert_eq!(
                    read_json_file::<Value>(&crate::codex_config::get_codex_auth_path()).unwrap(),
                    native_auth
                );
                // Stub the network refresh only after proving the account survived on disk.
                restarted
                    .codex_oauth_manager
                    .test_cache_access_token("new-local-id", "new-access")
                    .await;
            });
            let target = managed_codex_provider("current", "new-local-id");
            ProviderService::update(&restarted, AppType::Codex, None, target).unwrap();
            runtime.block_on(async {
                crate::mode::controller::enter(&restarted, &AppType::Codex)
                    .await
                    .unwrap();
                assert!(
                    restarted
                        .db
                        .get_proxy_config_for_app("codex")
                        .await
                        .unwrap()
                        .enabled
                );
                assert!(restarted
                    .proxy_service
                    .live_has_proxy_placeholder(&AppType::Codex));
                crate::mode::controller::exit(&restarted, &AppType::Codex)
                    .await
                    .unwrap();
                assert!(!restarted.proxy_service.is_running().await);
            });
            let auth: Value = read_json_file(&crate::codex_config::get_codex_auth_path()).unwrap();
            assert_eq!(auth["tokens"]["access_token"], "new-access", "{exit_state}");
        }
    }

    #[test]
    #[serial]
    fn codex_auth_center_removal_waits_for_provider_switch_lock() {
        with_test_home(|state, _| {
            crate::settings::reload_settings().expect("reload settings");
            tauri::async_runtime::block_on(async {
                state
                    .codex_oauth_manager
                    .add_test_account_with_user_identity(
                        "acct-managed",
                        "managed-access",
                        "managed-user",
                    )
                    .await
                    .expect("seed managed account");
            });

            let switch_guard = tauri::async_runtime::block_on(
                state
                    .proxy_service
                    .lock_switch_for_app(AppType::Codex.as_str()),
            );
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    started_tx.send(()).expect("signal removal start");
                    let result = tauri::async_runtime::block_on(
                        crate::commands::remove_codex_oauth_account_with_switch_lock(
                            state,
                            "acct-managed",
                        ),
                    );
                    done_tx.send(result).expect("send removal result");
                });
                started_rx.recv().expect("wait for removal task");
                assert!(
                    done_rx
                        .recv_timeout(std::time::Duration::from_millis(100))
                        .is_err(),
                    "Auth Center removal must wait while a provider transaction owns the Codex lock"
                );
                drop(switch_guard);
                done_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("removal should finish after lock release")
                    .expect("remove managed account");
            });
            assert!(
                tauri::async_runtime::block_on(state.codex_oauth_manager.list_accounts())
                    .is_empty()
            );
        });
    }

    #[test]
    #[serial]
    fn codex_auth_center_logout_waits_for_provider_switch_lock() {
        with_test_home(|state, _| {
            crate::settings::reload_settings().expect("reload settings");
            tauri::async_runtime::block_on(async {
                state
                    .codex_oauth_manager
                    .add_test_account_with_user_identity(
                        "acct-managed",
                        "managed-access",
                        "managed-user",
                    )
                    .await
                    .expect("seed managed account");
            });
            let provider = managed_codex_provider("managed-logout-lock", "acct-managed");
            state
                .db
                .save_provider(AppType::Codex.as_str(), &provider)
                .expect("save managed provider");
            ProviderService::switch(state, AppType::Codex, &provider.id)
                .expect("activate managed provider");
            assert!(crate::codex_config::get_codex_auth_path().exists());
            assert!(crate::codex_config::codex_managed_oauth_live_auth_marker_exists());

            let switch_guard = tauri::async_runtime::block_on(
                state
                    .proxy_service
                    .lock_switch_for_app(AppType::Codex.as_str()),
            );
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    started_tx.send(()).expect("signal logout start");
                    let result = tauri::async_runtime::block_on(
                        crate::commands::logout_codex_oauth_with_switch_lock(state),
                    );
                    done_tx.send(result).expect("send logout result");
                });
                started_rx.recv().expect("wait for logout task");
                assert!(
                    done_rx
                        .recv_timeout(std::time::Duration::from_millis(100))
                        .is_err(),
                    "Auth Center logout must wait while a provider transaction owns the Codex lock"
                );
                drop(switch_guard);
                done_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("logout should finish after lock release")
                    .expect("logout managed accounts");
            });

            assert!(
                tauri::async_runtime::block_on(state.codex_oauth_manager.list_accounts())
                    .is_empty()
            );
            assert!(
                !crate::codex_config::get_codex_auth_path().exists(),
                "logout must clear the active managed live auth"
            );
            assert!(
                !crate::codex_config::codex_managed_oauth_live_auth_marker_exists(),
                "logout must clear the managed live auth marker"
            );
        });
    }

    #[test]
    #[serial]
    fn switch_to_managed_codex_official_with_unresolvable_account_keeps_current_unchanged() {
        with_test_home(|state, _| {
            crate::settings::reload_settings().expect("reload settings");

            // 基线：一个普通第三方 provider，可正常切换，作为初始 current。
            // config 必须带自定义 provider 表：config-only 切换要求 key 有
            // provider 级落点。
            let mut baseline = Provider::with_id(
                "baseline".to_string(),
                "Baseline".to_string(),
                json!({
                    "auth": { "OPENAI_API_KEY": "sk-baseline" },
                    "config": "model_provider = \"baseline\"\n[model_providers.baseline]\nbase_url = \"https://baseline.example/v1\"\n"
                }),
                None,
            );
            baseline.category = Some("custom".to_string());

            // 托管 official provider，绑定一个 manager 中不存在的账号：切换预检
            // 取 token 必然失败。
            let mut managed = Provider::with_id(
                "managed-official".to_string(),
                "Managed Official".to_string(),
                json!({ "auth": {}, "config": "" }),
                None,
            );
            managed.category = Some("official".to_string());
            managed.meta = Some(ProviderMeta {
                auth_binding: Some(AuthBinding {
                    source: AuthBindingSource::ManagedAccount,
                    auth_provider: Some("codex_oauth".to_string()),
                    account_id: Some("acct-missing".to_string()),
                }),
                ..Default::default()
            });

            state
                .db
                .save_provider(AppType::Codex.as_str(), &baseline)
                .expect("save baseline");
            state
                .db
                .save_provider(AppType::Codex.as_str(), &managed)
                .expect("save managed");

            ProviderService::switch(state, AppType::Codex, "baseline").expect("switch to baseline");

            // 切到绑定了不存在账号的托管 provider：预检失败 → 返回 Err。
            let result = ProviderService::switch(state, AppType::Codex, "managed-official");
            assert!(
                result.is_err(),
                "switch must fail when the managed OAuth token cannot be resolved"
            );

            // current 必须仍是 baseline：预检在提交 current 之前失败，不留下
            // 「DB/UI 指向新 provider、但 live 仍是旧 provider」的不一致状态。
            let current =
                crate::settings::get_effective_current_provider(&state.db, &AppType::Codex)
                    .expect("read current");
            assert_eq!(
                current.as_deref(),
                Some("baseline"),
                "a failed managed switch must not move current off the previous provider"
            );
        });
    }

    #[test]
    #[serial]
    fn sync_universal_to_apps_preserves_child_metadata() {
        with_test_home(|state, _home| {
            let mut universal = UniversalProvider::new(
                "metadata".into(),
                "Original".into(),
                "custom".into(),
                "https://old.example".into(),
                "old-key".into(),
            );
            universal.apps.claude = true;
            universal.apps.codex = true;
            universal.meta = Some(
                serde_json::from_value(json!({
                    "usage_script": {"enabled": false, "language": "javascript", "code": "parent"}
                }))
                .unwrap(),
            );
            state.db.save_universal_provider(&universal).unwrap();
            ProviderService::sync_universal_to_apps(state, &universal.id).unwrap();

            let mut expected = Vec::new();
            for (index, app) in ["claude", "codex"].iter().enumerate() {
                let id = format!("universal-{app}-metadata");
                let mut child = state.db.get_provider_by_id(&id, app).unwrap().unwrap();
                assert_eq!(
                    serde_json::to_value(&child.meta).unwrap(),
                    serde_json::to_value(&universal.meta).unwrap()
                );
                child.meta = Some(
                    serde_json::from_value(json!({
                        "usage_script": {"enabled": true, "language": "javascript", "code": app,
                            "apiKey": "usage-only-key", "autoQueryInterval": 15},
                        "commonConfigEnabled": false,
                        "endpointAutoSelect": true
                    }))
                    .unwrap(),
                );
                child.created_at = Some(123 + index as i64);
                child.sort_index = Some(10 + index);
                child.settings_config["local_setting"] = json!(app);
                state.db.save_provider(app, &child).unwrap();
                state
                    .db
                    .add_custom_endpoint(app, &id, "https://extra.example")
                    .unwrap();
                expected.push(child);
            }

            universal.name = "Updated".into();
            universal.base_url = "https://new.example".into();
            universal.api_key = "new-key".into();
            universal.notes = Some("shared note".into());
            universal.models = serde_json::from_value(json!({
                "claude": {"model": "claude-new"},
                "codex": {"model": "codex-new", "reasoningEffort": "low"}
            }))
            .unwrap();
            // Both absent and present parent metadata must not overwrite child settings.
            for parent_meta in [None, universal.meta.clone()] {
                universal.meta = parent_meta;
                state.db.save_universal_provider(&universal).unwrap();
                ProviderService::sync_universal_to_apps(state, &universal.id).unwrap();
                for (app, before) in ["claude", "codex"].iter().zip(&expected) {
                    let after = state
                        .db
                        .get_provider_by_id(&before.id, app)
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        serde_json::to_value(&after.meta).unwrap(),
                        serde_json::to_value(&before.meta).unwrap(),
                        "{app}"
                    );
                    assert_eq!(after.created_at, before.created_at, "{app}");
                    assert_eq!(after.sort_index, before.sort_index, "{app}");
                    assert_eq!(after.name, "Updated");
                    assert_eq!(after.notes.as_deref(), Some("shared note"));
                    assert_eq!(after.settings_config["local_setting"], json!(app));
                    let generated = match *app {
                        "claude" => universal.to_claude_provider(),
                        _ => universal.to_codex_provider(),
                    }
                    .unwrap();
                    let mut expected_settings = before.settings_config.clone();
                    ProviderService::merge_json(&mut expected_settings, &generated.settings_config);
                    assert_eq!(after.settings_config, expected_settings);
                    assert_eq!(
                        state.db.get_all_providers(app).unwrap()[&before.id]
                            .meta
                            .as_ref()
                            .unwrap()
                            .custom_endpoints
                            .len(),
                        1
                    );
                }
            }
        });
    }

    #[test]
    #[serial]
    fn sync_universal_to_apps_reprojects_current_child_to_live() {
        with_test_home(|state, _home| {
            let mut universal = UniversalProvider::new(
                "shared".to_string(),
                "Shared Relay".to_string(),
                "custom".to_string(),
                "https://api.new.example".to_string(),
                "new-key".to_string(),
            );
            universal.apps.claude = true;
            universal.models.claude = Some(ClaudeModelConfig {
                model: Some("claude-sonnet-4".to_string()),
                ..Default::default()
            });
            state
                .db
                .save_universal_provider(&universal)
                .expect("save universal provider");

            let child = universal
                .to_claude_provider()
                .expect("claude child provider");
            state
                .db
                .save_provider("claude", &child)
                .expect("seed child provider");
            state
                .db
                .set_current_provider("claude", &child.id)
                .expect("set current child");
            crate::settings::set_current_provider(&AppType::Claude, Some(&child.id))
                .expect("set local current child");

            let mut old_live = child.settings_config.clone();
            old_live["env"]["ANTHROPIC_BASE_URL"] =
                Value::String("https://api.old.example".to_string());
            write_json_file(&get_claude_settings_path(), &old_live).expect("seed old live");

            ProviderService::sync_universal_to_apps(state, "shared")
                .expect("sync universal provider");

            let live: Value = read_json_file(&get_claude_settings_path()).expect("read live");
            assert_eq!(
                live["env"]["ANTHROPIC_BASE_URL"].as_str(),
                Some("https://api.new.example")
            );
            assert_eq!(
                live["env"]["ANTHROPIC_AUTH_TOKEN"].as_str(),
                Some("new-key")
            );
        });
    }

    #[test]
    #[serial]
    fn add_first_managed_codex_with_missing_account_leaves_no_provider_or_live_state() {
        with_test_home(|state, _| {
            crate::settings::reload_settings().expect("reload settings");
            let provider = managed_codex_provider("managed-missing", "acct-missing");
            let live_before = crate::codex_config::CodexLiveStateSnapshot::capture()
                .expect("capture empty Codex live state");

            ProviderService::add(state, AppType::Codex, provider.clone(), false)
                .expect_err("missing managed account should fail before add commits");

            assert!(
                state
                    .db
                    .get_provider_by_id(&provider.id, AppType::Codex.as_str())
                    .expect("query failed managed add")
                    .is_none(),
                "failed preflight must not leave an orphan provider row"
            );
            assert_eq!(
                state
                    .db
                    .get_current_provider(AppType::Codex.as_str())
                    .expect("read current after failed add"),
                None
            );
            assert_eq!(
                crate::codex_config::CodexLiveStateSnapshot::capture()
                    .expect("capture Codex live after failed add"),
                live_before,
                "failed preflight must not mutate Codex live files"
            );
        });
    }

    #[test]
    #[serial]
    fn add_first_managed_codex_current_failure_rolls_forward_on_recovery() {
        with_test_home(|state, _| {
            crate::settings::reload_settings().expect("reload settings");
            tauri::async_runtime::block_on(async {
                state
                    .codex_oauth_manager
                    .add_test_account_with_user_identity(
                        "acct-managed",
                        "managed-token",
                        "managed-user",
                    )
                    .await
                    .expect("seed managed Codex OAuth account");
            });

            let provider = managed_codex_provider("managed-first", "acct-managed");
            let live_before = crate::codex_config::CodexLiveStateSnapshot::capture()
                .expect("capture empty Codex live state");
            {
                let conn = state.db.conn.lock().expect("lock database");
                conn.execute_batch(
                    "CREATE TRIGGER reject_first_managed_current_update
                     BEFORE UPDATE OF is_current ON providers
                     WHEN NEW.app_type = 'codex'
                       AND NEW.id = 'managed-first'
                       AND NEW.is_current = 1
                     BEGIN
                       SELECT RAISE(ABORT, 'forced first managed Codex current failure');
                     END;",
                )
                .expect("install first-current failure trigger");
            }

            let error = ProviderService::add(state, AppType::Codex, provider.clone(), false)
                .expect_err("DB current failure surfaces");
            assert!(
                error
                    .to_string()
                    .contains("forced first managed Codex current failure"),
                "add should surface the DB current failure, got: {error}"
            );
            // 文件已经发布：行留着，pending 等着补完指针，live 不回滚。
            assert!(state
                .db
                .get_provider_by_id(&provider.id, AppType::Codex.as_str())
                .expect("query provider")
                .is_some());
            assert!(crate::mode::operation::has_pending(AppType::Codex.as_str()));
            assert_ne!(
                crate::codex_config::CodexLiveStateSnapshot::capture().expect("capture live"),
                live_before
            );

            state
                .db
                .conn
                .lock()
                .expect("lock database")
                .execute_batch("DROP TRIGGER reject_first_managed_current_update;")
                .expect("drop trigger");
            crate::mode::operation::recover_on_startup(&state.db);
            assert!(!crate::mode::operation::has_pending(
                AppType::Codex.as_str()
            ));
            assert_eq!(
                state
                    .db
                    .get_current_provider(AppType::Codex.as_str())
                    .expect("read current after recovery")
                    .as_deref(),
                Some(provider.id.as_str())
            );
        });
    }

    #[test]
    #[serial]
    fn missing_codex_account_preserves_native_login_but_corrupt_store_blocks_recovery() {
        for takeover in [false, true] {
            for corrupt in [false, true] {
                with_test_home(|state, _| {
                    crate::settings::reload_settings().unwrap();
                    crate::settings::update_settings(crate::settings::AppSettings {
                        preserve_codex_official_auth_on_switch: true,
                        ..Default::default()
                    })
                    .unwrap();
                    let runtime = tauri::async_runtime::handle();
                    runtime
                        .block_on(
                            state
                                .codex_oauth_manager
                                .add_test_account_with_user_identity("old", "access", "user"),
                        )
                        .unwrap();
                    let current = managed_codex_provider("current", "old");
                    state.db.save_provider("codex", &current).unwrap();
                    ProviderService::switch(state, AppType::Codex, "current").unwrap();
                    let mut auth: Value =
                        read_json_file(&crate::codex_config::get_codex_auth_path()).unwrap();
                    if takeover {
                        runtime.block_on(async {
                            // 端口 0 启动后实际端口会记进库，重开的 AppState 不启动代理
                            // 也能算出契约地址。
                            let mut config = state.db.get_proxy_config().await.unwrap();
                            config.listen_port = 0;
                            state.db.update_proxy_config(config).await.unwrap();
                            crate::mode::controller::enter(state, &AppType::Codex)
                                .await
                                .unwrap();
                        });
                    }
                    runtime
                        .block_on(
                            crate::commands::remove_codex_oauth_account_with_switch_lock(
                                state, "old",
                            ),
                        )
                        .unwrap();
                    // A stale marker must not claim a later native login of the same user.
                    auth["tokens"]["refresh_token"] = json!("native-rotated-token");
                    write_json_file(&crate::codex_config::get_codex_auth_path(), &auth).unwrap();
                    crate::codex_config::record_codex_managed_oauth_live_auth(&auth, "old")
                        .unwrap();
                    if corrupt {
                        fs::write(
                            crate::config::get_app_config_dir().join("codex_oauth_auth.json"),
                            "{broken",
                        )
                        .unwrap();
                    }
                    let restarted = AppState::new(state.db.clone());
                    let target = Provider::with_id(
                        "target".into(),
                        "Third party".into(),
                        codex_settings("https://example.test/v1", "sk-target"),
                        None,
                    );
                    state.db.save_provider("codex", &target).unwrap();
                    let before = crate::codex_config::CodexLiveStateSnapshot::capture().unwrap();
                    let result = ProviderService::switch(&restarted, AppType::Codex, "target");
                    if corrupt {
                        assert!(result.is_err());
                        assert_eq!(
                            crate::codex_config::CodexLiveStateSnapshot::capture().unwrap(),
                            before
                        );
                        assert_eq!(
                            crate::mode::current::provider_for(
                                &state.db,
                                &AppType::Codex,
                                crate::mode::current::Purpose::InUse,
                            )
                            .unwrap()
                            .as_deref(),
                            Some("current")
                        );
                    } else {
                        result.unwrap();
                        assert!(
                            !crate::codex_config::codex_managed_oauth_live_auth_marker_exists(),
                            "missing account must relinquish ownership"
                        );
                    }
                    assert_eq!(
                        read_json_file::<Value>(&crate::codex_config::get_codex_auth_path())
                            .unwrap(),
                        auth
                    );
                });
            }
        }
    }

    #[test]
    #[serial]
    fn managed_codex_switch_db_current_failure_rolls_forward_on_recovery() {
        with_test_home(|state, _| {
            crate::settings::reload_settings().expect("reload settings");
            tauri::async_runtime::block_on(async {
                state
                    .codex_oauth_manager
                    .add_test_account_with_user_identity(
                        "acct-managed-a",
                        "managed-token-a",
                        "user-a",
                    )
                    .await
                    .expect("seed first managed Codex OAuth account");
                state
                    .codex_oauth_manager
                    .add_test_account_with_user_identity(
                        "acct-managed-b",
                        "managed-token-b",
                        "user-b",
                    )
                    .await
                    .expect("seed second managed Codex OAuth account");
            });

            let managed_provider = |id: &str, account_id: &str, model: &str| {
                let mut provider = Provider::with_id(
                    id.to_string(),
                    format!("Managed {id}"),
                    json!({
                        "auth": {},
                        "config": format!("model = \"{model}\"\n"),
                        "modelCatalog": {
                            "models": [{ "model": model }]
                        }
                    }),
                    None,
                );
                provider.category = Some("official".to_string());
                provider.meta = Some(ProviderMeta {
                    auth_binding: Some(AuthBinding {
                        source: AuthBindingSource::ManagedAccount,
                        auth_provider: Some("codex_oauth".to_string()),
                        account_id: Some(account_id.to_string()),
                    }),
                    ..Default::default()
                });
                provider
            };

            let provider_a = managed_provider("managed-a", "acct-managed-a", "gpt-5.4-managed-a");
            let provider_b = managed_provider("managed-b", "acct-managed-b", "gpt-5.4-managed-b");
            state
                .db
                .save_provider(AppType::Codex.as_str(), &provider_a)
                .expect("save first managed provider");
            state
                .db
                .save_provider(AppType::Codex.as_str(), &provider_b)
                .expect("save second managed provider");

            ProviderService::switch(state, AppType::Codex, &provider_a.id)
                .expect("activate first managed provider");
            let auth_before: Value = read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read first managed auth");
            assert!(
                crate::codex_config::get_codex_config_path().exists(),
                "baseline must include config.toml"
            );
            assert!(
                crate::codex_config::get_codex_model_catalog_path().exists(),
                "baseline must include the generated model catalog"
            );
            assert!(
                crate::codex_config::codex_auth_matches_recorded_managed_oauth(
                    &auth_before,
                    "acct-managed-a",
                )
                .expect("check first managed auth marker"),
                "baseline must include a marker owned by the first managed account"
            );
            let live_before = crate::codex_config::CodexLiveStateSnapshot::capture()
                .expect("capture auth/config/catalog/marker before failed switch");

            {
                let conn = state.db.conn.lock().expect("lock database");
                conn.execute_batch(
                    "CREATE TRIGGER reject_managed_b_current_update
                     BEFORE UPDATE OF is_current ON providers
                     WHEN NEW.app_type = 'codex'
                       AND NEW.id = 'managed-b'
                       AND NEW.is_current = 1
                     BEGIN
                       SELECT RAISE(ABORT, 'forced managed Codex current failure');
                     END;",
                )
                .expect("install current-provider failure trigger");
            }

            let error = ProviderService::switch(state, AppType::Codex, &provider_b.id)
                .expect_err("DB current failure should abort managed switch");
            assert!(
                error
                    .to_string()
                    .contains("forced managed Codex current failure"),
                "switch should surface the DB commit failure, got: {error}"
            );

            // 文件已经发布，只是指针没落定：不回滚，pending 等着补完。
            assert_ne!(
                crate::codex_config::CodexLiveStateSnapshot::capture().expect("capture live"),
                live_before
            );
            let auth_after: Value = read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read managed auth");
            assert!(
                crate::codex_config::codex_auth_matches_recorded_managed_oauth(
                    &auth_after,
                    "acct-managed-b",
                )
                .expect("check managed marker"),
                "auth.json and the marker belong to the target account"
            );
            assert!(crate::mode::operation::has_pending(AppType::Codex.as_str()));

            state
                .db
                .conn
                .lock()
                .expect("lock database")
                .execute_batch("DROP TRIGGER reject_managed_b_current_update;")
                .expect("drop trigger");
            crate::mode::operation::recover_on_startup(&state.db);
            assert!(!crate::mode::operation::has_pending(
                AppType::Codex.as_str()
            ));
            assert_eq!(
                crate::settings::get_current_provider(&AppType::Codex).as_deref(),
                Some(provider_b.id.as_str())
            );
            assert_eq!(
                state
                    .db
                    .get_current_provider(AppType::Codex.as_str())
                    .expect("read DB current after recovery")
                    .as_deref(),
                Some(provider_b.id.as_str())
            );
        });
    }

    #[test]
    #[serial]
    fn managed_codex_update_rechecks_current_after_waiting_for_switch_lock() {
        with_test_home(|state, _| {
            crate::settings::reload_settings().expect("reload settings");
            tauri::async_runtime::block_on(async {
                state
                    .codex_oauth_manager
                    .add_test_account_with_user_identity(
                        "acct-managed-a",
                        "managed-token-a",
                        "user-a",
                    )
                    .await
                    .expect("seed managed account A");
                state
                    .codex_oauth_manager
                    .add_test_account_with_user_identity(
                        "acct-managed-b",
                        "managed-token-b",
                        "user-b",
                    )
                    .await
                    .expect("seed managed account B");
            });

            let mut official = Provider::with_id(
                "managed-official-a".to_string(),
                "OpenAI Official".to_string(),
                json!({ "auth": {}, "config": "model = \"gpt-5.4\"\n" }),
                None,
            );
            official.category = Some("official".to_string());
            official.meta = Some(ProviderMeta {
                auth_binding: Some(AuthBinding {
                    source: AuthBindingSource::ManagedAccount,
                    auth_provider: Some("codex_oauth".to_string()),
                    account_id: Some("acct-managed-a".to_string()),
                }),
                ..Default::default()
            });
            state
                .db
                .save_provider(AppType::Codex.as_str(), &official)
                .expect("save official A");
            state
                .db
                .set_current_provider(AppType::Codex.as_str(), &official.id)
                .expect("set official current");
            crate::settings::set_current_provider(&AppType::Codex, Some(&official.id))
                .expect("set local official current");

            let mut third_party = Provider::with_id(
                "third-party-current".to_string(),
                "Third Party".to_string(),
                json!({
                    "auth": { "OPENAI_API_KEY": "sk-third" },
                    "config": r#"model_provider = "third"
[model_providers.third]
name = "Third"
base_url = "https://third.example/v1"
wire_api = "responses"
"#
                }),
                None,
            );
            third_party.category = Some("custom".to_string());
            state
                .db
                .save_provider(AppType::Codex.as_str(), &third_party)
                .expect("save third party");

            let mut updated = official.clone();
            updated
                .meta
                .as_mut()
                .and_then(|meta| meta.auth_binding.as_mut())
                .expect("managed binding")
                .account_id = Some("acct-managed-b".to_string());

            let switch_guard = tauri::async_runtime::block_on(
                state
                    .proxy_service
                    .lock_switch_for_app(AppType::Codex.as_str()),
            );
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (update_result, live_after_switch) = std::thread::scope(|scope| {
                let updater = scope.spawn(move || {
                    started_tx.send(()).expect("signal updater start");
                    ProviderService::update(state, AppType::Codex, None, updated)
                });
                started_rx.recv().expect("wait for updater");

                // This emulates a switch that already owns the per-app lock and
                // commits a different current target before the queued update is
                // allowed to inspect current/existing state.
                state
                    .db
                    .set_current_provider(AppType::Codex.as_str(), &third_party.id)
                    .expect("switch DB current to third party");
                crate::settings::set_current_provider(
                    &AppType::Codex,
                    Some(third_party.id.as_str()),
                )
                .expect("switch local current to third party");
                write_live_for_state(state, &AppType::Codex, &third_party)
                    .expect("write third-party live");
                let live_after_switch = crate::codex_config::CodexLiveStateSnapshot::capture()
                    .expect("capture third-party live");

                drop(switch_guard);
                let result = updater.join().expect("join managed updater");
                (result, live_after_switch)
            });

            update_result.expect("save queued non-current managed row");
            assert_eq!(
                state
                    .db
                    .get_current_provider(AppType::Codex.as_str())
                    .expect("read DB current")
                    .as_deref(),
                Some(third_party.id.as_str())
            );
            assert_eq!(
                crate::codex_config::CodexLiveStateSnapshot::capture()
                    .expect("capture live after queued update"),
                live_after_switch,
                "queued provider edit must not rewrite the newly switched current live"
            );
            let saved_official = state
                .db
                .get_provider_by_id(&official.id, AppType::Codex.as_str())
                .expect("read saved official")
                .expect("official exists");
            assert_eq!(
                saved_official
                    .meta
                    .as_ref()
                    .and_then(|meta| meta.managed_account_id_for("codex_oauth")),
                Some("acct-managed-b".to_string())
            );
        });
    }
}

impl ProviderService {
    pub(crate) fn managed_codex_oauth_account_id(provider: &Provider) -> Option<String> {
        provider
            .meta
            .as_ref()
            .and_then(|meta| meta.managed_account_id_for("codex_oauth"))
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
    }

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

    fn provider_live_config_managed(provider: &Provider) -> Option<bool> {
        provider
            .meta
            .as_ref()
            .and_then(|meta| meta.live_config_managed)
    }

    fn set_provider_live_config_managed(provider: &mut Provider, managed: bool) {
        provider
            .meta
            .get_or_insert_with(Default::default)
            .live_config_managed = Some(managed);
    }

    fn normalize_usage_script_credential_overrides(app_type: &AppType, provider: &mut Provider) {
        let current_credentials = provider.resolve_usage_credentials(app_type);

        let Some(usage_script) = provider
            .meta
            .as_mut()
            .and_then(|meta| meta.usage_script.as_mut())
        else {
            return;
        };

        if usage_script.template_type.as_deref() == Some("token_plan") {
            return;
        }

        if usage_script.api_key.as_deref().is_some_and(|api_key| {
            Self::should_clear_usage_api_key_override(api_key, &current_credentials)
        }) {
            usage_script.api_key = None;
        }

        if usage_script.base_url.as_deref().is_some_and(|base_url| {
            Self::should_clear_usage_base_url_override(base_url, &current_credentials)
        }) {
            usage_script.base_url = None;
        }
    }

    fn should_clear_usage_api_key_override(
        script_api_key: &str,
        current_credentials: &(String, String),
    ) -> bool {
        let candidate = script_api_key.trim();
        if candidate.is_empty() {
            return true;
        }

        let matches_provider_key = |api_key: &str| {
            let api_key = api_key.trim();
            !api_key.is_empty() && api_key == candidate
        };

        matches_provider_key(&current_credentials.1)
    }

    fn should_clear_usage_base_url_override(
        script_base_url: &str,
        current_credentials: &(String, String),
    ) -> bool {
        let candidate = Self::normalize_usage_base_url_for_compare(script_base_url);
        if candidate.is_empty() {
            return true;
        }

        let matches_provider_base_url = |base_url: &str| {
            let base_url = Self::normalize_usage_base_url_for_compare(base_url);
            !base_url.is_empty() && base_url == candidate
        };

        matches_provider_base_url(&current_credentials.0)
    }

    fn normalize_usage_base_url_for_compare(base_url: &str) -> String {
        base_url.trim().trim_end_matches('/').to_string()
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
    /// 对于累加模式应用（OpenCode, OpenClaw），不存在"当前供应商"概念，直接返回空字符串。
    pub fn current(state: &AppState, app_type: AppType) -> Result<String, AppError> {
        // Additive mode apps have no "current" provider concept
        if app_type.is_additive_mode() {
            return Ok(String::new());
        }
        // 代理模式下界面上的「当前」是代理路由到的那家。
        crate::mode::current::provider_for(
            &state.db,
            &app_type,
            crate::mode::current::Purpose::InUse,
        )
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
        Self::normalize_usage_script_credential_overrides(&app_type, &mut provider);
        if app_type.is_additive_mode() {
            Self::set_provider_live_config_managed(&mut provider, add_to_live);
        }
        if matches!(app_type, AppType::Claude | AppType::Codex) {
            keep_common_config_for_old_versions(&mut provider);
        }

        if matches!(app_type, AppType::Codex) {
            return Self::add_codex(state, provider);
        }

        // 还没有当前供应商时这一家会被写进 live：和进入代理、切换互斥（同 `update`）。
        let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &app_type)?;

        // Save to database
        state.db.save_provider(app_type.as_str(), &provider)?;

        // Additive mode apps: optionally write to live config.
        if app_type.is_additive_mode() {
            if !add_to_live {
                return Ok(true);
            }
            write_live_for_state(state, &app_type, &provider)?;
            return Ok(true);
        }

        // For other apps: Check if sync is needed (if this is current provider, or no current provider)
        let current = crate::mode::current::provider_for(
            &state.db,
            &app_type,
            crate::mode::current::Purpose::Direct,
        )?;
        if current.is_none() {
            // 第一个供应商同样只写关键字段，不覆盖用户已有的配置文件。
            if matches!(app_type, AppType::Claude) {
                claude_direct::switch_to(state.db.as_ref(), None, &provider)?;
                return Ok(true);
            }
            // No current provider, set as current and sync. Managed Codex adds
            // use the transactional path above because token resolution can fail.
            state
                .db
                .set_current_provider(app_type.as_str(), &provider.id)?;
            write_live_for_state(state, &app_type, &provider)?;
        }

        Ok(true)
    }

    /// 新增 Codex 供应商。还没有当前供应商时，它就是第一个：行、live 和指针一起提交，
    /// live 只写关键字段，不覆盖用户已有的 config.toml。写 live 失败时撤回刚存的行，
    /// 免得留下一个看得见却用不了的供应商。
    fn add_codex(state: &AppState, provider: Provider) -> Result<bool, AppError> {
        let app_type = AppType::Codex;
        // 和切换互斥：等着的切换不能看到只存了一半的托管账号绑定。
        let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &app_type)?;
        let current = crate::mode::current::provider_for(
            &state.db,
            &app_type,
            crate::mode::current::Purpose::Direct,
        )?;
        if current.is_some() {
            state.db.save_provider(app_type.as_str(), &provider)?;
            return Ok(true);
        }

        // 行有问题（会把官方登录发给第三方等）在存行之前就拒绝。
        codex_direct::preflight(state.db.as_ref(), &provider)?;
        let previous = state
            .db
            .get_provider_by_id(&provider.id, app_type.as_str())?;
        state.db.save_provider(app_type.as_str(), &provider)?;
        let written = codex_direct::write_direct(
            state.db.as_ref(),
            &state.codex_oauth_manager,
            crate::mode::state::op::SWITCH,
            codex_direct::Owner::None,
            Some(&provider),
            crate::mode::state::PendingTarget::pointer(Some(provider.id.clone())),
        );
        if let Err(error) = written {
            // 文件已经发布、只是落定状态失败时，pending 会在下次操作或启动时补完指针，
            // 行要留着；还没发布就失败，撤回刚存的行。
            if crate::mode::operation::has_pending(AppType::Codex.as_str()) {
                return Err(error);
            }
            let rollback = match &previous {
                Some(previous) => state.db.save_provider(app_type.as_str(), previous),
                None => state.db.delete_provider(app_type.as_str(), &provider.id),
            };
            if let Err(rollback) = rollback {
                return Err(AppError::Message(format!(
                    "新增首个 Codex 供应商失败: {error}; 恢复供应商数据同时失败: {rollback}"
                )));
            }
            return Err(error);
        }
        Ok(true)
    }

    /// 供应商编辑器底部配置的显示内容（Claude Code、Codex）：切到
    /// 这个供应商之后配置文件会是什么样，以及行里不随切换生效的字段。`category` 用来认出
    /// 官方卡。
    pub fn editor_view(
        state: &AppState,
        app_type: AppType,
        settings_config: &Value,
        category: Option<&str>,
    ) -> Result<EditorView, AppError> {
        match app_type {
            AppType::Claude => claude_editor::view(state, settings_config),
            AppType::Codex => codex_editor::view(state, settings_config, category),
            other => Err(AppError::InvalidInput(format!(
                "{} 的编辑器还不支持按关键字段显示",
                other.as_str()
            ))),
        }
    }

    /// 编辑已有供应商时给 [`Self::editor_view`] 的 `category`：按库里那一行，用切换时同一个
    /// 判断认官方卡。Codex 还认固定 id 和托管账号；只看 `category` 的话，这些卡预览里的登录
    /// 方式和切换写的不同。
    /// 没有 `provider_id`（新增）或行不存在时原样用 `category`。
    pub fn editor_category(
        state: &AppState,
        app_type: &AppType,
        provider_id: Option<&str>,
        category: Option<String>,
    ) -> Result<Option<String>, AppError> {
        let Some(id) = provider_id else {
            return Ok(category);
        };
        let Some(row) = state.db.get_provider_by_id(id, app_type.as_str())? else {
            return Ok(category);
        };
        let official = match app_type {
            AppType::Codex => codex_direct::is_official(&row),
            _ => false,
        };
        Ok(if official {
            Some("official".to_string())
        } else {
            category
        })
    }

    /// 从编辑器新增供应商。Claude Code、Codex 按关键字段拆开保存
    /// （见各自的 `*_editor`），其余应用和 `add` 一样。
    pub fn add_from_editor(
        state: &AppState,
        app_type: AppType,
        provider: Provider,
        add_to_live: bool,
        editor: Option<EditorSave>,
    ) -> Result<bool, AppError> {
        match (app_type, editor) {
            (AppType::Claude, Some(editor)) => {
                Self::add_claude_from_editor(state, provider, editor)
            }
            (AppType::Codex, Some(editor)) => {
                Self::save_codex_from_editor(state, provider, editor, EditorSaveKind::Add)
            }
            (app_type, _) => Self::add(state, app_type, provider, add_to_live),
        }
    }

    /// 从编辑器保存供应商。Claude Code、Codex 按关键字段拆开保存
    /// （见各自的 `*_editor`），其余应用和 `update` 一样。
    pub fn update_from_editor(
        state: &AppState,
        app_type: AppType,
        original_id: Option<&str>,
        provider: Provider,
        editor: Option<EditorSave>,
    ) -> Result<bool, AppError> {
        match (app_type, editor) {
            (AppType::Claude, Some(editor))
                if original_id.is_none_or(|original| original == provider.id) =>
            {
                Self::update_claude_from_editor(state, provider, editor)
            }
            (AppType::Codex, Some(editor))
                if original_id.is_none_or(|original| original == provider.id) =>
            {
                Self::save_codex_from_editor(state, provider, editor, EditorSaveKind::Update)
            }
            (app_type, _) => Self::update(state, app_type, original_id, provider),
        }
    }

    /// 从编辑器新增或保存 Codex 供应商：关键字段、独有字段存回行，其余改动作为全局设置
    /// 写进 live（三方比较）。直连模式下编辑当前供应商、或新增第一个供应商时，关键字段在
    /// 同一次写入里换进 live；代理模式下编辑路由那家，全局设置写完后按新行重写代理契约。
    fn save_codex_from_editor(
        state: &AppState,
        provider: Provider,
        editor: EditorSave,
        kind: EditorSaveKind,
    ) -> Result<bool, AppError> {
        let app_type = AppType::Codex;
        let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &app_type)?;
        let existing = state
            .db
            .get_provider_by_id(&provider.id, app_type.as_str())?;
        let mut provider = provider;
        let plan = codex_editor::plan_save(
            existing.as_ref().map(|row| &row.settings_config),
            &provider.settings_config,
            &editor.base,
            &match (existing.as_ref(), editor.draft.as_ref()) {
                (Some(row), _) => codex_editor::Origin::row(&row.settings_config)?,
                (None, Some(draft)) => codex_editor::Origin::row(draft)?,
                (None, None) => codex_editor::Origin::Live(codex_editor::live_exclusive(state)?),
            },
            codex_direct::is_official(&provider),
            provider.uses_proxy_injected_oauth(),
            editor.on_conflict,
        )?;
        provider.settings_config = plan.row_settings.clone();
        Self::validate_provider_settings(&app_type, &provider)?;
        Self::normalize_usage_script_credential_overrides(&app_type, &mut provider);
        if kind == EditorSaveKind::Add {
            keep_common_config_for_old_versions(&mut provider);
        }

        let mode = crate::mode::current::mode_state(&app_type);
        let key_fields = kind.writes_key_fields(state, &app_type, &mode, &provider.id)?;
        let is_route = mode.routes_to(&provider.id);

        state.db.save_provider(app_type.as_str(), &provider)?;
        let written = if key_fields {
            codex_editor::write_live(
                state.db.as_ref(),
                &state.codex_oauth_manager,
                &plan.edits,
                codex_editor::KeyFields::Direct {
                    prev: existing.as_ref(),
                    target: &provider,
                    set_pointer: kind == EditorSaveKind::Add,
                },
            )
        } else {
            codex_editor::write_live(
                state.db.as_ref(),
                &state.codex_oauth_manager,
                &plan.edits,
                codex_editor::KeyFields::None,
            )
            .and_then(|()| Self::rewrite_route_if(is_route, state, &app_type, &provider))
        };
        Self::keep_row_if_written(state, &app_type, &provider.id, existing.as_ref(), written)
    }

    /// 编辑器保存先存行、再写 live（`written`）。写 live 失败时：文件已经发布、只是落定
    /// 状态失败的，pending 会在下次操作或启动时补完，行要留着；还没发布就失败（没有
    /// pending），撤回刚存的行。
    fn keep_row_if_written(
        state: &AppState,
        app_type: &AppType,
        provider_id: &str,
        existing: Option<&Provider>,
        written: Result<(), AppError>,
    ) -> Result<bool, AppError> {
        let Err(error) = written else {
            return Ok(true);
        };
        if crate::mode::operation::has_pending(app_type.as_str()) {
            return Err(error);
        }
        let rollback = match existing {
            Some(existing) => state.db.save_provider(app_type.as_str(), existing),
            None => state.db.delete_provider(app_type.as_str(), provider_id),
        };
        if let Err(rollback) = rollback {
            log::warn!(
                "恢复 {} 供应商 '{provider_id}' 失败: {rollback}",
                app_type.as_str()
            );
        }
        Err(error)
    }

    /// 代理模式下保存的是路由那家（`is_route`）：按新行重写代理契约，契约没变就不碰客户端
    /// 文件。调用方持有这个应用的切换锁。
    fn rewrite_route_if(
        is_route: bool,
        state: &AppState,
        app_type: &AppType,
        provider: &Provider,
    ) -> Result<(), AppError> {
        if !is_route {
            return Ok(());
        }
        futures::executor::block_on(crate::mode::controller::switch_route_locked(
            state, app_type, provider,
        ))
        .map_err(AppError::Message)
    }

    fn add_claude_from_editor(
        state: &AppState,
        provider: Provider,
        editor: EditorSave,
    ) -> Result<bool, AppError> {
        let app_type = AppType::Claude;
        let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &app_type)?;
        let mut provider = provider;
        Self::normalize_provider_if_claude(&app_type, &mut provider);
        let plan = claude_editor::plan_save(None, &provider.settings_config, &editor.base)?;
        provider.settings_config = plan.row_settings.clone();
        Self::validate_provider_settings(&app_type, &provider)?;
        Self::normalize_usage_script_credential_overrides(&app_type, &mut provider);
        keep_common_config_for_old_versions(&mut provider);

        let existing = state
            .db
            .get_provider_by_id(&provider.id, app_type.as_str())?;
        let first = crate::mode::current::provider_for(
            &state.db,
            &app_type,
            crate::mode::current::Purpose::Direct,
        )?
        .is_none();
        state.db.save_provider(app_type.as_str(), &provider)?;

        let key_fields = first.then_some(claude_editor::KeyFieldWrite {
            prev: None,
            target: &provider,
            set_pointer: true,
        });
        let written =
            claude_editor::write_live(state.db.as_ref(), &plan, editor.on_conflict, key_fields);
        Self::keep_row_if_written(state, &app_type, &provider.id, existing.as_ref(), written)
    }

    /// 和其他编辑器一样先存行、再写 live：写 live 失败且没有 pending 时撤回行；已经发布的
    /// 由 pending 补完，行和 live 不会对不上。
    fn update_claude_from_editor(
        state: &AppState,
        provider: Provider,
        editor: EditorSave,
    ) -> Result<bool, AppError> {
        let app_type = AppType::Claude;
        let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &app_type)?;
        let existing = state
            .db
            .get_provider_by_id(&provider.id, app_type.as_str())?;
        let mut provider = provider;
        Self::normalize_provider_if_claude(&app_type, &mut provider);
        let plan = claude_editor::plan_save(
            existing.as_ref().map(|row| &row.settings_config),
            &provider.settings_config,
            &editor.base,
        )?;
        provider.settings_config = plan.row_settings.clone();
        Self::validate_provider_settings(&app_type, &provider)?;
        Self::normalize_usage_script_credential_overrides(&app_type, &mut provider);

        let mode = crate::mode::current::mode_state(&app_type);
        let is_route = mode.routes_to(&provider.id);
        // 代理模式下 live 的关键字段是代理契约，这里只写全局改动；编辑的是代理路由那家
        // 时，写完按新行重写契约（契约没变就不动）。直连指针那家在退出代理时写回。
        let key_fields = EditorSaveKind::Update
            .writes_key_fields(state, &app_type, &mode, &provider.id)?
            .then_some(claude_editor::KeyFieldWrite {
                prev: existing.as_ref(),
                target: &provider,
                set_pointer: false,
            });

        state.db.save_provider(app_type.as_str(), &provider)?;
        let written =
            claude_editor::write_live(state.db.as_ref(), &plan, editor.on_conflict, key_fields)
                .and_then(|()| Self::rewrite_route_if(is_route, state, &app_type, &provider));
        Self::keep_row_if_written(state, &app_type, &provider.id, existing.as_ref(), written)
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
        // 读旧行、判断谁是当前供应商、存行、写 live 都在切换锁里：进入代理和切换都等这次
        // 保存写完，这里也不会在读完模式之后、写 live 之前被它们改掉模式（否则直连的关键
        // 字段会盖掉刚写的代理契约）。Codex 还要在锁里读旧行，看它是不是托管账号的行。
        let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &app_type)?;
        let existing_provider = state
            .db
            .get_provider_by_id(&original_id, app_type.as_str())?;
        // Normalize Claude model keys
        Self::normalize_provider_if_claude(&app_type, &mut provider);
        Self::validate_provider_settings(&app_type, &provider)?;
        if matches!(app_type, AppType::Codex) && provider.category.as_deref() == Some("official") {
            crate::codex_config::strip_codex_unified_session_bucket_from_settings(
                &mut provider.settings_config,
            )?;
        }
        Self::normalize_usage_script_credential_overrides(&app_type, &mut provider);

        if provider_id_changed {
            // Every app this fork ships is exclusive-mode, so a provider rename is
            // always rejected here: the live config entry is keyed by provider id.
            return Err(AppError::Message(
                "Only additive-mode providers support changing provider key".to_string(),
            ));
        }

        // Additive mode apps: only sync to live when the provider already exists in
        // live config. Editing a DB-only provider must not auto-add it.
        if app_type.is_additive_mode() {
            let live_config_managed = Self::check_live_config_exists(
                &app_type,
                &provider.id,
                Self::provider_live_config_managed(&provider).or_else(|| {
                    existing_provider
                        .as_ref()
                        .and_then(Self::provider_live_config_managed)
                }),
            )?;
            Self::set_provider_live_config_managed(&mut provider, live_config_managed);

            // Save to database after live-config presence is resolved so parse errors
            // do not report failure after already mutating DB state.
            state.db.save_provider(app_type.as_str(), &provider)?;

            if !live_config_managed {
                return Ok(true);
            }
            write_live_for_state(state, &app_type, &provider)?;
            return Ok(true);
        }

        // For other apps: 是否是直连指针那家，或代理模式下的代理路由那家。
        let mode = crate::mode::current::mode_state(&app_type);
        let is_direct_current = crate::mode::current::provider_for(
            &state.db,
            &app_type,
            crate::mode::current::Purpose::Direct,
        )?
        .as_deref()
            == Some(provider.id.as_str());
        let is_route = mode.routes_to(&provider.id);
        let is_current = is_direct_current || is_route;

        if matches!(app_type, AppType::Codex) {
            return Self::update_codex(
                state,
                &provider,
                existing_provider.as_ref(),
                is_direct_current,
                is_route,
            );
        }

        // Save to database
        state.db.save_provider(app_type.as_str(), &provider)?;

        if is_current {
            let outcome = live::sync_live_for_provider_respecting_mode(
                state,
                &app_type,
                &provider,
                existing_provider.as_ref(),
            )?;
            if outcome == LiveSyncOutcome::WroteLive {
                // MCP is stored in the database and projected after a successful
                // live write. Keep the failure best-effort so the provider save
                // itself is not reported as failed when MCP projection can retry.
                if let Err(err) = McpService::sync_enabled_for_app(state, &app_type) {
                    log::warn!(
                        "保存供应商后重投影 {app_type:?} MCP 失败（将在下次同步时自愈）: {err}"
                    );
                }
            }
        }

        Ok(true)
    }

    /// 保存 Codex 供应商。调用方持有这个应用的切换锁。
    ///
    /// - 直连模式下编辑直连那家：先存行，再只替换 live 里的关键字段和独有字段（换托管
    ///   账号时先采纳、再清掉旧账号的登录）；写 live 失败就把行恢复原样。
    /// - 代理模式下编辑路由那家：按新行重写代理契约（契约没变就不碰客户端文件）。
    /// - 其余只存行。
    fn update_codex(
        state: &AppState,
        provider: &Provider,
        existing: Option<&Provider>,
        is_direct_current: bool,
        is_route: bool,
    ) -> Result<bool, AppError> {
        let app_type = AppType::Codex;
        let mode = crate::mode::current::mode_state(&app_type);
        let writes_live = if mode.is_proxy() {
            is_route
        } else {
            is_direct_current
        };
        if !writes_live {
            state.db.save_provider(app_type.as_str(), provider)?;
            return Ok(true);
        }
        if !mode.is_proxy() {
            codex_direct::preflight(state.db.as_ref(), provider)?;
        }

        state.db.save_provider(app_type.as_str(), provider)?;
        let written = if mode.is_proxy() {
            Self::rewrite_route_if(true, state, &app_type, provider)
        } else {
            codex_direct::write_direct(
                state.db.as_ref(),
                &state.codex_oauth_manager,
                crate::mode::state::op::APPLY,
                existing.map_or(codex_direct::Owner::None, codex_direct::Owner::Provider),
                Some(provider),
                crate::mode::state::PendingTarget::default(),
            )
            .map(|_| ())
        };
        if let Err(error) = written {
            if crate::mode::operation::has_pending(AppType::Codex.as_str()) {
                return Err(error);
            }
            if let Some(existing) = existing {
                if let Err(rollback) = state.db.save_provider(app_type.as_str(), existing) {
                    return Err(AppError::Message(format!(
                        "更新 Codex 供应商失败: {error}; 恢复供应商数据同时失败: {rollback}"
                    )));
                }
            }
            return Err(error);
        }
        Ok(true)
    }

    /// Delete a provider
    ///
    /// 同时检查本地 settings 和数据库的当前供应商，防止删除任一端正在使用的供应商。
    pub fn delete(state: &AppState, app_type: AppType, id: &str) -> Result<(), AppError> {
        // For other apps: 本地记录、DB、代理路由任何一处指着它都不能删
        if crate::mode::current::is_referenced(&state.db, &app_type, id)? {
            return Err(AppError::Message(
                "无法删除当前正在使用的供应商".to_string(),
            ));
        }

        state.db.delete_provider(app_type.as_str(), id)
    }

    /// Remove provider from live config only (for additive mode apps like OpenCode, OpenClaw)
    ///
    /// Does NOT delete from database - provider remains in the list.
    /// This is used when user wants to "remove" a provider from active config
    /// but keep it available for future use.
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

    /// 切换供应商。
    ///
    /// - 代理模式：只换代理路由，直连指针不变；契约没变时客户端文件不读也不写。
    /// - 直连模式：切换式应用只替换客户端文件里的关键字段（不回填），文件和指针在同一个
    ///   操作里提交；累加式应用按各自的规则写入。
    pub fn switch(state: &AppState, app_type: AppType, id: &str) -> Result<SwitchResult, AppError> {
        // Check if provider exists
        let providers = state.db.get_all_providers(app_type.as_str())?;
        let _provider = providers
            .get(id)
            .ok_or_else(|| AppError::Message(format!("供应商 {id} 不存在")))?;

        if matches!(app_type, AppType::ClaudeDesktop) {
            return Self::switch_normal(state, app_type, id, &providers);
        }

        // 切换和进入 / 退出代理都会改客户端文件和指针。按应用串行，拿到锁、补完上一次
        // 没做完的写入之后再读模式和指针：刚进入代理的应用不会被一次直连写入覆盖，上次
        // 失败后重试也按补完后的指针删上一家的独有字段。
        let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &app_type)?;

        if crate::mode::current::is_proxy(&app_type) {
            // 代理模式：只换代理路由，直连指针不变。契约没变时客户端文件不读也不写；
            // 变了在同一个操作里先改写客户端，再发布路由。
            if _provider.category.as_deref() == Some("official")
                && !official_provider_supports_proxy_takeover(&app_type, _provider)
            {
                return Err(AppError::localized(
                    "switch.official_blocked_by_proxy",
                    "路由模式下不能切换到官方供应商，使用代理访问官方 API 可能导致账号被封禁。请先退出路由模式，或选择第三方供应商。",
                    "Cannot switch to an official provider in routing mode. Using a proxy with official APIs may cause account bans.",
                ));
            }
            log::info!("路由模式：{} 的代理路由切到 {}", app_type.as_str(), id);
            futures::executor::block_on(crate::mode::controller::switch_route_locked(
                state, &app_type, _provider,
            ))
            .map_err(|e| AppError::Message(format!("切换路由失败: {e}")))?;
            // MCP 不随路由变：客户端文件没按直连重写。
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

        if matches!(app_type, AppType::Claude) {
            return Self::switch_claude_direct(state, provider, providers);
        }
        if matches!(app_type, AppType::Codex) {
            return Self::switch_codex_direct(state, provider, providers);
        }

        let result = SwitchResult::default();

        // Additive mode apps skip setting is_current (no such concept).
        if !app_type.is_additive_mode() {
            crate::settings::set_current_provider(&app_type, Some(id))?;
            state.db.set_current_provider(app_type.as_str(), id)?;
        }

        // 写 live（Claude Desktop、累加式应用；切换式应用在上面各自的分支里写完了）。
        write_live_for_state(state, &app_type, provider)?;

        // For additive-mode providers that were DB-only (live_config_managed == Some(false)),
        // flip the flag to true now that the provider has been successfully written to the live
        // file. This ensures sync_all_providers_to_live() will include it on future syncs.
        //
        // If persisting the marker fails, roll back the just-written live config so we don't leave
        // the provider in a silent inconsistent state (present in live, but still marked DB-only).
        if app_type.is_additive_mode() && Self::provider_live_config_managed(provider) != Some(true)
        {
            let mut updated = provider.clone();
            Self::set_provider_live_config_managed(&mut updated, true);
            if let Err(e) = state.db.save_provider(app_type.as_str(), &updated) {
                let rollback_result: Result<(), AppError> = Ok(());

                match rollback_result {
                    Ok(()) => {
                        return Err(AppError::Message(format!(
                            "Failed to persist live_config_managed for '{}' after writing live config; live changes were rolled back: {e}",
                            provider.id
                        )));
                    }
                    Err(rollback_err) => {
                        return Err(AppError::Message(format!(
                            "Failed to persist live_config_managed for '{}' after writing live config: {e}; additionally failed to roll back live config: {rollback_err}",
                            provider.id
                        )));
                    }
                }
            }
        }

        // 切换重写了目标应用的 live，只重投影该应用的 MCP（其余应用的
        // MCP 文件独立于 live，投影是幂等维护）。不用全量 sync_all_enabled：
        // 无关应用的 live 损坏（如 ~/.claude.json 坏 JSON）不该阻断切换。
        // 走到这里 DB is_current 与 live 都已落盘，切换事实上已成功；
        // 投影失败上抛会让前端报"切换失败"制造分裂假象，故降级为警告
        // （MCP 投影可自愈：下次切换 / 任一 MCP 启停都会重新投影）。
        if let Err(err) = McpService::sync_enabled_for_app(state, &app_type) {
            log::warn!("切换供应商后重投影 {app_type:?} MCP 失败（将在下次同步时自愈）: {err}");
        }

        Ok(result)
    }

    /// Claude Code 直连切换：只替换关键字段和独有字段，文件和指针在同一个操作里提交。
    ///
    /// 不回填、不同步通用配置片段：用户在 live 里的改动本来就留在原处。发布前的失败
    /// （解析不了、并发冲突）什么都不改；开始发布后由 pending 保证前滚补完。
    fn switch_claude_direct(
        state: &AppState,
        provider: &Provider,
        providers: &IndexMap<String, Provider>,
    ) -> Result<SwitchResult, AppError> {
        let current_id = crate::mode::current::provider_for(
            &state.db,
            &AppType::Claude,
            crate::mode::current::Purpose::Direct,
        )?;
        let prev = current_id
            .as_deref()
            .and_then(|current_id| providers.get(current_id));
        claude_direct::switch_to(state.db.as_ref(), prev, provider)?;

        // MCP 在 ~/.claude.json，和 settings.json 无关；重投影是幂等维护，失败只记警告
        // （切换已经提交，下次同步会自愈）。
        if let Err(err) = McpService::sync_enabled_for_app(state, &AppType::Claude) {
            log::warn!("切换供应商后重投影 claude MCP 失败（将在下次同步时自愈）: {err}");
        }
        Ok(SwitchResult::default())
    }

    /// Codex 直连切换：`config.toml` 只替换关键字段和独有字段；`auth.json`、模型目录、
    /// 托管账号标记和指针在同一个操作里提交。
    ///
    /// 不回填、不同步通用配置片段、不补回 MCP：用户在 live 里的改动（含 `[mcp_servers]`）
    /// 本来就留在原处。行有问题（会把官方登录发给第三方、带 Key 却没地方放）时在写任何
    /// 东西之前报错，指针也不动。
    fn switch_codex_direct(
        state: &AppState,
        provider: &Provider,
        providers: &IndexMap<String, Provider>,
    ) -> Result<SwitchResult, AppError> {
        let current_id = crate::mode::current::provider_for(
            &state.db,
            &AppType::Codex,
            crate::mode::current::Purpose::Direct,
        )?;
        let owner = current_id
            .as_deref()
            .and_then(|current_id| providers.get(current_id))
            .map_or(codex_direct::Owner::None, codex_direct::Owner::Provider);
        codex_direct::write_direct(
            state.db.as_ref(),
            &state.codex_oauth_manager,
            crate::mode::state::op::SWITCH,
            owner,
            Some(provider),
            crate::mode::state::PendingTarget::pointer(Some(provider.id.clone())),
        )?;

        let mut result = SwitchResult::default();
        // 保留登录关闭时切到第三方要删掉 auth.json。删不掉（只读目录、被占用）不让切换
        // 失败：配置和指针都已提交，但要让用户看到官方登录还在盘上。
        if !codex_direct::is_official(provider)
            && !crate::settings::preserve_codex_official_auth_on_switch()
            && crate::codex_config::get_codex_auth_path().exists()
        {
            log::warn!("Codex auth.json still present after a preservation-off third-party switch");
            result
                .warnings
                .push("codex_auth_cleanup_failed".to_string());
        }
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
        // 没有正在用的那家、或者在代理模式（客户端文件没按直连重写）时不重投影 MCP。
        let outcome = live::sync_current_provider_for_app_respecting_mode(state, &app_type)?;
        if outcome != Some(LiveSyncOutcome::WroteLive) {
            return Ok(());
        }
        McpService::sync_enabled_for_app(state, &app_type)
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

    /// 判断一个 env / 顶层配置键名是否为凭据/机密：凡命中一律不得写入共享的
    /// 通用配置片段。**故意从严**——多剥一个非机密键只是它不被共享（可恢复的小
    /// 不便），漏剥一个凭据则会把密钥注入到每个供应商（不可恢复的泄漏）。因此用
    /// 模式匹配覆盖整类，而非枚举具体名字（枚举永远会漏掉下一个 `*_API_KEY`）。
    ///
    /// 覆盖：Anthropic / OpenRouter / Google / OpenAI / Gemini 等 `*_API_KEY`
    /// （Claude provider 的凭据见 `Provider::resolve_usage_credentials`，确实支持
    /// `OPENROUTER_API_KEY` / `GOOGLE_API_KEY` 等回退）、各类 `*_AUTH_TOKEN` /
    /// 单数 `*_TOKEN`、AWS Bedrock / Vertex 凭据、通用 secret / password /
    /// 私钥命名，以及发往上游的自定义请求头、Cookie、Authorization。
    pub(crate) fn is_sensitive_config_key(name: &str) -> bool {
        let upper = name.to_ascii_uppercase();

        // 单数 `_TOKEN` 命中 AWS_SESSION_TOKEN 等，但**不**误伤复数 `_TOKENS`
        // （CLAUDE_CODE_MAX_OUTPUT_TOKENS / MAX_THINKING_TOKENS 是正常可共享配置）。
        const SENSITIVE_SUFFIXES: &[&str] = &[
            // 裸 `_KEY` 是最常见的凭据写法（OPENAI_KEY / GROQ_KEY / XAI_KEY…），
            // 必须单列：只枚举 `_API_KEY` / `_ACCESS_KEY` 这些子类，等于把最普通
            // 的那一种漏在外面。下面几条 `_*_KEY` 被它蕴含，保留是为了说明覆盖面。
            "_KEY",
            "_API_KEY",
            "_ACCESS_KEY",
            "_ACCESS_KEY_ID",
            "_KEY_ID",
            "_PRIVATE_KEY",
            // 不带分隔符的复合写法各走各的后缀：`_KEY` 够不着 `..._APIKEY`
            // （倒数第四个字符是 I 不是下划线）。VOLC_ACCESSKEY 是火山引擎文档
            // 里的正式变量名，本仓库就实现了火山 AK/SK 用量查询。
            "_APIKEY",
            "_ACCESSKEY",
            "_SECRETKEY",
            "_APITOKEN",
            "_AUTH_TOKEN",
            "_TOKEN",
            // GITHUB_PAT / GITLAB_PAT 等 personal access token 的惯用写法，
            // 既不含 TOKEN 也不含 KEY，前面每一条规则都够不着。
            "_PAT",
            // 口令类的常见缩写。`_PASS` 不会误伤 `*_BYPASS`（那个以 `_BYPASS`
            // 结尾），`_PWD` 也不会误伤 shell 的 PWD / OLDPWD。
            "_PWD",
            "_PASS",
            "_PASSPHRASE",
            "_CREDS",
            // 发往上游的自定义请求头（ANTHROPIC_CUSTOM_HEADERS、
            // GEMINI_CLI_CUSTOM_HEADERS）：常见写法是 `Authorization: Bearer …`
            // 或 `Cookie: …`，整串就是凭据。只认 `_CUSTOM_HEADERS`，不按 HEADER
            // 一刀切：CLAUDE_CODE_ATTRIBUTION_HEADER 是普通开关，
            // OTEL_EXPORTER_OTLP_HEADERS 发往用户自己的遥测端点，都应照常共享。
            "_CUSTOM_HEADERS",
        ];
        const SENSITIVE_EXACT: &[&str] = &[
            "APIKEY",
            "API_KEY",
            "TOKEN",
            "SECRET",
            "PASSWORD",
            "CREDENTIALS",
            "HEADERS",
        ];
        // contains：覆盖 AWS_SECRET_ACCESS_KEY / *_CLIENT_SECRET /
        // GOOGLE_APPLICATION_CREDENTIALS / AWS_BEARER_TOKEN_BEDROCK 等变体。
        const SENSITIVE_CONTAINS: &[&str] = &[
            "SECRET",
            "PASSWORD",
            "PASSWD",
            "CREDENTIAL",
            "PRIVATE_KEY",
            "BEARER_TOKEN",
            "COOKIE",
            "AUTHORIZATION",
        ];

        SENSITIVE_EXACT.contains(&upper.as_str())
            || SENSITIVE_SUFFIXES.iter().any(|s| upper.ends_with(s))
            || SENSITIVE_CONTAINS.iter().any(|c| upper.contains(c))
    }

    /// Claude `env` 里的键能否进通用配置片段。
    ///
    /// 片段会合并进每一家勾选了它的供应商，所以只能放与供应商无关的设置。关键字段
    /// （请求发到哪、凭什么鉴权、哪个模型名、哪种协议）一旦进了片段，就会跟着切换带给
    /// 下一家：从 Bedrock 切到官方后，官方的 live 里还留着 `CLAUDE_CODE_USE_BEDROCK=1`，
    /// Claude Code 继续走 Bedrock。凭据另由 `is_sensitive_config_key` 统一剥离。
    fn claude_env_key_is_shared(key: &str) -> bool {
        // Context limits follow the actual upstream model. Sharing these
        // across providers can cap GPT/Kimi to the wrong window and make
        // Claude Code compact too early or miss the upstream limit.
        const UPSTREAM_WINDOW_KEYS: &[&str] = &[
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
            "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
        ];

        !crate::live::floor::claude_floor_env(key)
            && !UPSTREAM_WINDOW_KEYS.contains(&key)
            && !Self::is_sensitive_config_key(key)
    }

    /// Claude 顶层键能否进通用配置片段，口径同 [`Self::claude_env_key_is_shared`]。
    /// `model` 是 `/model` 保存的选择，属于当时那一家。
    fn claude_top_key_is_shared(key: &str) -> bool {
        !crate::live::floor::claude_floor_top(key) && !Self::is_sensitive_config_key(key)
    }

    /// 提取规则收紧后，把存量通用配置片段里按现行规则不再共享的条目（凭据、关键字段）
    /// 剥掉。返回 `None` 表示无需改动。
    ///
    /// 上游在「切走时按旧片段剥离供应商行」处理（`retired_snippet_entries`）；fork 没有
    /// `sync_common_config_snippet_from_live`，片段的条目只会留在片段本身（供应商行由
    /// `normalize_provider_common_config_for_storage` 负责剥离），所以在片段上归一化
    /// 一次即可达到同样效果。
    pub fn sanitize_claude_common_config_snippet(snippet: &str) -> Option<String> {
        let original: Value = serde_json::from_str(snippet).ok()?;
        let cleaned_text = Self::extract_claude_common_config(&original).ok()?;
        let cleaned: Value = serde_json::from_str(&cleaned_text).ok()?;
        (cleaned != original).then_some(cleaned_text)
    }

    /// Extract common config for Claude (JSON format)
    fn extract_claude_common_config(settings: &Value) -> Result<String, AppError> {
        let mut config = settings.clone();

        if let Some(obj) = config.as_object_mut() {
            if let Some(Value::Object(env)) = obj.get_mut("env") {
                env.retain(|key, _| Self::claude_env_key_is_shared(key));
            }
            obj.retain(|key, value| match key.as_str() {
                "env" => !value.as_object().is_some_and(|env| env.is_empty()),
                _ => Self::claude_top_key_is_shared(key),
            });
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

        // 关键字段（选路、模型名、推理档位等，见 `live::floor`）归供应商，不进片段：
        // 片段已冻结，收进去的值会从行里被迁移剥掉、再也写不回 live。
        let root = doc.as_table_mut();
        for key in crate::live::floor::CODEX_FLOOR_TOP {
            root.remove(key);
        }
        for path in crate::live::floor::CODEX_FLOOR_NESTED {
            let [parent, key] = path else { continue };
            if let Some(table) = root
                .get_mut(parent)
                .and_then(|item| item.as_table_like_mut())
            {
                table.remove(key);
                if table.is_empty() {
                    root.remove(parent);
                }
            }
        }

        // Remove entire model_providers table (provider-specific configuration)
        root.remove("model_providers");

        // MCP 服务器归 DB mcp_servers 表所有：进了共享片段会绕过按应用的
        // 启用状态被合并进所有勾选通用配置的供应商，且在通用配置编辑框里
        // 显示为一份"重复"的 MCP 配置。
        root.remove("mcp_servers");
        // 历史错误格式 [mcp.servers] 一并剥离（与 strip_codex_mcp_servers_from_settings
        // 一致）：sync_all_enabled 只管理 [mcp_servers.*]，legacy 形态一旦进了
        // 片段就会被合并进所有供应商，且没有任何同步路径能清掉这个孤儿。
        if let Some(mcp_tbl) = root
            .get_mut("mcp")
            .and_then(|item| item.as_table_like_mut())
        {
            mcp_tbl.remove("servers");
            if mcp_tbl.is_empty() {
                root.remove("mcp");
            }
        }

        // cc-switch 写 live 时注入的产物一律不进共享片段：
        // - experimental_bearer_token 正常写在 [model_providers.<id>] 内（上面
        //   整表已剥），但无活跃路由 / 内建保留 id / 路由表缺失三种 fallback
        //   会落在顶层——不剥等于把 API 密钥写进共享片段。
        root.remove("experimental_bearer_token");
        // - model_catalog_json 指向按供应商生成的 catalog 投影文件（DB 为 SSOT）。
        root.remove("model_catalog_json");
        // - web_search 只剥 cc-switch 注入的 "disabled" 哨兵；用户手设的其它值
        //   属于可共享偏好，保留。
        if root
            .get(crate::codex_config::CODEX_WEB_SEARCH_FIELD)
            .and_then(|item| item.as_str())
            == Some(crate::codex_config::CODEX_WEB_SEARCH_DISABLED)
        {
            root.remove(crate::codex_config::CODEX_WEB_SEARCH_FIELD);
        }

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

        // Keep DB and live projections in sync independently per application:
        // one broken config file must not prevent the other two apps from being
        // updated, but it must still be reported instead of returning success.
        let mut live_failures = Vec::new();

        // 同步到 Claude
        if let Some(mut claude_provider) = provider.to_claude_provider() {
            // 合并已有配置
            if let Some(existing) = state.db.get_provider_by_id(&claude_provider.id, "claude")? {
                let mut merged = existing.settings_config.clone();
                Self::merge_json(&mut merged, &claude_provider.settings_config);
                claude_provider.settings_config = merged;
                // 已有子供应商的应用专属配置与排序不属于统一供应商管理的字段。
                claude_provider.meta = existing.meta;
                claude_provider.created_at = existing.created_at;
                claude_provider.sort_index = existing.sort_index;
            }
            state.db.save_provider("claude", &claude_provider)?;
            Self::project_universal_child_to_live(
                state,
                AppType::Claude,
                &claude_provider.id,
                &mut live_failures,
            );
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
                // 已有子供应商的应用专属配置与排序不属于统一供应商管理的字段。
                codex_provider.meta = existing.meta;
                codex_provider.created_at = existing.created_at;
                codex_provider.sort_index = existing.sort_index;
            }
            state.db.save_provider("codex", &codex_provider)?;
            Self::project_universal_child_to_live(
                state,
                AppType::Codex,
                &codex_provider.id,
                &mut live_failures,
            );
        } else {
            let codex_id = format!("universal-codex-{id}");
            let _ = state.db.delete_provider("codex", &codex_id);
        }

        if live_failures.is_empty() {
            Ok(true)
        } else {
            Err(AppError::Message(format!(
                "统一供应商已保存到数据库，但以下应用的配置文件未能写入，仍是旧内容：{}。请重试同步，或切换一次该应用的供应商。",
                live_failures.join("、")
            )))
        }
    }

    /// Re-project a generated universal child only when it is the effective
    /// current provider for that app. Failures are collected by the caller so
    /// the other applications can continue syncing.
    fn project_universal_child_to_live(
        state: &AppState,
        app_type: AppType,
        child_id: &str,
        failures: &mut Vec<String>,
    ) {
        // 正在用的那家（代理模式下是代理路由）才需要重投影。
        let is_current = match crate::mode::current::provider_for(
            &state.db,
            &app_type,
            crate::mode::current::Purpose::InUse,
        ) {
            Ok(current) => current.as_deref() == Some(child_id),
            Err(err) => {
                log::warn!(
                    "读取 {} 当前供应商失败，跳过统一供应商的 live 重投影: {err}",
                    app_type.as_str()
                );
                failures.push(app_type.as_str().to_string());
                return;
            }
        };
        if !is_current {
            return;
        }

        if let Err(err) = Self::sync_current_provider_for_app(state, app_type.clone()) {
            log::warn!(
                "统一供应商同步后重写 {} live 配置失败: {err}",
                app_type.as_str()
            );
            failures.push(app_type.as_str().to_string());
        }
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
