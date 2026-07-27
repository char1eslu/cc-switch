//! MCP (Model Context Protocol) 服务器管理模块
//!
//! 本模块负责 MCP 服务器配置的验证、同步和导入导出。
//!
//! ## 模块结构
//!
//! - `validation` - 服务器配置验证
//! - `claude` - Claude MCP 同步和导入
//! - `codex` - Codex MCP 同步和导入（含 TOML 转换）
//!
//! ## 不支持 Claude Desktop
//!
//! cc-switch 管理的 3P Claude Desktop 实例运行在 gateway 模式
//! （profile 里 `inferenceProvider: "gateway"`），该模式下 Desktop 从
//! managed config 加载 MCP，`claude_desktop_config.json` 的 `mcpServers`
//! 段被忽略并标记为 invalid 跳过。因此 MCP 不同步到 Claude Desktop，
//! 与上游一致。

mod claude;
mod codex;
mod validation;

// 重新导出公共 API
pub use claude::{
    import_from_claude, remove_server_from_claude, sync_enabled_to_claude,
    sync_single_server_to_claude,
};
pub use codex::{
    import_from_codex, remove_server_from_codex, sync_enabled_to_codex, sync_single_server_to_codex,
};
