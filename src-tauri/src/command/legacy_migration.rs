//! Explicit, copy-only import from the upstream application identity.
use super::{base::CommandResult, user_resumes::UserResumes};
use crate::{
    config::{self, AppRuntimeConfig, CURRENT_SCHEMA_VERSION, PRIMARY_LLM_ENTRY_ID},
    credential::{self, CredentialBackend, KeyringCredentialBackend},
    dao::model::*,
    error::AppError,
};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};
use tauri::Manager;

const LEGACY_ID: &str = "me.pgthinker.fj";
const MARKER: &str = "offerflow-legacy-import-completed.json";
const DATA_FILES: &[&str] = &[
    "job_details.json",
    "chat_messages.json",
    "interview_analyses.json",
    "job_profile_snapshots.json",
    "auto_reply_log.json",
    "manual_review.json",
    "user_resumes.json",
];
const IMPORT_NOTICE: &str = "原应用数据与凭证保持不变；浏览器登录状态、Cookie、WebView 与日志不复制。PDF/图片等外部附件只保留原路径，不复制文件；请确认原路径仍可访问。";

#[derive(Debug, Serialize)]
pub struct LegacyMigrationStatus {
    pub available: bool,
    pub eligible: bool,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct LegacyMigrationReport {
    pub message: String,
    pub credentials_migrated: usize,
}

struct Paths {
    source_config: PathBuf,
    source_data: PathBuf,
    target_config: PathBuf,
    target_data: PathBuf,
}

impl Paths {
    fn for_app(app: &tauri::AppHandle) -> Result<Self, AppError> {
        if app.config().identifier != "io.github.ye-feng0510.offerflow" {
            return Err(failure("应用标识不是 OfferFlow，拒绝迁移以保护原应用数据"));
        }
        // Tauri app_data_dir is data_dir/<identifier> (Roaming on Windows,
        // Application Support on macOS, XDG data on Linux), not local_data_dir.
        Ok(Self {
            source_config: app
                .path()
                .config_dir()
                .map_err(|_| failure("无法定位系统配置目录"))?
                .join(LEGACY_ID)
                .join("app_config.yaml"),
            source_data: app
                .path()
                .data_dir()
                .map_err(|_| failure("无法定位系统数据目录"))?
                .join(LEGACY_ID),
            target_config: config::config_path(app)?,
            target_data: app
                .path()
                .app_data_dir()
                .map_err(|_| failure("无法定位应用数据目录"))?,
        })
    }
}

fn failure(message: &str) -> AppError {
    AppError::storage(message)
}

/// Reject all link/reparse components, including a junction in an ancestor.
/// No path received from configuration is ever used as a copy source.
fn checked_exists(path: &Path) -> Result<bool, AppError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(failure("迁移路径必须是无跳转的绝对路径"));
    }
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                let mut linked = metadata.file_type().is_symlink();
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    linked |= metadata.file_attributes() & 0x400 != 0;
                }
                if linked {
                    return Err(failure("迁移路径包含符号链接或重解析点，已拒绝导入"));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(failure("无法检查迁移路径")),
        }
    }
    Ok(true)
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, AppError> {
    if !checked_exists(path)? {
        return Ok(None);
    }
    if !fs::metadata(path)
        .map_err(|_| failure("无法检查迁移文件"))?
        .is_file()
    {
        return Err(failure("迁移数据项不是普通文件"));
    }
    fs::read(path)
        .map(Some)
        .map_err(|_| failure("无法读取迁移文件"))
}

fn parse_config(bytes: &[u8]) -> Result<AppRuntimeConfig, AppError> {
    let raw: serde_yaml::Value =
        serde_yaml::from_slice(bytes).map_err(|_| failure("旧配置不是有效 YAML"))?;
    if !raw.is_mapping() {
        return Err(failure("旧配置必须是对象"));
    }
    if let Some(version) = raw.get("schema_version") {
        if version
            .as_u64()
            .is_none_or(|n| n > u64::from(CURRENT_SCHEMA_VERSION))
        {
            return Err(failure("旧配置版本无效或高于当前支持版本"));
        }
    }
    // Never return parser diagnostics that could contain a legacy plaintext key.
    config::parse_config_content(std::str::from_utf8(bytes).map_err(|_| failure("旧配置编码无效"))?)
        .map_err(|_| failure("旧配置结构或字段无效，未修改数据"))
}

