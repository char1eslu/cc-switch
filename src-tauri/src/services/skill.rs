//! Skills 服务层
//!
//! v3.10.0+ 统一管理架构：
//! - SSOT（单一事实源）：`~/.cc-switch/skills/`
//! - 安装时下载到 SSOT，按需同步到各应用目录
//! - 数据库存储安装记录和启用状态

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::timeout;

use crate::app_config::{AppType, InstalledSkill, SkillApps, UnmanagedSkill};
use crate::config::get_app_config_dir;
use crate::database::Database;
use crate::error::format_skill_error;

// ========== Skills state coordination ==========

/// Coordinates the database `skills` state with the filesystem SSOT.
///
/// Lock order is: global sync operation lock -> this lock -> database mutex.
/// Async install/update paths acquire the write guard only after downloads have
/// completed, so no non-Send std guard is held across an `.await`.
fn skill_state_lock() -> &'static RwLock<()> {
    static LOCK: OnceLock<RwLock<()>> = OnceLock::new();
    LOCK.get_or_init(|| RwLock::new(()))
}

pub(crate) fn skill_state_read_guard() -> RwLockReadGuard<'static, ()> {
    skill_state_lock().read().unwrap_or_else(|poisoned| {
        log::warn!("Skills state read lock was poisoned; recovering the protected state");
        poisoned.into_inner()
    })
}

pub(crate) fn skill_state_write_guard() -> RwLockWriteGuard<'static, ()> {
    skill_state_lock().write().unwrap_or_else(|poisoned| {
        log::warn!("Skills state write lock was poisoned; recovering the protected state");
        poisoned.into_inner()
    })
}

// ========== 数据结构 ==========

/// Skill 同步方式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SyncMethod {
    /// 自动选择：优先 symlink，失败时回退到 copy
    #[default]
    Auto,
    /// 符号链接（推荐，节省磁盘空间）
    Symlink,
    /// 文件复制（兼容模式）
    Copy,
}

/// Skill 存储位置（SSOT 目录选择）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SkillStorageLocation {
    /// CC Switch 管理目录 (~/.cc-switch/skills/)
    #[default]
    CcSwitch,
    /// Agent Skills 统一标准目录 (~/.agents/skills/)
    Unified,
}

/// 可发现的技能（来自仓库）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverableSkill {
    /// 唯一标识: "owner/name:directory"
    pub key: String,
    /// 显示名称 (从 SKILL.md 解析)
    pub name: String,
    /// 技能描述
    pub description: String,
    /// 目录名称 (安装路径的最后一段)
    pub directory: String,
    /// GitHub README URL
    #[serde(rename = "readmeUrl")]
    pub readme_url: Option<String>,
    /// 仓库所有者
    #[serde(rename = "repoOwner")]
    pub repo_owner: String,
    /// 仓库名称
    #[serde(rename = "repoName")]
    pub repo_name: String,
    /// 分支名称
    #[serde(rename = "repoBranch")]
    pub repo_branch: String,
}

/// 技能对象（兼容旧 API，内部使用 DiscoverableSkill）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    /// 唯一标识: "owner/name:directory" 或 "local:directory"
    pub key: String,
    /// 显示名称 (从 SKILL.md 解析)
    pub name: String,
    /// 技能描述
    pub description: String,
    /// 目录名称 (安装路径的最后一段)
    pub directory: String,
    /// GitHub README URL
    #[serde(rename = "readmeUrl")]
    pub readme_url: Option<String>,
    /// 是否已安装
    pub installed: bool,
    /// 仓库所有者
    #[serde(rename = "repoOwner")]
    pub repo_owner: Option<String>,
    /// 仓库名称
    #[serde(rename = "repoName")]
    pub repo_name: Option<String>,
    /// 分支名称
    #[serde(rename = "repoBranch")]
    pub repo_branch: Option<String>,
}

/// 仓库配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRepo {
    /// GitHub 用户/组织名
    pub owner: String,
    /// 仓库名称
    pub name: String,
    /// 分支 (默认 "main")
    pub branch: String,
    /// 是否启用
    pub enabled: bool,
}

/// 技能安装状态（旧版兼容）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillState {
    /// 是否已安装
    pub installed: bool,
    /// 安装时间
    #[serde(rename = "installedAt")]
    pub installed_at: DateTime<Utc>,
}

/// 持久化存储结构（仓库配置）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillStore {
    /// directory -> 安装状态（旧版兼容，新版不使用）
    pub skills: HashMap<String, SkillState>,
    /// 仓库列表
    pub repos: Vec<SkillRepo>,
}

impl Default for SkillStore {
    fn default() -> Self {
        SkillStore {
            skills: HashMap::new(),
            repos: vec![
                SkillRepo {
                    owner: "anthropics".to_string(),
                    name: "skills".to_string(),
                    branch: "main".to_string(),
                    enabled: true,
                },
                SkillRepo {
                    owner: "ComposioHQ".to_string(),
                    name: "awesome-claude-skills".to_string(),
                    branch: "master".to_string(),
                    enabled: true,
                },
                SkillRepo {
                    owner: "cexll".to_string(),
                    name: "myclaude".to_string(),
                    branch: "master".to_string(),
                    enabled: true,
                },
                SkillRepo {
                    owner: "JimLiu".to_string(),
                    name: "baoyu-skills".to_string(),
                    branch: "main".to_string(),
                    enabled: true,
                },
            ],
        }
    }
}

/// Skill 卸载结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillUninstallResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<String>,
}

/// Skill 更新检测结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillUpdateInfo {
    /// Skill ID
    pub id: String,
    /// Skill 名称
    pub name: String,
    /// 当前本地哈希
    pub current_hash: Option<String>,
    /// 远程最新哈希
    pub remote_hash: String,
}

/// Skill 存储位置迁移结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationResult {
    pub migrated_count: usize,
    pub skipped_count: usize,
    pub errors: Vec<String>,
}

// ========== skills.sh API 类型 ==========

/// skills.sh API 原始响应
///
/// 注意：API 命名不一致（searchType 是 camelCase，duration_ms 是 snake_case），
/// 因此不能用 rename_all，需要逐字段指定。
#[derive(Debug, Clone, Deserialize)]
struct SkillsShApiResponse {
    pub query: String,
    #[serde(rename = "searchType")]
    #[allow(dead_code)]
    pub search_type: String,
    pub skills: Vec<SkillsShApiSkill>,
    pub count: usize,
    #[allow(dead_code)]
    pub duration_ms: u64,
}

/// skills.sh API 原始技能条目
#[derive(Debug, Clone, Deserialize)]
struct SkillsShApiSkill {
    pub id: String,
    #[serde(rename = "skillId")]
    pub skill_id: String,
    pub name: String,
    pub installs: u64,
    pub source: String,
}

/// skills.sh 搜索结果（返回给前端）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillsShSearchResult {
    pub skills: Vec<SkillsShDiscoverableSkill>,
    pub total_count: usize,
    pub query: String,
}

