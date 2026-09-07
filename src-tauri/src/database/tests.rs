//! 数据库模块测试
//!
//! 包含 Schema 迁移和基本功能的测试。

use super::*;
use crate::app_config::MultiAppConfig;
use crate::provider::{Provider, ProviderManager};
use indexmap::IndexMap;
use rusqlite::{params, Connection};
use serde_json::json;
use std::collections::HashMap;
use tempfile::NamedTempFile;

const LEGACY_SCHEMA_SQL: &str = r#"
    CREATE TABLE providers (
        id TEXT NOT NULL,
        app_type TEXT NOT NULL,
        name TEXT NOT NULL,
        settings_config TEXT NOT NULL,
        PRIMARY KEY (id, app_type)
    );
    CREATE TABLE provider_endpoints (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        provider_id TEXT NOT NULL,
        app_type TEXT NOT NULL,
        url TEXT NOT NULL
    );
    CREATE TABLE mcp_servers (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        server_config TEXT NOT NULL
    );
    CREATE TABLE prompts (
        id TEXT NOT NULL,
        app_type TEXT NOT NULL,
        name TEXT NOT NULL,
        content TEXT NOT NULL,
        PRIMARY KEY (id, app_type)
    );
    CREATE TABLE skills (
        key TEXT PRIMARY KEY,
        installed BOOLEAN NOT NULL DEFAULT 0
    );
    CREATE TABLE skill_repos (
        owner TEXT NOT NULL,
        name TEXT NOT NULL,
        PRIMARY KEY (owner, name)
    );
    CREATE TABLE settings (
        key TEXT PRIMARY KEY,
        value TEXT
    );
"#;

// v3.8.x（schema v1）的真实表结构快照：用于验证从 v3.8.* 升级到当前版本的迁移链路
// 参考：tag v3.8.3 的 src-tauri/src/database/schema.rs
const V3_8_SCHEMA_V1_SQL: &str = r#"
    CREATE TABLE providers (
        id TEXT NOT NULL,
        app_type TEXT NOT NULL,
        name TEXT NOT NULL,
        settings_config TEXT NOT NULL,
        website_url TEXT,
        category TEXT,
        created_at INTEGER,
        sort_index INTEGER,
        notes TEXT,
        icon TEXT,
        icon_color TEXT,
        meta TEXT NOT NULL DEFAULT '{}',
        is_current BOOLEAN NOT NULL DEFAULT 0,
        PRIMARY KEY (id, app_type)
    );
    CREATE TABLE provider_endpoints (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        provider_id TEXT NOT NULL,
        app_type TEXT NOT NULL,
        url TEXT NOT NULL,
        added_at INTEGER,
        FOREIGN KEY (provider_id, app_type) REFERENCES providers(id, app_type) ON DELETE CASCADE
    );
    CREATE TABLE mcp_servers (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        server_config TEXT NOT NULL,
        description TEXT,
        homepage TEXT,
        docs TEXT,
        tags TEXT NOT NULL DEFAULT '[]',
        enabled_claude BOOLEAN NOT NULL DEFAULT 0,
        enabled_codex BOOLEAN NOT NULL DEFAULT 0
    );
    CREATE TABLE prompts (
        id TEXT NOT NULL,
        app_type TEXT NOT NULL,
        name TEXT NOT NULL,
        content TEXT NOT NULL,
        description TEXT,
        enabled BOOLEAN NOT NULL DEFAULT 1,
        created_at INTEGER,
        updated_at INTEGER,
        PRIMARY KEY (id, app_type)
    );
    CREATE TABLE skills (
        key TEXT PRIMARY KEY,
        installed BOOLEAN NOT NULL DEFAULT 0,
        installed_at INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE skill_repos (
        owner TEXT NOT NULL,
        name TEXT NOT NULL,
        branch TEXT NOT NULL DEFAULT 'main',
        enabled BOOLEAN NOT NULL DEFAULT 1,
        PRIMARY KEY (owner, name)
    );
    CREATE TABLE settings (
        key TEXT PRIMARY KEY,
        value TEXT
    );
"#;

#[derive(Debug)]
struct ColumnInfo {
    r#type: String,
    notnull: i64,
    default: Option<String>,
}

fn get_column_info(conn: &Connection, table: &str, column: &str) -> ColumnInfo {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info(\"{table}\");"))
        .expect("prepare pragma");
    let mut rows = stmt.query([]).expect("query pragma");
    while let Some(row) = rows.next().expect("read row") {
        let column_name: String = row.get(1).expect("name");
        if column_name.eq_ignore_ascii_case(column) {
            return ColumnInfo {
                r#type: row.get::<_, String>(2).expect("type"),
                notnull: row.get::<_, i64>(3).expect("notnull"),
                default: row.get::<_, Option<String>>(4).ok().flatten(),
            };
        }
    }
    panic!("column {table}.{column} not found");
}

fn normalize_default(default: &Option<String>) -> Option<String> {
    default
        .as_ref()
        .map(|s| s.trim_matches('\'').trim_matches('"').to_string())
}

#[test]
fn schema_migration_sets_user_version_when_missing() {
    let conn = Connection::open_in_memory().expect("open memory db");

    Database::create_tables_on_conn(&conn).expect("create tables");
    assert_eq!(
        Database::get_user_version(&conn).expect("read version before"),
        0
    );

    Database::apply_schema_migrations_on_conn(&conn).expect("apply migration");

    assert_eq!(
        Database::get_user_version(&conn).expect("read version after"),
        SCHEMA_VERSION
    );
}

#[test]
fn schema_migration_rejects_future_version() {
    let conn = Connection::open_in_memory().expect("open memory db");
    Database::create_tables_on_conn(&conn).expect("create tables");
    Database::set_user_version(&conn, SCHEMA_VERSION + 1).expect("set future version");

    let err =
        Database::apply_schema_migrations_on_conn(&conn).expect_err("should reject higher version");
    assert!(
        err.to_string().contains("数据库版本过新"),
        "unexpected error: {err}"
    );
}