fn validate_json(name: &str, bytes: &[u8]) -> Result<usize, AppError> {
    fn rows<T: DeserializeOwned>(bytes: &[u8]) -> Result<usize, serde_json::Error> {
        serde_json::from_slice::<Vec<T>>(bytes).map(|v| v.len())
    }
    let result = match name {
        "job_details.json" => rows::<JobDetail>(bytes),
        "chat_messages.json" => rows::<ChatMessageRecord>(bytes),
        "interview_analyses.json" => rows::<InterviewJobAnalysis>(bytes),
        "job_profile_snapshots.json" => rows::<JobProfileSnapshot>(bytes),
        "auto_reply_log.json" => rows::<AutoReplyLogRecord>(bytes),
        "manual_review.json" => rows::<ManualReviewRecord>(bytes),
        "user_resumes.json" => serde_json::from_slice::<UserResumes>(bytes).map(|v| v.len()),
        _ => return Err(failure("不支持的迁移文件")),
    };
    result.map_err(|_| failure(&format!("迁移文件 {name} 结构无效，未修改数据")))
}

fn ensure_eligible(paths: &Paths, tasks_active: bool) -> Result<(), AppError> {
    if tasks_active {
        return Err(failure("请先停止所有运行中或排队中的任务"));
    }
    if checked_exists(&paths.target_data.join(MARKER))? {
        return Err(failure("已完成旧数据导入，不能重复覆盖"));
    }
    // Missing config is allowed only for injected/new-install fixtures.
    if let Some(bytes) = read_optional(&paths.target_config)? {
        if parse_config(&bytes)?.onboarding_completed {
            return Err(failure("仅未完成首次设置的新安装可导入旧数据"));
        }
    }
    let directory = paths.target_data.join("data");
    if checked_exists(&directory)? {
        for entry in fs::read_dir(&directory).map_err(|_| failure("无法检查目标数据目录"))?
        {
            let path = entry.map_err(|_| failure("无法检查目标数据目录"))?.path();
            let name = path.file_name().and_then(|v| v.to_str()).unwrap_or("");
            if !DATA_FILES.contains(&name) {
                return Err(failure("目标目录已有未知数据，拒绝覆盖"));
            }
            let bytes = read_optional(&path)?.ok_or_else(|| failure("目标文件发生变化"))?;
            if validate_json(name, &bytes)? != 0 {
                return Err(failure("目标应用已有业务数据，拒绝覆盖"));
            }
        }
    }
    if checked_exists(&paths.target_data.join("user_resumes.json"))? {
        return Err(failure("目标应用已有旧格式简历文件，拒绝覆盖"));
    }
    Ok(())
}

struct Prepared {
    files: Vec<(PathBuf, Vec<u8>)>,
    entry_ids: Vec<String>,
}

