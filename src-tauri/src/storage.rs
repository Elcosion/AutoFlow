use crate::behavior::v2::{
    BehaviorProfileV2, BehaviorProfileV2File, BehaviorSessionFile, BehaviorSessionV2,
    SourceRetention,
};
use crate::behavior::{BehaviorProfile, BehaviorProfileFile, BiomimeticInput, BiomimeticInputFile};
use crate::{AppConfig, AppError};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

const BEHAVIOR_PROFILE_DIRECTORY: &str = "behavior-profiles";
const BIOMIMETIC_INPUT_DIRECTORY: &str = "biomimetic-inputs";
const BEHAVIOR_SESSION_V2_DIRECTORY: &str = "behavior-sessions-v2";
const BEHAVIOR_PROFILE_V2_DIRECTORY: &str = "behavior-profiles-v2";

pub fn load_config(app: &AppHandle) -> Result<AppConfig, AppError> {
    let path = config_path(app)?;
    if !path.exists() {
        let config = AppConfig::default();
        save_config(app, &config)?;
        return Ok(config);
    }

    let content = fs::read_to_string(&path).map_err(|error| {
        AppError::with_detail(
            "config_read_failed",
            "配置读取失败，请检查 AutoFlow 的本地数据目录权限",
            error.to_string(),
        )
    })?;

    match serde_json::from_str::<AppConfig>(&content) {
        Ok(config) => {
            let (mut config, migrated) = config.migrate()?;
            let inline_profiles = !config.behavior_profiles.is_empty();
            let inline_inputs = !config.biomimetic_inputs.is_empty();
            let inline_sessions_v2 = !config.behavior_sessions_v2.is_empty();
            let inline_profiles_v2 = !config.behavior_profiles_v2.is_empty();
            let (profiles, profile_files, missing_profiles) = if inline_profiles {
                let files = sync_behavior_profiles(app, &config.behavior_profiles)?;
                (config.behavior_profiles.clone(), files, false)
            } else {
                load_behavior_profiles(app, &config.behavior_profile_files)?
            };
            let (inputs, input_files, missing_inputs) = if inline_inputs {
                let files = sync_biomimetic_inputs(app, &config.biomimetic_inputs)?;
                (config.biomimetic_inputs.clone(), files, false)
            } else {
                load_biomimetic_inputs(app, &config.biomimetic_input_files)?
            };
            let (sessions_v2, session_files_v2, missing_sessions_v2) = if inline_sessions_v2 {
                let files = sync_behavior_sessions_v2(app, &config.behavior_sessions_v2)?;
                (config.behavior_sessions_v2.clone(), files, false)
            } else {
                load_behavior_sessions_v2(app, &config.behavior_session_files_v2)?
            };
            let (profiles_v2, profile_files_v2, mut missing_profiles_v2) = if inline_profiles_v2 {
                let files = sync_behavior_profiles_v2(app, &config.behavior_profiles_v2)?;
                (config.behavior_profiles_v2.clone(), files, false)
            } else {
                load_behavior_profiles_v2(app, &config.behavior_profile_v2_files)?
            };
            config.behavior_profiles = profiles;
            config.behavior_profile_files = profile_files;
            config.biomimetic_inputs = inputs;
            config.biomimetic_input_files = input_files;
            config.behavior_sessions_v2 = sessions_v2;
            config.behavior_session_files_v2 = session_files_v2;
            config.behavior_profiles_v2 = profiles_v2;
            config.behavior_profile_v2_files = profile_files_v2;
            missing_profiles_v2 |= reconcile_v2_references(&mut config);
            let profile_ids = config
                .behavior_profiles
                .iter()
                .map(|profile| profile.id.as_str())
                .collect::<HashSet<_>>();
            config
                .selected_behavior_profile_ids
                .retain(|id| profile_ids.contains(id.as_str()));
            if config
                .active_behavior_profile_id
                .as_ref()
                .is_some_and(|id| !profile_ids.contains(id.as_str()))
            {
                config.active_behavior_profile_id =
                    config.selected_behavior_profile_ids.first().cloned();
            }
            let input_ids = config
                .biomimetic_inputs
                .iter()
                .map(|input| input.id.as_str())
                .collect::<HashSet<_>>();
            config
                .selected_biomimetic_input_ids
                .retain(|id| input_ids.contains(id.as_str()));
            config.validate()?;
            if migrated
                || inline_profiles
                || inline_inputs
                || inline_sessions_v2
                || inline_profiles_v2
                || missing_profiles
                || missing_inputs
                || missing_sessions_v2
                || missing_profiles_v2
            {
                save_config(app, &config)?;
            }
            Ok(config)
        }
        Err(error) => {
            let backup_path = path.with_extension("json.corrupt");
            let _ = fs::copy(&path, backup_path);
            let config = AppConfig::default();
            save_config(app, &config)?;
            log::warn!("配置损坏，已切换到安全默认配置: {error}");
            Ok(config)
        }
    }
}

