use tauri::State;

use crate::commands::codex_oauth::CodexOAuthState;
use crate::proxy::providers::codex_oauth_auth::{
    CodexOAuthError, OAuthAccount, OAuthDeviceCodeResponse,
};

const AUTH_PROVIDER_CODEX_OAUTH: &str = "codex_oauth";

#[derive(Debug, Clone, serde::Serialize)]
pub struct ManagedAuthAccount {
    pub id: String,
    pub provider: String,
    pub login: String,
    pub avatar_url: Option<String>,
    pub authenticated_at: i64,
    pub is_default: bool,
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
    state: State<'_, CodexOAuthState>,
) -> Result<ManagedAuthDeviceCodeResponse, String> {
    ensure_codex_oauth(&auth_provider)?;
    let auth_manager = state.0.read().await;
    auth_manager
        .start_device_flow()
        .await
        .map(map_device_code_response)
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn auth_poll_for_account(
    auth_provider: String,
    device_code: String,
    state: State<'_, CodexOAuthState>,
) -> Result<Option<ManagedAuthAccount>, String> {
    ensure_codex_oauth(&auth_provider)?;
    let auth_manager = state.0.write().await;
    match auth_manager.poll_for_token(&device_code).await {
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
    let auth_manager = state.0.read().await;
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
    let auth_manager = state.0.read().await;
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
    state: State<'_, CodexOAuthState>,
) -> Result<(), String> {
    ensure_codex_oauth(&auth_provider)?;
    state
        .0
        .write()
        .await
        .remove_account(&account_id)
        .await
        .map_err(|e| e.to_string())
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
        .write()
        .await
        .set_default_account(&account_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn auth_logout(
    auth_provider: String,
    state: State<'_, CodexOAuthState>,
) -> Result<(), String> {
    ensure_codex_oauth(&auth_provider)?;
    state
        .0
        .write()
        .await
        .clear_auth()
        .await
        .map_err(|e| e.to_string())
}