#[test]
fn schema_migration_adds_missing_columns_for_providers() {
    let conn = Connection::open_in_memory().expect("open memory db");

    // 创建旧版 providers 表，缺少新增列
    conn.execute_batch(LEGACY_SCHEMA_SQL)
        .expect("seed old schema");

    Database::apply_schema_migrations_on_conn(&conn).expect("apply migrations");

    // 验证关键新增列已补齐
    for (table, column) in [
        ("providers", "meta"),
        ("providers", "is_current"),
        ("provider_endpoints", "added_at"),
        ("prompts", "updated_at"),
        ("skills", "installed_at"),
        ("skill_repos", "enabled"),
    ] {
        assert!(
            Database::has_column(&conn, table, column).expect("check column"),
            "{table}.{column} should exist after migration"
        );
    }

    // 验证 meta 列约束保持一致
    let meta = get_column_info(&conn, "providers", "meta");
    assert_eq!(meta.notnull, 1, "meta should be NOT NULL");
    assert_eq!(
        normalize_default(&meta.default).as_deref(),
        Some("{}"),
        "meta default should be '{{}}'"
    );

    assert_eq!(
        Database::get_user_version(&conn).expect("version after migration"),
        SCHEMA_VERSION
    );
}

#[test]
fn schema_migration_aligns_column_defaults_and_types() {
    let conn = Connection::open_in_memory().expect("open memory db");
    conn.execute_batch(LEGACY_SCHEMA_SQL)
        .expect("seed old schema");

    Database::apply_schema_migrations_on_conn(&conn).expect("apply migrations");

    let is_current = get_column_info(&conn, "providers", "is_current");
    assert_eq!(is_current.r#type, "BOOLEAN");
    assert_eq!(is_current.notnull, 1);
    assert_eq!(normalize_default(&is_current.default).as_deref(), Some("0"));

    let tags = get_column_info(&conn, "mcp_servers", "tags");
    assert_eq!(tags.r#type, "TEXT");
    assert_eq!(tags.notnull, 1);
    assert_eq!(normalize_default(&tags.default).as_deref(), Some("[]"));

    let enabled = get_column_info(&conn, "prompts", "enabled");
    assert_eq!(enabled.r#type, "BOOLEAN");
    assert_eq!(enabled.notnull, 1);
    assert_eq!(normalize_default(&enabled.default).as_deref(), Some("1"));

    let installed_at = get_column_info(&conn, "skills", "installed_at");
    assert_eq!(installed_at.r#type, "INTEGER");
    assert_eq!(installed_at.notnull, 1);
    assert_eq!(
        normalize_default(&installed_at.default).as_deref(),
        Some("0")
    );

    let branch = get_column_info(&conn, "skill_repos", "branch");
    assert_eq!(branch.r#type, "TEXT");
    assert_eq!(normalize_default(&branch.default).as_deref(), Some("main"));

    let skill_repo_enabled = get_column_info(&conn, "skill_repos", "enabled");
    assert_eq!(skill_repo_enabled.r#type, "BOOLEAN");
    assert_eq!(skill_repo_enabled.notnull, 1);
    assert_eq!(
        normalize_default(&skill_repo_enabled.default).as_deref(),
        Some("1")
    );
}

#[test]
fn schema_create_tables_include_pricing_model_columns() {
    let conn = Connection::open_in_memory().expect("open memory db");
    Database::create_tables_on_conn(&conn).expect("create tables");

    let multiplier = get_column_info(&conn, "proxy_config", "default_cost_multiplier");
    assert_eq!(multiplier.r#type, "TEXT");
    assert_eq!(multiplier.notnull, 1);
    assert_eq!(normalize_default(&multiplier.default).as_deref(), Some("1"));

    let pricing_source = get_column_info(&conn, "proxy_config", "pricing_model_source");
    assert_eq!(pricing_source.r#type, "TEXT");
    assert_eq!(pricing_source.notnull, 1);
    assert_eq!(
        normalize_default(&pricing_source.default).as_deref(),
        Some("response")
    );

    let request_model = get_column_info(&conn, "proxy_request_logs", "request_model");
    assert_eq!(request_model.r#type, "TEXT");
    assert_eq!(request_model.notnull, 0);
}

#[test]
fn schema_migration_v4_adds_pricing_model_columns() {
    let conn = Connection::open_in_memory().expect("open memory db");
    conn.execute_batch(
        r#"
        CREATE TABLE providers (
            id TEXT NOT NULL,
            app_type TEXT NOT NULL,
            name TEXT NOT NULL,
            settings_config TEXT NOT NULL DEFAULT '{}',
            meta TEXT NOT NULL DEFAULT '{}',
            PRIMARY KEY (id, app_type)
        );
        CREATE TABLE proxy_config (app_type TEXT PRIMARY KEY);
        CREATE TABLE proxy_request_logs (request_id TEXT PRIMARY KEY, model TEXT NOT NULL);
        CREATE TABLE mcp_servers (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            server_config TEXT NOT NULL,
            enabled_claude INTEGER NOT NULL DEFAULT 0,
            enabled_codex INTEGER NOT NULL DEFAULT 0
        );
        "#,
    )
    .expect("seed v4 schema");

    Database::set_user_version(&conn, 4).expect("set user_version=4");
    Database::apply_schema_migrations_on_conn(&conn).expect("apply migrations");

    let multiplier = get_column_info(&conn, "proxy_config", "default_cost_multiplier");
    assert_eq!(multiplier.r#type, "TEXT");
    assert_eq!(multiplier.notnull, 1);
    assert_eq!(normalize_default(&multiplier.default).as_deref(), Some("1"));

    let pricing_source = get_column_info(&conn, "proxy_config", "pricing_model_source");
    assert_eq!(pricing_source.r#type, "TEXT");
    assert_eq!(pricing_source.notnull, 1);
    assert_eq!(
        normalize_default(&pricing_source.default).as_deref(),
        Some("response")
    );

    let request_model = get_column_info(&conn, "proxy_request_logs", "request_model");
    assert_eq!(request_model.r#type, "TEXT");
    assert_eq!(request_model.notnull, 0);

    assert_eq!(
        Database::get_user_version(&conn).expect("version after migration"),
        SCHEMA_VERSION
    );
}

#[test]
fn migration_v10_to_v11_rebuilds_rollups_with_request_model_dimension() {
    let conn = Connection::open_in_memory().expect("open memory db");

    // 模拟 v10 形状的 rollup 表（主键不含 request_model）+ 一行历史聚合数据，
    // 以及 v10 形状的明细表（无 pricing_model 列）
    conn.execute_batch(
        r#"
        CREATE TABLE proxy_request_logs (
            request_id TEXT PRIMARY KEY,
            model TEXT NOT NULL,
            request_model TEXT
        );
        CREATE TABLE usage_daily_rollups (
            date TEXT NOT NULL,
            app_type TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            model TEXT NOT NULL,
            request_count INTEGER NOT NULL DEFAULT 0,
            success_count INTEGER NOT NULL DEFAULT 0,
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            cache_read_tokens INTEGER NOT NULL DEFAULT 0,
            cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
            total_cost_usd TEXT NOT NULL DEFAULT '0',
            avg_latency_ms INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (date, app_type, provider_id, model)
        );
        INSERT INTO usage_daily_rollups
            (date, app_type, provider_id, model, request_count, success_count,
             input_tokens, output_tokens, total_cost_usd, avg_latency_ms)
        VALUES ('2026-05-01', 'claude', 'p1', 'kimi-k2', 7, 7, 1000, 500, '0.07', 120);
        "#,
    )
    .expect("seed v10 rollup table");

    Database::set_user_version(&conn, 10).expect("set user_version=10");
    Database::apply_schema_migrations_on_conn(&conn).expect("apply migrations");

    // 新列存在且 NOT NULL DEFAULT ''
    let request_model = get_column_info(&conn, "usage_daily_rollups", "request_model");
    assert_eq!(request_model.r#type, "TEXT");
    assert_eq!(request_model.notnull, 1);
    let rollup_pricing_model = get_column_info(&conn, "usage_daily_rollups", "pricing_model");
    assert_eq!(rollup_pricing_model.r#type, "TEXT");
    assert_eq!(rollup_pricing_model.notnull, 1);

    // 明细表补上 pricing_model 列（可空，历史行 NULL）
    let pricing_model = get_column_info(&conn, "proxy_request_logs", "pricing_model");
    assert_eq!(pricing_model.r#type, "TEXT");
    assert_eq!(pricing_model.notnull, 0);

    // 历史行保留，request_model 填 ''（未知）
    let (rm, count, input, cost): (String, i64, i64, String) = conn
        .query_row(
            "SELECT request_model, request_count, input_tokens, total_cost_usd
             FROM usage_daily_rollups WHERE model = 'kimi-k2'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("migrated row");
    assert_eq!(rm, "");
    assert_eq!(count, 7);
    assert_eq!(input, 1000);
    assert_eq!(cost, "0.07");

    // 主键包含 request_model：同 model 不同别名可共存
    conn.execute(
        "INSERT INTO usage_daily_rollups
            (date, app_type, provider_id, model, request_model, request_count)
         VALUES ('2026-05-01', 'claude', 'p1', 'kimi-k2', 'claude-sonnet-4-6', 1)",
        [],
    )
    .expect("insert row with same model but different request_model");

    assert_eq!(
        Database::get_user_version(&conn).expect("version after migration"),
        SCHEMA_VERSION
    );
}

#[test]
fn schema_create_tables_repairs_legacy_proxy_config_singleton_to_per_app() {
    let conn = Connection::open_in_memory().expect("open memory db");

    // 模拟测试版 v2：user_version=2，但 proxy_config 仍是单例结构（无 app_type）
    Database::set_user_version(&conn, 2).expect("set user_version");
    conn.execute_batch(
        r#"
        CREATE TABLE proxy_config (
            id INTEGER PRIMARY KEY,
            enabled INTEGER NOT NULL DEFAULT 0,
            listen_address TEXT NOT NULL DEFAULT '127.0.0.1',
            listen_port INTEGER NOT NULL DEFAULT 5000,
            max_retries INTEGER NOT NULL DEFAULT 3,
            request_timeout INTEGER NOT NULL DEFAULT 300,
            enable_logging INTEGER NOT NULL DEFAULT 1,
            target_app TEXT NOT NULL DEFAULT 'claude',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        INSERT INTO proxy_config (id, enabled) VALUES (1, 1);
        "#,
    )
    .expect("seed legacy proxy_config");

    Database::create_tables_on_conn(&conn).expect("create tables should repair proxy_config");

    assert!(
        Database::has_column(&conn, "proxy_config", "app_type").expect("check app_type"),
        "proxy_config should be migrated to per-app structure"
    );

    let count: i32 = conn
        .query_row("SELECT COUNT(*) FROM proxy_config", [], |r| r.get(0))
        .expect("count rows");
    assert_eq!(count, 2, "per-app proxy_config should have 2 rows");

    // 新结构下应能按 app_type 查询
    let _: i32 = conn
        .query_row(
            "SELECT COUNT(*) FROM proxy_config WHERE app_type = 'claude'",
            [],
            |r| r.get(0),
        )
        .expect("query by app_type");
}

#[test]
fn migration_from_v3_8_schema_v1_to_current_schema_v3() {
    let conn = Connection::open_in_memory().expect("open memory db");
    conn.execute("PRAGMA foreign_keys = ON;", [])
        .expect("enable foreign keys");

    // 模拟 v3.8.* 用户的数据库（schema v1）
    conn.execute_batch(V3_8_SCHEMA_V1_SQL)
        .expect("seed v3.8 schema v1");
    Database::set_user_version(&conn, 1).expect("set user_version=1");

    // 插入一条旧版 Provider + Skill（用于验证迁移不会破坏既有数据）
    conn.execute(
        "INSERT INTO providers (
            id, app_type, name, settings_config, website_url, category,
            created_at, sort_index, notes, icon, icon_color, meta, is_current
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            "p1",
            "claude",
            "Test Provider",
            serde_json::to_string(&json!({ "anthropicApiKey": "sk-test" })).unwrap(),
            Option::<String>::None,
            Option::<String>::None,
            Option::<i64>::None,
            Option::<usize>::None,
            Option::<String>::None,
            Option::<String>::None,
            Option::<String>::None,
            "{}",
            1,
        ],
    )
    .expect("seed provider");

    conn.execute(
        "INSERT INTO skills (key, installed, installed_at) VALUES (?1, ?2, ?3)",
        params!["claude:demo-skill", 1, 1700000000i64],
    )
    .expect("seed legacy skill");

    // 按应用启动流程：先 create_tables（补齐新增表），再 apply_schema_migrations（按 user_version 迁移）
    Database::create_tables_on_conn(&conn).expect("create tables");
    Database::apply_schema_migrations_on_conn(&conn).expect("apply migrations");

    assert_eq!(
        Database::get_user_version(&conn).expect("user_version after migration"),
        SCHEMA_VERSION
    );

    // v1 -> v2：providers 新增字段必须补齐
    for column in [
        "cost_multiplier",
        "limit_daily_usd",
        "limit_monthly_usd",
        "provider_type",
        "in_failover_queue",
    ] {
        assert!(
            Database::has_column(&conn, "providers", column).expect("check column"),
            "providers.{column} should exist after migration"
        );
    }

    // 旧 provider 不应丢失，且新增字段应有默认值
    let provider_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM providers WHERE id = 'p1' AND app_type = 'claude'",
            [],
            |r| r.get(0),
        )
        .expect("count providers");
    assert_eq!(provider_count, 1);

    let cost_multiplier: String = conn
        .query_row(
            "SELECT cost_multiplier FROM providers WHERE id = 'p1' AND app_type = 'claude'",
            [],
            |r| r.get(0),
        )
        .expect("read cost_multiplier");
    assert_eq!(cost_multiplier, "1.0");

    // v2 -> v3：skills 表重建为统一结构，并设置 pending 标记（后续由启动时扫描文件系统重建数据）
    assert!(
        Database::has_column(&conn, "skills", "enabled_claude").expect("check skills v3 column"),
        "skills table should be migrated to v3 structure"
    );
    let skills_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM skills", [], |r| r.get(0))
        .expect("count skills");
    assert_eq!(skills_count, 0, "skills table should be rebuilt empty");

    let pending: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'skills_ssot_migration_pending'",
            [],
            |r| r.get(0),
        )
        .ok();
    assert!(
        matches!(pending.as_deref(), Some("true") | Some("1")),
        "skills_ssot_migration_pending should be set after v2->v3 migration"
    );
    let snapshot: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'skills_ssot_migration_snapshot'",
            [],
            |r| r.get(0),
        )
        .ok();
    let snapshot = snapshot.expect("skills migration snapshot should be recorded");
    let snapshot_rows: serde_json::Value =
        serde_json::from_str(&snapshot).expect("parse skills migration snapshot");
    assert!(
        snapshot_rows
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| {
                row.get("directory").and_then(|v| v.as_str()) == Some("demo-skill")
                    && row.get("app_type").and_then(|v| v.as_str()) == Some("claude")
            })),
        "skills migration snapshot should preserve legacy app mapping"
    );

    // 上游兼容 schema 保留四行；fork UI 只使用 claude + codex。
    let proxy_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM proxy_config", [], |r| r.get(0))
        .expect("count proxy_config rows");
    assert_eq!(proxy_rows, 4);

    // model_pricing 应具备默认数据（迁移时会 seed）
    let pricing_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM model_pricing", [], |r| r.get(0))
        .expect("count model_pricing rows");
    assert!(pricing_rows > 0, "model_pricing should be seeded");
}

#[test]
fn schema_dry_run_does_not_write_to_disk() {
    // Create minimal valid config for migration
    let mut apps = HashMap::new();
    apps.insert("claude".to_string(), ProviderManager::default());

    let config = MultiAppConfig {
        version: 2,
        apps,
        mcp: Default::default(),
        prompts: Default::default(),
        skills: Default::default(),
        common_config_snippets: Default::default(),
        claude_common_config_snippet: None,
    };

    // Dry-run should succeed without any file I/O errors
    let result = Database::migrate_from_json_dry_run(&config);
    assert!(
        result.is_ok(),
        "Dry-run should succeed with valid config: {result:?}"
    );
}

#[test]
fn dry_run_validates_schema_compatibility() {
    // Create config with actual provider data
    let mut providers = IndexMap::new();
    providers.insert(
        "test-provider".to_string(),
        Provider {
            id: "test-provider".to_string(),
            name: "Test Provider".to_string(),
            settings_config: json!({
                "anthropicApiKey": "sk-test-123",
            }),
            website_url: None,
            category: None,
            created_at: Some(1234567890),
            sort_index: None,
            notes: None,
            meta: None,
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        },
    );

    let manager = ProviderManager {
        providers,
        current: "test-provider".to_string(),
    };

    let mut apps = HashMap::new();
    apps.insert("claude".to_string(), manager);

    let config = MultiAppConfig {
        version: 2,
        apps,
        mcp: Default::default(),
        prompts: Default::default(),
        skills: Default::default(),
        common_config_snippets: Default::default(),
        claude_common_config_snippet: None,
    };

    // Dry-run should validate the full migration path
    let result = Database::migrate_from_json_dry_run(&config);
    assert!(
        result.is_ok(),
        "Dry-run should succeed with provider data: {result:?}"
    );
}

#[test]
fn schema_model_pricing_is_seeded_on_init() {
    let db = Database::memory().expect("create memory db");

    let conn = db.conn.lock().expect("lock conn");

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM model_pricing", [], |row| row.get(0))
        .expect("count pricing");

    assert!(
        count > 0,
        "模型定价数据应该在初始化时自动填充，实际数量: {}",
        count
    );

    // 验证包含 Claude 模型
    let claude_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM model_pricing WHERE model_id LIKE 'claude-%'",
            [],
            |row| row.get(0),
        )
        .expect("check claude");
    assert!(
        claude_count > 0,
        "应该包含 Claude 模型定价，实际数量: {}",
        claude_count
    );

    // 验证包含 GPT 模型
    let gpt_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM model_pricing WHERE model_id LIKE 'gpt-%'",
            [],
            |row| row.get(0),
        )
        .expect("check gpt");
    assert!(
        gpt_count > 0,
        "应该包含 GPT 模型定价，实际数量: {}",
        gpt_count
    );
}

#[test]
fn model_pricing_seed_repairs_known_outdated_builtin_prices() {
    let db = Database::memory().expect("create memory db");

    {
        let conn = db.conn.lock().expect("lock conn");
        conn.execute(
            "UPDATE model_pricing
             SET input_cost_per_million = '1.68',
                 output_cost_per_million = '3.36',
                 cache_read_cost_per_million = '0.14',
                 cache_creation_cost_per_million = '0'
             WHERE model_id = 'deepseek-v4-pro'",
            [],
        )
        .expect("restore old DeepSeek price");
        conn.execute(
            "UPDATE model_pricing
             SET input_cost_per_million = '9',
                 output_cost_per_million = '9',
                 cache_read_cost_per_million = '9',
                 cache_creation_cost_per_million = '0'
             WHERE model_id = 'glm-5.1'",
            [],
        )
        .expect("set custom GLM price");
    }

    db.ensure_model_pricing_seeded()
        .expect("ensure pricing seeded");

    let conn = db.conn.lock().expect("lock conn");
    let deepseek: (String, String, String) = conn
        .query_row(
            "SELECT input_cost_per_million, output_cost_per_million, cache_read_cost_per_million
             FROM model_pricing WHERE model_id = 'deepseek-v4-pro'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("query DeepSeek price");
    // 从远古价 1.68/3.36/0.14 出发要连跳两级才能到位：
    //   1.68/3.36/0.14 →(2026-07 条目)→ 0.435/0.87/0.003625
    //                  →(2026-08-16 峰谷调价条目)→ 1.32/3.96/0.044
    // 这同时锁住了 repair 条目的顺序：新条目必须排在旧条目之后，
    // 否则老库会停在中间价位，本断言即会失败。
    assert_eq!(
        deepseek,
        ("1.32".to_string(), "3.96".to_string(), "0.044".to_string())
    );

    let glm: (String, String, String) = conn
        .query_row(
            "SELECT input_cost_per_million, output_cost_per_million, cache_read_cost_per_million
             FROM model_pricing WHERE model_id = 'glm-5.1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("query GLM price");
    assert_eq!(glm, ("9".to_string(), "9".to_string(), "9".to_string()));
}

#[test]
fn model_pricing_seed_includes_claude_5_1_and_standard_sonnet_5_prices() {
    let db = Database::memory().expect("create memory db");
    let conn = db.conn.lock().expect("lock conn");

    for model_id in ["claude-fable-5-1", "claude-mythos-5-1"] {
        let price: (String, String, String, String) = conn
            .query_row(
                "SELECT input_cost_per_million, output_cost_per_million,
                        cache_read_cost_per_million, cache_creation_cost_per_million
                 FROM model_pricing WHERE model_id = ?1",
                [model_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("query Fable 5.1 family price");
        // 缓存读 0.025x = $0.25，不是 Fable 5 的 $1
        assert_eq!(
            price,
            (
                "10".to_string(),
                "50".to_string(),
                "0.25".to_string(),
                "12.50".to_string(),
            ),
            "{model_id}"
        );
    }

    let sonnet: (String, String, String, String) = conn
        .query_row(
            "SELECT input_cost_per_million, output_cost_per_million,
                    cache_read_cost_per_million, cache_creation_cost_per_million
             FROM model_pricing WHERE model_id = 'claude-sonnet-5'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("query Sonnet 5 price");
    // $2/$10 介绍价已转为正式价（原定 2026-09-01 涨至 $3/$15 取消）
    assert_eq!(
        sonnet,
        (
            "2".to_string(),
            "10".to_string(),
            "0.20".to_string(),
            "2.50".to_string(),
        )
    );
}

#[test]
fn model_pricing_seed_repairs_sonnet_5_list_price_but_keeps_custom_price() {
    let db = Database::memory().expect("create memory db");

    {
        let conn = db.conn.lock().expect("lock conn");
        // 旧 seed 按 list 价录入的行 → 应被修正
        conn.execute(
            "UPDATE model_pricing
             SET input_cost_per_million = '3',
                 output_cost_per_million = '15',
                 cache_read_cost_per_million = '0.30',
                 cache_creation_cost_per_million = '3.75'
             WHERE model_id = 'claude-sonnet-5'",
            [],
        )
        .expect("restore old Sonnet 5 list price");
    }

    db.ensure_model_pricing_seeded()
        .expect("ensure pricing seeded");

    {
        let conn = db.conn.lock().expect("lock conn");
        let sonnet: (String, String, String, String) = conn
            .query_row(
                "SELECT input_cost_per_million, output_cost_per_million,
                        cache_read_cost_per_million, cache_creation_cost_per_million
                 FROM model_pricing WHERE model_id = 'claude-sonnet-5'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("query repaired Sonnet 5 price");
        assert_eq!(
            sonnet,
            (
                "2".to_string(),
                "10".to_string(),
                "0.20".to_string(),
                "2.50".to_string(),
            )
        );

        // 用户手改过的价（不匹配旧 seed 值）不动
        conn.execute(
            "UPDATE model_pricing
             SET input_cost_per_million = '9',
                 output_cost_per_million = '9',
                 cache_read_cost_per_million = '9',
                 cache_creation_cost_per_million = '9'
             WHERE model_id = 'claude-sonnet-5'",
            [],
        )
        .expect("set custom Sonnet 5 price");
    }

    db.ensure_model_pricing_seeded()
        .expect("ensure pricing seeded again");

    let conn = db.conn.lock().expect("lock conn");
    let custom: (String, String, String, String) = conn
        .query_row(
            "SELECT input_cost_per_million, output_cost_per_million,
                    cache_read_cost_per_million, cache_creation_cost_per_million
             FROM model_pricing WHERE model_id = 'claude-sonnet-5'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("query custom Sonnet 5 price");
    assert_eq!(
        custom,
        (
            "9".to_string(),
            "9".to_string(),
            "9".to_string(),
            "9".to_string(),
        )
    );
}

#[test]
fn model_pricing_seed_includes_september_models() {
    let db = Database::memory().expect("create memory db");
    let conn = db.conn.lock().expect("lock conn");
    for (model, expected) in [
        ("gpt-6-astra", ["10", "50", "1", "12.5"]),
        ("glm-5.3", ["1.4", "4.4", "0.26", "0"]),
        ("glm-5.3-flash", ["0.15", "0.50", "0.03", "0"]),
        ("qwen3.8-flash", ["0.15", "0.47", "0.016", "0.20"]),
        ("minimax-m2", ["0.30", "1.20", "0.03", "0.375"]),
        ("minimax-m2.1", ["0.30", "1.20", "0.03", "0.375"]),
        ("minimax-m2.5", ["0.30", "1.20", "0.03", "0.375"]),
    ] {
        let actual: [String; 4] = conn
            .query_row(
                "SELECT input_cost_per_million, output_cost_per_million,
                    cache_read_cost_per_million, cache_creation_cost_per_million
             FROM model_pricing WHERE model_id = ?1",
                [model],
                |row| Ok([row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?]),
            )
            .expect("query seeded price");
        assert_eq!(actual, expected.map(str::to_string), "{model}");
    }
}

#[test]
fn model_pricing_repairs_minimax_history_without_overwriting_custom_prices() {
    let db = Database::memory().expect("create memory db");
    for (model, old_input, expected) in [
        ("minimax-m2", "0.27", ["0.30", "1.20", "0.03", "0.375"]),
        ("minimax-m2.1", "0.27", ["0.30", "1.20", "0.03", "0.375"]),
        ("minimax-m2.5", "0.12", ["0.30", "1.20", "0.03", "0.375"]),
        ("minimax-m2.5", "0.15", ["0.30", "1.20", "0.03", "0.375"]),
        ("minimax-m2.5", "9", ["9", "0.95", "0.03", "0"]),
    ] {
        {
            let conn = db.conn.lock().expect("lock conn");
            conn.execute(
                "UPDATE model_pricing SET input_cost_per_million = ?2,
                    output_cost_per_million = '0.95', cache_read_cost_per_million = '0.03',
                    cache_creation_cost_per_million = '0' WHERE model_id = ?1",
                [model, old_input],
            )
            .expect("restore old or custom MiniMax price");
        }
        db.ensure_model_pricing_seeded().expect("repair pricing");
        let conn = db.conn.lock().expect("lock conn");
        let actual: [String; 4] = conn
            .query_row(
                "SELECT input_cost_per_million, output_cost_per_million,
                    cache_read_cost_per_million, cache_creation_cost_per_million
             FROM model_pricing WHERE model_id = ?1",
                [model],
                |row| Ok([row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?]),
            )
            .expect("query repaired price");
        assert_eq!(
            actual,
            expected.map(str::to_string),
            "{model} from {old_input}"
        );
    }
}

#[test]
fn ensure_incremental_auto_vacuum_rebuilds_existing_file_db() {
    let temp = NamedTempFile::new().expect("create temp db file");
    let path = temp.path().to_path_buf();

    let conn = Connection::open(&path).expect("open temp db");
    conn.execute("PRAGMA auto_vacuum = NONE;", [])
        .expect("set none auto_vacuum");
    Database::create_tables_on_conn(&conn).expect("create tables");

    assert_eq!(
        Database::get_auto_vacuum_mode(&conn).expect("auto_vacuum before rebuild"),
        0,
        "existing file db should start with NONE auto_vacuum"
    );

    let rebuilt =
        Database::ensure_incremental_auto_vacuum_on_conn(&conn).expect("enable incremental mode");
    assert!(rebuilt, "existing db should require rebuild via VACUUM");
    drop(conn);

    let reopened = Connection::open(&path).expect("reopen temp db");
    assert_eq!(
        Database::get_auto_vacuum_mode(&reopened).expect("auto_vacuum after rebuild"),
        2,
        "file db should persist INCREMENTAL auto_vacuum after VACUUM rebuild"
    );
}

#[test]
fn migrate_v12_to_v13_adds_input_token_semantics_columns() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE TABLE proxy_request_logs (request_id TEXT PRIMARY KEY);
         CREATE TABLE usage_daily_rollups (date TEXT PRIMARY KEY);",
    )
    .expect("seed v12 tables");
    Database::set_user_version(&conn, 12).expect("set user_version=12");

    Database::apply_schema_migrations_on_conn(&conn).expect("apply migrations");

    assert_eq!(
        Database::get_user_version(&conn).expect("version"),
        SCHEMA_VERSION
    );
    for table in ["proxy_request_logs", "usage_daily_rollups"] {
        let column = get_column_info(&conn, table, "input_token_semantics");
        assert_eq!(column.r#type, "INTEGER");
        assert_eq!(column.notnull, 1, "{table} column must be NOT NULL");
    }
}

#[test]
fn migrate_v15_to_v16_resets_only_codex_session_usage() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    Database::create_tables_on_conn(&conn).expect("create tables");
    conn.execute_batch(
        "INSERT INTO proxy_request_logs (
            request_id, provider_id, app_type, model, input_tokens, output_tokens,
            cache_read_tokens, latency_ms, status_code, created_at, data_source
         ) VALUES
            ('codex-row', '_codex_session', 'codex', 'gpt', 1, 1, 0, 0, 200, 1, 'codex_session'),
            ('claude-row', '_claude_session', 'claude', 'claude', 1, 1, 0, 0, 200, 1, 'claude_session');
         INSERT INTO usage_daily_rollups (date, app_type, provider_id, model)
         VALUES
            ('2026-08-04', 'codex', '_codex_session', 'gpt'),
            ('2026-08-04', 'claude', '_claude_session', 'claude');
         INSERT INTO session_log_sync
            (file_path, last_modified, last_line_offset, last_synced_at)
         VALUES
            ('/old/sessions/rollout-2026-08-04T00-00-00-00000000-0000-4000-8000-000000000001.jsonl', 1, 1, 1),
            ('/claude/projects/session.jsonl', 1, 1, 1);",
    )
    .expect("seed v15 usage");
    Database::set_user_version(&conn, 15).expect("set v15");

    Database::apply_schema_migrations_on_conn(&conn).expect("migrate v15 to v16");

    let counts: (i64, i64, i64, i64) = conn
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM proxy_request_logs WHERE data_source='codex_session'),
                (SELECT COUNT(*) FROM proxy_request_logs WHERE data_source='claude_session'),
                (SELECT COUNT(*) FROM usage_daily_rollups WHERE provider_id='_codex_session'),
                (SELECT COUNT(*) FROM session_log_sync)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read post-migration counts");
    assert_eq!(counts, (0, 1, 0, 1));
}

/// 官方 v16 数据库必须能被 fork 幂等打开，且不改写
/// `PRAGMA user_version`。fork 自身版本改用 settings 独立记录。
#[test]
fn upstream_v16_database_is_accepted() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    Database::create_tables_on_conn(&conn).expect("create tables");
    Database::set_user_version(&conn, 16).expect("set user_version=16");

    Database::apply_schema_migrations_on_conn(&conn).expect("v16 db must be accepted");

    assert_eq!(
        Database::get_user_version(&conn).expect("version"),
        SCHEMA_VERSION
    );
    let fork_version: String = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'fork_schema_version'",
            [],
            |row| row.get(0),
        )
        .expect("fork schema marker");
    assert_eq!(fork_version, FORK_SCHEMA_VERSION.to_string());
    assert!(
        Database::has_column(&conn, "mcp_servers", "enabled_grokbuild")
            .expect("official compatibility column")
    );
    let proxy_defaults: (i64, i64) = conn
        .query_row(
            "SELECT
                (SELECT max_retries FROM proxy_config WHERE app_type='gemini'),
                (SELECT max_retries FROM proxy_config WHERE app_type='grokbuild')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("official proxy defaults");
    assert_eq!(proxy_defaults, (5, 3));
}

/// 上游 v17 数据库（跑过上游 pi 会话统计构建后落盘的形态）必须能被 fork
/// 打开：走一次 v17 → v18 迁移（补齐会话日志字节游标列）后收敛。
#[test]
fn upstream_v17_database_is_accepted() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    Database::create_tables_on_conn(&conn).expect("create tables");
    Database::set_user_version(&conn, 17).expect("set user_version=17");

    Database::apply_schema_migrations_on_conn(&conn).expect("v17 db must be accepted");

    assert_eq!(
        Database::get_user_version(&conn).expect("version"),
        SCHEMA_VERSION
    );
}

/// 上游 v18 数据库（当前对齐版本）必须原样接受：版本号相等走不进迁移循环，
/// 只补幂等兼容结构。
#[test]
fn upstream_v18_database_is_accepted() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    Database::create_tables_on_conn(&conn).expect("create tables");
    Database::set_user_version(&conn, 18).expect("set user_version=18");

    Database::apply_schema_migrations_on_conn(&conn).expect("v18 db must be accepted");

    assert_eq!(
        Database::get_user_version(&conn).expect("version"),
        SCHEMA_VERSION
    );
}

/// v17 → v18：给存量 `session_log_sync` 表补上字节游标与尾部指纹列，
/// 存量行保持 NULL（首轮按旧行号游标转换为字节位置）。
#[test]
fn migration_v17_to_v18_adds_byte_cursor_to_existing_sync_table() {
    // 真实升级路径：v17 库带旧 DDL 的 session_log_sync（无字节游标列）
    // 与存量游标行，迁移后列补上、存量行保持 NULL。
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE TABLE session_log_sync (
            file_path TEXT PRIMARY KEY,
            last_modified INTEGER NOT NULL,
            last_line_offset INTEGER NOT NULL DEFAULT 0,
            last_synced_at INTEGER NOT NULL
         );
         INSERT INTO session_log_sync VALUES ('/tmp/a.jsonl', 5, 3, 1);",
    )
    .expect("seed legacy sync table");
    Database::set_user_version(&conn, 17).expect("set user_version=17");

    Database::apply_schema_migrations_on_conn(&conn).expect("migrate v17 -> v18");

    assert!(Database::has_column(&conn, "session_log_sync", "last_byte_offset").expect("column"));
    assert!(
        Database::has_column(&conn, "session_log_sync", "last_tail_fingerprint").expect("column")
    );
    let (byte_offset, fingerprint): (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT last_byte_offset, last_tail_fingerprint
             FROM session_log_sync WHERE file_path = '/tmp/a.jsonl'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read migrated row");
    assert_eq!(byte_offset, None, "存量行的字节游标必须为 NULL");
    assert_eq!(fingerprint, None, "存量行的尾部指纹必须为 NULL");
}

/// v16 → v17 迁移与上游逐字一致：建出 `session_usage_dedup` 去重账本表，
/// 且迁移链整体收敛到 SCHEMA_VERSION。
#[test]
fn migration_v16_to_v17_creates_session_usage_dedup_ledger() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    Database::create_tables_on_conn(&conn).expect("create tables");
    Database::set_user_version(&conn, 16).expect("set user_version=16");

    Database::apply_schema_migrations_on_conn(&conn).expect("migrate to SCHEMA_VERSION");

    assert_eq!(
        Database::get_user_version(&conn).expect("version"),
        SCHEMA_VERSION
    );
    conn.execute(
        "INSERT INTO session_usage_dedup
         (data_source, request_id, semantic_id, has_entry_id)
         VALUES ('pi_session', 'request', 'semantic', 1)",
        [],
    )
    .expect("dedup ledger table must exist with upstream columns");
}

#[test]
fn legacy_fork_v19_is_normalized_without_losing_core_rows() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    Database::create_tables_on_conn(&conn).expect("create current tables");
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config)
         VALUES ('p1', 'codex', 'Provider 1', '{}')",
        [],
    )
    .expect("seed provider");
    conn.execute(
        "INSERT INTO mcp_servers
         (id, name, server_config, enabled_claude, enabled_codex)
         VALUES ('m1', 'MCP 1', '{}', 1, 1)",
        [],
    )
    .expect("seed mcp");
    for table in ["mcp_servers", "skills"] {
        for column in [
            "enabled_gemini",
            "enabled_grokbuild",
            "enabled_opencode",
            "enabled_hermes",
        ] {
            conn.execute(&format!("ALTER TABLE {table} DROP COLUMN {column}"), [])
                .expect("remove official column from legacy fixture");
        }
    }
    conn.execute_batch(
        "DROP TABLE proxy_config;
         CREATE TABLE proxy_config (
            app_type TEXT PRIMARY KEY CHECK (app_type IN ('claude','codex')),
            max_retries INTEGER NOT NULL DEFAULT 3
         );",
    )
    .expect("restore legacy two-app proxy constraint");
    Database::set_user_version(&conn, 19).expect("set legacy fork version");

    Database::apply_schema_migrations_on_conn(&conn).expect("normalize legacy fork db");

    assert_eq!(
        Database::get_user_version(&conn).expect("official version"),
        SCHEMA_VERSION
    );
    let provider_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM providers WHERE id='p1'", [], |row| {
            row.get(0)
        })
        .expect("provider count");
    let mcp_flags: (i64, i64) = conn
        .query_row(
            "SELECT enabled_claude, enabled_codex FROM mcp_servers WHERE id='m1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("mcp flags");
    assert_eq!(provider_count, 1);
    assert_eq!(mcp_flags, (1, 1));
    assert!(
        Database::has_column(&conn, "mcp_servers", "enabled_grokbuild")
            .expect("restored official column")
    );
}

#[test]
fn partially_normalized_fork_v19_is_completed() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    Database::create_tables_on_conn(&conn).expect("create current tables");
    for table in ["mcp_servers", "skills"] {
        for column in [
            "enabled_gemini",
            "enabled_grokbuild",
            "enabled_opencode",
            "enabled_hermes",
        ] {
            conn.execute(&format!("ALTER TABLE {table} DROP COLUMN {column}"), [])
                .expect("restore legacy fork columns");
        }
    }
    Database::set_user_version(&conn, 19).expect("set partially normalized version");

    Database::apply_schema_migrations_on_conn(&conn)
        .expect("complete partially normalized fork migration");

    assert_eq!(
        Database::get_user_version(&conn).expect("official version"),
        SCHEMA_VERSION
    );
    for table in ["mcp_servers", "skills"] {
        assert!(Database::has_column(&conn, table, "enabled_grokbuild")
            .expect("restored official compatibility column"));
    }
}

#[test]
fn future_v19_without_complete_legacy_fingerprint_is_rejected() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    Database::create_tables_on_conn(&conn).expect("create current tables");
    for column in [
        "enabled_gemini",
        "enabled_grokbuild",
        "enabled_opencode",
        "enabled_hermes",
    ] {
        conn.execute(&format!("ALTER TABLE mcp_servers DROP COLUMN {column}"), [])
            .expect("simulate an incomplete or hand-edited future schema");
    }
    Database::set_user_version(&conn, 19).expect("set future version");

    let error = Database::apply_schema_migrations_on_conn(&conn)
        .expect_err("incomplete legacy fingerprint must not be downgraded");

    assert!(error.to_string().contains("数据库版本过新"));
    assert_eq!(
        Database::get_user_version(&conn).expect("version after rejection"),
        19
    );
}

#[test]
fn legacy_fork_v17_is_distinguished_from_future_upstream_v17() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    Database::create_tables_on_conn(&conn).expect("create tables");
    for table in ["mcp_servers", "skills"] {
        for column in [
            "enabled_gemini",
            "enabled_grokbuild",
            "enabled_opencode",
            "enabled_hermes",
        ] {
            conn.execute(&format!("ALTER TABLE {table} DROP COLUMN {column}"), [])
                .expect("remove official column from legacy fixture");
        }
    }
    conn.execute_batch(
        "DROP TABLE proxy_config;
         CREATE TABLE proxy_config (
            app_type TEXT PRIMARY KEY CHECK (app_type IN ('claude','codex')),
            max_retries INTEGER NOT NULL DEFAULT 3
         );",
    )
    .expect("restore legacy two-app proxy constraint");
    conn.execute(
        "ALTER TABLE mcp_servers ADD COLUMN enabled_claude_desktop BOOLEAN NOT NULL DEFAULT 0",
        [],
    )
    .expect("add legacy fork marker column");
    Database::set_user_version(&conn, 17).expect("set legacy fork version");

    Database::apply_schema_migrations_on_conn(&conn).expect("normalize legacy fork v17");

    assert_eq!(
        Database::get_user_version(&conn).expect("official version"),
        SCHEMA_VERSION
    );
}

#[test]
fn saving_mcp_preserves_official_app_flags() {
    let db = Database::memory().expect("create memory db");
    {
        let conn = db.conn.lock().expect("lock db");
        conn.execute(
            "INSERT INTO mcp_servers (
                id, name, server_config, enabled_gemini, enabled_grokbuild,
                enabled_opencode, enabled_hermes
             ) VALUES ('shared', 'old', '{}', 1, 1, 1, 1)",
            [],
        )
        .expect("seed upstream flags");
    }

    db.save_mcp_server(&crate::app_config::McpServer {
        id: "shared".into(),
        name: "updated".into(),
        server: serde_json::json!({"command": "demo"}),
        apps: crate::app_config::McpApps {
            claude: true,
            codex: true,
        },
        description: None,
        homepage: None,
        docs: None,
        tags: Vec::new(),
    })
    .expect("save fork mcp fields");

    let conn = db.conn.lock().expect("lock db");
    let flags: (i64, i64, i64, i64) = conn
        .query_row(
            "SELECT enabled_gemini, enabled_grokbuild, enabled_opencode, enabled_hermes
             FROM mcp_servers WHERE id='shared'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read upstream flags");
    assert_eq!(flags, (1, 1, 1, 1));
}

/// 全新库走完整迁移链后必须落在 SCHEMA_VERSION，且新列齐备。
#[test]
fn fresh_database_migrates_to_current_schema_version() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    Database::create_tables_on_conn(&conn).expect("create tables");

    Database::apply_schema_migrations_on_conn(&conn).expect("apply migrations");

    assert_eq!(
        Database::get_user_version(&conn).expect("version"),
        SCHEMA_VERSION
    );
    get_column_info(&conn, "proxy_request_logs", "input_token_semantics");
    get_column_info(&conn, "usage_daily_rollups", "input_token_semantics");
    assert!(Database::table_exists(&conn, "profiles").expect("profiles table"));
    for table in ["mcp_servers", "skills"] {
        for column in [
            "enabled_gemini",
            "enabled_grokbuild",
            "enabled_opencode",
            "enabled_hermes",
        ] {
            assert!(Database::has_column(&conn, table, column).expect("compatibility column"));
        }
    }
}