pub fn save_config(app: &AppHandle, config: &AppConfig) -> Result<(), AppError> {
    config.validate()?;
    let behavior_profile_files = sync_behavior_profiles(app, &config.behavior_profiles)?;
    let biomimetic_input_files = sync_biomimetic_inputs(app, &config.biomimetic_inputs)?;
    let behavior_session_files_v2 = sync_behavior_sessions_v2(app, &config.behavior_sessions_v2)?;
    let behavior_profile_v2_files = sync_behavior_profiles_v2(app, &config.behavior_profiles_v2)?;
    let path = config_path(app)?;
    let directory = path
        .parent()
        .ok_or_else(|| AppError::internal("配置目录解析失败，请重启 AutoFlow"))?;
    fs::create_dir_all(directory).map_err(|error| {
        AppError::with_detail(
            "config_directory_failed",
            "无法创建 AutoFlow 配置目录，请检查本地权限",
            error.to_string(),
        )
    })?;

    let mut persisted = config.clone();
    persisted.behavior_profiles.clear();
    persisted.behavior_profile_files = behavior_profile_files;
    persisted.biomimetic_inputs.clear();
    persisted.biomimetic_input_files = biomimetic_input_files;
    persisted.behavior_sessions_v2.clear();
    persisted.behavior_session_files_v2 = behavior_session_files_v2;
    persisted.behavior_profiles_v2.clear();
    persisted.behavior_profile_v2_files = behavior_profile_v2_files;
    let content = serde_json::to_string_pretty(&persisted).map_err(|error| {
        AppError::with_detail("config_encode_failed", "配置序列化失败", error.to_string())
    })?;
    write_json_file(
        &path,
        &content,
        "config_write_failed",
        "配置写入失败，请检查磁盘空间和权限",
    )?;
    Ok(())
}

fn load_behavior_profiles(
    app: &AppHandle,
    entries: &[BehaviorProfileFile],
) -> Result<(Vec<BehaviorProfile>, Vec<BehaviorProfileFile>, bool), AppError> {
    let root = behavior_directory(app, BEHAVIOR_PROFILE_DIRECTORY)?;
    let mut profiles = Vec::new();
    let mut valid_entries = Vec::new();
    let mut missing = false;
    for entry in entries {
        let path = root.join(profile_file_name(&entry.id));
        if !path.exists() {
            missing = true;
            log::warn!("行为档案文件不存在，已从索引移除: {}", entry.id);
            continue;
        }
        match read_json_file::<BehaviorProfile>(&path) {
            Ok(profile) if profile.id == entry.id && profile.validate().is_ok() => {
                profiles.push(profile);
                valid_entries.push(entry.clone());
            }
            Ok(_) | Err(_) => {
                missing = true;
                log::warn!("行为档案文件无效，已从索引移除: {}", entry.id);
            }
        }
    }
    Ok((profiles, valid_entries, missing))
}

fn load_biomimetic_inputs(
    app: &AppHandle,
    entries: &[BiomimeticInputFile],
) -> Result<(Vec<BiomimeticInput>, Vec<BiomimeticInputFile>, bool), AppError> {
    let root = behavior_directory(app, BIOMIMETIC_INPUT_DIRECTORY)?;
    let mut inputs = Vec::new();
    let mut valid_entries = Vec::new();
    let mut missing = false;
    for entry in entries {
        let path = root.join(input_file_name(&entry.id));
        if !path.exists() {
            missing = true;
            log::warn!("仿生输入文件不存在，已从索引移除: {}", entry.id);
            continue;
        }
        match read_json_file::<BiomimeticInput>(&path) {
            Ok(input) if input.id == entry.id && input.validate().is_ok() => {
                inputs.push(input);
                valid_entries.push(entry.clone());
            }
            Ok(_) | Err(_) => {
                missing = true;
                log::warn!("仿生输入文件无效，已从索引移除: {}", entry.id);
            }
        }
    }
    Ok((inputs, valid_entries, missing))
}

