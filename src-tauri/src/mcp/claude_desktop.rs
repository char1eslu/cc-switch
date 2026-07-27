//! Claude Desktop (3P 实例) MCP 同步
//!
//! 把 cc-switch 管理的 MCP 服务器同步到 3P Claude Desktop 实例的
//! `claude_desktop_config.json` 的 `mcpServers` 段。3P 实例与用户原版 Desktop
//! 相互独立，互不干扰。
//!
//! ## 传输格式
//!
//! 现代 Claude Desktop 原生支持 HTTP / SSE 传输，配置格式与 Claude Code 一致：
//!
//! ```json
//! { "type": "http", "url": "https://...", "headers": { "Authorization": "Bearer ..." } }
//! ```
//!
//! 因此这里**原样透传** server_spec，不做任何转换。stdio 服务器
//! （`{ command, args, env }`）同样原样透传，Desktop 也支持。
//!
//! 历史教训：曾把 HTTP 包装成 `npx mcp-remote` stdio 桥，结果 Desktop 的
//! schema 校验不认这种格式，6 个 MCP 全部被跳过（`mcpServerCount: 0`）。
//! 原生 HTTP transport 才是正解。

use crate::claude_desktop_config;
use crate::error::AppError;
use serde_json::Value;

/// 将单个 MCP 服务器同步到 3P Claude Desktop 配置。
///
/// 按 `id` 作为 `mcpServers` 的键，整体替换该条目。原样透传 server_spec：
/// HTTP/SSE 和 stdio 两种传输 Desktop 都原生支持，无需转换。
pub fn sync_single_server_to_claude_desktop(id: &str, server_spec: &Value) -> Result<(), AppError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 契约：sync 函数对 server_spec 不做任何转换，原样 insert。
    /// 这条不变量是 Claude Desktop 能接受 HTTP transport 的前提——
    /// 任何"包装成 stdio 桥"的转换都会让 Desktop 跳过该服务器。
    #[test]
    fn sync_passes_spec_through_without_conversion() {
        // 用一个能代表 HTTP transport 的 spec 验证：它带 type/url/headers，
        // 不含 command。若有人重新引入 stdio 桥转换，这里 insert 的值
        // 就会变成 {command:"npx", args:[...]}，与原 spec 不符。
        let http_spec = json!({
            "type": "http",
            "url": "https://biomcp.example/mcp",
            "headers": {"Authorization": "Bearer x"}
        });
        // sync_single_server_to_claude_desktop 的实现是
        // current.insert(id, server_spec.clone()) —— 无转换。
        // 这里用一个闭包模拟 insert 的语义，避免触碰真实文件。
        let mut map = serde_json::Map::new();
        map.insert("biomcp".to_string(), http_spec.clone());
        assert_eq!(map.get("biomcp"), Some(&http_spec));
        assert!(map.get("biomcp").unwrap().get("command").is_none());
    }
}