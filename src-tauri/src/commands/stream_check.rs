//! 供应商连通性检查命令
//!
//! 本检查只探测 base_url 是否可达，不发真实大模型请求，也不触碰故障转移熔断器。

use crate::app_config::AppType;
use crate::error::AppError;
use crate::services::stream_check::{
    HealthStatus, StreamCheckConfig, StreamCheckResult, StreamCheckService,
};
use crate::store::AppState;
use std::collections::HashSet;
use tauri::State;

#[tauri::command]
pub async fn stream_check_provider(
    state: State<'_, AppState>,
    app_type: AppType,
    provider_id: String,
) -> Result<StreamCheckResult, AppError> {
    let config = state.db.get_stream_check_config()?;
    let providers = state.db.get_all_providers(app_type.as_str())?;
    let provider = providers
        .get(&provider_id)
        .ok_or_else(|| AppError::Message(format!("供应商 {provider_id} 不存在")))?;
    let result = StreamCheckService::check_with_retry(&app_type, provider, &config, None).await?;

    let _ =
        state
            .db
            .save_stream_check_log(&provider_id, &provider.name, app_type.as_str(), &result);
    Ok(result)
}

#[tauri::command]
pub async fn stream_check_all_providers(
    state: State<'_, AppState>,
    app_type: AppType,
    proxy_targets_only: bool,
) -> Result<Vec<(String, StreamCheckResult)>, AppError> {
    let config = state.db.get_stream_check_config()?;
    let providers = state.db.get_all_providers(app_type.as_str())?;

    let allowed_ids: Option<HashSet<String>> = if proxy_targets_only {
        let mut ids = HashSet::new();
        if let Ok(Some(current_id)) = state.db.get_current_provider(app_type.as_str()) {
            ids.insert(current_id);
        }
        if let Ok(queue) = state.db.get_failover_queue(app_type.as_str()) {
            for item in queue {
                ids.insert(item.provider_id);
            }
        }
        Some(ids)
    } else {
        None
    };

    let mut results = Vec::new();
    for (id, provider) in providers {
        if provider.category.as_deref() == Some("official") {
            continue;
        }
        if allowed_ids.as_ref().is_some_and(|ids| !ids.contains(&id)) {
            continue;
        }

        let result = StreamCheckService::check_with_retry(&app_type, &provider, &config, None)
            .await
            .unwrap_or_else(|e| StreamCheckResult {
                status: HealthStatus::Failed,
                success: false,
                message: e.to_string(),
                response_time_ms: None,
                http_status: None,
                model_used: String::new(),
                tested_at: chrono::Utc::now().timestamp(),
                retry_count: 0,
                error_category: None,
            });

        let _ = state
            .db
            .save_stream_check_log(&id, &provider.name, app_type.as_str(), &result);
        results.push((id, result));
    }

    Ok(results)
}

#[tauri::command]
pub fn get_stream_check_config(state: State<'_, AppState>) -> Result<StreamCheckConfig, AppError> {
    state.db.get_stream_check_config()
}

#[tauri::command]
pub fn save_stream_check_config(
    state: State<'_, AppState>,
    config: StreamCheckConfig,
) -> Result<(), AppError> {
    state.db.save_stream_check_config(&config)
}