fn sync_behavior_profiles(
    app: &AppHandle,
    profiles: &[BehaviorProfile],
) -> Result<Vec<BehaviorProfileFile>, AppError> {
    let root = behavior_directory(app, BEHAVIOR_PROFILE_DIRECTORY)?;
    let mut entries = Vec::with_capacity(profiles.len());
    let mut expected = HashSet::new();
    for profile in profiles {
        profile.validate()?;
        let file_name = profile_file_name(&profile.id);
        let path = root.join(&file_name);
        if !path.exists() {
            let content = serde_json::to_string_pretty(profile).map_err(|error| {
                AppError::with_detail(
                    "behavior_profile_encode_failed",
                    "行为档案序列化失败",
                    error.to_string(),
                )
            })?;
            write_json_file(
                &path,
                &content,
                "behavior_profile_write_failed",
                "行为档案文件写入失败，请检查磁盘空间和权限",
            )?;
        }
        expected.insert(file_name.clone());
        entries.push(BehaviorProfileFile::from_profile(profile, file_name));
    }
    remove_stale_json_files(&root, &expected);
    Ok(entries)
}

fn sync_biomimetic_inputs(
    app: &AppHandle,
    inputs: &[BiomimeticInput],
) -> Result<Vec<BiomimeticInputFile>, AppError> {
    let root = behavior_directory(app, BIOMIMETIC_INPUT_DIRECTORY)?;
    let mut entries = Vec::with_capacity(inputs.len());
    let mut expected = HashSet::new();
    for input in inputs {
        input.validate()?;
        let file_name = input_file_name(&input.id);
        let path = root.join(&file_name);
        if !path.exists() {
            let content = serde_json::to_string_pretty(input).map_err(|error| {
                AppError::with_detail(
                    "biomimetic_input_encode_failed",
                    "仿生输入序列化失败",
                    error.to_string(),
                )
            })?;
            write_json_file(
                &path,
                &content,
                "biomimetic_input_write_failed",
                "仿生输入文件写入失败，请检查磁盘空间和权限",
            )?;
        }
        expected.insert(file_name.clone());
        entries.push(BiomimeticInputFile::from_input(input, file_name));
    }
    remove_stale_json_files(&root, &expected);
    Ok(entries)
}

fn load_behavior_sessions_v2(
    app: &AppHandle,
    entries: &[BehaviorSessionFile],
) -> Result<(Vec<BehaviorSessionV2>, Vec<BehaviorSessionFile>, bool), AppError> {
    let root = behavior_directory(app, BEHAVIOR_SESSION_V2_DIRECTORY)?;
    let mut sessions = Vec::new();
    let mut valid_entries = Vec::new();
    let mut missing = false;
    for entry in entries {
        let path = root.join(&entry.file_name);
        if !path.exists() {
            missing = true;
            continue;
        }
        match read_json_file_with_backup::<BehaviorSessionV2>(&path) {
            Ok((session, recovered)) if session.id == entry.id && session.validate().is_ok() => {
                sessions.push(session);
                valid_entries.push(entry.clone());
                missing |= recovered;
            }
            Ok(_) | Err(_) => {
                missing = true;
            }
        }
    }
    Ok((sessions, valid_entries, missing))
}

fn load_behavior_profiles_v2(
    app: &AppHandle,
    entries: &[BehaviorProfileV2File],
) -> Result<(Vec<BehaviorProfileV2>, Vec<BehaviorProfileV2File>, bool), AppError> {
    let root = behavior_directory(app, BEHAVIOR_PROFILE_V2_DIRECTORY)?;
    let mut profiles = Vec::new();
    let mut valid_entries = Vec::new();
    let mut missing = false;
    for entry in entries {
        let path = root.join(&entry.file_name);
        if !path.exists() {
            missing = true;
            continue;
        }
        match read_json_file_with_backup::<BehaviorProfileV2>(&path) {
            Ok((profile, recovered)) if profile.id == entry.id && profile.validate().is_ok() => {
                profiles.push(profile);
                valid_entries.push(entry.clone());
                missing |= recovered;
            }
            Ok(_) | Err(_) => {
                missing = true;
            }
        }
    }
    Ok((profiles, valid_entries, missing))
}

