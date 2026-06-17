//! MCP 服务器配置验证模块

use serde_json::Value;

use crate::error::AppError;

fn normalized_type(spec: &Value) -> &str {
    match spec.get("type").and_then(|x| x.as_str()) {
        Some("streamable-http") => "http",
        Some(t) => t,
        None if spec.get("url").and_then(|x| x.as_str()).is_some() => "http",
        None => "stdio",
    }
}

/// 基础校验：允许 stdio/http/sse；或省略 type 时从 command/url 推断。
pub fn validate_server_spec(spec: &Value) -> Result<(), AppError> {
    if !spec.is_object() {
        return Err(AppError::McpValidation(
            "MCP 服务器连接定义必须为 JSON 对象".into(),
        ));
    }
    let typ = normalized_type(spec);
    let is_stdio = typ == "stdio";
    let is_http = typ == "http";
    let is_sse = typ == "sse";

    if !(is_stdio || is_http || is_sse) {
        return Err(AppError::McpValidation(
            "MCP 服务器 type 必须是 'stdio'、'http'、'streamable-http' 或 'sse'（或省略并提供 command/url）".into(),
        ));
    }

    if is_stdio {
        let cmd = spec.get("command").and_then(|x| x.as_str()).unwrap_or("");
        if cmd.trim().is_empty() {
            return Err(AppError::McpValidation(
                "stdio 类型的 MCP 服务器缺少 command 字段".into(),
            ));
        }
    }
    if is_http {
        let url = spec.get("url").and_then(|x| x.as_str()).unwrap_or("");
        if url.trim().is_empty() {
            return Err(AppError::McpValidation(
                "http 类型的 MCP 服务器缺少 url 字段".into(),
            ));
        }
    }
    if is_sse {
        let url = spec.get("url").and_then(|x| x.as_str()).unwrap_or("");
        if url.trim().is_empty() {
            return Err(AppError::McpValidation(
                "sse 类型的 MCP 服务器缺少 url 字段".into(),
            ));
        }
    }
    Ok(())
}

/// 从 MCP 条目中提取服务器规范
pub fn extract_server_spec(entry: &Value) -> Result<Value, AppError> {
    let obj = entry
        .as_object()
        .ok_or_else(|| AppError::McpValidation("MCP 服务器条目必须为 JSON 对象".into()))?;
    let server = obj
        .get("server")
        .ok_or_else(|| AppError::McpValidation("MCP 服务器条目缺少 server 字段".into()))?;

    if !server.is_object() {
        return Err(AppError::McpValidation(
            "MCP 服务器 server 字段必须为 JSON 对象".into(),
        ));
    }

    Ok(server.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_url_only_server_as_http() {
        let spec = json!({
            "url": "https://example.test/mcp"
        });

        assert!(validate_server_spec(&spec).is_ok());
    }

    #[test]
    fn validates_streamable_http_as_http() {
        let spec = json!({
            "type": "streamable-http",
            "url": "https://example.test/mcp"
        });

        assert!(validate_server_spec(&spec).is_ok());
    }
}