fn prepare(paths: &Paths) -> Result<Prepared, AppError> {
    checked_exists(&paths.source_data)?;
    checked_exists(&paths.source_data.join("data"))?;
    checked_exists(&paths.target_data)?;
    let bytes = read_optional(&paths.source_config)?.ok_or_else(|| failure("未找到旧应用配置"))?;
    let mut config = parse_config(&bytes)?;
    let mut entry_ids = vec![PRIMARY_LLM_ENTRY_ID.to_string()];
    entry_ids.extend(config.llm_fallbacks.iter().map(|entry| entry.id.clone()));
    config.schema_version = CURRENT_SCHEMA_VERSION;
    config.browser_config.user_data_dir = paths
        .target_data
        .join("browser-profile")
        .to_string_lossy()
        .into_owned();
    checked_exists(&paths.target_data.join("browser-profile"))?;
    config::normalize_loaded_config(&mut config).map_err(|_| failure("旧配置无法规范化"))?;
    let mut files = vec![(
        paths.target_config.clone(),
        serde_yaml::to_string(&config)
            .map_err(|_| failure("无法序列化导入配置"))?
            .into_bytes(),
    )];
    for name in DATA_FILES {
        let modern = read_optional(&paths.source_data.join("data").join(name))?;
        let content = if *name == "user_resumes.json" {
            let legacy = read_optional(&paths.source_data.join(name))?;
            match (legacy, modern) {
                (Some(old), Some(new)) => {
                    validate_json(name, &old)?;
                    validate_json(name, &new)?;
                    let old_value: serde_json::Value = serde_json::from_slice(&old).unwrap();
                    let new_value: serde_json::Value = serde_json::from_slice(&new).unwrap();
                    if old_value != new_value {
                        return Err(failure(
                            "旧应用有两份不同的简历数据，请先在旧应用中完成数据迁移",
                        ));
                    }
                    Some(new)
                }
                (old, new) => new.or(old),
            }
        } else {
            modern
        };
        if let Some(content) = content {
            validate_json(name, &content)?;
            files.push((paths.target_data.join("data").join(name), content));
        }
    }
    // Last promoted file is the completion marker. Failed imports never keep it.
    files.push((
        paths.target_data.join(MARKER),
        br#"{"source":"me.pgthinker.fj","version":1}"#.to_vec(),
    ));
    Ok(Prepared { files, entry_ids })
}

struct Staged {
    target: PathBuf,
    staged: Option<tempfile::NamedTempFile>,
    backup: Option<PathBuf>,
    promoted: bool,
    // Owns backup storage until rollback succeeds or commit completes.
    workspace: tempfile::TempDir,
}

/// Staging and backups live on each target's filesystem. Every promotion is an
/// atomic rename; on a recoverable failure all prior promotions are reversed.
fn commit_files(files: &[(PathBuf, Vec<u8>)]) -> Result<(), AppError> {
    commit_files_with(files, |_| Ok(()))
}

fn commit_files_with(
    files: &[(PathBuf, Vec<u8>)],
    before_promote: impl Fn(usize) -> Result<(), AppError>,
) -> Result<(), AppError> {
    let mut staged = Vec::new();
    for (target, bytes) in files {
        checked_exists(target)?;
        let parent = target.parent().ok_or_else(|| failure("目标路径无效"))?;
        fs::create_dir_all(parent).map_err(|_| failure("无法创建目标目录"))?;
        let workspace = tempfile::Builder::new()
            .prefix(".offerflow-import-")
            .tempdir_in(parent)
            .map_err(|_| failure("无法创建迁移暂存目录"))?;
        let mut file = tempfile::NamedTempFile::new_in(workspace.path())
            .map_err(|_| failure("无法创建迁移暂存文件"))?;
        file.write_all(bytes)
            .and_then(|_| file.as_file().sync_all())
            .map_err(|_| failure("无法写入迁移暂存文件"))?;
        staged.push(Staged {
            target: target.clone(),
            staged: Some(file),
            backup: None,
            promoted: false,
            workspace,
        });
    }
    let result = (|| {
        for (index, item) in staged.iter_mut().enumerate() {
            before_promote(index)?;
            if checked_exists(&item.target)? {
                let backup = item.workspace.path().join("original");
                fs::rename(&item.target, &backup).map_err(|_| failure("无法暂存原目标文件"))?;
                item.backup = Some(backup);
            }
            item.staged
                .take()
                .unwrap()
                .persist_noclobber(&item.target)
                .map_err(|_| failure("无法原子提交迁移文件"))?;
            item.promoted = true;
        }
        Ok(())
    })();
    if result.is_err() {
        let mut rollback_failed = false;
        for item in staged.iter_mut().rev() {
            let removed = !item.promoted || fs::remove_file(&item.target).is_ok();
            let restored = match &item.backup {
                Some(backup) if removed => fs::rename(backup, &item.target).is_ok(),
                Some(_) => false,
                None => true,
            };
            if !removed || !restored {
                rollback_failed = true;
            }
        }
        if rollback_failed {
            // Keep original backups for recovery rather than deleting user data.
            for item in staged {
                let _ = item.workspace.keep();
            }
            return Err(failure("导入失败且文件回滚未完成；原文件保留在目标目录的 .offerflow-import-* 暂存目录，请恢复后重试"));
        }
    }
    result
}