fn sync_behavior_sessions_v2(
    app: &AppHandle,
    sessions: &[BehaviorSessionV2],
) -> Result<Vec<BehaviorSessionFile>, AppError> {
    let root = behavior_directory(app, BEHAVIOR_SESSION_V2_DIRECTORY)?;
    let mut entries = Vec::with_capacity(sessions.len());
    for session in sessions {
        session.validate()?;
        let file_name = session_file_name(&session.id);
        let path = root.join(&file_name);
        if !path.exists() {
            let content = serde_json::to_string_pretty(session).map_err(|error| {
                AppError::with_detail(
                    "behavior_v2_session_encode_failed",
                    "V2 训练会话序列化失败",
                    error.to_string(),
                )
            })?;
            write_json_file(
                &path,
                &content,
                "behavior_v2_session_write_failed",
                "V2 训练会话文件写入失败",
            )?;
        }
        entries.push(BehaviorSessionFile::from_session(session, file_name));
    }
    Ok(entries)
}

fn sync_behavior_profiles_v2(
    app: &AppHandle,
    profiles: &[BehaviorProfileV2],
) -> Result<Vec<BehaviorProfileV2File>, AppError> {
    let root = behavior_directory(app, BEHAVIOR_PROFILE_V2_DIRECTORY)?;
    let mut entries = Vec::with_capacity(profiles.len());
    for profile in profiles {
        profile.validate()?;
        let file_name = profile_v2_file_name(&profile.id);
        let path = root.join(&file_name);
        let content = serde_json::to_string_pretty(profile).map_err(|error| {
            AppError::with_detail(
                "behavior_v2_profile_encode_failed",
                "V2 行为档案序列化失败",
                error.to_string(),
            )
        })?;
        // Retraining and source-retention changes update an existing profile;
        // the atomic writer preserves the previous JSON as .bak.
        write_json_file(
            &path,
            &content,
            "behavior_v2_profile_write_failed",
            "V2 行为档案文件写入失败",
        )?;
        entries.push(BehaviorProfileV2File::from_profile(profile, file_name));
    }
    Ok(entries)
}

pub fn delete_behavior_session_v2(app: &AppHandle, id: &str) -> Result<(), AppError> {
    let root = behavior_directory(app, BEHAVIOR_SESSION_V2_DIRECTORY)?;
    delete_behavior_file(
        &root,
        &session_file_name(id),
        "behavior_v2_session_delete_failed",
        "V2 训练会话文件删除失败",
    )
}

pub fn delete_behavior_profile_v2(app: &AppHandle, id: &str) -> Result<(), AppError> {
    let root = behavior_directory(app, BEHAVIOR_PROFILE_V2_DIRECTORY)?;
    delete_behavior_file(
        &root,
        &profile_v2_file_name(id),
        "behavior_v2_profile_delete_failed",
        "V2 行为档案文件删除失败",
    )
}

fn behavior_directory(app: &AppHandle, name: &str) -> Result<PathBuf, AppError> {
    let directory = app.path().app_data_dir().map_err(|error| {
        AppError::with_detail(
            "behavior_directory_failed",
            "无法定位 AutoFlow 行为训练目录",
            error.to_string(),
        )
    })?;
    let directory = directory.join(name);
    fs::create_dir_all(&directory).map_err(|error| {
        AppError::with_detail(
            "behavior_directory_failed",
            "无法创建 AutoFlow 行为训练目录，请检查本地权限",
            error.to_string(),
        )
    })?;
    Ok(directory)
}

fn read_json_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, AppError> {
    let content = fs::read_to_string(path).map_err(|error| {
        AppError::with_detail(
            "behavior_file_read_failed",
            "行为训练文件读取失败",
            error.to_string(),
        )
    })?;
    serde_json::from_str(&content).map_err(|error| {
        AppError::with_detail(
            "behavior_file_decode_failed",
            "行为训练文件格式无效",
            error.to_string(),
        )
    })
}