/// skills.sh 可安装技能（返回给前端）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillsShDiscoverableSkill {
    pub key: String,
    pub name: String,
    pub directory: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub repo_branch: String,
    pub installs: u64,
    pub readme_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillBackupEntry {
    pub backup_id: String,
    pub backup_path: String,
    pub created_at: i64,
    pub skill: InstalledSkill,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillBackupMetadata {
    skill: InstalledSkill,
    backup_created_at: i64,
    source_path: String,
}

/// 一个待裁剪的备份目录。
struct BackupCandidate {
    path: PathBuf,
    /// 分组键：`meta.json` 里的 `skill.directory`，不可读时为
    /// [`SKILL_BACKUP_ORPHAN_GROUP`]。
    group: String,
    /// 排序键：优先用 `meta.json` 记录的创建时间。目录 mtime 只是兜底——
    /// `copy_dir_recursive` 之后再写 `meta.json`，mtime 未必等于创建顺序。
    created_at: i64,
    size: u64,
}

/// 每个 Skill 保留的备份代数。
///
/// 早先是全局「最多 20 个」，与体积无关且不分 Skill：高频更新的小 Skill 会把
/// 低频更新的大 Skill 挤出额度，单个 Skill 实际留不住几代；同时 20 个大备份
/// （实测单个技能仓库可达数十 MB）合计可轻松突破数百 MB。改为按 Skill 分组各留
/// 3 代，再叠加总体积上限兜底。
const SKILL_BACKUP_RETAIN_PER_SKILL: usize = 3;

/// 备份目录总体积上限。分组保留已能防止代数无限增长，此项只兜住「少数超大
/// Skill 把磁盘吃满」的情形，按最旧优先删除。
const SKILL_BACKUP_TOTAL_SIZE_LIMIT_BYTES: u64 = 1024 * 1024 * 1024;

/// `meta.json` 不可读时的分组键。
///
/// 这类目录在界面上不可见也无法恢复（`list_backups` 会跳过），若各自独立成组
/// 就永远不会被裁剪。归入同一组，使分组保留规则同样能收走它们。
const SKILL_BACKUP_ORPHAN_GROUP: &str = "\0orphan";
const REPO_DOWNLOAD_TIMEOUT_SECONDS: u64 = 300;
const REPO_DOWNLOAD_TIMEOUT_LABEL: &str = "300";
const REPO_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(REPO_DOWNLOAD_TIMEOUT_SECONDS);
const REPO_DISCOVERY_FALLBACK_TIMEOUT: Duration = Duration::from_secs(20);
/// 更新检测回退到整仓 ZIP 时的超时。
///
/// 20 秒只够列目录用的小仓库；技能仓库可达数百 MB（实测 hugohe3/ppt-master 约 632MB），
/// 用发现路径的短超时会让整个仓库被静默跳过，永远检测不到更新。
const REPO_UPDATE_FALLBACK_TIMEOUT_LABEL: &str = "300";
const REPO_UPDATE_FALLBACK_TIMEOUT: Duration = Duration::from_secs(REPO_DOWNLOAD_TIMEOUT_SECONDS);
const REPO_METADATA_TIMEOUT: Duration = Duration::from_secs(10);
const GITHUB_API_USER_AGENT: &str = "cc-switch";

/// 技能元数据 (从 SKILL.md 解析)
#[derive(Debug, Clone, Deserialize)]
pub struct SkillMetadata {
    pub name: Option<String>,
    pub description: Option<String>,
}

/// 导入已有 Skill 时，前端显式提交的启用应用选择
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSkillSelection {
    pub directory: String,
    #[serde(default)]
    pub apps: SkillApps,
}

#[derive(Debug, Clone, Deserialize)]
struct LegacySkillMigrationRow {
    directory: String,
    app_type: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubTreeResponse {
    tree: Vec<GithubTreeEntry>,
    #[serde(default)]
    truncated: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubTreeEntry {
    path: String,
    #[serde(rename = "type")]
    entry_type: String,
    sha: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubContentResponse {
    content: Option<String>,
    encoding: Option<String>,
    #[serde(rename = "download_url")]
    download_url: Option<String>,
}

// ========== ~/.agents/ lock 文件解析 ==========

/// `~/.agents/.skill-lock.json` 文件结构
#[derive(Deserialize)]
struct AgentsLockFile {
    skills: HashMap<String, AgentsLockSkill>,
}

/// lock 文件中单个 skill 的信息
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentsLockSkill {
    source: Option<String>,
    source_type: Option<String>,
    source_url: Option<String>,
    skill_path: Option<String>,
    branch: Option<String>,
    source_branch: Option<String>,
}

#[derive(Debug, Clone)]
struct LockRepoInfo {
    owner: String,
    repo: String,
    skill_path: Option<String>,
    branch: Option<String>,
}

fn normalize_optional_branch(branch: Option<String>) -> Option<String> {
    branch.and_then(|b| {
        let trimmed = b.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn parse_branch_from_source_url(source_url: Option<&str>) -> Option<String> {
    let source_url = source_url?;
    let source_url = source_url.trim();
    if source_url.is_empty() {
        return None;
    }

    // 支持 https://github.com/owner/repo/tree/<branch>/...
    if let Some((_, after_tree)) = source_url.split_once("/tree/") {
        let branch = after_tree
            .split('/')
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        return Some(branch.to_string());
    }

    // 支持 URL fragment: ...git#branch
    if let Some((_, fragment)) = source_url.split_once('#') {
        let branch = fragment
            .split('&')
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        return Some(branch.to_string());
    }

    // 支持 query: ...?branch=xxx / ?ref=xxx
    if let Some((_, query)) = source_url.split_once('?') {
        for pair in query.split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            if matches!(key, "branch" | "ref") {
                let branch = value.trim();
                if !branch.is_empty() {
                    return Some(branch.to_string());
                }
            }
        }
    }

    None
}

/// 获取 `~/.agents/skills/` 目录（存在时返回）
fn get_agents_skills_dir() -> Option<PathBuf> {
    dirs::home_dir()
        .map(|h| h.join(".agents").join("skills"))
        .filter(|p| p.exists())
}

/// 解析 `~/.agents/.skill-lock.json`，返回 skill_name -> 仓库信息
fn parse_agents_lock() -> HashMap<String, LockRepoInfo> {
    let path = match dirs::home_dir() {
        Some(h) => h.join(".agents").join(".skill-lock.json"),
        None => {
            log::warn!("无法获取 HOME 目录，跳过解析 agents lock 文件");
            return HashMap::new();
        }
    };
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                log::debug!("未找到 agents lock 文件: {}", path.display());
            } else {
                log::warn!("读取 agents lock 文件失败 ({}): {}", path.display(), e);
            }
            return HashMap::new();
        }
    };
    let lock: AgentsLockFile = match serde_json::from_str(&content) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("解析 agents lock 文件失败 ({}): {}", path.display(), e);
            return HashMap::new();
        }
    };
    let parsed: HashMap<String, LockRepoInfo> = lock
        .skills
        .into_iter()
        .filter_map(|(name, skill)| {
            let source = skill.source?;
            if skill.source_type.as_deref() != Some("github") {
                return None;
            }
            let (owner, repo) = source.split_once('/')?;
            let branch = normalize_optional_branch(skill.branch)
                .or_else(|| normalize_optional_branch(skill.source_branch))
                .or_else(|| parse_branch_from_source_url(skill.source_url.as_deref()));
            Some((
                name,
                LockRepoInfo {
                    owner: owner.to_string(),
                    repo: repo.to_string(),
                    skill_path: skill.skill_path,
                    branch,
                },
            ))
        })
        .collect();
    log::info!(
        "agents lock 文件解析完成，共识别 {} 个 github skill",
        parsed.len()
    );
    parsed
}

// ========== SkillService ==========

pub struct SkillService;

impl Default for SkillService {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillService {
    pub fn new() -> Self {
        Self
    }

    /// 构建 Skill 文档 URL（指向仓库中的 SKILL.md 文件）
    fn build_skill_doc_url(owner: &str, repo: &str, branch: &str, doc_path: &str) -> String {
        format!("https://github.com/{owner}/{repo}/blob/{branch}/{doc_path}")
    }

    /// 从旧 readme_url 中提取仓库内文档路径，兼容 `blob`/`tree` 两种格式
    fn extract_doc_path_from_url(url: &str) -> Option<String> {
        let marker = if url.contains("/blob/") {
            "/blob/"
        } else if url.contains("/tree/") {
            "/tree/"
        } else {
            return None;
        };

        let (_, tail) = url.split_once(marker)?;
        let (_, path) = tail.split_once('/')?;
        if path.is_empty() {
            return None;
        }
        Some(path.to_string())
    }

    // ========== 路径管理 ==========

    /// 获取 SSOT 目录（根据设置返回 ~/.cc-switch/skills/ 或 ~/.agents/skills/）
    pub fn get_ssot_dir() -> Result<PathBuf> {
        let location = crate::settings::get_skill_storage_location();
        let dir = match location {
            SkillStorageLocation::CcSwitch => get_app_config_dir().join("skills"),
            SkillStorageLocation::Unified => {
                let home = dirs::home_dir().context(format_skill_error(
                    "GET_HOME_DIR_FAILED",
                    &[],
                    Some("checkPermission"),
                ))?;
                home.join(".agents").join("skills")
            }
        };
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// 获取 Skill 卸载备份目录（~/.cc-switch/skill-backups/）
    fn get_backup_dir() -> Result<PathBuf> {
        let dir = get_app_config_dir().join("skill-backups");
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// 获取应用的 skills 目录
    pub fn get_app_skills_dir(app: &AppType) -> Result<PathBuf> {
        // 目录覆盖：优先使用用户在 settings.json 中配置的 override 目录
        match app {
            AppType::Claude => {
                if let Some(custom) = crate::settings::get_claude_override_dir() {
                    return Ok(custom.join("skills"));
                }
            }
            AppType::ClaudeDesktop => {}
            AppType::Codex => {
                if let Some(custom) = crate::settings::get_codex_override_dir() {
                    return Ok(custom.join("skills"));
                }
            }
        }

        // 默认路径：回退到用户主目录下的标准位置
        let home = dirs::home_dir().context(format_skill_error(
            "GET_HOME_DIR_FAILED",
            &[],
            Some("checkPermission"),
        ))?;

        Ok(match app {
            AppType::Claude => home.join(".claude").join("skills"),
            AppType::ClaudeDesktop => home.join(".claude-desktop").join("skills"),
            AppType::Codex => home.join(".codex").join("skills"),
        })
    }

    // ========== 统一管理方法 ==========

    /// 获取所有已安装的 Skills
    pub fn get_all_installed(db: &Arc<Database>) -> Result<Vec<InstalledSkill>> {
        let skills = db.get_all_installed_skills()?;
        Ok(skills.into_values().collect())
    }

    /// Reuse an existing installation or reject a directory owned by another repo.
    /// The caller must hold [`skill_state_write_guard`] because this can update the
    /// database and materialized app directory.
    fn reuse_existing_install(
        db: &Arc<Database>,
        skill: &DiscoverableSkill,
        install_name: &str,
        current_app: &AppType,
    ) -> Result<Option<InstalledSkill>> {
        let existing_skills = db.get_all_installed_skills()?;
        for existing in existing_skills.values() {
            if !existing.directory.eq_ignore_ascii_case(install_name) {
                continue;
            }

            let same_repo = existing.repo_owner.as_deref() == Some(&skill.repo_owner)
                && existing.repo_name.as_deref() == Some(&skill.repo_name);
            if same_repo {
                let mut updated = existing.clone();
                updated.apps.set_enabled_for(current_app, true);
                db.save_skill(&updated)?;
                Self::sync_to_app_dir(&updated.directory, current_app)?;
                log::info!(
                    "Skill {} 已存在，更新 {:?} 启用状态",
                    updated.name,
                    current_app
                );
                return Ok(Some(updated));
            }

            return Err(anyhow!(format_skill_error(
                "SKILL_DIRECTORY_CONFLICT",
                &[
                    ("directory", install_name),
                    (
                        "existing_repo",
                        &format!(
                            "{}/{}",
                            existing.repo_owner.as_deref().unwrap_or("unknown"),
                            existing.repo_name.as_deref().unwrap_or("unknown")
                        )
                    ),
                    (
                        "new_repo",
                        &format!("{}/{}", skill.repo_owner, skill.repo_name)
                    ),
                ],
                Some("uninstallFirst"),
            )));
        }

        Ok(None)
    }

    /// 安装 Skill
    ///
    /// 流程：
    /// 1. 下载到 SSOT 目录
    /// 2. 保存到数据库
    /// 3. 同步到启用的应用目录
    pub async fn install(
        &self,
        db: &Arc<Database>,
        skill: &DiscoverableSkill,
        current_app: &AppType,
    ) -> Result<InstalledSkill> {
        let ssot_dir = Self::get_ssot_dir()?;

        // 允许多级目录（如 a/b/c），但必须是安全的相对路径。
        let source_rel = Self::sanitize_skill_source_path(&skill.directory).ok_or_else(|| {
            anyhow!(format_skill_error(
                "INVALID_SKILL_DIRECTORY",
                &[("directory", &skill.directory)],
                Some("checkZipContent"),
            ))
        })?;
        // 安装目录名始终使用最后一段，避免在 SSOT 中创建多级目录。
        let install_name = source_rel
            .file_name()
            .and_then(|name| Self::sanitize_install_name(&name.to_string_lossy()))
            .ok_or_else(|| {
                anyhow!(format_skill_error(
                    "INVALID_SKILL_DIRECTORY",
                    &[("directory", &skill.directory)],
                    Some("checkZipContent"),
                ))
            })?;

        // Fast path for an existing installation. The write guard makes the DB
        // row and app projection indivisible from a concurrent cloud snapshot.
        {
            let _state_guard = skill_state_write_guard();
            if let Some(existing) =
                Self::reuse_existing_install(db, skill, &install_name, current_app)?
            {
                return Ok(existing);
            }
        }

        let dest = ssot_dir.join(&install_name);

        let mut repo_branch = skill.repo_branch.clone();
        // 真实解析出的源目录推导的文档路径（仅本次真正下载解析时可得）
        let mut resolved_doc_path: Option<String> = None;
        let mut downloaded_source: Option<(PathBuf, PathBuf)> = None;

        // 如果已存在则跳过下载
        if !dest.exists() {
            let repo = SkillRepo {
                owner: skill.repo_owner.clone(),
                name: skill.repo_name.clone(),
                branch: skill.repo_branch.clone(),
                enabled: true,
            };

            // 下载仓库
            let (temp_dir, used_branch) = timeout(REPO_DOWNLOAD_TIMEOUT, self.download_repo(&repo))
                .await
                .map_err(|_| {
                    anyhow!(format_skill_error(
                        "DOWNLOAD_TIMEOUT",
                        &[
                            ("owner", repo.owner.as_str()),
                            ("name", repo.name.as_str()),
                            ("timeout", REPO_DOWNLOAD_TIMEOUT_LABEL),
                        ],
                        Some("checkNetwork"),
                    ))
                })??;
            repo_branch = used_branch;

            // 复制到 SSOT
            let source =
                Self::resolve_skill_source_dir(&temp_dir, &skill.directory).ok_or_else(|| {
                    let missing = temp_dir.join(&source_rel).display().to_string();
                    let _ = fs::remove_dir_all(&temp_dir);
                    anyhow!(format_skill_error(
                        "SKILL_DIR_NOT_FOUND",
                        &[("path", &missing)],
                        Some("checkRepoUrl"),
                    ))
                })?;

            let canonical_temp = temp_dir.canonicalize().unwrap_or_else(|_| temp_dir.clone());
            let canonical_source = source.canonicalize().map_err(|_| {
                anyhow!(format_skill_error(
                    "SKILL_DIR_NOT_FOUND",
                    &[("path", &source.display().to_string())],
                    Some("checkRepoUrl"),
                ))
            })?;
            if !canonical_source.starts_with(&canonical_temp) || !canonical_source.is_dir() {
                let _ = fs::remove_dir_all(&temp_dir);
                return Err(anyhow!(format_skill_error(
                    "INVALID_SKILL_DIRECTORY",
                    &[("directory", &skill.directory)],
                    Some("checkZipContent"),
                )));
            }

            // 用真实解析出的源目录推导文档路径——skills.sh 的 directory 只是
            // skillId（末级目录名），嵌套目录场景直接拼接会丢路径、链接 404（#6111）
            resolved_doc_path = Self::doc_path_for_source(&canonical_temp, &canonical_source);

            downloaded_source = Some((temp_dir, canonical_source));

            // 使用实际下载成功的分支，避免 readme_url / repo_branch 与真实分支不一致。
            if repo_branch != skill.repo_branch {
                log::info!(
                    "Skill {}/{} 分支自动回退: {} -> {}",
                    skill.repo_owner,
                    skill.repo_name,
                    skill.repo_branch,
                    repo_branch
                );
            }
        }

        let doc_path = Self::choose_doc_path(
            resolved_doc_path,
            skill.readme_url.as_deref(),
            &skill.directory,
        );

        let readme_url = Some(Self::build_skill_doc_url(
            &skill.repo_owner,
            &skill.repo_name,
            &repo_branch,
            &doc_path,
        ));

        // Re-check after the network download: another install/uninstall may have
        // completed while the lock was intentionally released around `.await`.
        let _state_guard = skill_state_write_guard();
        if let Some(existing) = Self::reuse_existing_install(db, skill, &install_name, current_app)?
        {
            if let Some((temp_dir, _)) = downloaded_source.as_ref() {
                let _ = fs::remove_dir_all(temp_dir);
            }
            return Ok(existing);
        }

        if !dest.exists() {
            let (temp_dir, source) = downloaded_source
                .as_ref()
                .ok_or_else(|| anyhow!("Skill directory changed during install; please retry"))?;
            let copy_result = Self::copy_dir_recursive(source, &dest);
            let _ = fs::remove_dir_all(temp_dir);
            copy_result?;
        } else if let Some((temp_dir, _)) = downloaded_source.as_ref() {
            let _ = fs::remove_dir_all(temp_dir);
        }

        // 创建 InstalledSkill 记录
        // 计算内容哈希
        let content_hash = Self::compute_dir_hash(&dest).map(Some).unwrap_or_else(|e| {
            log::warn!("Failed to compute content hash for {}: {e}", install_name);
            None
        });

        let installed_skill = InstalledSkill {
            id: skill.key.clone(),
            name: skill.name.clone(),
            description: if skill.description.is_empty() {
                None
            } else {
                Some(skill.description.clone())
            },
            directory: install_name.clone(),
            repo_owner: Some(skill.repo_owner.clone()),
            repo_name: Some(skill.repo_name.clone()),
            repo_branch: Some(repo_branch),
            readme_url,
            apps: SkillApps::only(current_app),
            installed_at: chrono::Utc::now().timestamp(),
            content_hash,
            updated_at: 0,
        };

        // 保存到数据库
        db.save_skill(&installed_skill)?;

        // 同步到当前应用目录
        Self::sync_to_app_dir(&install_name, current_app)?;

        log::info!(
            "Skill {} 安装成功，已启用 {:?}",
            installed_skill.name,
            current_app
        );

        Ok(installed_skill)
    }

    /// 卸载 Skill
    ///
    /// 流程：
    /// 1. 从所有应用目录删除
    /// 2. 从 SSOT 删除
    /// 3. 从数据库删除
    pub fn uninstall(db: &Arc<Database>, id: &str) -> Result<SkillUninstallResult> {
        let _state_guard = skill_state_write_guard();

        // 获取 skill 信息
        let skill = db
            .get_installed_skill(id)?
            .ok_or_else(|| anyhow!("Skill not found: {id}"))?;

        let backup_path =
            Self::create_uninstall_backup(&skill)?.map(|path| path.to_string_lossy().to_string());

        // 从所有应用目录删除
        for app in AppType::all() {
            let _ = Self::remove_from_app(&skill.directory, &app);
        }

        // 从 SSOT 删除
        let ssot_dir = Self::get_ssot_dir()?;
        let skill_path = ssot_dir.join(&skill.directory);
        if skill_path.exists() {
            fs::remove_dir_all(&skill_path)?;
        }

        // 从数据库删除
        db.delete_skill(id)?;

        log::info!(
            "Skill {} 卸载成功{}",
            skill.name,
            backup_path
                .as_deref()
                .map(|path| format!(", backup: {path}"))
                .unwrap_or_default()
        );

        Ok(SkillUninstallResult { backup_path })
    }

    // ========== 更新检测 ==========

    /// 计算已安装 Skill 目录的内容哈希。
    ///
    /// 必须与 [`Self::compute_dir_git_tree_hash`] 完全一致：安装/更新/恢复
    /// 走这里写入 `content_hash`，而更新检测拿本地目录重算后与远程比对。
    /// 两侧一旦用不同算法，本地值永远对不上远程值，界面会每次都提示有更新，
    /// 更新完写回的仍是对不上的值——形成"反复更新"死循环。
    ///
    /// 远程侧的哈希由 GitHub tree API 的 `entry.sha`（git blob SHA-1）组成，
    /// 所以本地也必须按 git blob SHA 计算，而不是直接摘要文件原始内容。
    pub fn compute_dir_hash(dir: &Path) -> Result<String> {
        Self::compute_dir_git_tree_hash(dir)
    }

    /// 递归收集目录下所有非隐藏文件
    #[allow(clippy::only_used_in_recursion)]
    fn collect_files_for_hash(base: &Path, current: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        let entries = fs::read_dir(current)
            .with_context(|| format!("读取目录失败: {}", current.display()))?;
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                Self::collect_files_for_hash(base, &path, files)?;
            } else {
                files.push(path);
            }
        }
        Ok(())
    }

    /// 检查所有已安装 Skill 的更新
    ///
    /// 仅检查有 repo_owner 的 Skill（本地 Skill 跳过），
    /// 按仓库分组下载，避免重复下载同一仓库。
    pub async fn check_updates(&self, db: &Arc<Database>) -> Result<Vec<SkillUpdateInfo>> {
        let skills = db.get_all_installed_skills()?;
        let mut updates = Vec::new();

        // 按 (owner, name, branch) 分组
        let mut repo_groups: HashMap<(String, String, String), Vec<InstalledSkill>> =
            HashMap::new();

        for skill in skills.into_values() {
            let (owner, name, branch) =
                match (&skill.repo_owner, &skill.repo_name, &skill.repo_branch) {
                    (Some(o), Some(n), Some(b)) => (o.clone(), n.clone(), b.clone()),
                    (Some(o), Some(n), None) => (o.clone(), n.clone(), "main".to_string()),
                    _ => continue,
                };
            repo_groups
                .entry((owner, name, branch))
                .or_default()
                .push(skill);
        }

        let ssot_dir = Self::get_ssot_dir()?;

        for ((owner, name, branch), group_skills) in &repo_groups {
            let repo = SkillRepo {
                owner: owner.clone(),
                name: name.clone(),
                branch: branch.clone(),
                enabled: true,
            };

            // Prefer GitHub metadata APIs so update checks do not download whole repositories.
            let remote_hashes = match self.fetch_repo_skill_hashes(&repo).await {
                Ok(result) => result,
                Err(err) => {
                    log::warn!(
                        "通过 GitHub API 检查 {}/{} 更新失败，将回退 ZIP: {err}",
                        owner,
                        name
                    );
                    match self.fetch_repo_skill_hashes_from_zip(&repo).await {
                        Ok(result) => result,
                        Err(err) => {
                            log::warn!("检查更新时下载 {}/{} 失败: {err}", owner, name);
                            continue;
                        }
                    }
                }
            };

            // Remote I/O is complete. Stabilize the local DB + SSOT while hashes
            // are read and any missing hash metadata is backfilled.
            let _state_guard = skill_state_read_guard();

            for skill in group_skills {
                let Some(remote_hash) =
                    Self::find_remote_hash_for_skill(&remote_hashes, &skill.directory, name)
                else {
                    log::warn!(
                        "跳过 skill {} 的更新检查：在 {}/{} 中找不到匹配的远程目录（候选: {:?}）",
                        skill.directory,
                        owner,
                        name,
                        remote_hashes.keys().collect::<Vec<_>>()
                    );
                    continue;
                };

                let local_hash = Self::compute_local_git_tree_hash(&ssot_dir, skill);

                if local_hash.as_deref() != Some(remote_hash) {
                    updates.push(SkillUpdateInfo {
                        id: skill.id.clone(),
                        name: skill.name.clone(),
                        current_hash: local_hash,
                        remote_hash: remote_hash.clone(),
                    });
                }
            }
        }

        Ok(updates)
    }

    /// 更新单个 Skill（重新下载并替换本地文件）
    pub async fn update_skill(&self, db: &Arc<Database>, skill_id: &str) -> Result<InstalledSkill> {
        let skill = db
            .get_installed_skill(skill_id)?
            .ok_or_else(|| anyhow!("Skill not found: {skill_id}"))?;

        let (owner, name, branch) = match (&skill.repo_owner, &skill.repo_name) {
            (Some(o), Some(n)) => (
                o.clone(),
                n.clone(),
                skill
                    .repo_branch
                    .clone()
                    .unwrap_or_else(|| "main".to_string()),
            ),
            _ => return Err(anyhow!("Cannot update local skill: {skill_id}")),
        };

        let repo = SkillRepo {
            owner: owner.clone(),
            name: name.clone(),
            branch: branch.clone(),
            enabled: true,
        };

        let ssot_dir = Self::get_ssot_dir()?;

        // 下载仓库
        let (temp_dir, used_branch) = timeout(REPO_DOWNLOAD_TIMEOUT, self.download_repo(&repo))
            .await
            .map_err(|_| {
                anyhow!(format_skill_error(
                    "DOWNLOAD_TIMEOUT",
                    &[
                        ("owner", &owner),
                        ("name", &name),
                        ("timeout", REPO_DOWNLOAD_TIMEOUT_LABEL),
                    ],
                    Some("checkNetwork"),
                ))
            })??;

        // 在解压的仓库中查找 Skill 源目录
        let mut remote_skills: Vec<DiscoverableSkill> = Vec::new();
        let _ = self.scan_dir_recursive(&temp_dir, &temp_dir, &repo, &mut remote_skills);

        // 必须与 check_updates 选中同一个副本：同名 skill 在一个仓库里可能有多份
        // 内容不同的副本（例如 .openclaw/skills/x 与 skills/x）。若两处各选一份，
        // 更新写入 B 而检查仍比对 A，会导致“更新完仍报有更新”的死循环。
        let selected_remote_dir = Self::select_remote_dir_for_skill(
            remote_skills.iter().map(|rs| rs.directory.as_str()),
            &skill.directory,
            &name,
        )
        .map(|dir| dir.to_string());

        let remote_match = selected_remote_dir
            .and_then(|selected| remote_skills.iter().find(|rs| rs.directory == selected))
            .ok_or_else(|| {
                let _ = fs::remove_dir_all(&temp_dir);
                anyhow!(format_skill_error(
                    "SKILL_DIR_NOT_FOUND",
                    &[("path", &skill.directory)],
                    Some("checkRepoUrl"),
                ))
            })?;

        let source = Self::resolve_skill_source_dir(&temp_dir, &remote_match.directory)
            .ok_or_else(|| {
                let missing = temp_dir.join(&remote_match.directory).display().to_string();
                let _ = fs::remove_dir_all(&temp_dir);
                anyhow!(format_skill_error(
                    "SKILL_DIR_NOT_FOUND",
                    &[("path", &missing)],
                    Some("checkRepoUrl"),
                ))
            })?;

        // Downloads do not mutate local state, so acquire only now and hold the
        // guard through the SSOT replacement, DB metadata update, and app sync.
        let _state_guard = skill_state_write_guard();

        // 下载和扫描期间用户可能已经卸载了该 Skill。必须在任何备份、删除或
        // 复制之前重新确认记录仍存在；否则即使最终的 metadata UPDATE 能发现
        // 缺行，这里也会先把已卸载的 SSOT 目录重新创建出来。
        let current_skill = db
            .get_installed_skill(&skill.id)?
            .ok_or_else(|| anyhow!("Skill no longer installed: {}", skill.id))?;
        if current_skill.directory != skill.directory
            || current_skill.repo_owner != skill.repo_owner
            || current_skill.repo_name != skill.repo_name
            || current_skill.repo_branch != skill.repo_branch
            || current_skill.installed_at != skill.installed_at
        {
            return Err(anyhow!("Skill changed during update: {}", skill.id));
        }
        Self::require_valid_directory(&current_skill.directory)?;
        let skill = current_skill;

        // 备份旧文件
        let _ = Self::create_uninstall_backup(&skill);

        // 删除旧 SSOT 目录并复制新文件
        let dest = ssot_dir.join(&skill.directory);
        if dest.exists() {
            fs::remove_dir_all(&dest)?;
        }
        Self::copy_dir_recursive(&source, &dest)?;
        let _ = fs::remove_dir_all(&temp_dir);

        // 计算新哈希 + 解析新元数据
        let new_hash = Self::compute_dir_hash(&dest).ok();
        let skill_md = dest.join("SKILL.md");
        let (new_name, new_description) = Self::read_skill_name_desc(&skill_md, &skill.directory);

        // 更新 readme_url
        let doc_path = skill
            .readme_url
            .as_deref()
            .and_then(Self::extract_doc_path_from_url)
            .unwrap_or_else(|| format!("{}/SKILL.md", skill.directory.trim_end_matches('/')));
        let readme_url = Some(Self::build_skill_doc_url(
            &owner,
            &name,
            &used_branch,
            &doc_path,
        ));

        let updated_skill = InstalledSkill {
            id: skill.id.clone(),
            name: new_name,
            description: new_description,
            directory: skill.directory.clone(),
            repo_owner: skill.repo_owner.clone(),
            repo_name: skill.repo_name.clone(),
            repo_branch: Some(used_branch),
            readme_url,
            apps: skill.apps.clone(),
            installed_at: skill.installed_at,
            content_hash: new_hash,
            updated_at: chrono::Utc::now().timestamp(),
        };

        db.save_skill(&updated_skill)?;

        // 同步到所有已启用的应用目录
        for app in updated_skill.apps.enabled_apps() {
            if let Err(e) = Self::sync_to_app_dir(&updated_skill.directory, &app) {
                log::warn!("同步更新后的 skill 到 {:?} 失败: {e}", app);
            }
        }

        log::info!("Skill {} 更新成功", updated_skill.name);
        Ok(updated_skill)
    }

    /// 为缺少 content_hash 的已安装 Skill 补算哈希
    pub fn backfill_content_hashes(db: &Arc<Database>) -> Result<usize> {
        let _state_guard = skill_state_write_guard();
        let skills = db.get_all_installed_skills()?;
        let ssot_dir = Self::get_ssot_dir()?;
        let mut count = 0;

        for skill in skills.values() {
            if skill.content_hash.is_some() {
                continue;
            }
            let skill_dir = ssot_dir.join(&skill.directory);
            if !skill_dir.exists() {
                continue;
            }
            match Self::compute_dir_hash(&skill_dir) {
                Ok(hash) => {
                    let _ = db.update_skill_hash(&skill.id, &hash, 0);
                    count += 1;
                }
                Err(e) => {
                    log::warn!("补算哈希失败 {}: {e}", skill.id);
                }
            }
        }

        if count > 0 {
            log::info!("已为 {count} 个 Skill 补算内容哈希");
        }
        Ok(count)
    }

    /// 一次性重算全部已安装 Skill 的 content_hash。
    ///
    /// 历史版本的安装路径用「相对路径 + 文件原始内容」摘要写入 content_hash，
    /// 而更新检测比对的是「相对路径 + git blob SHA」摘要。两种算法对同一目录
    /// 永不相等，于是每次检查都报有更新、更新完下次继续报，形成死循环。
    /// 算法已统一到 git blob 口径（与 GitHub tree API 的 `sha` 可比），但存量
    /// 行仍是旧口径，必须整表重算一次才能收敛。
    ///
    /// 靠 settings 标记保证只跑一次：重算要读全部文件（实测单个 skill 可达
    /// 上万文件），不该每次启动都做。
    pub fn rehash_installed_skills_once(db: &Arc<Database>) -> Result<usize> {
        const REHASH_FLAG: &str = "skills_content_hash_rehashed_git_blob";

        if db.get_bool_flag(REHASH_FLAG)? {
            return Ok(0);
        }

        let skills = db.get_all_installed_skills()?;
        let ssot_dir = Self::get_ssot_dir()?;
        let mut count = 0;

        for skill in skills.values() {
            let skill_dir = ssot_dir.join(&skill.directory);
            if !skill_dir.exists() {
                continue;
            }
            match Self::compute_dir_hash(&skill_dir) {
                Ok(hash) => {
                    if skill.content_hash.as_deref() != Some(hash.as_str()) {
                        let updated_at = skill.updated_at;
                        let _ = db.update_skill_hash(&skill.id, &hash, updated_at);
                        count += 1;
                    }
                }
                Err(e) => {
                    log::warn!("重算 skill 哈希失败 {}: {e}", skill.id);
                }
            }
        }

        db.set_setting(REHASH_FLAG, "true")?;
        if count > 0 {
            log::info!("已按 git blob 口径重算 {count} 个 Skill 的内容哈希");
        }
        Ok(count)
    }

    /// 迁移 Skill 存储位置（在两个 SSOT 目录间移动文件）
    ///
    /// 安全策略：先移文件，后改设置。中途崩溃时设置仍指向旧目录。
    pub fn migrate_storage(
        db: &Arc<Database>,
        target: SkillStorageLocation,
    ) -> Result<MigrationResult> {
        let _state_guard = skill_state_write_guard();
        let current = crate::settings::get_skill_storage_location();
        if current == target {
            return Ok(MigrationResult {
                migrated_count: 0,
                skipped_count: 0,
                errors: vec![],
            });
        }

        // 1. 解析旧目录和新目录（不改设置）
        let old_dir = Self::get_ssot_dir()?;
        let new_dir = match target {
            SkillStorageLocation::CcSwitch => get_app_config_dir().join("skills"),
            SkillStorageLocation::Unified => {
                let home = dirs::home_dir().context("Cannot determine home directory")?;
                home.join(".agents").join("skills")
            }
        };
        fs::create_dir_all(&new_dir)?;

        // 2. 逐个移动 skill 目录
        let skills = db.get_all_installed_skills()?;
        let mut result = MigrationResult {
            migrated_count: 0,
            skipped_count: 0,
            errors: vec![],
        };

        for skill in skills.values() {
            let src = old_dir.join(&skill.directory);
            let dst = new_dir.join(&skill.directory);

            if !src.exists() {
                result.skipped_count += 1;
                continue;
            }
            if dst.exists() {
                result.skipped_count += 1;
                continue;
            }

            // 优先 rename（同文件系统原子操作），失败则 copy+delete
            match fs::rename(&src, &dst) {
                Ok(()) => result.migrated_count += 1,
                Err(_) => match Self::copy_dir_recursive(&src, &dst) {
                    Ok(()) => {
                        let _ = fs::remove_dir_all(&src);
                        result.migrated_count += 1;
                    }
                    Err(e) => {
                        result.errors.push(format!("{}: {e}", skill.directory));
                    }
                },
            }
        }

        // 3. 文件移动完成后才持久化设置
        crate::settings::set_skill_storage_location(target)?;

        // 4. 刷新所有应用目录的 symlink（指向新 SSOT）
        for app in AppType::all() {
            let _ = Self::sync_to_app_unlocked(db, &app);
        }

        log::info!(
            "Skill 存储迁移完成: {} 迁移, {} 跳过, {} 错误",
            result.migrated_count,
            result.skipped_count,
            result.errors.len()
        );

        Ok(result)
    }

    pub fn list_backups() -> Result<Vec<SkillBackupEntry>> {
        let backup_dir = Self::get_backup_dir()?;
        let mut entries = Vec::new();

        for entry in fs::read_dir(&backup_dir)? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    log::warn!("读取 Skill 备份目录项失败: {err}");
                    continue;
                }
            };
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            match Self::read_backup_metadata(&path) {
                Ok(metadata) => entries.push(SkillBackupEntry {
                    backup_id: entry.file_name().to_string_lossy().to_string(),
                    backup_path: path.to_string_lossy().to_string(),
                    created_at: metadata.backup_created_at,
                    skill: metadata.skill,
                }),
                Err(err) => {
                    log::warn!("解析 Skill 备份失败 {}: {err:#}", path.display());
                }
            }
        }

        entries.sort_by_key(|entry| std::cmp::Reverse(entry.created_at));
        Ok(entries)
    }

    pub fn delete_backup(backup_id: &str) -> Result<()> {
        let backup_path = Self::backup_path_for_id(backup_id)?;
        let metadata = fs::symlink_metadata(&backup_path)
            .with_context(|| format!("failed to access {}", backup_path.display()))?;

        if !metadata.is_dir() {
            return Err(anyhow!(
                "Skill backup is not a directory: {}",
                backup_path.display()
            ));
        }

        fs::remove_dir_all(&backup_path)
            .with_context(|| format!("failed to delete {}", backup_path.display()))?;

        log::info!("Skill 备份已删除: {}", backup_path.display());
        Ok(())
    }

    pub fn restore_from_backup(
        db: &Arc<Database>,
        backup_id: &str,
        current_app: &AppType,
    ) -> Result<InstalledSkill> {
        let _state_guard = skill_state_write_guard();
        let backup_path = Self::backup_path_for_id(backup_id)?;
        let metadata = Self::read_backup_metadata(&backup_path)?;
        let backup_skill_dir = backup_path.join("skill");
        if !backup_skill_dir.join("SKILL.md").exists() {
            return Err(anyhow!(
                "Skill backup is invalid or missing SKILL.md: {}",
                backup_path.display()
            ));
        }

        let existing_skills = db.get_all_installed_skills()?;
        if existing_skills.contains_key(&metadata.skill.id)
            || existing_skills.values().any(|skill| {
                skill
                    .directory
                    .eq_ignore_ascii_case(&metadata.skill.directory)
            })
        {
            return Err(anyhow!(
                "Skill already exists, please uninstall the current one first: {}",
                metadata.skill.directory
            ));
        }

        let ssot_dir = Self::get_ssot_dir()?;
        let restore_path = ssot_dir.join(&metadata.skill.directory);
        if restore_path.exists() || Self::is_symlink(&restore_path) {
            return Err(anyhow!(
                "Restore target already exists: {}",
                restore_path.display()
            ));
        }

        let mut restored_skill = metadata.skill;
        restored_skill.installed_at = Utc::now().timestamp();
        restored_skill.apps = SkillApps::only(current_app);
        restored_skill.updated_at = 0;

        Self::copy_dir_recursive(&backup_skill_dir, &restore_path)?;

        // 重新计算内容哈希
        restored_skill.content_hash = Self::compute_dir_hash(&restore_path).ok();

        if let Err(err) = db.save_skill(&restored_skill) {
            let _ = fs::remove_dir_all(&restore_path);
            return Err(err.into());
        }

        if !restored_skill.apps.is_empty() {
            if let Err(err) = Self::sync_to_app_dir(&restored_skill.directory, current_app) {
                let _ = db.delete_skill(&restored_skill.id);
                let _ = fs::remove_dir_all(&restore_path);
                return Err(err);
            }
        }

        log::info!(
            "Skill {} 已从备份恢复到 {}",
            restored_skill.name,
            restore_path.display()
        );

        Ok(restored_skill)
    }

    /// 切换应用启用状态
    ///
    /// 启用：复制到应用目录
    /// 禁用：从应用目录删除
    pub fn toggle_app(db: &Arc<Database>, id: &str, app: &AppType, enabled: bool) -> Result<()> {
        let _state_guard = skill_state_write_guard();
        // 获取当前 skill
        let mut skill = db
            .get_installed_skill(id)?
            .ok_or_else(|| anyhow!("Skill not found: {id}"))?;

        // 更新状态
        skill.apps.set_enabled_for(app, enabled);

        // 同步文件
        if enabled {
            Self::sync_to_app_dir(&skill.directory, app)?;
        } else {
            Self::remove_from_app(&skill.directory, app)?;
        }

        // 更新数据库
        db.update_skill_apps(id, &skill.apps)?;

        log::info!("Skill {} 的 {:?} 状态已更新为 {}", skill.name, app, enabled);

        Ok(())
    }

    /// 扫描未管理的 Skills
    ///
    /// 扫描各应用目录，找出未被 CC Switch 管理的 Skills
    pub fn scan_unmanaged(db: &Arc<Database>) -> Result<Vec<UnmanagedSkill>> {
        let _state_guard = skill_state_read_guard();
        let managed_skills = db.get_all_installed_skills()?;
        let managed_dirs: HashSet<String> = managed_skills
            .values()
            .map(|s| s.directory.clone())
            .collect();

        // 收集所有待扫描的目录及其来源标签
        let mut scan_sources: Vec<(PathBuf, String)> = Vec::new();
        for app in AppType::all() {
            if let Ok(d) = Self::get_app_skills_dir(&app) {
                scan_sources.push((d, app.as_str().to_string()));
            }
        }
        if let Some(agents_dir) = get_agents_skills_dir() {
            scan_sources.push((agents_dir, "agents".to_string()));
        }
        if let Ok(ssot_dir) = Self::get_ssot_dir() {
            scan_sources.push((ssot_dir, "cc-switch".to_string()));
        }

        let mut unmanaged: HashMap<String, UnmanagedSkill> = HashMap::new();

        for (scan_dir, label) in &scan_sources {
            let entries = match fs::read_dir(scan_dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let dir_name = entry.file_name().to_string_lossy().to_string();
                if dir_name.starts_with('.') || managed_dirs.contains(&dir_name) {
                    continue;
                }

                let skill_md = path.join("SKILL.md");
                if !skill_md.exists() {
                    continue;
                }
                let (name, description) = Self::read_skill_name_desc(&skill_md, &dir_name);

                unmanaged
                    .entry(dir_name.clone())
                    .and_modify(|s| s.found_in.push(label.clone()))
                    .or_insert(UnmanagedSkill {
                        directory: dir_name,
                        name,
                        description,
                        found_in: vec![label.clone()],
                        path: path.display().to_string(),
                    });
            }
        }

        Ok(unmanaged.into_values().collect())
    }

    /// 从应用目录导入 Skills
    ///
    /// 将未管理的 Skills 导入到 CC Switch 统一管理
    pub fn import_from_apps(
        db: &Arc<Database>,
        imports: Vec<ImportSkillSelection>,
    ) -> Result<Vec<InstalledSkill>> {
        let _state_guard = skill_state_write_guard();
        let ssot_dir = Self::get_ssot_dir()?;
        let agents_lock = parse_agents_lock();
        let mut imported = Vec::new();

        // 将 lock 文件中发现的仓库保存到 skill_repos
        save_repos_from_lock(
            db,
            &agents_lock,
            imports.iter().map(|selection| selection.directory.as_str()),
        );

        // 收集所有候选搜索目录
        let mut search_sources: Vec<(PathBuf, String)> = Vec::new();
        for app in AppType::all() {
            if let Ok(d) = Self::get_app_skills_dir(&app) {
                search_sources.push((d, app.as_str().to_string()));
            }
        }
        if let Some(agents_dir) = get_agents_skills_dir() {
            search_sources.push((agents_dir, "agents".to_string()));
        }
        search_sources.push((ssot_dir.clone(), "cc-switch".to_string()));

        for selection in imports {
            let dir_name = selection.directory;
            // 在所有候选目录中查找
            let mut source_path: Option<PathBuf> = None;

            for (base, label) in &search_sources {
                let skill_path = base.join(&dir_name);
                if skill_path.exists() {
                    if source_path.is_none() {
                        source_path = Some(skill_path);
                    }
                    log::debug!("Skill '{dir_name}' found in source '{label}'");
                }
            }

            let source = match source_path {
                Some(p) => p,
                None => continue,
            };
            if !source.join("SKILL.md").exists() {
                log::warn!(
                    "Skip importing '{}' because source '{}' has no SKILL.md",
                    dir_name,
                    source.display()
                );
                continue;
            }

            // 复制到 SSOT
            let dest = ssot_dir.join(&dir_name);
            if !dest.exists() {
                Self::copy_dir_recursive(&source, &dest)?;
            }

            // 解析元数据
            let skill_md = dest.join("SKILL.md");
            let (name, description) = Self::read_skill_name_desc(&skill_md, &dir_name);

            // 启用状态仅信任用户本次显式选择，不再根据“在哪些位置找到”自动推断。
            let apps = selection.apps;

            // 从 lock 文件提取仓库信息
            let (id, repo_owner, repo_name, repo_branch, readme_url) =
                build_repo_info_from_lock(&agents_lock, &dir_name);

            // 计算内容哈希
            let ssot_skill_dir = ssot_dir.join(&dir_name);
            let content_hash = Self::compute_dir_hash(&ssot_skill_dir).ok();

            // 创建记录
            let skill = InstalledSkill {
                id,
                name,
                description,
                directory: dir_name,
                repo_owner,
                repo_name,
                repo_branch,
                readme_url,
                apps,
                installed_at: chrono::Utc::now().timestamp(),
                content_hash,
                updated_at: 0,
            };

            // 保存到数据库
            db.save_skill(&skill)?;

            imported.push(skill);
        }

        log::info!("成功导入 {} 个 Skills", imported.len());

        Ok(imported)
    }

    // ========== 文件同步方法 ==========

    /// 创建符号链接（跨平台）
    ///
    /// - Unix: 使用 std::os::unix::fs::symlink
    /// - Windows: 使用 std::os::windows::fs::symlink_dir
    #[cfg(unix)]
    fn create_symlink(src: &Path, dest: &Path) -> Result<()> {
        std::os::unix::fs::symlink(src, dest)
            .with_context(|| format!("创建符号链接失败: {} -> {}", src.display(), dest.display()))
    }

    #[cfg(windows)]
    fn create_symlink(src: &Path, dest: &Path) -> Result<()> {
        std::os::windows::fs::symlink_dir(src, dest)
            .with_context(|| format!("创建符号链接失败: {} -> {}", src.display(), dest.display()))
    }

    /// 检查路径是否为符号链接
    fn is_symlink(path: &Path) -> bool {
        path.symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    }

    /// 获取当前同步方式配置
    fn get_sync_method() -> SyncMethod {
        crate::settings::get_skill_sync_method()
    }

    /// 同步 Skill 到应用目录（使用 symlink 或 copy）
    ///
    /// 根据配置和平台选择最佳同步方式：
    /// - Auto: 优先尝试 symlink，失败时回退到 copy
    /// - Symlink: 仅使用 symlink
    /// - Copy: 仅使用文件复制
    pub fn sync_to_app_dir(directory: &str, app: &AppType) -> Result<()> {
        if matches!(app, AppType::ClaudeDesktop) {
            return Ok(());
        }

        let ssot_dir = Self::get_ssot_dir()?;
        let source = ssot_dir.join(directory);

        Self::validate_sync_source_dir(&source, directory)?;

        let app_dir = Self::get_app_skills_dir(app)?;
        fs::create_dir_all(&app_dir)?;

        let dest = app_dir.join(directory);

        let sync_method = Self::get_sync_method();

        match sync_method {
            SyncMethod::Auto => {
                if dest.exists() && !Self::is_symlink(&dest) {
                    Self::replace_dest_with_copy(&source, &dest, directory)?;
                    log::debug!("Skill {directory} 已通过复制同步到 {app:?}");
                    return Ok(());
                }

                if Self::is_symlink(&dest) {
                    Self::remove_path(&dest)?;
                }

                // 优先尝试 symlink
                match Self::create_symlink(&source, &dest) {
                    Ok(()) => {
                        log::debug!("Skill {directory} 已通过 symlink 同步到 {app:?}");
                        return Ok(());
                    }
                    Err(err) => {
                        log::warn!(
                            "Symlink 创建失败，将回退到文件复制: {} -> {}. 错误: {err:#}",
                            source.display(),
                            dest.display()
                        );
                    }
                }
                // Fallback 到 copy
                Self::replace_dest_with_copy(&source, &dest, directory)?;
                log::debug!("Skill {directory} 已通过复制同步到 {app:?}");
            }
            SyncMethod::Symlink => {
                if dest.exists() || Self::is_symlink(&dest) {
                    Self::remove_path(&dest)?;
                }
                Self::create_symlink(&source, &dest)?;
                log::debug!("Skill {directory} 已通过 symlink 同步到 {app:?}");
            }
            SyncMethod::Copy => {
                Self::replace_dest_with_copy(&source, &dest, directory)?;
                log::debug!("Skill {directory} 已通过复制同步到 {app:?}");
            }
        }

        Ok(())
    }

    /// 复制 Skill 到应用目录（保留用于向后兼容）
    #[deprecated(note = "请使用 sync_to_app_dir() 代替")]
    pub fn copy_to_app(directory: &str, app: &AppType) -> Result<()> {
        Self::sync_to_app_dir(directory, app)
    }

    /// 删除路径（支持 symlink 和真实目录）
    fn remove_path(path: &Path) -> Result<()> {
        if Self::is_symlink(path) {
            // 符号链接：仅删除链接本身，不影响源文件
            #[cfg(unix)]
            fs::remove_file(path)?;
            #[cfg(windows)]
            fs::remove_dir(path)?; // Windows 的目录 symlink 需要用 remove_dir
        } else if path.is_dir() {
            // 真实目录：递归删除
            fs::remove_dir_all(path)?;
        } else if path.exists() {
            // 普通文件
            fs::remove_file(path)?;
        }
        Ok(())
    }

    fn validate_sync_source_dir(source: &Path, directory: &str) -> Result<()> {
        if !source.is_dir() {
            return Err(anyhow!("Skill 不存在于 SSOT: {directory}"));
        }

        let manifest = source.join("SKILL.md");
        if !manifest.is_file() {
            return Err(anyhow!(
                "Skill 源目录缺少 SKILL.md，拒绝同步以避免覆盖目标目录: {}",
                source.display()
            ));
        }

        Ok(())
    }

    fn replace_dest_with_copy(source: &Path, dest: &Path, directory: &str) -> Result<()> {
        Self::validate_sync_source_dir(source, directory)?;

        let parent = dest
            .parent()
            .ok_or_else(|| anyhow!("Invalid skill destination: {}", dest.display()))?;
        fs::create_dir_all(parent)?;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp_name = Self::sanitize_backup_segment(directory);
        let tmp = parent.join(format!(".{tmp_name}.tmp-{}-{nonce}", std::process::id()));

        if tmp.exists() || Self::is_symlink(&tmp) {
            Self::remove_path(&tmp)?;
        }

        let copy_result = Self::copy_dir_recursive(source, &tmp);
        if let Err(err) = copy_result {
            let _ = Self::remove_path(&tmp);
            return Err(err);
        }

        if dest.exists() || Self::is_symlink(dest) {
            Self::remove_path(dest)?;
        }

        fs::rename(&tmp, dest).with_context(|| {
            let _ = Self::remove_path(&tmp);
            format!(
                "替换 Skill 目录失败: {} -> {}",
                tmp.display(),
                dest.display()
            )
        })?;

        Ok(())
    }

    /// 判断路径是否为指向 SSOT 目录内的符号链接。
    fn is_symlink_to_ssot(path: &Path, ssot_dir: &Path) -> bool {
        if !Self::is_symlink(path) {
            return false;
        }

        let Ok(target) = fs::read_link(path) else {
            return false;
        };

        if target.is_absolute() && target.starts_with(ssot_dir) {
            return true;
        }

        let resolved = path
            .parent()
            .map(|parent| parent.join(&target))
            .unwrap_or(target.clone());

        let canonical_ssot = ssot_dir
            .canonicalize()
            .unwrap_or_else(|_| ssot_dir.to_path_buf());
        let canonical_target = resolved.canonicalize().unwrap_or(resolved);

        canonical_target.starts_with(&canonical_ssot)
    }

    /// 从应用目录删除 Skill（支持 symlink 和真实目录）
    pub fn remove_from_app(directory: &str, app: &AppType) -> Result<()> {
        if matches!(app, AppType::ClaudeDesktop) {
            return Ok(());
        }

        let app_dir = Self::get_app_skills_dir(app)?;
        let skill_path = app_dir.join(directory);

        if skill_path.exists() || Self::is_symlink(&skill_path) {
            Self::remove_path(&skill_path)?;
            log::debug!("Skill {directory} 已从 {app:?} 删除");
        }

        Ok(())
    }

    /// 同步所有已启用的 Skills 到指定应用
    pub fn sync_to_app(db: &Arc<Database>, app: &AppType) -> Result<()> {
        let _state_guard = skill_state_read_guard();
        Self::sync_to_app_unlocked(db, app)
    }

    /// Caller must hold either the Skills state read or write guard.
    fn sync_to_app_unlocked(db: &Arc<Database>, app: &AppType) -> Result<()> {
        if matches!(app, AppType::ClaudeDesktop) {
            return Ok(());
        }

        let skills = db.get_all_installed_skills()?;
        let ssot_dir = Self::get_ssot_dir()?;
        let app_dir = Self::get_app_skills_dir(app)?;

        let indexed_skills: HashMap<String, &InstalledSkill> = skills
            .values()
            .map(|skill| (skill.directory.to_lowercase(), skill))
            .collect();

        if app_dir.exists() {
            for entry in fs::read_dir(&app_dir)? {
                let entry = entry?;
                let path = entry.path();
                let dir_name = entry.file_name().to_string_lossy().to_string();

                if dir_name.starts_with('.') {
                    continue;
                }

                if let Some(skill) = indexed_skills.get(&dir_name.to_lowercase()) {
                    if !skill.apps.is_enabled_for(app) {
                        Self::remove_path(&path)?;
                    }
                    continue;
                }

                if Self::is_symlink_to_ssot(&path, &ssot_dir) {
                    Self::remove_path(&path)?;
                }
            }
        }

        for skill in skills.values() {
            if skill.apps.is_enabled_for(app) {
                Self::sync_to_app_dir(&skill.directory, app)?;
            }
        }

        Ok(())
    }

    // ========== 发现功能（保留原有逻辑）==========

    /// 列出所有可发现的技能（从仓库获取）
    pub async fn discover_available(
        &self,
        repos: Vec<SkillRepo>,
    ) -> Result<Vec<DiscoverableSkill>> {
        let mut skills = Vec::new();

        // 仅使用启用的仓库
        let enabled_repos: Vec<SkillRepo> = repos.into_iter().filter(|repo| repo.enabled).collect();

        let fetch_tasks = enabled_repos
            .iter()
            .map(|repo| self.fetch_repo_skills(repo));

        let results: Vec<Result<Vec<DiscoverableSkill>>> =
            futures::future::join_all(fetch_tasks).await;

        for (repo, result) in enabled_repos.into_iter().zip(results) {
            match result {
                Ok(repo_skills) => skills.extend(repo_skills),
                Err(e) => log::warn!("获取仓库 {}/{} 技能失败: {}", repo.owner, repo.name, e),
            }
        }

        // 去重并排序
        Self::deduplicate_discoverable_skills(&mut skills);
        skills.sort_by_key(|skill| skill.name.to_lowercase());

        Ok(skills)
    }

    /// 列出所有技能（兼容旧 API）
    pub async fn list_skills(
        &self,
        repos: Vec<SkillRepo>,
        db: &Arc<Database>,
    ) -> Result<Vec<Skill>> {
        // 获取可发现的技能
        let discoverable = self.discover_available(repos).await?;

        // 获取已安装的技能
        let installed = db.get_all_installed_skills()?;
        let installed_dirs: HashSet<String> =
            installed.values().map(|s| s.directory.clone()).collect();

        // 转换为 Skill 格式
        let mut skills: Vec<Skill> = discoverable
            .into_iter()
            .map(|d| {
                let install_name = Path::new(&d.directory)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| d.directory.clone());

                Skill {
                    key: d.key,
                    name: d.name,
                    description: d.description,
                    directory: d.directory,
                    readme_url: d.readme_url,
                    installed: installed_dirs.contains(&install_name),
                    repo_owner: Some(d.repo_owner),
                    repo_name: Some(d.repo_name),
                    repo_branch: Some(d.repo_branch),
                }
            })
            .collect();

        // 添加本地已安装但不在仓库中的技能
        for skill in installed.values() {
            let already_in_list = skills.iter().any(|s| {
                let s_install_name = Path::new(&s.directory)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| s.directory.clone());
                s_install_name == skill.directory
            });

            if !already_in_list {
                skills.push(Skill {
                    key: skill.id.clone(),
                    name: skill.name.clone(),
                    description: skill.description.clone().unwrap_or_default(),
                    directory: skill.directory.clone(),
                    readme_url: skill.readme_url.clone(),
                    installed: true,
                    repo_owner: skill.repo_owner.clone(),
                    repo_name: skill.repo_name.clone(),
                    repo_branch: skill.repo_branch.clone(),
                });
            }
        }

        skills.sort_by_key(|skill| skill.name.to_lowercase());

        Ok(skills)
    }

    /// 从仓库获取技能列表
    async fn fetch_repo_skills(&self, repo: &SkillRepo) -> Result<Vec<DiscoverableSkill>> {
        match self.fetch_repo_skills_via_api(repo).await {
            Ok(skills) => return Ok(skills),
            Err(err) => {
                log::warn!(
                    "通过 GitHub API 获取仓库 {}/{} skills 失败，将回退 ZIP: {err}",
                    repo.owner,
                    repo.name
                );
            }
        }

        let (temp_dir, resolved_branch) =
            timeout(REPO_DISCOVERY_FALLBACK_TIMEOUT, self.download_repo(repo))
                .await
                .map_err(|_| {
                    anyhow!(format_skill_error(
                        "DOWNLOAD_TIMEOUT",
                        &[
                            ("owner", &repo.owner),
                            ("name", &repo.name),
                            ("timeout", "20")
                        ],
                        Some("checkNetwork"),
                    ))
                })??;

        let mut skills = Vec::new();
        let scan_dir = temp_dir.clone();
        let mut resolved_repo = repo.clone();
        resolved_repo.branch = resolved_branch;
        self.scan_dir_recursive(&scan_dir, &scan_dir, &resolved_repo, &mut skills)?;

        let _ = fs::remove_dir_all(&temp_dir);

        Ok(skills)
    }

    async fn fetch_repo_skills_via_api(&self, repo: &SkillRepo) -> Result<Vec<DiscoverableSkill>> {
        let (tree, branch) = self.fetch_github_tree(repo).await?;
        if tree.truncated {
            return Err(anyhow::anyhow!("GitHub tree response was truncated"));
        }

        let skill_paths = Self::select_top_level_skill_paths(&tree.tree);

        let fetches = skill_paths.iter().map(|skill_path| async {
            let directory = skill_path
                .strip_suffix("/SKILL.md")
                .unwrap_or(skill_path.as_str())
                .trim_end_matches('/');
            let directory = if directory.is_empty() {
                repo.name.as_str()
            } else {
                directory
            };
            let metadata = self
                .fetch_skill_metadata_from_github(repo, &branch, skill_path)
                .await?;
            Ok(DiscoverableSkill {
                key: format!("{}/{}:{}", repo.owner, repo.name, directory),
                name: metadata.name.unwrap_or_else(|| directory.to_string()),
                description: metadata.description.unwrap_or_default(),
                directory: directory.to_string(),
                readme_url: Some(Self::build_skill_doc_url(
                    &repo.owner,
                    &repo.name,
                    &branch,
                    skill_path,
                )),
                repo_owner: repo.owner.clone(),
                repo_name: repo.name.clone(),
                repo_branch: branch.clone(),
            })
        });

        let results: Vec<Result<DiscoverableSkill>> = futures::future::join_all(fetches).await;
        let mut skills = Vec::new();
        let mut error_count = 0;
        for result in results {
            match result {
                Ok(skill) => skills.push(skill),
                Err(err) => {
                    error_count += 1;
                    log::warn!(
                        "解析仓库 {}/{} skill 元数据失败: {err}",
                        repo.owner,
                        repo.name
                    );
                }
            }
        }

        if skills.is_empty() && !skill_paths.is_empty() && error_count > 0 {
            return Err(anyhow::anyhow!(
                "All GitHub content metadata requests failed for {}/{}",
                repo.owner,
                repo.name
            ));
        }

        Ok(skills)
    }

    async fn fetch_repo_skill_hashes(&self, repo: &SkillRepo) -> Result<HashMap<String, String>> {
        let (tree, _branch) = self.fetch_github_tree(repo).await?;
        if tree.truncated {
            return Err(anyhow::anyhow!("GitHub tree response was truncated"));
        }
        Ok(Self::compute_remote_hashes_from_tree(repo, tree.tree))
    }

    async fn fetch_repo_skill_hashes_from_zip(
        &self,
        repo: &SkillRepo,
    ) -> Result<HashMap<String, String>> {
        let (temp_dir, resolved_branch) =
            timeout(REPO_UPDATE_FALLBACK_TIMEOUT, self.download_repo(repo))
                .await
                .map_err(|_| {
                    anyhow!(format_skill_error(
                        "DOWNLOAD_TIMEOUT",
                        &[
                            ("owner", &repo.owner),
                            ("name", &repo.name),
                            ("timeout", REPO_UPDATE_FALLBACK_TIMEOUT_LABEL)
                        ],
                        Some("checkNetwork"),
                    ))
                })??;

        let mut resolved_repo = repo.clone();
        resolved_repo.branch = resolved_branch;
        let mut remote_skills: Vec<DiscoverableSkill> = Vec::new();
        let _ = self.scan_dir_recursive(&temp_dir, &temp_dir, &resolved_repo, &mut remote_skills);

        let mut hashes = HashMap::new();
        for skill in remote_skills {
            let Some(remote_skill_dir) =
                Self::resolve_skill_source_dir(&temp_dir, &skill.directory)
            else {
                continue;
            };
            match Self::compute_dir_git_tree_hash(&remote_skill_dir) {
                Ok(hash) => {
                    hashes.insert(skill.directory, hash);
                }
                Err(err) => {
                    log::warn!("计算远程哈希失败 {}: {err}", skill.key);
                }
            }
        }

        let _ = fs::remove_dir_all(&temp_dir);
        Ok(hashes)
    }

    fn compute_remote_hashes_from_tree(
        repo: &SkillRepo,
        entries: Vec<GithubTreeEntry>,
    ) -> HashMap<String, String> {
        use sha2::{Digest, Sha256};

        let mut skill_dirs: Vec<String> = Self::select_top_level_skill_paths(&entries)
            .into_iter()
            .map(|path| {
                path.strip_suffix("/SKILL.md")
                    .filter(|path| !path.is_empty())
                    .unwrap_or(repo.name.as_str())
                    .to_string()
            })
            .collect();
        skill_dirs.sort();

        let mut hashes = HashMap::new();
        for skill_dir in skill_dirs {
            let prefix = if skill_dir == repo.name {
                String::new()
            } else {
                format!("{}/", skill_dir.trim_end_matches('/'))
            };

            let mut skill_files: Vec<&GithubTreeEntry> = entries
                .iter()
                .filter(|entry| {
                    entry.entry_type == "blob"
                        && if prefix.is_empty() {
                            true
                        } else {
                            entry.path.starts_with(&prefix)
                        }
                })
                .collect();
            skill_files.sort_by(|a, b| a.path.cmp(&b.path));

            let mut hasher = Sha256::new();
            for entry in skill_files {
                let relative = if prefix.is_empty() {
                    entry.path.as_str()
                } else {
                    entry
                        .path
                        .strip_prefix(&prefix)
                        .unwrap_or(entry.path.as_str())
                };
                if relative
                    .split('/')
                    .any(|segment| segment.starts_with('.') || segment.is_empty())
                {
                    continue;
                }
                hasher.update(relative.as_bytes());
                hasher.update(b"\0");
                hasher.update(entry.sha.as_bytes());
                hasher.update(b"\0");
            }

            hashes.insert(skill_dir, format!("{:x}", hasher.finalize()));
        }

        hashes
    }

    fn select_top_level_skill_paths(entries: &[GithubTreeEntry]) -> Vec<String> {
        let mut paths: Vec<String> = entries
            .iter()
            .filter(|entry| {
                entry.entry_type == "blob"
                    && (entry.path == "SKILL.md" || entry.path.ends_with("/SKILL.md"))
            })
            .map(|entry| entry.path.clone())
            .collect();
        paths.sort();
        if paths.iter().any(|path| path == "SKILL.md") {
            return vec!["SKILL.md".to_string()];
        }

        let mut selected: Vec<String> = Vec::new();
        for path in paths {
            let parent = path.strip_suffix("SKILL.md").unwrap_or(path.as_str());
            if selected.iter().any(|selected_path| {
                let selected_parent = selected_path
                    .strip_suffix("SKILL.md")
                    .unwrap_or(selected_path.as_str());
                !selected_parent.is_empty() && parent.starts_with(selected_parent)
            }) {
                continue;
            }
            selected.push(path);
        }

        selected
    }

    /// 远程候选目录的确定性排序键。
    ///
    /// 同一个仓库可能在多处放置同名 skill（例如 `skills/x` 与 `.openclaw/skills/x`），
    /// 且两份内容未必相同。检测与更新必须用同一条规则挑选，否则会出现
    /// “检测拿 A 比、更新写 B” 的死循环，表现为反复提示有更新。
    /// 排序优先级：不含隐藏路径段 > 路径层级浅 > 字典序。
    fn remote_dir_rank(remote_dir: &str) -> (bool, usize, String) {
        let has_hidden_segment = remote_dir
            .split('/')
            .any(|segment| segment.starts_with('.') && segment != "." && segment != "..");
        let depth = remote_dir.split('/').count();
        (has_hidden_segment, depth, remote_dir.to_string())
    }

    /// 在远程候选中挑出与已安装 skill 对应的目录（确定性）。
    ///
    /// 先按叶子名匹配；匹配不到时，若该仓库把 SKILL.md 放在根目录，
    /// 则回退到根级条目——本地安装名可能被用户改成了别的名字。
    fn select_remote_dir_for_skill<'a>(
        remote_dirs: impl IntoIterator<Item = &'a str>,
        install_directory: &str,
        repo_name: &str,
    ) -> Option<&'a str> {
        let all: Vec<&'a str> = remote_dirs.into_iter().collect();

        let mut leaf_matches: Vec<&'a str> = all
            .iter()
            .copied()
            .filter(|remote_dir| {
                let leaf = remote_dir.rsplit('/').next().unwrap_or(remote_dir);
                leaf.eq_ignore_ascii_case(install_directory)
            })
            .collect();

        if !leaf_matches.is_empty() {
            leaf_matches.sort_by_key(|dir| Self::remote_dir_rank(dir));
            return leaf_matches.into_iter().next();
        }

        if all.len() == 1 && all[0].eq_ignore_ascii_case(repo_name) {
            return Some(all[0]);
        }

        None
    }

    fn find_remote_hash_for_skill<'a>(
        remote_hashes: &'a HashMap<String, String>,
        install_directory: &str,
        repo_name: &str,
    ) -> Option<&'a String> {
        let selected = Self::select_remote_dir_for_skill(
            remote_hashes.keys().map(|key| key.as_str()),
            install_directory,
            repo_name,
        )?;
        remote_hashes.get(selected)
    }

    fn compute_local_git_tree_hash(ssot_dir: &Path, skill: &InstalledSkill) -> Option<String> {
        let local_dir = ssot_dir.join(&skill.directory);
        if !local_dir.exists() {
            return None;
        }

        match Self::compute_dir_git_tree_hash(&local_dir) {
            Ok(hash) => Some(hash),
            Err(err) => {
                log::warn!("计算本地哈希失败 {}: {err}", skill.id);
                None
            }
        }
    }

    fn compute_dir_git_tree_hash(dir: &Path) -> Result<String> {
        use sha2::{Digest, Sha256};

        let mut files: Vec<PathBuf> = Vec::new();
        Self::collect_files_for_hash(dir, dir, &mut files)?;

        // 必须按「相对路径字符串的字节序」排序，与远程侧
        // compute_remote_hashes_from_tree 的 `a.path.cmp(&b.path)` 完全一致。
        //
        // 不能用 files.sort()：`PathBuf: Ord` 是逐路径组件比较，与字节序在
        // 「某目录名是另一文件名前缀」时结果相反。例如 `-`(0x2D) < `/`(0x2F)，
        // 字节序把 `academic-paper-reviewer/x` 排在 `academic-paper/y` 之前，
        // 而组件比较认为 `academic-paper` 更小（前缀短者在前）。顺序一变，
        // 聚合摘要就变，本地与远程恒不相等 → 每次检查都报有更新、更新完
        // 下次继续报。只有恰好两种排序同序的 skill 不受影响。
        let mut entries: Vec<(String, PathBuf)> = files
            .into_iter()
            .map(|file_path| {
                let relative = file_path
                    .strip_prefix(dir)
                    .unwrap_or(&file_path)
                    .to_string_lossy()
                    .replace('\\', "/");
                (relative, file_path)
            })
            .collect();
        entries.sort_by(|(a, _), (b, _)| a.as_bytes().cmp(b.as_bytes()));

        let mut hasher = Sha256::new();
        for (rel_str, file_path) in &entries {
            let content = fs::read(file_path)
                .with_context(|| format!("读取文件失败: {}", file_path.display()))?;
            let blob_sha = Self::compute_git_blob_sha(&content);

            hasher.update(rel_str.as_bytes());
            hasher.update(b"\0");
            hasher.update(blob_sha.as_bytes());
            hasher.update(b"\0");
        }

        Ok(format!("{:x}", hasher.finalize()))
    }

    fn compute_git_blob_sha(content: &[u8]) -> String {
        use sha1::{Digest, Sha1};

        let mut hasher = Sha1::new();
        hasher.update(format!("blob {}\0", content.len()).as_bytes());
        hasher.update(content);
        format!("{:x}", hasher.finalize())
    }

    /// 递归扫描目录查找 SKILL.md
    fn scan_dir_recursive(
        &self,
        current_dir: &Path,
        base_dir: &Path,
        repo: &SkillRepo,
        skills: &mut Vec<DiscoverableSkill>,
    ) -> Result<()> {
        let skill_md = current_dir.join("SKILL.md");

        if skill_md.exists() {
            let directory = if current_dir == base_dir {
                repo.name.clone()
            } else {
                current_dir
                    .strip_prefix(base_dir)
                    .unwrap_or(current_dir)
                    .to_string_lossy()
                    .replace('\\', "/")
            };

            let doc_path = skill_md
                .strip_prefix(base_dir)
                .unwrap_or(skill_md.as_path())
                .to_string_lossy()
                .replace('\\', "/");

            if let Ok(skill) =
                self.build_skill_from_metadata(&skill_md, &directory, &doc_path, repo)
            {
                skills.push(skill);
            }

            return Ok(());
        }

        for entry in fs::read_dir(current_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                self.scan_dir_recursive(&path, base_dir, repo, skills)?;
            }
        }

        Ok(())
    }

    /// 从 SKILL.md 构建技能对象
    fn build_skill_from_metadata(
        &self,
        skill_md: &Path,
        directory: &str,
        doc_path: &str,
        repo: &SkillRepo,
    ) -> Result<DiscoverableSkill> {
        let meta = self.parse_skill_metadata(skill_md)?;

        Ok(DiscoverableSkill {
            key: format!("{}/{}:{}", repo.owner, repo.name, directory),
            name: meta.name.unwrap_or_else(|| directory.to_string()),
            description: meta.description.unwrap_or_default(),
            directory: directory.to_string(),
            readme_url: Some(Self::build_skill_doc_url(
                &repo.owner,
                &repo.name,
                &repo.branch,
                doc_path,
            )),
            repo_owner: repo.owner.clone(),
            repo_name: repo.name.clone(),
            repo_branch: repo.branch.clone(),
        })
    }

    /// 解析技能元数据
    fn parse_skill_metadata(&self, path: &Path) -> Result<SkillMetadata> {
        Self::parse_skill_metadata_static(path)
    }

    async fn fetch_github_tree(&self, repo: &SkillRepo) -> Result<(GithubTreeResponse, String)> {
        let client = crate::proxy::http_client::get();
        let mut branches = Vec::new();
        if !repo.branch.is_empty() && !repo.branch.eq_ignore_ascii_case("HEAD") {
            branches.push(repo.branch.as_str());
        }
        if !branches.contains(&"main") {
            branches.push("main");
        }
        if !branches.contains(&"master") {
            branches.push("master");
        }

        let mut last_error = None;
        for branch in branches {
            let url = format!(
                "https://api.github.com/repos/{}/{}/git/trees/{}?recursive=1",
                repo.owner, repo.name, branch
            );
            let result = client
                .get(&url)
                .header(reqwest::header::USER_AGENT, GITHUB_API_USER_AGENT)
                .timeout(REPO_METADATA_TIMEOUT)
                .send()
                .await;

            match result {
                Ok(response) if response.status().is_success() => {
                    let tree = response.json::<GithubTreeResponse>().await?;
                    return Ok((tree, branch.to_string()));
                }
                Ok(response) => {
                    let status = response.status();
                    last_error = Some(anyhow::anyhow!(
                        "GitHub tree request failed with HTTP {}",
                        status
                    ));
                    if status != reqwest::StatusCode::NOT_FOUND {
                        break;
                    }
                }
                Err(err) => {
                    if err.is_timeout() || err.is_connect() {
                        return Err(err.into());
                    }
                    last_error = Some(err.into());
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("GitHub tree request failed")))
    }

    async fn fetch_skill_metadata_from_github(
        &self,
        repo: &SkillRepo,
        branch: &str,
        path: &str,
    ) -> Result<SkillMetadata> {
        let client = crate::proxy::http_client::get();
        let url = format!(
            "https://api.github.com/repos/{}/{}/contents/{}",
            repo.owner, repo.name, path
        );
        let response = client
            .get(url)
            .header(reqwest::header::USER_AGENT, GITHUB_API_USER_AGENT)
            .query(&[("ref", branch)])
            .timeout(REPO_METADATA_TIMEOUT)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "GitHub content request failed with HTTP {}",
                response.status()
            ));
        }

        let content = response.json::<GithubContentResponse>().await?;
        let text = match (
            content.content.as_deref(),
            content.encoding.as_deref(),
            content.download_url.as_deref(),
        ) {
            (Some(encoded), Some("base64"), _) => {
                let normalized = encoded.replace(['\n', '\r'], "");
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(normalized.as_bytes())
                    .context("failed to decode GitHub content")?;
                String::from_utf8(bytes).context("GitHub content is not valid UTF-8")?
            }
            (_, _, Some(download_url)) => {
                client
                    .get(download_url)
                    .header(reqwest::header::USER_AGENT, GITHUB_API_USER_AGENT)
                    .timeout(REPO_METADATA_TIMEOUT)
                    .send()
                    .await?
                    .text()
                    .await?
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "GitHub content response is missing content"
                ))
            }
        };

        Self::parse_skill_metadata_content(&text)
    }

    /// 静态方法：解析技能元数据
    fn parse_skill_metadata_static(path: &Path) -> Result<SkillMetadata> {
        let content = fs::read_to_string(path)?;
        Self::parse_skill_metadata_content(&content)
    }

    fn parse_skill_metadata_content(content: &str) -> Result<SkillMetadata> {
        let content = content.trim_start_matches('\u{feff}');

        let parts: Vec<&str> = content.splitn(3, "---").collect();
        if parts.len() < 3 {
            return Ok(SkillMetadata {
                name: None,
                description: None,
            });
        }

        let front_matter = parts[1].trim();
        let meta: SkillMetadata = serde_yaml::from_str(front_matter).unwrap_or(SkillMetadata {
            name: None,
            description: None,
        });

        Ok(meta)
    }

    /// 从 SKILL.md 读取名称和描述，不存在则用目录名兜底
    fn read_skill_name_desc(skill_md: &Path, fallback_name: &str) -> (String, Option<String>) {
        if skill_md.exists() {
            match Self::parse_skill_metadata_static(skill_md) {
                Ok(meta) => (
                    meta.name.unwrap_or_else(|| fallback_name.to_string()),
                    meta.description,
                ),
                Err(_) => (fallback_name.to_string(), None),
            }
        } else {
            (fallback_name.to_string(), None)
        }
    }

    /// 校验并规范化技能源路径（允许多级目录），拒绝路径穿越和绝对路径
    fn sanitize_skill_source_path(raw: &str) -> Option<PathBuf> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }

        let mut normalized = PathBuf::new();
        let mut has_component = false;

        for component in Path::new(trimmed).components() {
            match component {
                Component::Normal(name) => {
                    let segment = name.to_string_lossy().trim().to_string();
                    if segment.is_empty() || segment == "." || segment == ".." {
                        return None;
                    }
                    normalized.push(segment);
                    has_component = true;
                }
                Component::CurDir
                | Component::ParentDir
                | Component::RootDir
                | Component::Prefix(_) => {
                    return None;
                }
            }
        }

        has_component.then_some(normalized)
    }

    /// 校验并规范化安装目录名（最终落盘目录名，仅单段）
    fn sanitize_install_name(raw: &str) -> Option<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }

        let path = Path::new(trimmed);
        let mut components = path.components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(name)), None) => {
                let normalized = name.to_string_lossy().trim().to_string();
                if normalized.is_empty()
                    || normalized == "."
                    || normalized == ".."
                    || normalized.starts_with('.')
                {
                    None
                } else {
                    Some(normalized)
                }
            }
            _ => None,
        }
    }

    fn require_valid_directory(directory: &str) -> Result<String> {
        match Self::sanitize_install_name(directory) {
            Some(normalized) if normalized == directory => Ok(normalized),
            _ => Err(anyhow!(
                "Invalid skill directory (possible path traversal): {directory:?}"
            )),
        }
    }

    /// 在目录树中查找名称匹配且包含 SKILL.md 的子目录
    ///
    /// 用于 skills.sh 安装回退：API 只返回 skillId（如 "find-skills"），
    /// 但实际文件可能在仓库子目录中（如 "skills/find-skills"）。
    fn find_skill_dir_by_name(root: &Path, target_name: &str) -> Option<PathBuf> {
        fn walk(dir: &Path, target: &str, depth: usize) -> Option<PathBuf> {
            if depth > 3 {
                return None;
            }
            let entries = fs::read_dir(dir).ok()?;
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with('.') {
                    continue;
                }
                if name_str.eq_ignore_ascii_case(target) && path.join("SKILL.md").exists() {
                    return Some(path);
                }
                if let Some(found) = walk(&path, target, depth + 1) {
                    return Some(found);
                }
            }
            None
        }
        walk(root, target_name, 0)
    }

    /// 将 discoverable skill 的目录信息重新解析为解压目录中的真实源目录。
    ///
    /// 返回的目录必须包含 `SKILL.md`。解析顺序：
    /// 1. 校验 `skills/foo` 这类直接相对路径；
    /// 2. 按安装名递归查找含 `SKILL.md` 的目录；
    /// 3. 仓库根目录本身是 skill 时回退到解压根目录。
    fn resolve_skill_source_dir(root: &Path, raw_directory: &str) -> Option<PathBuf> {
        let source_rel = Self::sanitize_skill_source_path(raw_directory)?;
        let target_name = source_rel.file_name()?.to_string_lossy().to_string();
        let direct = root.join(&source_rel);
        if direct.is_dir() && direct.join("SKILL.md").is_file() {
            return Some(direct);
        }

        if let Some(found) = Self::find_skill_dir_by_name(root, &target_name) {
            log::info!(
                "Skill directory '{}' not found at direct path, using fallback: {}",
                target_name,
                found.display()
            );
            return Some(found);
        }

        if root.join("SKILL.md").is_file() {
            log::info!(
                "Skill directory '{}' not found, but SKILL.md exists at root, using repo root",
                target_name,
            );
            return Some(root.to_path_buf());
        }

        None
    }

    /// 由真实解析出的源目录推导 SKILL.md 在仓库内的相对文档路径（正斜杠）。
    /// 两个参数都应是已 canonicalize 的路径（安装流程已做包含性校验）。
    fn doc_path_for_source(repo_root: &Path, source: &Path) -> Option<String> {
        let rel = source.strip_prefix(repo_root).ok()?;
        let mut parts: Vec<String> = rel
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(part) => Some(part.to_string_lossy().to_string()),
                _ => None,
            })
            .collect();
        parts.push("SKILL.md".to_string());
        Some(parts.join("/"))
    }

    /// 选择 readme_url 使用的仓库内文档路径：真实解析出的源目录优先，其次是
    /// 旧 readme_url 中保存的路径，最后才按 directory 拼接。skills.sh 的
    /// `directory` 只是 skillId（末级目录名），嵌套目录场景直接拼接会丢路径、
    /// 文档链接 404（#6111），所以真实源目录必须排第一优先级。
    fn choose_doc_path(
        resolved_source_doc_path: Option<String>,
        readme_url: Option<&str>,
        directory: &str,
    ) -> String {
        if let Some(path) = resolved_source_doc_path {
            return path;
        }
        if let Some(path) = readme_url.and_then(Self::extract_doc_path_from_url) {
            if path.ends_with("/SKILL.md") || path == "SKILL.md" {
                return path;
            }
            return format!("{}/SKILL.md", path.trim_end_matches('/'));
        }
        format!("{}/SKILL.md", directory.trim_end_matches('/'))
    }

    /// 去重技能列表（基于完整 key，不同仓库的同名 skill 分开显示）
    fn deduplicate_discoverable_skills(skills: &mut Vec<DiscoverableSkill>) {
        let mut seen = HashMap::new();
        skills.retain(|skill| {
            // 使用完整 key（owner/repo:directory）作为唯一标识
            // 这样不同仓库的同名 skill 会分开显示
            let unique_key = skill.key.to_lowercase();
            if let std::collections::hash_map::Entry::Vacant(e) = seen.entry(unique_key) {
                e.insert(true);
                true
            } else {
                false
            }
        });
    }

    /// 下载仓库
    async fn download_repo(&self, repo: &SkillRepo) -> Result<(PathBuf, String)> {
        let temp_dir = tempfile::tempdir()?;
        let temp_path = temp_dir.path().to_path_buf();
        let _ = temp_dir.keep();

        let mut branches = Vec::new();
        if !repo.branch.is_empty() && !repo.branch.eq_ignore_ascii_case("HEAD") {
            branches.push(repo.branch.as_str());
        }
        if !branches.contains(&"main") {
            branches.push("main");
        }
        if !branches.contains(&"master") {
            branches.push("master");
        }

        let mut last_error = None;
        for branch in branches {
            let url = format!(
                "https://github.com/{}/{}/archive/refs/heads/{}.zip",
                repo.owner, repo.name, branch
            );

            match self.download_and_extract(&url, &temp_path).await {
                Ok(_) => {
                    return Ok((temp_path, branch.to_string()));
                }
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("所有分支下载失败")))
    }

    /// 下载并解压 ZIP
    async fn download_and_extract(&self, url: &str, dest: &Path) -> Result<()> {
        let client = crate::proxy::http_client::get();
        let response = client.get(url).send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16().to_string();
            return Err(anyhow::anyhow!(format_skill_error(
                "DOWNLOAD_FAILED",
                &[("status", &status)],
                match status.as_str() {
                    "403" => Some("http403"),
                    "404" => Some("http404"),
                    "429" => Some("http429"),
                    _ => Some("checkNetwork"),
                },
            )));
        }

        let bytes = response.bytes().await?;
        let cursor = std::io::Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor)?;

        let root_name = if !archive.is_empty() {
            let first_file = archive.by_index(0)?;
            let name = first_file.name();
            name.split('/').next().unwrap_or("").to_string()
        } else {
            return Err(anyhow::anyhow!(format_skill_error(
                "EMPTY_ARCHIVE",
                &[],
                Some("checkRepoUrl"),
            )));
        };

        // 第一遍：解压普通文件和目录，收集 symlink 条目
        let mut symlinks: Vec<(PathBuf, String)> = Vec::new();

        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            let file_path = file.name().to_string();

            let relative_path =
                if let Some(stripped) = file_path.strip_prefix(&format!("{root_name}/")) {
                    stripped
                } else {
                    continue;
                };

            if relative_path.is_empty() {
                continue;
            }

            // zip-slip 防护：压缩包条目名里可能含 `..`，join 后逃出 dest。
            // 逐组件比 contains("..") 更稳：同时挡掉 `a/./b`、`a/.../b` 这类变形
            // （Windows 上 root_name 可含反斜杠而被当作多段，逃逸深度随之放大）。
            // join 之前必须对实际使用的相对路径再验一次。
            if std::path::Path::new(relative_path)
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                log::warn!("跳过越界的压缩包条目: {}", file.name());
                continue;
            }

            let outpath = dest.join(relative_path);

            if file.is_symlink() {
                // 读取 symlink 目标路径
                let mut target = String::new();
                std::io::Read::read_to_string(&mut file, &mut target)?;
                symlinks.push((outpath, target.trim().to_string()));
            } else if file.is_dir() {
                fs::create_dir_all(&outpath)?;
            } else {
                if let Some(parent) = outpath.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut outfile = fs::File::create(&outpath)?;
                std::io::copy(&mut file, &mut outfile)?;
            }
        }

        // 第二遍：解析 symlink，将目标内容复制到 symlink 位置
        Self::resolve_symlinks_in_dir(dest, &symlinks)?;

        Ok(())
    }

    /// 递归复制目录
    fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
        fs::create_dir_all(dest)?;

        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let path = entry.path();
            let dest_path = dest.join(entry.file_name());

            if path.is_dir() {
                Self::copy_dir_recursive(&path, &dest_path)?;
            } else {
                fs::copy(&path, &dest_path)?;
            }
        }

        Ok(())
    }

    fn resolve_uninstall_backup_source(skill: &InstalledSkill) -> Result<Option<PathBuf>> {
        let ssot_path = Self::get_ssot_dir()?.join(&skill.directory);
        if ssot_path.is_dir() {
            return Ok(Some(ssot_path));
        }

        for app in AppType::all() {
            let app_dir = match Self::get_app_skills_dir(&app) {
                Ok(dir) => dir,
                Err(_) => continue,
            };
            let candidate = app_dir.join(&skill.directory);
            if candidate.is_dir() {
                return Ok(Some(candidate));
            }
        }

        Ok(None)
    }

    fn sanitize_backup_segment(segment: &str) -> String {
        let sanitized = segment
            .chars()
            .map(|c| match c {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c,
                _ => '-',
            })
            .collect::<String>()
            .trim_matches('-')
            .to_string();

        if sanitized.is_empty() {
            "skill".to_string()
        } else {
            sanitized
        }
    }

    /// 收集备份目录下所有候选项，附带分组键、创建时间与体积。
    fn collect_backup_candidates(dir: &Path) -> Result<Vec<BackupCandidate>> {
        let mut candidates = Vec::new();

        for entry in fs::read_dir(dir)? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    log::warn!("读取 Skill 备份目录项失败: {err}");
                    continue;
                }
            };
            let path = entry.path();
            // symlink_metadata：不跟随符号链接，避免把链接目标算进体积。
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(err) => {
                    log::warn!("读取 {} 元数据失败: {err}", path.display());
                    continue;
                }
            };
            if !metadata.is_dir() {
                continue;
            }

            let (group, created_at) = match Self::read_backup_metadata(&path) {
                Ok(meta) => (meta.skill.directory, meta.backup_created_at),
                Err(err) => {
                    log::warn!(
                        "备份 {} 的 meta.json 不可读，按孤儿备份裁剪: {err:#}",
                        path.display()
                    );
                    let fallback = metadata
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                        .map(|elapsed| elapsed.as_secs() as i64)
                        .unwrap_or(0);
                    (SKILL_BACKUP_ORPHAN_GROUP.to_string(), fallback)
                }
            };

            let size = Self::dir_size_bytes(&path);
            candidates.push(BackupCandidate {
                path,
                group,
                created_at,
                size,
            });
        }

        Ok(candidates)
    }

    /// 递归累加目录体积。
    ///
    /// 单项失败只跳过该项：清理是尽力而为的旁路操作，不能因一个坏文件让整轮
    /// 裁剪失败、任由备份目录继续增长。
    fn dir_size_bytes(dir: &Path) -> u64 {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) => {
                log::warn!("统计体积时读取 {} 失败: {err}", dir.display());
                return 0;
            }
        };

        let mut total = 0u64;
        for entry in entries.filter_map(|entry| entry.ok()) {
            let path = entry.path();
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() => {
                    total = total.saturating_add(Self::dir_size_bytes(&path));
                }
                // 符号链接按自身大小计，不解引用（与 collect 侧一致）。
                Ok(metadata) => total = total.saturating_add(metadata.len()),
                Err(err) => log::warn!("统计体积时跳过 {}: {err}", path.display()),
            }
        }
        total
    }

    /// 裁剪旧备份：每个 Skill 各留 [`SKILL_BACKUP_RETAIN_PER_SKILL`] 代，
    /// 若总体积仍超 [`SKILL_BACKUP_TOTAL_SIZE_LIMIT_BYTES`]，再全局按最旧优先删。
    fn cleanup_old_skill_backups(dir: &Path) -> Result<()> {
        Self::cleanup_old_skill_backups_with_limit(dir, SKILL_BACKUP_TOTAL_SIZE_LIMIT_BYTES)
    }

    /// [`Self::cleanup_old_skill_backups`] 的实现，体积上限作为参数注入。
    ///
    /// 单独提参数只为测试：按真实的 1 GiB 上限造用例需要实际写入 1 GiB 载荷。
    ///
    /// 体积回收始终至少留下 1 个备份：刚创建的备份是最新的、排在最后，
    /// 单个备份即便自身超限也不会被立刻删掉——否则「更新前已备份」的承诺会静默失效。
    fn cleanup_old_skill_backups_with_limit(dir: &Path, size_limit: u64) -> Result<()> {
        let candidates = Self::collect_backup_candidates(dir)?;

        let mut by_group: HashMap<String, Vec<BackupCandidate>> = HashMap::new();
        for candidate in candidates {
            by_group
                .entry(candidate.group.clone())
                .or_default()
                .push(candidate);
        }

        let mut survivors: Vec<BackupCandidate> = Vec::new();
        for mut group in by_group.into_values() {
            // 新的在前，取前 N 代留存，其余立即删除。
            group.sort_by_key(|candidate| std::cmp::Reverse(candidate.created_at));
            let overflow = group.split_off(group.len().min(SKILL_BACKUP_RETAIN_PER_SKILL));
            for candidate in overflow {
                Self::remove_backup_dir(&candidate.path);
            }
            survivors.extend(group);
        }

        let mut total: u64 = survivors
            .iter()
            .fold(0u64, |acc, candidate| acc.saturating_add(candidate.size));
        if total <= size_limit {
            return Ok(());
        }

        // 最旧优先回收，但至少留 1 个：故上界是 len()-1，最新的那个永不参与。
        survivors.sort_by_key(|candidate| candidate.created_at);
        let reclaimable = survivors.len().saturating_sub(1);
        for candidate in survivors.iter().take(reclaimable) {
            if total <= size_limit {
                return Ok(());
            }
            log::info!(
                "Skill 备份超总体积上限，删除最旧备份 {}",
                candidate.path.display()
            );
            Self::remove_backup_dir(&candidate.path);
            total = total.saturating_sub(candidate.size);
        }

        if total > size_limit {
            log::warn!("Skill 备份总体积 {total} 字节仍超上限，但仅剩 1 个备份，保留不删");
        }

        Ok(())
    }

    fn remove_backup_dir(path: &Path) {
        if let Err(err) = fs::remove_dir_all(path) {
            log::warn!("删除旧 Skill 备份 {} 失败: {err}", path.display());
        }
    }

    fn backup_path_for_id(backup_id: &str) -> Result<PathBuf> {
        if backup_id.contains("..")
            || backup_id.contains('/')
            || backup_id.contains('\\')
            || backup_id.trim().is_empty()
        {
            return Err(anyhow!("Invalid backup id: {backup_id}"));
        }

        Ok(Self::get_backup_dir()?.join(backup_id))
    }

    fn read_backup_metadata(backup_path: &Path) -> Result<SkillBackupMetadata> {
        let metadata_path = backup_path.join("meta.json");
        let content = fs::read_to_string(&metadata_path)
            .with_context(|| format!("failed to read {}", metadata_path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("failed to parse {}", metadata_path.display()))
    }

    fn create_uninstall_backup(skill: &InstalledSkill) -> Result<Option<PathBuf>> {
        let Some(source_path) = Self::resolve_uninstall_backup_source(skill)? else {
            log::warn!(
                "Skill {} 卸载前未找到可备份的目录，将跳过备份",
                skill.directory
            );
            return Ok(None);
        };

        let backup_root = Self::get_backup_dir()?;
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let slug = Self::sanitize_backup_segment(&skill.directory);
        let mut backup_path = backup_root.join(format!("{timestamp}_{slug}"));
        let mut counter = 1;
        while backup_path.exists() {
            backup_path = backup_root.join(format!("{timestamp}_{slug}_{counter}"));
            counter += 1;
        }

        let write_backup = || -> Result<()> {
            let skill_backup_dir = backup_path.join("skill");
            Self::copy_dir_recursive(&source_path, &skill_backup_dir)?;

            let metadata = SkillBackupMetadata {
                skill: skill.clone(),
                backup_created_at: Utc::now().timestamp(),
                source_path: source_path.to_string_lossy().to_string(),
            };
            let metadata_path = backup_path.join("meta.json");
            let metadata_json = serde_json::to_string_pretty(&metadata)
                .context("failed to serialize skill backup metadata")?;
            fs::write(&metadata_path, metadata_json)
                .with_context(|| format!("failed to write {}", metadata_path.display()))?;
            Ok(())
        };

        if let Err(err) = write_backup() {
            let _ = fs::remove_dir_all(&backup_path);
            return Err(err);
        }

        if let Err(err) = Self::cleanup_old_skill_backups(&backup_root) {
            log::warn!("清理旧 Skill 备份失败: {err:#}");
        }

        log::info!(
            "Skill {} 已在卸载前备份到 {}",
            skill.name,
            backup_path.display()
        );

        Ok(Some(backup_path))
    }

    /// 解析 ZIP 中的符号链接：将目标内容复制到 symlink 位置
    ///
    /// GitHub ZIP 归档保留了 symlink 元数据，解压时可通过 `is_symlink()` 检测。
    /// 此方法将 symlink 解析为实际文件/目录内容（而非创建真实 symlink），
    /// 以确保跨平台兼容且 skill 内容自包含。
    fn resolve_symlinks_in_dir(base_dir: &Path, symlinks: &[(PathBuf, String)]) -> Result<()> {
        // 规范化 base_dir（macOS 上 /tmp → /private/tmp，需保持一致）
        let canonical_base = base_dir
            .canonicalize()
            .unwrap_or_else(|_| base_dir.to_path_buf());

        for (link_path, target) in symlinks {
            // 计算 symlink 的父目录，然后拼接目标的相对路径
            let parent = link_path.parent().unwrap_or(base_dir);
            let resolved = parent.join(target);

            // 规范化路径（解析 .. 等）
            let resolved = match resolved.canonicalize() {
                Ok(p) => p,
                Err(_) => {
                    log::warn!(
                        "Symlink 目标不存在，跳过: {} -> {}",
                        link_path.display(),
                        target
                    );
                    continue;
                }
            };

            // 安全检查：确保目标在 base_dir 内（防止路径穿越）
            if !resolved.starts_with(&canonical_base) {
                log::warn!(
                    "Symlink 目标超出仓库范围，跳过: {} -> {}",
                    link_path.display(),
                    resolved.display()
                );
                continue;
            }

            // 复制目标内容到 symlink 位置
            if resolved.is_dir() {
                Self::copy_dir_recursive(&resolved, link_path)?;
            } else if resolved.is_file() {
                if let Some(parent) = link_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&resolved, link_path)?;
            }
        }
        Ok(())
    }

    // ========== 从 ZIP 文件安装 ==========

    /// 从本地 ZIP 文件安装 Skills
    ///
    /// 流程：
    /// 1. 解压 ZIP 到临时目录
    /// 2. 扫描目录查找包含 SKILL.md 的技能
    /// 3. 复制到 SSOT 并保存到数据库
    /// 4. 同步到当前应用目录
    pub fn install_from_zip(
        db: &Arc<Database>,
        zip_path: &Path,
        current_app: &AppType,
    ) -> Result<Vec<InstalledSkill>> {
        // 解压到临时目录
        let temp_dir = Self::extract_local_zip(zip_path)?;

        // 扫描所有包含 SKILL.md 的目录
        let skill_dirs = Self::scan_skills_in_dir(&temp_dir)?;

        if skill_dirs.is_empty() {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(anyhow!(format_skill_error(
                "NO_SKILLS_IN_ZIP",
                &[],
                Some("checkZipContent"),
            )));
        }

        let _state_guard = skill_state_write_guard();
        let ssot_dir = Self::get_ssot_dir()?;
        let mut installed = Vec::new();
        let existing_skills = db.get_all_installed_skills()?;
        let zip_stem = zip_path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());

        for skill_dir in skill_dirs {
            // 解析元数据（提前解析，用于确定安装名）
            let skill_md = skill_dir.join("SKILL.md");
            let meta = if skill_md.exists() {
                Self::parse_skill_metadata_static(&skill_md).ok()
            } else {
                None
            };

            // 获取目录名称作为安装名
            // 当 SKILL.md 在 ZIP 根目录时，skill_dir == temp_dir，
            // file_name() 会返回临时目录名（如 .tmpDZKGpF），需要回退到其他来源
            let install_name = {
                let dir_name = skill_dir
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();

                if skill_dir == temp_dir || dir_name.is_empty() || dir_name.starts_with('.') {
                    // SKILL.md 在根目录：优先用元数据 name，否则用 ZIP 文件名
                    meta.as_ref()
                        .and_then(|m| m.name.as_deref())
                        .and_then(Self::sanitize_install_name)
                        .or_else(|| zip_stem.as_deref().and_then(Self::sanitize_install_name))
                } else {
                    Self::sanitize_install_name(&dir_name)
                        .or_else(|| {
                            meta.as_ref()
                                .and_then(|m| m.name.as_deref())
                                .and_then(Self::sanitize_install_name)
                        })
                        .or_else(|| zip_stem.as_deref().and_then(Self::sanitize_install_name))
                }
            };
            let install_name = match install_name {
                Some(name) => name,
                None => {
                    let _ = fs::remove_dir_all(&temp_dir);
                    return Err(anyhow!(format_skill_error(
                        "INVALID_SKILL_DIRECTORY",
                        &[("zip", &zip_path.display().to_string())],
                        Some("checkZipContent"),
                    )));
                }
            };

            // 检查是否已有同名 directory 的 skill
            let conflict = existing_skills
                .values()
                .find(|s| s.directory.eq_ignore_ascii_case(&install_name));

            if let Some(existing) = conflict {
                log::warn!(
                    "Skill directory '{}' already exists (from {}), skipping",
                    install_name,
                    existing.id
                );
                continue;
            }

            let (name, description) = match meta {
                Some(m) => (
                    m.name.unwrap_or_else(|| install_name.clone()),
                    m.description,
                ),
                None => (install_name.clone(), None),
            };

            // 复制到 SSOT
            let dest = ssot_dir.join(&install_name);
            if dest.exists() {
                let _ = fs::remove_dir_all(&dest);
            }
            Self::copy_dir_recursive(&skill_dir, &dest)?;

            // 计算内容哈希
            let content_hash = Self::compute_dir_hash(&dest).ok();

            // 创建 InstalledSkill 记录
            let skill = InstalledSkill {
                id: format!("local:{install_name}"),
                name,
                description,
                directory: install_name.clone(),
                repo_owner: None,
                repo_name: None,
                repo_branch: None,
                readme_url: None,
                apps: SkillApps::only(current_app),
                installed_at: chrono::Utc::now().timestamp(),
                content_hash,
                updated_at: 0,
            };

            // 保存到数据库
            db.save_skill(&skill)?;

            // 同步到当前应用目录
            Self::sync_to_app_dir(&install_name, current_app)?;

            log::info!(
                "Skill {} installed from ZIP, enabled for {:?}",
                skill.name,
                current_app
            );
            installed.push(skill);
        }

        // 清理临时目录
        let _ = fs::remove_dir_all(&temp_dir);

        Ok(installed)
    }

    /// 解压本地 ZIP 文件到临时目录
    fn extract_local_zip(zip_path: &Path) -> Result<PathBuf> {
        let file = fs::File::open(zip_path)
            .with_context(|| format!("Failed to open ZIP file: {}", zip_path.display()))?;

        let mut archive = zip::ZipArchive::new(file)
            .with_context(|| format!("Failed to read ZIP file: {}", zip_path.display()))?;

        if archive.is_empty() {
            return Err(anyhow!(format_skill_error(
                "EMPTY_ARCHIVE",
                &[],
                Some("checkZipContent"),
            )));
        }

        let temp_dir = tempfile::tempdir()?;
        let temp_path = temp_dir.path().to_path_buf();
        let _ = temp_dir.keep(); // Keep the directory, we'll clean up later

        let mut symlinks: Vec<(PathBuf, String)> = Vec::new();

        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            let file_path = match file.enclosed_name() {
                Some(path) => path.to_owned(),
                None => continue,
            };

            let outpath = temp_path.join(&file_path);

            if file.is_symlink() {
                let mut target = String::new();
                std::io::Read::read_to_string(&mut file, &mut target)?;
                symlinks.push((outpath, target.trim().to_string()));
            } else if file.is_dir() {
                fs::create_dir_all(&outpath)?;
            } else {
                if let Some(parent) = outpath.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut outfile = fs::File::create(&outpath)?;
                std::io::copy(&mut file, &mut outfile)?;
            }
        }

        // 解析 symlink
        Self::resolve_symlinks_in_dir(&temp_path, &symlinks)?;

        Ok(temp_path)
    }

    /// 递归扫描目录查找包含 SKILL.md 的技能目录
    fn scan_skills_in_dir(dir: &Path) -> Result<Vec<PathBuf>> {
        let mut skill_dirs = Vec::new();
        Self::scan_skills_recursive(dir, &mut skill_dirs)?;
        Ok(skill_dirs)
    }

    /// 递归扫描辅助函数
    fn scan_skills_recursive(current: &Path, results: &mut Vec<PathBuf>) -> Result<()> {
        // 检查当前目录是否包含 SKILL.md
        let skill_md = current.join("SKILL.md");
        if skill_md.exists() {
            results.push(current.to_path_buf());
            // 找到后不再递归子目录（一个 skill 目录）
            return Ok(());
        }

        // 递归子目录
        if let Ok(entries) = fs::read_dir(current) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // 跳过隐藏目录
                    let dir_name = entry.file_name().to_string_lossy().to_string();
                    if dir_name.starts_with('.') {
                        continue;
                    }
                    Self::scan_skills_recursive(&path, results)?;
                }
            }
        }

        Ok(())
    }

    // ========== 仓库管理（保留原有逻辑）==========

    /// 列出仓库
    pub fn list_repos(&self, store: &SkillStore) -> Vec<SkillRepo> {
        store.repos.clone()
    }

    /// 添加仓库
    pub fn add_repo(&self, store: &mut SkillStore, repo: SkillRepo) -> Result<()> {
        if let Some(pos) = store
            .repos
            .iter()
            .position(|r| r.owner == repo.owner && r.name == repo.name)
        {
            store.repos[pos] = repo;
        } else {
            store.repos.push(repo);
        }

        Ok(())
    }

    /// 删除仓库
    pub fn remove_repo(&self, store: &mut SkillStore, owner: String, name: String) -> Result<()> {
        store
            .repos
            .retain(|r| !(r.owner == owner && r.name == name));

        Ok(())
    }

    // ========== skills.sh 搜索 ==========

    /// 搜索 skills.sh 公共目录
    pub async fn search_skills_sh(
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<SkillsShSearchResult> {
        let client = crate::proxy::http_client::get();

        let url = url::Url::parse_with_params(
            "https://skills.sh/api/search",
            &[
                ("q", query),
                ("limit", &limit.to_string()),
                ("offset", &offset.to_string()),
            ],
        )?;

        let resp = client
            .get(url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?
            .error_for_status()?
            .json::<SkillsShApiResponse>()
            .await?;

        let skills = resp
            .skills
            .into_iter()
            .filter_map(|s| {
                let parts: Vec<&str> = s.source.splitn(2, '/').collect();
                if parts.len() != 2 {
                    return None;
                }
                let (owner, repo) = (parts[0].to_string(), parts[1].to_string());
                // 过滤非 GitHub 来源（如 "skills.volces.com"、"mcp-hub.momenta.works"）
                if owner.contains('.') || repo.contains('.') {
                    return None;
                }
                Some(SkillsShDiscoverableSkill {
                    key: s.id,
                    name: s.name,
                    directory: s.skill_id.clone(),
                    repo_owner: owner.clone(),
                    repo_name: repo.clone(),
                    repo_branch: "main".to_string(),
                    installs: s.installs,
                    readme_url: Some(format!("https://github.com/{}/{}", owner, repo)),
                })
            })
            .collect();

        Ok(SkillsShSearchResult {
            skills,
            total_count: resp.count,
            query: resp.query,
        })
    }
}

// ========== 迁移支持 ==========

/// 从 lock 文件信息构建 skill 的 ID、仓库字段和 readme URL
///
/// 返回 (id, repo_owner, repo_name, repo_branch, readme_url)
fn build_repo_info_from_lock(
    lock: &HashMap<String, LockRepoInfo>,
    dir_name: &str,
) -> (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    match lock.get(dir_name) {
        Some(info) => {
            let branch = info.branch.clone();
            let url_branch = branch.clone().unwrap_or_else(|| "HEAD".to_string());
            // 优先使用 lock 文件中的 skillPath，否则回退到 dir_name/SKILL.md
            let fallback = format!("{dir_name}/SKILL.md");
            let doc_path = info.skill_path.as_deref().unwrap_or(&fallback);
            let url = Some(SkillService::build_skill_doc_url(
                &info.owner,
                &info.repo,
                &url_branch,
                doc_path,
            ));
            (
                format!("{}/{}:{dir_name}", info.owner, info.repo),
                Some(info.owner.clone()),
                Some(info.repo.clone()),
                branch,
                url,
            )
        }
        None => (format!("local:{dir_name}"), None, None, None, None),
    }
}

/// 将 lock 文件中发现的仓库保存到 skill_repos（去重）
fn save_repos_from_lock(
    db: &Arc<Database>,
    lock: &HashMap<String, LockRepoInfo>,
    directories: impl Iterator<Item = impl AsRef<str>>,
) {
    let existing_repos: HashSet<(String, String)> = db
        .get_skill_repos()
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.owner, r.name))
        .collect();
    let mut added = HashSet::new();

    for dir_name in directories {
        if let Some(info) = lock.get(dir_name.as_ref()) {
            let key = (info.owner.clone(), info.repo.clone());
            if !existing_repos.contains(&key) && added.insert(key) {
                let skill_repo = SkillRepo {
                    owner: info.owner.clone(),
                    name: info.repo.clone(),
                    // 未知分支时使用 HEAD 语义，后续下载会回退到 main/master。
                    branch: info.branch.clone().unwrap_or_else(|| "HEAD".to_string()),
                    enabled: true,
                };
                if let Err(e) = db.save_skill_repo(&skill_repo) {
                    log::warn!("保存 skill 仓库 {}/{} 失败: {}", info.owner, info.repo, e);
                } else {
                    log::info!(
                        "从 agents lock 文件发现并添加仓库: {}/{} ({})",
                        info.owner,
                        info.repo,
                        skill_repo.branch
                    );
                }
            }
        }
    }
}

