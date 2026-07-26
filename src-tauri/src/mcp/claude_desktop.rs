//! Claude Desktop (3P 实例) MCP 同步
//!
//! 把 cc-switch 管理的 MCP 服务器同步到 3P Claude Desktop 实例的
//! `claude_desktop_config.json` 的 `mcpServers` 段。3P 实例与用户原版 Desktop
//! 相互独立，互不干扰。

use crate::claude_desktop_config;
use crate::error::AppError;
use serde_json::Value;

/// 将单个 MCP 服务器同步到 3P Claude Desktop 配置。
///
/// 与 Claude Code 的同步同口径：按 `id` 作为 `mcpServers` 的键，整体替换该条目。
/// `server_spec` 是标准 `{ command, args, env }` 形态，与 Claude Desktop 期望一致。
pub fn sync_single_server_to_claude_desktop(
    id: &str,
    server_spec: &Value,
) -> Result<(), AppError> {
    let mut current = claude_desktop_config::read_mcp_servers_map()?;
    current.insert(id.to_string(), server_spec.clone());
    claude_desktop_config::write_mcp_servers_map(&current)
}

/// 从 3P Claude Desktop 配置中移除单个 MCP 服务器。
pub fn remove_server_from_claude_desktop(id: &str) -> Result<(), AppError> {
    let mut current = claude_desktop_config::read_mcp_servers_map()?;
    // 仅当确有该键时才写回，避免无谓的磁盘 IO。
    if current.remove(id).is_some() {
        claude_desktop_config::write_mcp_servers_map(&current)?;
    }
    Ok(())
}