/// Read a V2 data file and fall back to its exact `.bak` sibling when the
/// formal file is missing or unreadable after an interrupted replacement.
/// The boolean tells the caller to persist the recovered value back to the
/// formal path on the next config sync.
fn read_json_file_with_backup<T: serde::de::DeserializeOwned>(
    path: &Path,
) -> Result<(T, bool), AppError> {
    match read_json_file(path) {
        Ok(value) => Ok((value, false)),
        Err(primary_error) => {
            let backup_path = path.with_extension("json.bak");
            if !backup_path.exists() {
                return Err(primary_error);
            }
            match read_json_file(&backup_path) {
                Ok(value) => {
                    log::warn!(
                        "V2 行为文件正式路径不可读，已使用精确备份恢复: {}",
                        path.display()
                    );
                    Ok((value, true))
                }
                Err(_) => Err(primary_error),
            }
        }
    }
}

fn write_json_file(path: &Path, content: &str, code: &str, message: &str) -> Result<(), AppError> {
    let temp_path = path.with_extension("json.tmp");
    if let Err(error) = fs::write(&temp_path, content) {
        let _ = fs::remove_file(&temp_path);
        return Err(AppError::with_detail(code, message, error.to_string()));
    }
    if !path.exists() {
        return match fs::rename(&temp_path, path) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = fs::remove_file(&temp_path);
                Err(AppError::with_detail(code, message, error.to_string()))
            }
        };
    }

    let backup_path = path.with_extension("json.bak");
    if backup_path.exists() {
        if let Err(error) = fs::remove_file(&backup_path) {
            let _ = fs::remove_file(&temp_path);
            return Err(AppError::with_detail(code, message, error.to_string()));
        }
    }
    if let Err(error) = fs::rename(path, &backup_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(AppError::with_detail(code, message, error.to_string()));
    }
    match fs::rename(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::rename(&backup_path, path);
            let _ = fs::remove_file(&temp_path);
            Err(AppError::with_detail(code, message, error.to_string()))
        }
    }
}

fn delete_behavior_file(
    root: &Path,
    file_name: &str,
    code: &str,
    message: &str,
) -> Result<(), AppError> {
    let path = root.join(file_name);
    let backup_path = path.with_extension("json.bak");
    for candidate in [path, backup_path] {
        if candidate.exists() {
            fs::remove_file(&candidate)
                .map_err(|error| AppError::with_detail(code, message, error.to_string()))?;
        }
    }
    Ok(())
}

fn remove_stale_json_files(root: &Path, expected: &HashSet<String>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.extension().and_then(|extension| extension.to_str()) == Some("json")
            && !expected.contains(file_name)
        {
            let _ = fs::remove_file(path);
        }
    }
}

fn profile_file_name(id: &str) -> String {
    format!("profile-{}.json", stable_id_hash(id))
}

fn input_file_name(id: &str) -> String {
    format!("input-{}.json", stable_id_hash(id))
}

fn session_file_name(id: &str) -> String {
    format!("session-{}.json", stable_id_hash(id))
}

fn profile_v2_file_name(id: &str) -> String {
    format!("profile-{}.json", stable_id_hash(id))
}