/// 首次启动迁移：扫描应用目录，重建数据库
pub fn migrate_skills_to_ssot(db: &Arc<Database>) -> Result<usize> {
    let _state_guard = skill_state_write_guard();
    let ssot_dir = SkillService::get_ssot_dir()?;
    let agents_lock = parse_agents_lock();
    let snapshot: Vec<LegacySkillMigrationRow> =
        match db.get_setting("skills_ssot_migration_snapshot")? {
            Some(value) if !value.trim().is_empty() => match serde_json::from_str(&value) {
                Ok(rows) => rows,
                Err(err) => {
                    log::warn!("解析 skills 迁移快照失败，将回退到文件系统扫描: {err}");
                    Vec::new()
                }
            },
            _ => Vec::new(),
        };

    let has_snapshot = !snapshot.is_empty();
    let mut discovered: HashMap<String, SkillApps> = HashMap::new();

    if has_snapshot {
        for row in &snapshot {
            if let Ok(app) = row.app_type.parse::<AppType>() {
                discovered
                    .entry(row.directory.clone())
                    .or_default()
                    .set_enabled_for(&app, true);
            }
        }
    }

    // 扫描各应用目录
    for app in AppType::all() {
        let app_dir = match SkillService::get_app_skills_dir(&app) {
            Ok(d) => d,
            Err(_) => continue,
        };

        let entries = match fs::read_dir(&app_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let dir_name = entry.file_name().to_string_lossy().to_string();
            if dir_name.starts_with('.') {
                continue;
            }
            if !path.join("SKILL.md").exists() {
                continue;
            }
            if has_snapshot && !discovered.contains_key(&dir_name) {
                continue;
            }

            // 复制到 SSOT（如果不存在）
            let ssot_path = ssot_dir.join(&dir_name);
            if !ssot_path.exists() {
                SkillService::copy_dir_recursive(&path, &ssot_path)?;
            }

            if !has_snapshot {
                discovered
                    .entry(dir_name)
                    .or_default()
                    .set_enabled_for(&app, true);
            }
        }
    }

    // 重建数据库
    db.clear_skills()?;

    // 将 lock 文件中发现的仓库保存到 skill_repos
    save_repos_from_lock(db, &agents_lock, discovered.keys());

    let mut count = 0;
    for (directory, apps) in discovered {
        let ssot_path = ssot_dir.join(&directory);
        let skill_md = ssot_path.join("SKILL.md");

        let (name, description) = SkillService::read_skill_name_desc(&skill_md, &directory);

        let (id, repo_owner, repo_name, repo_branch, readme_url) =
            build_repo_info_from_lock(&agents_lock, &directory);

        let content_hash = SkillService::compute_dir_hash(&ssot_path).ok();

        let skill = InstalledSkill {
            id,
            name,
            description,
            directory,
            repo_owner,
            repo_name,
            repo_branch,
            readme_url,
            apps,
            installed_at: chrono::Utc::now().timestamp(),
            content_hash,
            updated_at: 0,
        };

        db.save_skill(&skill)?;
        count += 1;
    }

    let _ = db.set_setting("skills_ssot_migration_snapshot", "");

    log::info!("Skills 迁移完成，共 {count} 个");

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_skill(dir: &Path, name: &str) {
        fs::create_dir_all(dir).expect("create skill dir");
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Test skill\n---\n"),
        )
        .expect("write SKILL.md");
    }

    #[test]
    fn resolve_skill_source_dir_returns_repo_root_for_root_level_skill() {
        let temp = tempdir().expect("tempdir");
        write_skill(temp.path(), "Root Skill");

        let resolved = SkillService::resolve_skill_source_dir(temp.path(), "last30days-skill-cn")
            .expect("root-level skill should resolve to the extracted repo root");

        assert_eq!(resolved, temp.path());
    }

    #[test]
    fn resolve_skill_source_dir_returns_direct_nested_directory_when_present() {
        let temp = tempdir().expect("tempdir");
        let nested = temp.path().join("skills").join("nested-skill");
        write_skill(&nested, "Nested Skill");

        let resolved = SkillService::resolve_skill_source_dir(temp.path(), "skills/nested-skill")
            .expect("nested skill should resolve from its relative source path");

        assert_eq!(resolved, nested);
    }

    #[test]
    fn resolve_skill_source_dir_falls_back_to_matching_install_name() {
        let temp = tempdir().expect("tempdir");
        let nested = temp.path().join("skills").join("nested-skill");
        write_skill(&nested, "Nested Skill");

        let resolved = SkillService::resolve_skill_source_dir(temp.path(), "nested-skill")
            .expect("install name should fall back to the matching discovered skill directory");

        assert_eq!(resolved, nested);
    }

    #[test]
    fn resolve_skill_source_dir_skips_same_name_wrapper_without_skill_md() {
        let temp = tempdir().expect("tempdir");
        let wrapper = temp.path().join("ast-grep");
        fs::create_dir_all(wrapper.join(".claude-plugin")).expect("create wrapper plugin dir");
        let real_skill = wrapper.join("skills").join("ast-grep");
        write_skill(&real_skill, "ast-grep");

        let resolved = SkillService::resolve_skill_source_dir(temp.path(), "ast-grep")
            .expect("resolve inner skill");

        assert_eq!(resolved, real_skill);
    }

    #[test]
    fn resolve_skill_source_dir_rejects_wrapper_without_any_skill_md() {
        let temp = tempdir().expect("tempdir");
        fs::create_dir_all(temp.path().join("ast-grep").join(".claude-plugin"))
            .expect("create wrapper plugin dir");

        assert!(SkillService::resolve_skill_source_dir(temp.path(), "ast-grep").is_none());
    }

    #[test]
    fn replace_dest_with_copy_rejects_empty_source_without_touching_existing_dest() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source-skill");
        let dest = temp.path().join("app-skills").join("source-skill");
        fs::create_dir_all(&source).expect("create empty source");
        write_skill(&dest, "Existing Skill");

        let err = SkillService::replace_dest_with_copy(&source, &dest, "source-skill")
            .expect_err("empty source should not replace existing app skill");

        assert!(
            err.to_string().contains("SKILL.md"),
            "unexpected error: {err:#}"
        );
        assert!(
            dest.join("SKILL.md").is_file(),
            "existing destination skill should be preserved"
        );
    }

    #[test]
    fn select_remote_dir_prefers_visible_path_over_hidden_duplicate() {
        // DietrichGebert/ponytail 同时存在 skills/x 与 .openclaw/skills/x，且两份内容不同。
        let candidates = [".openclaw/skills/ponytail-audit", "skills/ponytail-audit"];

        for ordering in [[0usize, 1usize], [1, 0]] {
            let ordered: Vec<&str> = ordering.iter().map(|i| candidates[*i]).collect();
            let selected = SkillService::select_remote_dir_for_skill(
                ordered.iter().copied(),
                "ponytail-audit",
                "ponytail",
            );
            assert_eq!(
                selected,
                Some("skills/ponytail-audit"),
                "候选顺序不应影响选择结果"
            );
        }
    }

    #[test]
    fn select_remote_dir_is_independent_of_hashmap_iteration_order() {
        // check_updates 从 HashMap 取候选，迭代顺序每进程随机；
        // 选择必须只依赖路径本身，否则会与 update_skill 选中不同副本。
        let mut remote_hashes = HashMap::new();
        remote_hashes.insert(
            ".openclaw/skills/ponytail".to_string(),
            "hash-a".to_string(),
        );
        remote_hashes.insert("skills/ponytail".to_string(), "hash-b".to_string());

        for _ in 0..64 {
            let hash =
                SkillService::find_remote_hash_for_skill(&remote_hashes, "ponytail", "ponytail");
            assert_eq!(hash.map(String::as_str), Some("hash-b"));
        }
    }

    #[test]
    fn select_remote_dir_falls_back_to_root_entry_for_renamed_install() {
        // openbiox/Bizard 的 SKILL.md 在仓库根，远程 key 是仓库名，
        // 本地安装名是 "Bizard — Biomedical Visualization Atlas"，叶子名永不相等。
        let selected = SkillService::select_remote_dir_for_skill(
            ["Bizard"],
            "Bizard — Biomedical Visualization Atlas",
            "Bizard",
        );
        assert_eq!(selected, Some("Bizard"));
    }

    #[test]
    fn select_remote_dir_does_not_guess_when_multiple_candidates_mismatch() {
        let selected = SkillService::select_remote_dir_for_skill(
            ["skills/alpha", "skills/beta"],
            "gamma",
            "some-repo",
        );
        assert_eq!(selected, None, "无法确定时不应任选一个");
    }

    #[test]
    fn select_remote_dir_prefers_shallower_path_among_visible_duplicates() {
        let selected = SkillService::select_remote_dir_for_skill(
            ["plugins/ars-codex/skills/suite", "skills/suite"],
            "suite",
            "repo",
        );
        assert_eq!(selected, Some("skills/suite"));
    }

    #[test]
    fn install_hash_matches_update_check_hash() {
        // 写入侧（compute_dir_hash）与检测侧（compute_dir_git_tree_hash）必须同口径。
        // 两者一旦分叉，本地值永远对不上远程，界面每次都提示有更新且无法收敛。
        let temp = tempdir().expect("tempdir");
        let skill_dir = temp.path().join("demo-skill");
        write_skill(&skill_dir, "Demo Skill");
        fs::create_dir_all(skill_dir.join("references")).expect("create nested dir");
        fs::write(skill_dir.join("references/notes.md"), "# notes\n").expect("write nested file");

        let written = SkillService::compute_dir_hash(&skill_dir).expect("install-side hash");
        let checked = SkillService::compute_dir_git_tree_hash(&skill_dir).expect("check-side hash");

        assert_eq!(
            written, checked,
            "安装写入的哈希必须与更新检测重算的哈希一致"
        );
    }

    #[test]
    fn local_hash_matches_remote_tree_order_for_prefix_named_siblings() {
        // 回归：本地曾用 Vec<PathBuf>::sort()（逐路径组件比较），远程用 tree API 的
        // path 字符串字节序。当某目录名是同级另一名字的前缀时（如 academic-paper/
        // 与 academic-paper-reviewer/），两种排序给出不同顺序 —— '-'(0x2D) 字节序
        // 小于 '/'(0x2F)，而组件比较认为更短的 "academic-paper" 更小。文件内容完全
        // 一致，哈希却不等，界面每次都报有更新且更新后无法收敛。
        let temp = tempdir().expect("tempdir");
        let skill_dir = temp.path().join("suite");
        write_skill(&skill_dir, "Suite");
        for (rel, body) in [
            ("academic-paper/WORKFLOW.md", "# a\n"),
            ("academic-paper-reviewer/WORKFLOW.md", "# b\n"),
            ("scripts/pptx_to_svg.py", "print(1)\n"),
            ("scripts/pptx_to_svg/__init__.py", "print(2)\n"),
        ] {
            let path = skill_dir.join(rel);
            fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
            fs::write(&path, body).expect("write file");
        }

        let local = SkillService::compute_dir_git_tree_hash(&skill_dir).expect("local hash");

        // 用与 compute_remote_hashes_from_tree 相同的方式构造远程期望值：
        // 相对路径字符串按字节序排序，逐项喂入 path\0blob_sha\0。
        let mut entries: Vec<(String, String)> = Vec::new();
        for rel in [
            "SKILL.md",
            "academic-paper/WORKFLOW.md",
            "academic-paper-reviewer/WORKFLOW.md",
            "scripts/pptx_to_svg.py",
            "scripts/pptx_to_svg/__init__.py",
        ] {
            let content = fs::read(skill_dir.join(rel)).expect("read file");
            entries.push((
                rel.to_string(),
                SkillService::compute_git_blob_sha(&content),
            ));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        for (rel, blob_sha) in &entries {
            hasher.update(rel.as_bytes());
            hasher.update(b"\0");
            hasher.update(blob_sha.as_bytes());
            hasher.update(b"\0");
        }
        let remote = format!("{:x}", hasher.finalize());

        assert_eq!(
            local, remote,
            "本地哈希的文件顺序必须与 GitHub tree API 的字节序一致"
        );
    }

    #[test]
    fn dir_hash_uses_git_blob_sha_not_raw_content() {
        // 远程侧哈希由 GitHub tree API 的 blob SHA 组成，本地必须同口径。
        // 用单文件目录反推：期望值 = sha256("SKILL.md\0" + git_blob_sha + "\0")
        use sha2::{Digest, Sha256};

        let temp = tempdir().expect("tempdir");
        let skill_dir = temp.path().join("one-file");
        fs::create_dir_all(&skill_dir).expect("create dir");
        let content = b"hello skill\n";
        fs::write(skill_dir.join("SKILL.md"), content).expect("write file");

        let blob_sha = SkillService::compute_git_blob_sha(content);
        let mut hasher = Sha256::new();
        hasher.update(b"SKILL.md");
        hasher.update(b"\0");
        hasher.update(blob_sha.as_bytes());
        hasher.update(b"\0");
        let expected = format!("{:x}", hasher.finalize());

        assert_eq!(
            SkillService::compute_dir_hash(&skill_dir).expect("hash"),
            expected
        );
    }

    #[test]
    fn choose_doc_path_prefers_resolved_source_over_stale_readme_url() {
        // #6111 现场还原：skills.sh 安装嵌套目录 skill——directory 是 skillId
        // （末级目录名），readme_url 是仓库根 URL，真实嵌套路径只能来自
        // 解析出的源目录。若退回"readme_url 提取优先、directory 拼接兜底"，
        // 本断言即失败（旧逻辑产出 alibabacloud-cli-guidance/SKILL.md → 404）。
        let doc_path = SkillService::choose_doc_path(
            Some("skills/developertools/solutions/alibabacloud-cli-guidance/SKILL.md".to_string()),
            Some("https://github.com/aliyun/alibabacloud-aiops-skills"),
            "alibabacloud-cli-guidance",
        );
        assert_eq!(
            doc_path,
            "skills/developertools/solutions/alibabacloud-cli-guidance/SKILL.md"
        );
    }

    #[test]
    fn choose_doc_path_falls_back_to_readme_url_path_then_directory() {
        // 无解析结果（SSOT 目录已存在、未重新下载）时沿用旧 readme_url 的仓库内
        // 路径，兼容 blob/tree 两种格式并补 SKILL.md 后缀
        let doc_path = SkillService::choose_doc_path(
            None,
            Some("https://github.com/o/r/tree/main/skills/foo"),
            "foo",
        );
        assert_eq!(doc_path, "skills/foo/SKILL.md");

        // 旧 readme_url 已是完整文档路径时原样保留
        let doc_path = SkillService::choose_doc_path(
            None,
            Some("https://github.com/o/r/blob/main/skills/foo/SKILL.md"),
            "foo",
        );
        assert_eq!(doc_path, "skills/foo/SKILL.md");

        // 两者都没有时按 directory 拼接
        let doc_path = SkillService::choose_doc_path(None, None, "skills/foo");
        assert_eq!(doc_path, "skills/foo/SKILL.md");
    }

    #[test]
    fn doc_path_for_source_returns_repo_relative_skill_md_path() {
        let temp = tempdir().expect("tempdir");
        let nested = temp
            .path()
            .join("skills")
            .join("developertools")
            .join("solutions")
            .join("foo");
        fs::create_dir_all(&nested).expect("create nested dirs");

        assert_eq!(
            SkillService::doc_path_for_source(temp.path(), &nested),
            Some("skills/developertools/solutions/foo/SKILL.md".to_string())
        );
        // 源目录即仓库根：文档路径就是根下的 SKILL.md
        assert_eq!(
            SkillService::doc_path_for_source(temp.path(), temp.path()),
            Some("SKILL.md".to_string())
        );
        // 仓库根之外：None（调用方已做包含性校验，防御性兜底）
        assert_eq!(
            SkillService::doc_path_for_source(temp.path(), std::path::Path::new("/elsewhere")),
            None
        );
    }

    /// 造一个备份目录：`skill/` 放指定字节数的载荷，`meta.json` 记录分组与时间。
    fn write_backup(root: &Path, id: &str, directory: &str, created_at: i64, payload_bytes: usize) {
        let backup_path = root.join(id);
        let skill_dir = backup_path.join("skill");
        fs::create_dir_all(&skill_dir).expect("create backup skill dir");
        fs::write(skill_dir.join("payload.bin"), vec![0u8; payload_bytes])
            .expect("write backup payload");

        let skill = InstalledSkill {
            id: format!("owner/repo:{directory}"),
            name: directory.to_string(),
            description: None,
            directory: directory.to_string(),
            repo_owner: Some("owner".to_string()),
            repo_name: Some("repo".to_string()),
            repo_branch: Some("main".to_string()),
            readme_url: None,
            apps: SkillApps::default(),
            installed_at: created_at,
            content_hash: None,
            updated_at: 0,
        };
        let metadata = SkillBackupMetadata {
            skill,
            backup_created_at: created_at,
            source_path: format!("/tmp/{directory}"),
        };
        fs::write(
            backup_path.join("meta.json"),
            serde_json::to_string_pretty(&metadata).expect("serialize meta"),
        )
        .expect("write meta.json");
    }

    fn backup_ids(root: &Path) -> Vec<String> {
        let mut ids = fs::read_dir(root)
            .expect("read backup root")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }

    #[test]
    fn cleanup_keeps_three_generations_per_skill_independently() {
        // 关键回归：旧实现是全局「最多 20 个」，高频更新的 Skill 会把另一个
        // Skill 的备份挤掉。分组后两个 Skill 各自留满 3 代，互不影响。
        let root = tempdir().expect("tempdir");
        for i in 1..=5 {
            write_backup(root.path(), &format!("a{i}"), "skill-a", 1_000 + i, 16);
        }
        write_backup(root.path(), "b1", "skill-b", 1_001, 16);
        write_backup(root.path(), "b2", "skill-b", 1_002, 16);

        SkillService::cleanup_old_skill_backups(root.path()).expect("cleanup");

        // skill-a 只留最新 3 代（a3/a4/a5），skill-b 未超额度全留。
        assert_eq!(backup_ids(root.path()), vec!["a3", "a4", "a5", "b1", "b2"]);
    }

    #[test]
    fn cleanup_uses_metadata_timestamp_not_directory_mtime() {
        // 目录 mtime 不可靠：copy_dir_recursive 之后才写 meta.json，且恢复/拷贝
        // 都会重置 mtime。必须按 meta.json 的 backup_created_at 判定新旧，
        // 否则会删错代——把真正最新的备份当成最旧的删掉。
        let root = tempdir().expect("tempdir");
        // 写入顺序（即 mtime 顺序）与 created_at 顺序刻意相反。
        write_backup(root.path(), "newest", "skill-a", 9_000, 16);
        write_backup(root.path(), "middle", "skill-a", 5_000, 16);
        write_backup(root.path(), "older", "skill-a", 3_000, 16);
        write_backup(root.path(), "oldest", "skill-a", 1_000, 16);

        SkillService::cleanup_old_skill_backups(root.path()).expect("cleanup");

        assert_eq!(backup_ids(root.path()), vec!["middle", "newest", "older"]);
    }

    #[test]
    fn cleanup_groups_unreadable_metadata_backups_together() {
        // meta.json 缺失的目录在界面上既不可见也无法恢复。若按目录名各自成组，
        // 每组都只有 1 个、永远达不到 3 代上限 → 永不回收。归入同一孤儿组后
        // 同样受 3 代限制约束。
        let root = tempdir().expect("tempdir");
        for i in 1..=5 {
            let orphan = root.path().join(format!("orphan{i}"));
            fs::create_dir_all(orphan.join("skill")).expect("create orphan dir");
        }

        SkillService::cleanup_old_skill_backups(root.path()).expect("cleanup");

        assert_eq!(backup_ids(root.path()).len(), SKILL_BACKUP_RETAIN_PER_SKILL);
    }

    #[test]
    fn cleanup_enforces_total_size_limit_oldest_first() {
        // 3 个 Skill 各 1 代都在额度内，分组规则一个都不删；只有总体积上限
        // 会介入，且必须从最旧开始删。
        // 载荷取 100 KiB、上限 150 KiB：meta.json 的几百字节相对余量可忽略，
        // 断言不受其体积波动影响。
        let root = tempdir().expect("tempdir");
        let payload = 100 * 1024;
        let limit = 150 * 1024;
        write_backup(root.path(), "oldest", "skill-a", 1_000, payload);
        write_backup(root.path(), "middle", "skill-b", 2_000, payload);
        write_backup(root.path(), "newest", "skill-c", 3_000, 1_024);

        SkillService::cleanup_old_skill_backups_with_limit(root.path(), limit).expect("cleanup");

        // 删掉最旧的一个即回到上限内，middle 不该被连带删除。
        assert_eq!(backup_ids(root.path()), vec!["middle", "newest"]);
    }

    #[test]
    fn cleanup_never_deletes_the_last_remaining_backup() {
        // 单个备份自身超上限时也必须保留：刚更新完就把唯一备份删了，
        // 「更新前已备份」的承诺会静默失效，用户无从回退。
        let root = tempdir().expect("tempdir");
        write_backup(root.path(), "only", "skill-a", 1_000, 100 * 1024);

        SkillService::cleanup_old_skill_backups_with_limit(root.path(), 4_096).expect("cleanup");

        assert_eq!(backup_ids(root.path()), vec!["only"]);
    }
}
