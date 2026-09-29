use tauri::State;

use crate::app_config::AppType;
use crate::commands::codex_oauth::CodexOAuthState;
use crate::proxy::providers::codex_oauth_auth::{
    CodexOAuthError, OAuthAccount, OAuthDeviceCodeResponse,
};
use crate::store::AppState;

const AUTH_PROVIDER_CODEX_OAUTH: &str = "codex_oauth";

#[derive(Debug, Clone, serde::Serialize)]
pub struct ManagedAuthAccount {
    pub id: String,
    pub provider: String,
    pub login: String,
    pub avatar_url: Option<String>,
    pub authenticated_at: i64,
    pub is_default: bool,
    /// Codex 专用：旧账号缺少写入原生 Codex auth.json 所需的 id_token。
    pub reauth_required: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ManagedAuthStatus {
    pub provider: String,
    pub authenticated: bool,
    pub default_account_id: Option<String>,
    pub accounts: Vec<ManagedAuthAccount>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ManagedAuthDeviceCodeResponse {
    pub provider: String,
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

fn ensure_codex_oauth(auth_provider: &str) -> Result<(), String> {
    if auth_provider == AUTH_PROVIDER_CODEX_OAUTH {
        Ok(())
    } else {
        Err(format!("Unsupported auth provider: {auth_provider}"))
    }
}

fn map_account(account: OAuthAccount, default_account_id: Option<&str>) -> ManagedAuthAccount {
    ManagedAuthAccount {
        is_default: default_account_id == Some(account.id.as_str()),
        reauth_required: account.reauth_required,
        id: account.id,
        provider: AUTH_PROVIDER_CODEX_OAUTH.to_string(),
        login: account.login,
        avatar_url: account.avatar_url,
        authenticated_at: account.authenticated_at,
    }
}

fn map_device_code_response(response: OAuthDeviceCodeResponse) -> ManagedAuthDeviceCodeResponse {
    ManagedAuthDeviceCodeResponse {
        provider: AUTH_PROVIDER_CODEX_OAUTH.to_string(),
        device_code: response.device_code,
        user_code: response.user_code,
        verification_uri: response.verification_uri,
        expires_in: response.expires_in,
        interval: response.interval,
    }
}

#[tauri::command(rename_all = "camelCase")]
pub async fn auth_start_login(
    auth_provider: String,
    target_account_id: Option<String>,
    state: State<'_, CodexOAuthState>,
) -> Result<ManagedAuthDeviceCodeResponse, String> {
    ensure_codex_oauth(&auth_provider)?;
    let auth_manager = &state.0;
    auth_manager
        .start_device_flow(target_account_id.as_deref())
        .await
        .map(map_device_code_response)
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn auth_poll_for_account(
    auth_provider: String,
    device_code: String,
    app_state: State<'_, AppState>,
    state: State<'_, CodexOAuthState>,
) -> Result<Option<ManagedAuthAccount>, String> {
    ensure_codex_oauth(&auth_provider)?;
    let auth_manager = &state.0;
    // 提交账号前先拿 Codex 的代理切换锁：切换流程可能已经预检过 token 束，
    // 不串行化就会在删除/清空之后又被写回 auth.json。
    match auth_manager
        .poll_for_token(&device_code, || async {
            app_state
                .proxy_service
                .lock_switch_for_app(AppType::Codex.as_str())
                .await
        })
        .await
    {
        Ok(account) => {
            let default_account_id = auth_manager.get_status().await.default_account_id;
            Ok(account.map(|account| map_account(account, default_account_id.as_deref())))
        }
        Err(CodexOAuthError::AuthorizationPending) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command(rename_all = "camelCase")]
pub async fn auth_list_accounts(
    auth_provider: String,
    state: State<'_, CodexOAuthState>,
) -> Result<Vec<ManagedAuthAccount>, String> {
    ensure_codex_oauth(&auth_provider)?;
    let auth_manager = &state.0;
    let status = auth_manager.get_status().await;
    let default_account_id = status.default_account_id;
    Ok(status
        .accounts
        .into_iter()
        .map(|account| map_account(account, default_account_id.as_deref()))
        .collect())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn auth_get_status(
    auth_provider: String,
    state: State<'_, CodexOAuthState>,
) -> Result<ManagedAuthStatus, String> {
    ensure_codex_oauth(&auth_provider)?;
    let auth_manager = &state.0;
    let status = auth_manager.get_status().await;
    let default_account_id = status.default_account_id;
    Ok(ManagedAuthStatus {
        provider: AUTH_PROVIDER_CODEX_OAUTH.to_string(),
        authenticated: status.authenticated,
        default_account_id: default_account_id.clone(),
        accounts: status
            .accounts
            .into_iter()
            .map(|account| map_account(account, default_account_id.as_deref()))
            .collect(),
    })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn auth_remove_account(
    auth_provider: String,
    account_id: String,
    app_state: State<'_, AppState>,
) -> Result<(), String> {
    ensure_codex_oauth(&auth_provider)?;
    remove_codex_oauth_account_with_switch_lock(app_state.inner(), &account_id).await
}

/// 删除托管账号与「托管供应商增删改/切换/热切」串行化。
///
/// 否则一个已经预检过 token 束的切换流程可能在账号被删掉之后又把 auth.json 写回来。
pub(crate) async fn remove_codex_oauth_account_with_switch_lock(
    app_state: &AppState,
    account_id: &str,
) -> Result<(), String> {
    let _switch_guard = app_state
        .proxy_service
        .lock_switch_for_app(AppType::Codex.as_str())
        .await;
    app_state
        .codex_oauth_manager
        .remove_account(account_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn auth_set_default_account(
    auth_provider: String,
    account_id: String,
    state: State<'_, CodexOAuthState>,
) -> Result<(), String> {
    ensure_codex_oauth(&auth_provider)?;
    state
        .0
        .set_default_account(&account_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn auth_logout(
    auth_provider: String,
    app_state: State<'_, AppState>,
) -> Result<(), String> {
    ensure_codex_oauth(&auth_provider)?;
    logout_codex_oauth_with_switch_lock(app_state.inner()).await
}

/// 清空托管账号，同样与托管供应商写流程串行化（见上）。
pub(crate) async fn logout_codex_oauth_with_switch_lock(
    app_state: &AppState,
) -> Result<(), String> {
    let _switch_guard = app_state
        .proxy_service
        .lock_switch_for_app(AppType::Codex.as_str())
        .await;
    app_state
        .codex_oauth_manager
        .clear_auth()
        .await
        .map_err(|error| error.to_string())
}