fn stable_id_hash(id: &str) -> String {
    let digest = Sha256::digest(id.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn config_path(app: &AppHandle) -> Result<PathBuf, AppError> {
    let directory = app.path().app_config_dir().map_err(|error| {
        AppError::with_detail(
            "config_path_failed",
            "无法定位 AutoFlow 本地配置目录",
            error.to_string(),
        )
    })?;
    Ok(directory.join("config.json"))
}

fn reconcile_v2_references(config: &mut AppConfig) -> bool {
    let mut changed = false;
    let v2_session_ids = config
        .behavior_sessions_v2
        .iter()
        .map(|session| session.id.as_str())
        .collect::<HashSet<_>>();
    for profile in &mut config.behavior_profiles_v2 {
        if profile.source_retention == SourceRetention::Persisted
            && !profile
                .source_session_ids
                .iter()
                .any(|id| v2_session_ids.contains(id.as_str()))
        {
            // A legacy profile or a damaged index may not have a source
            // Session on disk. Keep the model usable, but do not claim
            // that its raw source is still retained.
            profile.source_retention = SourceRetention::Ephemeral;
            changed = true;
        }
    }
    let v2_profile_ids = config
        .behavior_profiles_v2
        .iter()
        .map(|profile| profile.id.as_str())
        .collect::<HashSet<_>>();
    if config
        .active_behavior_profile_v2_id
        .as_ref()
        .is_some_and(|id| !v2_profile_ids.contains(id.as_str()))
    {
        config.active_behavior_profile_v2_id = config
            .behavior_profiles_v2
            .first()
            .map(|profile| profile.id.clone());
        changed = true;
    } else if config.active_behavior_profile_v2_id.is_none() {
        config.active_behavior_profile_v2_id = config
            .behavior_profiles_v2
            .first()
            .map(|profile| profile.id.clone());
        if config.active_behavior_profile_v2_id.is_some() {
            changed = true;
        }
    }
    if config
        .behavior_policy
        .profile_id
        .as_ref()
        .is_some_and(|id| !v2_profile_ids.contains(id.as_str()))
    {
        config.behavior_policy.profile_id = None;
        changed = true;
    }
    for macro_rule in &mut config.macros {
        if macro_rule
            .behavior_policy
            .as_ref()
            .and_then(|policy| policy.profile_id.as_ref())
            .is_some_and(|id| !v2_profile_ids.contains(id.as_str()))
        {
            macro_rule.behavior_policy = None;
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::v2::{
        train_behavior_profile_with_retention, BehaviorCaptureMetadata, BehaviorSessionV2,
        SourceRetention,
    };
    use crate::behavior::BehaviorEvent;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(1);

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "autoflow-storage-{name}-{}-{}",
            std::process::id(),
            TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn test_session() -> BehaviorSessionV2 {
        BehaviorSessionV2 {
            id: "storage-session".to_string(),
            name: "storage test".to_string(),
            api_version: crate::BEHAVIOR_V2_API_VERSION,
            created_at_ms: 1,
            duration_ms: 100,
            task_tag: None,
            capture_metadata: BehaviorCaptureMetadata::default(),
            raw_events: vec![
                BehaviorEvent::MouseMove {
                    timestamp_ms: 0,
                    x: 0,
                    y: 0,
                },
                BehaviorEvent::MouseMove {
                    timestamp_ms: 50,
                    x: 20,
                    y: 0,
                },
                BehaviorEvent::MouseMove {
                    timestamp_ms: 100,
                    x: 50,
                    y: 0,
                },
            ],
        }
    }

    #[test]
    fn v2_session_and_profile_round_trip_through_json_files() {
        let root = test_root("round-trip");
        fs::create_dir_all(&root).unwrap();
        let session = test_session();
        let profile =
            train_behavior_profile_with_retention(&session, SourceRetention::Persisted).unwrap();
        let session_path = root.join("session.json");
        let profile_path = root.join("profile.json");
        write_json_file(
            &session_path,
            &serde_json::to_string(&session).unwrap(),
            "test_write",
            "test write failed",
        )
        .unwrap();
        write_json_file(
            &profile_path,
            &serde_json::to_string(&profile).unwrap(),
            "test_write",
            "test write failed",
        )
        .unwrap();
        assert_eq!(
            read_json_file::<BehaviorSessionV2>(&session_path).unwrap(),
            session
        );
        assert_eq!(
            read_json_file::<crate::BehaviorProfileV2>(&profile_path).unwrap(),
            profile
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn v2_index_mismatch_drops_stale_references_without_dropping_the_model() {
        let session = test_session();
        let profile =
            train_behavior_profile_with_retention(&session, SourceRetention::Persisted).unwrap();
        let profile_id = profile.id.clone();
        let mut config = AppConfig::default();
        config.behavior_profiles_v2.push(profile);
        config.active_behavior_profile_v2_id = Some("missing-profile".to_string());
        config.behavior_policy.profile_id = Some("missing-profile".to_string());

        assert!(reconcile_v2_references(&mut config));
        assert_eq!(config.behavior_profiles_v2.len(), 1);
        assert_eq!(
            config.behavior_profiles_v2[0].source_retention,
            SourceRetention::Ephemeral
        );
        assert_eq!(
            config.active_behavior_profile_v2_id.as_deref(),
            Some(profile_id.as_str())
        );
        assert!(config.behavior_policy.profile_id.is_none());
    }

    #[test]
    fn missing_and_corrupt_json_have_stable_errors() {
        let root = test_root("errors");
        fs::create_dir_all(&root).unwrap();
        let missing = read_json_file::<serde_json::Value>(&root.join("missing.json")).unwrap_err();
        assert_eq!(missing.code, "behavior_file_read_failed");
        let corrupt_path = root.join("corrupt.json");
        fs::write(&corrupt_path, b"not-json").unwrap();
        let corrupt = read_json_file::<serde_json::Value>(&corrupt_path).unwrap_err();
        assert_eq!(corrupt.code, "behavior_file_decode_failed");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replacement_keeps_a_backup_until_the_new_file_is_valid() {
        let root = test_root("backup");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("config.json");
        write_json_file(&path, "{\"version\":1}", "test_write", "test write failed").unwrap();
        write_json_file(&path, "{\"version\":2}", "test_write", "test write failed").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"version\":2}");
        assert_eq!(
            fs::read_to_string(path.with_extension("json.bak")).unwrap(),
            "{\"version\":1}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn readable_backup_recovers_when_formal_path_is_missing() {
        let root = test_root("backup-recovery");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("session.json");
        write_json_file(&path, "{\"version\":1}", "test_write", "test write failed").unwrap();
        write_json_file(&path, "{\"version\":2}", "test_write", "test write failed").unwrap();
        fs::remove_file(&path).unwrap();

        let (recovered, used_backup) =
            read_json_file_with_backup::<serde_json::Value>(&path).unwrap();
        assert!(used_backup);
        assert_eq!(recovered["version"], 1);
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_cleanup_only_removes_expected_json_files() {
        let root = test_root("stale");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("keep.json"), b"keep").unwrap();
        fs::write(root.join("stale.json"), b"stale").unwrap();
        fs::write(root.join("notes.txt"), b"keep").unwrap();
        remove_stale_json_files(&root, &HashSet::from(["keep.json".to_string()]));
        assert!(root.join("keep.json").exists());
        assert!(!root.join("stale.json").exists());
        assert!(root.join("notes.txt").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ephemeral_profile_is_model_only_and_needs_no_session_file() {
        let session = test_session();
        let profile =
            train_behavior_profile_with_retention(&session, SourceRetention::Ephemeral).unwrap();
        let root = test_root("ephemeral-write");
        fs::create_dir_all(&root).unwrap();
        let profile_path = root.join("profile.json");
        write_json_file(
            &profile_path,
            &serde_json::to_string(&profile).unwrap(),
            "test_write",
            "test write failed",
        )
        .unwrap();
        let serialized = serde_json::to_string(&profile).unwrap();
        assert!(!serialized.contains("rawEvents"));
        assert!(serialized.contains("\"sourceRetention\":\"ephemeral\""));
        assert!(profile_path.exists());
        assert!(!root.join("session.json").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_behavior_delete_removes_only_formal_and_backup_files() {
        let root = test_root("delete");
        fs::create_dir_all(&root).unwrap();
        let formal = root.join("session-hash.json");
        let backup = root.join("session-hash.json.bak");
        let similarly_named = root.join("session-hash-extra.json");
        fs::write(&formal, b"formal").unwrap();
        fs::write(&backup, b"backup").unwrap();
        fs::write(&similarly_named, b"keep").unwrap();

        delete_behavior_file(&root, "session-hash.json", "test_delete", "delete failed").unwrap();

        assert!(!formal.exists());
        assert!(!backup.exists());
        assert!(similarly_named.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn temporary_write_failure_keeps_the_original_path_unmodified() {
        let root = test_root("temp-failure");
        let path = root.join("missing-parent").join("config.json");
        let error = write_json_file(&path, "new", "test_write", "write failed").unwrap_err();
        assert_eq!(error.code, "test_write");
        assert!(!path.exists());
        assert!(!path.with_extension("json.tmp").exists());
    }
}