fn tasks_active() -> bool {
    let status = crate::task::JOB_TASK_MANAGER.overview();
    status.running_count != 0 || status.queued_count != 0
}

#[tauri::command]
pub fn inspect_legacy_migration(
    app_handle: tauri::AppHandle,
) -> CommandResult<LegacyMigrationStatus> {
    let _permit = crate::storage::write_lock();
    let inspect = || -> Result<LegacyMigrationStatus, AppError> {
        let paths = Paths::for_app(&app_handle)?;
        let available = checked_exists(&paths.source_config)?;
        let validation = ensure_eligible(&paths, tasks_active()).and_then(|_| {
            if available {
                prepare(&paths).map(|_| ())
            } else {
                Err(failure("未找到旧应用数据"))
            }
        });
        Ok(LegacyMigrationStatus {
            available,
            eligible: validation.is_ok(),
            message: match validation {
                Ok(()) => format!("可以手动导入旧应用数据。{IMPORT_NOTICE}"),
                Err(error) => error.message,
            },
        })
    };
    match inspect() {
        Ok(status) => CommandResult::ok(status),
        Err(error) => CommandResult::err(error),
    }
}

#[tauri::command]
pub fn migrate_legacy_data(
    app_handle: tauri::AppHandle,
    confirmed: bool,
) -> CommandResult<LegacyMigrationReport> {
    if !confirmed {
        return CommandResult::err(AppError::validation("请先确认导入旧应用数据"));
    }
    let _permit = crate::storage::write_lock();
    let migrate = || -> Result<LegacyMigrationReport, AppError> {
        let paths = Paths::for_app(&app_handle)?;
        ensure_eligible(&paths, tasks_active())?;
        let prepared = prepare(&paths)?;
        let backends: Vec<_> = prepared
            .entry_ids
            .iter()
            .map(|id| {
                (
                    KeyringCredentialBackend::legacy_for_entry(id),
                    KeyringCredentialBackend::for_entry(id),
                )
            })
            .collect();
        let pairs: Vec<(&dyn CredentialBackend, &dyn CredentialBackend)> = backends
            .iter()
            .map(|(old, new)| (old as &dyn CredentialBackend, new as &dyn CredentialBackend))
            .collect();
        let (_, count) = credential::import_with_backends(&pairs, || {
            ensure_eligible(&paths, tasks_active())?;
            commit_files(&prepared.files)
        })?;
        Ok(LegacyMigrationReport {
            message: format!("导入完成。{IMPORT_NOTICE} 仅复制系统凭证库中的密钥；旧配置中的明文密钥不会复制，请按需重新设置。"),
            credentials_migrated: count,
        })
    };
    match migrate() {
        Ok(report) => CommandResult::ok(report),
        Err(error) => CommandResult::err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn promotion_failure_restores_originals_and_removes_only_created_files() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("original");
        let created = root.path().join("created");
        let marker = root.path().join(MARKER);
        fs::write(&original, b"original").unwrap();
        let files = vec![
            (original.clone(), b"new".to_vec()),
            (created.clone(), b"created".to_vec()),
            (marker.clone(), b"marker".to_vec()),
        ];
        assert!(commit_files_with(&files, |index| {
            if index == 2 {
                Err(failure("injected failure"))
            } else {
                Ok(())
            }
        })
        .is_err());
        assert_eq!(fs::read(original).unwrap(), b"original");
        assert!(!created.exists());
        assert!(!marker.exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_sources_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let paths = fixture(root.path());
        let outside = root.path().join("outside.json");
        fs::write(&outside, b"[]").unwrap();
        std::os::unix::fs::symlink(&outside, paths.source_data.join("data/job_details.json"))
            .unwrap();
        assert!(prepare(&paths).is_err());
    }

    fn fixture(root: &Path) -> Paths {
        let paths = Paths {
            source_config: root.join("old-config/app_config.yaml"),
            source_data: root.join("old-data"),
            target_config: root.join("new-config/app_config.yaml"),
            target_data: root.join("new-data"),
        };
        for path in [&paths.source_config, &paths.target_config] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                path,
                serde_yaml::to_string(&config::default_app_config()).unwrap(),
            )
            .unwrap();
        }
        fs::create_dir_all(paths.source_data.join("data")).unwrap();
        fs::create_dir_all(paths.target_data.join("data")).unwrap();
        paths
    }

    #[test]
    fn imports_only_allowed_data_and_resets_custom_browser_path() {
        let root = tempfile::tempdir().unwrap();
        let paths = fixture(root.path());
        let mut old = config::default_app_config();
        old.browser_config.user_data_dir = "C:/private-old-profile".into();
        fs::write(&paths.source_config, serde_yaml::to_string(&old).unwrap()).unwrap();
        let original = fs::read(&paths.source_config).unwrap();
        fs::write(
            paths.source_data.join("data/user_resumes.json"),
            br#"{"resume":{"content":"text","thumbnail":null}}"#,
        )
        .unwrap();
        fs::write(paths.source_data.join("cookies"), "private").unwrap();
        ensure_eligible(&paths, false).unwrap();
        let plan = prepare(&paths).unwrap();
        commit_files(&plan.files).unwrap();
        assert_eq!(fs::read(&paths.source_config).unwrap(), original);
        assert!(paths.source_data.join("data/user_resumes.json").exists());
        assert!(!paths.target_data.join("cookies").exists());
        let new = parse_config(&fs::read(&paths.target_config).unwrap()).unwrap();
        assert_eq!(
            new.browser_config.user_data_dir,
            paths.target_data.join("browser-profile").to_string_lossy()
        );
        assert!(ensure_eligible(&paths, false).is_err());
    }

    #[test]
    fn malformed_source_and_future_schema_are_rejected_without_writes() {
        let root = tempfile::tempdir().unwrap();
        let paths = fixture(root.path());
        let original = fs::read(&paths.target_config).unwrap();
        fs::write(paths.source_data.join("data/job_details.json"), b"{}").unwrap();
        assert!(prepare(&paths).is_err());
        assert!(parse_config(b"schema_version: 999").is_err());
        assert_eq!(fs::read(&paths.target_config).unwrap(), original);
        assert!(!paths.target_data.join(MARKER).exists());
    }

    #[test]
    fn existing_data_onboarding_and_active_tasks_refuse_import() {
        let root = tempfile::tempdir().unwrap();
        let paths = fixture(root.path());
        assert!(ensure_eligible(&paths, true).is_err());
        let mut config = config::default_app_config();
        config.onboarding_completed = true;
        fs::write(
            &paths.target_config,
            serde_yaml::to_string(&config).unwrap(),
        )
        .unwrap();
        assert!(ensure_eligible(&paths, false).is_err());
        config.onboarding_completed = false;
        fs::write(
            &paths.target_config,
            serde_yaml::to_string(&config).unwrap(),
        )
        .unwrap();
        fs::write(
            paths.target_data.join("data/user_resumes.json"),
            br#"{"existing":{"content":"safe"}}"#,
        )
        .unwrap();
        assert!(ensure_eligible(&paths, false).is_err());
    }

    #[test]
    fn conflicting_resume_locations_and_traversal_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let paths = fixture(root.path());
        fs::write(
            paths.source_data.join("user_resumes.json"),
            br#"{"old":{"content":"text"}}"#,
        )
        .unwrap();
        fs::write(paths.source_data.join("data/user_resumes.json"), b"{}").unwrap();
        assert!(prepare(&paths).is_err());
        assert!(checked_exists(&root.path().join("../outside")).is_err());
    }
}
