use crate::behavior::v2::{
    BehaviorProfileV2, BehaviorProfileV2File, BehaviorSessionFile, BehaviorSessionV2,
    SourceRetention,
};
use crate::behavior::{BehaviorProfile, BehaviorProfileFile, BiomimeticInput, BiomimeticInputFile};
use crate::{AppConfig, AppError, AutomationProgram, MacroRule, MacroRuleFile};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

const BEHAVIOR_PROFILE_DIRECTORY: &str = "behavior-profiles";
const BIOMIMETIC_INPUT_DIRECTORY: &str = "biomimetic-inputs";
const MANAGED_DATA_DIRECTORY: &str = "data";
const BEHAVIOR_SESSION_V2_DIRECTORY: &str = "sessions";
const BEHAVIOR_PROFILE_V2_DIRECTORY: &str = "profiles";
const MACRO_SCRIPT_DIRECTORY: &str = "scripts";
const IMAGE_DIRECTORY: &str = "images";
const LEGACY_BEHAVIOR_SESSION_V2_DIRECTORY: &str = "behavior-sessions-v2";
const LEGACY_BEHAVIOR_PROFILE_V2_DIRECTORY: &str = "behavior-profiles-v2";

pub fn load_config(app: &AppHandle) -> Result<AppConfig, AppError> {
    let path = config_path(app)?;
    if !path.exists() {
        let config = AppConfig::default();
        return save_config(app, &config);
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
            let inline_macros = !config.macros.is_empty();
            let inline_profiles = !config.behavior_profiles.is_empty();
            let inline_inputs = !config.biomimetic_inputs.is_empty();
            let inline_sessions_v2 = !config.behavior_sessions_v2.is_empty();
            let inline_profiles_v2 = !config.behavior_profiles_v2.is_empty();
            let (macros, macro_files, missing_macros) = if inline_macros {
                let files = sync_macro_scripts(app, &config.macros)?;
                (config.macros.clone(), files, false)
            } else {
                load_macro_scripts(app, &config.macro_files)?
            };
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
                let profiles = config
                    .behavior_profiles_v2
                    .iter()
                    .cloned()
                    .map(BehaviorProfileV2::normalize_derived_fields)
                    .collect::<Vec<_>>();
                let files = sync_behavior_profiles_v2(app, &profiles)?;
                (profiles, files, true)
            } else {
                load_behavior_profiles_v2(app, &config.behavior_profile_v2_files)?
            };
            config.macros = macros;
            let normalized_macro_names = normalize_macro_display_names(&mut config.macros);
            config.macro_files = macro_files;
            config.behavior_profiles = profiles;
            config.behavior_profile_files = profile_files;
            config.biomimetic_inputs = inputs;
            config.biomimetic_input_files = input_files;
            config.behavior_sessions_v2 = sessions_v2;
            config.behavior_session_files_v2 = session_files_v2;
            config.behavior_profiles_v2 = profiles_v2;
            config.behavior_profile_v2_files = profile_files_v2;
            let (assets, assets_changed) = crate::automation::sync_asset_directory(
                &managed_image_directory(app)?,
                &config.assets,
            )
            .map_err(|error| AppError::invalid(error.code, error.message))?;
            config.assets = assets;
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
                || inline_macros
                || inline_inputs
                || inline_sessions_v2
                || inline_profiles_v2
                || missing_profiles
                || missing_macros
                || normalized_macro_names
                || missing_inputs
                || missing_sessions_v2
                || missing_profiles_v2
                || assets_changed
            {
                config = save_config(app, &config)?;
            }
            Ok(config)
        }
        Err(error) => {
            let backup_path = path.with_extension("json.corrupt");
            let _ = fs::copy(&path, backup_path);
            let config = AppConfig::default();
            log::warn!("配置损坏，已切换到安全默认配置: {error}");
            save_config(app, &config)
        }
    }
}

pub fn save_config(app: &AppHandle, config: &AppConfig) -> Result<AppConfig, AppError> {
    let mut saved = config.clone();
    normalize_macro_display_names(&mut saved.macros);
    let (assets, _) =
        crate::automation::sync_asset_directory(&managed_image_directory(app)?, &saved.assets)
            .map_err(|error| AppError::invalid(error.code, error.message))?;
    saved.assets = assets;
    saved.validate()?;
    let previous_macro_files = saved.macro_files.clone();
    let previous_session_files = saved.behavior_session_files_v2.clone();
    let previous_profile_files = saved.behavior_profile_v2_files.clone();
    let macro_files = sync_macro_scripts(app, &saved.macros)?;
    let behavior_profile_files = sync_behavior_profiles(app, &saved.behavior_profiles)?;
    let biomimetic_input_files = sync_biomimetic_inputs(app, &saved.biomimetic_inputs)?;
    let behavior_session_files_v2 = sync_behavior_sessions_v2(app, &saved.behavior_sessions_v2)?;
    let behavior_profile_v2_files = sync_behavior_profiles_v2(app, &saved.behavior_profiles_v2)?;
    let expected_macro_files = macro_files
        .iter()
        .map(|entry| entry.file_name.clone())
        .collect::<HashSet<_>>();
    let expected_session_files = behavior_session_files_v2
        .iter()
        .map(|entry| entry.file_name.clone())
        .collect::<HashSet<_>>();
    let expected_profile_files = behavior_profile_v2_files
        .iter()
        .map(|entry| entry.file_name.clone())
        .collect::<HashSet<_>>();
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

    saved.macro_files = macro_files;
    saved.behavior_profile_files = behavior_profile_files;
    saved.biomimetic_input_files = biomimetic_input_files;
    saved.behavior_session_files_v2 = behavior_session_files_v2;
    saved.behavior_profile_v2_files = behavior_profile_v2_files;
    let mut persisted = saved.clone();
    persisted.macros.clear();
    persisted.behavior_profiles.clear();
    persisted.biomimetic_inputs.clear();
    persisted.behavior_sessions_v2.clear();
    persisted.behavior_profiles_v2.clear();
    let content = serde_json::to_string_pretty(&persisted).map_err(|error| {
        AppError::with_detail("config_encode_failed", "配置序列化失败", error.to_string())
    })?;
    write_json_file(
        &path,
        &content,
        "config_write_failed",
        "配置写入失败，请检查磁盘空间和权限",
    )?;
    remove_stale_indexed_json_files(
        &managed_data_directory(app, MACRO_SCRIPT_DIRECTORY)?,
        previous_macro_files
            .iter()
            .map(|entry| entry.file_name.as_str()),
        &expected_macro_files,
    );
    remove_stale_indexed_json_files(
        &managed_data_directory(app, BEHAVIOR_SESSION_V2_DIRECTORY)?,
        previous_session_files
            .iter()
            .map(|entry| entry.file_name.as_str()),
        &expected_session_files,
    );
    remove_stale_indexed_json_files(
        &managed_data_directory(app, BEHAVIOR_PROFILE_V2_DIRECTORY)?,
        previous_profile_files
            .iter()
            .map(|entry| entry.file_name.as_str()),
        &expected_profile_files,
    );
    Ok(saved)
}

fn load_macro_scripts(
    app: &AppHandle,
    entries: &[MacroRuleFile],
) -> Result<(Vec<MacroRule>, Vec<MacroRuleFile>, bool), AppError> {
    let root = managed_data_directory(app, MACRO_SCRIPT_DIRECTORY)?;
    let mut macros = Vec::new();
    let mut valid_entries = Vec::new();
    let mut indexed_names = HashSet::new();
    let mut used_ids = HashSet::new();
    let mut indexed_modified = HashMap::new();
    let mut missing = false;
    for entry in entries {
        indexed_names.insert(entry.file_name.to_lowercase());
        let Some(path) = indexed_json_path(&root, &entry.file_name) else {
            missing = true;
            log::warn!("宏脚本索引包含无效文件名，已移除: {}", entry.id);
            continue;
        };
        if !json_file_or_backup_exists(&path) {
            missing = true;
            log::warn!("宏脚本文件不存在，已从索引移除: {}", entry.id);
            continue;
        }
        match read_text_file_with_backup(&path) {
            Ok((content, recovered)) => {
                let mut rule = decode_macro_file(
                    &content,
                    &entry.id,
                    &file_stem_or(&entry.file_name, &entry.name),
                    true,
                );
                if !used_ids.insert(rule.id.clone()) {
                    rule.enabled = false;
                    rule.import_error = Some("宏 ID 与其他文件重复".to_string());
                }
                indexed_modified.insert(
                    rule.id.clone(),
                    (file_modified_ms(&path), rule.name.clone()),
                );
                valid_entries.push(MacroRuleFile::from_macro(&rule, entry.file_name.clone()));
                macros.push(rule);
                missing |= recovered;
            }
            Err(_) => {
                missing = true;
                log::warn!("宏脚本文件无效，已从索引移除: {}", entry.id);
            }
        }
    }

    for path in unindexed_json_files(&root, &indexed_names) {
        let Some(file_name) = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let fallback_id = format!("imported-{}", &stable_id_hash(&file_name)[..16]);
        let fallback_name = file_stem_or(&file_name, "导入脚本");
        let mut rule = decode_macro_file(&content, &fallback_id, &fallback_name, false);
        if used_ids.contains(&rule.id) {
            let stale_rename =
                indexed_modified
                    .get(&rule.id)
                    .is_some_and(|(indexed_time, indexed_name)| {
                        file_modified_ms(&path) <= *indexed_time && rule.name != *indexed_name
                    });
            if stale_rename {
                // Older builds could leave the pre-rename file behind. The
                // indexed copy has the same ID, a newer name and write time.
                let _ = fs::remove_file(&path);
                let _ = fs::remove_file(path.with_extension("json.bak"));
                continue;
            }
            rule.id = fallback_id;
            rule.name = fallback_name.clone();
            rule.enabled = false;
            rule.import_error =
                Some("文件内 ID 与已有宏重复；已分配临时 ID，请检查源码并保存".to_string());
        }
        rule.enabled = false;
        rule.behavior_policy = None;
        used_ids.insert(rule.id.clone());
        valid_entries.push(MacroRuleFile::from_macro(&rule, file_name));
        macros.push(rule);
        missing = true;
    }

    let desired_file_names =
        display_json_file_names(macros.iter().map(|rule| rule.name.as_str()), "脚本");
    missing |= valid_entries
        .iter()
        .zip(desired_file_names)
        .any(|(entry, desired)| entry.file_name != desired);
    Ok((macros, valid_entries, missing))
}

fn sync_macro_scripts(
    app: &AppHandle,
    macros: &[MacroRule],
) -> Result<Vec<MacroRuleFile>, AppError> {
    let root = managed_data_directory(app, MACRO_SCRIPT_DIRECTORY)?;
    let mut entries = Vec::with_capacity(macros.len());
    let file_names = display_json_file_names(macros.iter().map(|rule| rule.name.as_str()), "脚本");
    for (rule, file_name) in macros.iter().zip(file_names) {
        if rule.import_error.is_some() {
            let existing = previous_macro_entry(rule, &file_name, &root)?;
            entries.push(existing);
            continue;
        }
        let path = root.join(&file_name);
        let content = serde_json::to_string_pretty(rule).map_err(|error| {
            AppError::with_detail(
                "macro_script_encode_failed",
                "宏脚本序列化失败",
                error.to_string(),
            )
        })?;
        write_json_file(
            &path,
            &content,
            "macro_script_write_failed",
            "宏脚本文件写入失败，请检查磁盘空间和权限",
        )?;
        entries.push(MacroRuleFile::from_macro(rule, file_name));
    }
    Ok(entries)
}

fn previous_macro_entry(
    rule: &MacroRule,
    desired_file_name: &str,
    root: &Path,
) -> Result<MacroRuleFile, AppError> {
    let path = root.join(desired_file_name);
    if path.exists() {
        return Ok(MacroRuleFile::from_macro(
            rule,
            desired_file_name.to_string(),
        ));
    }
    Err(AppError::invalid(
        "invalid_macro_source_missing",
        "不合法宏的原始文件已不存在，请删除该宏后重新添加文件",
    ))
}

fn decode_macro_file(
    content: &str,
    fallback_id: &str,
    fallback_name: &str,
    force_id: bool,
) -> MacroRule {
    match serde_json::from_str::<MacroRule>(content) {
        Ok(mut rule) => {
            let mut errors = Vec::new();
            if force_id && rule.id != fallback_id {
                errors.push("文件内 ID 与配置索引不一致".to_string());
                rule.id = fallback_id.to_string();
            } else if rule.id.trim().is_empty() {
                errors.push("宏 ID 不能为空".to_string());
                rule.id = fallback_id.to_string();
            }
            if rule.name.trim().is_empty() {
                errors.push("宏名称不能为空".to_string());
                rule.name = fallback_name.to_string();
            }
            if let Some(error) = rule.import_error.take() {
                errors.push(error);
            }
            if let Some(error) = macro_file_validation_error(&rule) {
                errors.push(error);
            }
            if !errors.is_empty() {
                rule.name = fallback_name.to_string();
                rule.enabled = false;
                rule.import_error = Some(errors.join("；"));
            }
            rule
        }
        Err(error) => MacroRule {
            id: fallback_id.to_string(),
            name: fallback_name.to_string(),
            import_error: Some(format!("JSON 格式不合法：{error}")),
            enabled: false,
            trigger_keys: Vec::new(),
            mode: crate::MacroMode::Once,
            repeat_count: 1,
            speed: 1.0,
            record_mouse_move: true,
            record_mouse_clicks: true,
            target: None,
            behavior_policy: None,
            program: AutomationProgram::Rhai {
                source: content.to_string(),
                api_version: 1,
            },
        },
    }
}

fn macro_file_validation_error(rule: &MacroRule) -> Option<String> {
    if !rule.speed.is_finite() || rule.speed <= 0.0 {
        return Some("宏速度必须大于 0".to_string());
    }
    if matches!(rule.mode, crate::MacroMode::Repeat) && rule.repeat_count == 0 {
        return Some("固定次数循环至少需要执行 1 次".to_string());
    }
    let trigger_keys = rule
        .trigger_keys
        .iter()
        .map(|key| key.trim().to_ascii_uppercase())
        .collect::<HashSet<_>>();
    if trigger_keys.len() == 3
        && trigger_keys
            == HashSet::from(["CTRL".to_string(), "SHIFT".to_string(), "F9".to_string()])
    {
        return Some("Ctrl + Shift + F9 是保留的录制快捷键".to_string());
    }
    match &rule.program {
        AutomationProgram::Macro { steps } => {
            if steps
                .iter()
                .any(|step| matches!(step, crate::MacroStep::Text { text } if text.is_empty()))
            {
                Some("宏的输入文本不能为空".to_string())
            } else {
                None
            }
        }
        AutomationProgram::Rhai {
            source,
            api_version,
        } => {
            if *api_version != 1 {
                Some("Rhai API 版本必须为 1".to_string())
            } else if source.trim().is_empty() {
                Some("高级 Rhai 脚本不能为空".to_string())
            } else {
                crate::rhai_runtime::validate_rhai_source(source).err()
            }
        }
    }
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
    let root = managed_data_directory(app, BEHAVIOR_SESSION_V2_DIRECTORY)?;
    let legacy_root = legacy_data_directory(app, LEGACY_BEHAVIOR_SESSION_V2_DIRECTORY)?;
    let mut sessions = Vec::new();
    let mut valid_entries = Vec::new();
    let indexed_names = entries
        .iter()
        .map(|entry| entry.file_name.to_lowercase())
        .collect::<HashSet<_>>();
    let mut used_ids = HashSet::new();
    let mut missing = false;
    for entry in entries {
        let Some(current_path) = indexed_json_path(&root, &entry.file_name) else {
            missing = true;
            continue;
        };
        let Some(legacy_path) = indexed_json_path(&legacy_root, &entry.file_name) else {
            missing = true;
            continue;
        };
        let (path, migrated) = if json_file_or_backup_exists(&current_path) {
            (current_path, false)
        } else if json_file_or_backup_exists(&legacy_path) {
            (legacy_path, true)
        } else {
            (current_path, false)
        };
        if !json_file_or_backup_exists(&path) {
            missing = true;
            continue;
        }
        match read_json_file_with_backup::<BehaviorSessionV2>(&path) {
            Ok((session, recovered)) if session.id == entry.id && session.validate().is_ok() => {
                used_ids.insert(session.id.clone());
                sessions.push(session);
                valid_entries.push(entry.clone());
                missing |= recovered || migrated;
            }
            Ok(_) | Err(_) => {
                missing = true;
            }
        }
    }
    for path in unindexed_json_files(&root, &indexed_names) {
        let Some(file_name) = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let Ok((session, _)) = read_json_file_with_backup::<BehaviorSessionV2>(&path) else {
            log::warn!("未索引的 Session 文件不合法，已保留原文件: {file_name}");
            continue;
        };
        if session.validate().is_err() || !used_ids.insert(session.id.clone()) {
            continue;
        }
        valid_entries.push(BehaviorSessionFile::from_session(&session, file_name));
        sessions.push(session);
        missing = true;
    }
    let desired_file_names = display_json_file_names(
        sessions.iter().map(|session| session.name.as_str()),
        "Session",
    );
    missing |= valid_entries
        .iter()
        .zip(desired_file_names)
        .any(|(entry, desired)| entry.file_name != desired);
    Ok((sessions, valid_entries, missing))
}

fn load_behavior_profiles_v2(
    app: &AppHandle,
    entries: &[BehaviorProfileV2File],
) -> Result<(Vec<BehaviorProfileV2>, Vec<BehaviorProfileV2File>, bool), AppError> {
    let root = managed_data_directory(app, BEHAVIOR_PROFILE_V2_DIRECTORY)?;
    let legacy_root = legacy_data_directory(app, LEGACY_BEHAVIOR_PROFILE_V2_DIRECTORY)?;
    let mut profiles = Vec::new();
    let mut valid_entries = Vec::new();
    let indexed_names = entries
        .iter()
        .map(|entry| entry.file_name.to_lowercase())
        .collect::<HashSet<_>>();
    let mut used_ids = HashSet::new();
    let mut missing = false;
    for entry in entries {
        let Some(current_path) = indexed_json_path(&root, &entry.file_name) else {
            missing = true;
            continue;
        };
        let Some(legacy_path) = indexed_json_path(&legacy_root, &entry.file_name) else {
            missing = true;
            continue;
        };
        let (path, migrated) = if json_file_or_backup_exists(&current_path) {
            (current_path, false)
        } else if json_file_or_backup_exists(&legacy_path) {
            (legacy_path, true)
        } else {
            (current_path, false)
        };
        if !json_file_or_backup_exists(&path) {
            missing = true;
            continue;
        }
        match read_json_file_with_backup::<BehaviorProfileV2>(&path) {
            Ok((profile, recovered)) => {
                let normalized = profile.clone().normalize_derived_fields();
                if normalized.id == entry.id && normalized.validate().is_ok() {
                    used_ids.insert(normalized.id.clone());
                    missing |= recovered || migrated || normalized != profile;
                    profiles.push(normalized);
                    valid_entries.push(entry.clone());
                } else {
                    missing = true;
                }
            }
            Err(_) => {
                missing = true;
            }
        }
    }
    for path in unindexed_json_files(&root, &indexed_names) {
        let Some(file_name) = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let Ok((profile, _)) = read_json_file_with_backup::<BehaviorProfileV2>(&path) else {
            log::warn!("未索引的 Profile 文件不合法，已保留原文件: {file_name}");
            continue;
        };
        let normalized = profile.normalize_derived_fields();
        if normalized.validate().is_err() || !used_ids.insert(normalized.id.clone()) {
            continue;
        }
        valid_entries.push(BehaviorProfileV2File::from_profile(&normalized, file_name));
        profiles.push(normalized);
        missing = true;
    }
    let desired_file_names = display_json_file_names(
        profiles.iter().map(|profile| profile.name.as_str()),
        "Profile",
    );
    missing |= valid_entries
        .iter()
        .zip(desired_file_names)
        .any(|(entry, desired)| entry.file_name != desired);
    Ok((profiles, valid_entries, missing))
}

fn sync_behavior_sessions_v2(
    app: &AppHandle,
    sessions: &[BehaviorSessionV2],
) -> Result<Vec<BehaviorSessionFile>, AppError> {
    let root = managed_data_directory(app, BEHAVIOR_SESSION_V2_DIRECTORY)?;
    let mut entries = Vec::with_capacity(sessions.len());
    let file_names = display_json_file_names(
        sessions.iter().map(|session| session.name.as_str()),
        "Session",
    );
    for (session, file_name) in sessions.iter().zip(file_names) {
        session.validate()?;
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
    let root = managed_data_directory(app, BEHAVIOR_PROFILE_V2_DIRECTORY)?;
    let mut entries = Vec::with_capacity(profiles.len());
    let file_names = display_json_file_names(
        profiles.iter().map(|profile| profile.name.as_str()),
        "Profile",
    );
    for (profile, file_name) in profiles.iter().zip(file_names) {
        profile.validate()?;
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
    let root = managed_data_directory(app, BEHAVIOR_SESSION_V2_DIRECTORY)?;
    delete_behavior_file(
        &root,
        &session_file_name(id),
        "behavior_v2_session_delete_failed",
        "V2 训练会话文件删除失败",
    )?;
    let legacy_root = legacy_data_directory(app, LEGACY_BEHAVIOR_SESSION_V2_DIRECTORY)?;
    delete_behavior_file(
        &legacy_root,
        &session_file_name(id),
        "behavior_v2_session_delete_failed",
        "V2 训练会话文件删除失败",
    )
}

pub fn delete_behavior_profile_v2(app: &AppHandle, id: &str) -> Result<(), AppError> {
    let root = managed_data_directory(app, BEHAVIOR_PROFILE_V2_DIRECTORY)?;
    delete_behavior_file(
        &root,
        &profile_v2_file_name(id),
        "behavior_v2_profile_delete_failed",
        "V2 行为档案文件删除失败",
    )?;
    let legacy_root = legacy_data_directory(app, LEGACY_BEHAVIOR_PROFILE_V2_DIRECTORY)?;
    delete_behavior_file(
        &legacy_root,
        &profile_v2_file_name(id),
        "behavior_v2_profile_delete_failed",
        "V2 行为档案文件删除失败",
    )
}

fn behavior_directory(app: &AppHandle, name: &str) -> Result<PathBuf, AppError> {
    let directory = app_data_directory(app)?.join(name);
    fs::create_dir_all(&directory).map_err(|error| {
        AppError::with_detail(
            "behavior_directory_failed",
            "无法创建 AutoFlow 行为训练目录，请检查本地权限",
            error.to_string(),
        )
    })?;
    Ok(directory)
}

fn app_data_directory(app: &AppHandle) -> Result<PathBuf, AppError> {
    app.path().app_data_dir().map_err(|error| {
        AppError::with_detail(
            "behavior_directory_failed",
            "无法定位 AutoFlow 数据目录",
            error.to_string(),
        )
    })
}

fn legacy_data_directory(app: &AppHandle, name: &str) -> Result<PathBuf, AppError> {
    Ok(app_data_directory(app)?.join(name))
}

pub fn managed_data_root(app: &AppHandle) -> Result<PathBuf, AppError> {
    let root = app_data_directory(app)?.join(MANAGED_DATA_DIRECTORY);
    fs::create_dir_all(&root).map_err(|error| {
        AppError::with_detail(
            "behavior_directory_failed",
            "无法创建 AutoFlow 数据目录，请检查本地权限",
            error.to_string(),
        )
    })?;
    for name in [
        BEHAVIOR_PROFILE_V2_DIRECTORY,
        BEHAVIOR_SESSION_V2_DIRECTORY,
        MACRO_SCRIPT_DIRECTORY,
        IMAGE_DIRECTORY,
    ] {
        fs::create_dir_all(root.join(name)).map_err(|error| {
            AppError::with_detail(
                "behavior_directory_failed",
                "无法创建 AutoFlow 数据子目录，请检查本地权限",
                error.to_string(),
            )
        })?;
    }
    Ok(root)
}

pub fn managed_image_directory(app: &AppHandle) -> Result<PathBuf, AppError> {
    let root = managed_data_directory(app, IMAGE_DIRECTORY)?;
    let migration_marker = managed_data_root(app)?.join(".images-migrated-v1");
    let legacy_root = app_data_directory(app)?.join("assets").join("images");
    if legacy_root.exists() && !migration_marker.exists() {
        let mut migration_complete = true;
        match fs::read_dir(&legacy_root) {
            Ok(entries) => {
                for entry in entries.filter_map(Result::ok) {
                    let Ok(file_type) = entry.file_type() else {
                        continue;
                    };
                    if !file_type.is_file() {
                        continue;
                    }
                    let destination = root.join(entry.file_name());
                    if !destination.exists() {
                        if let Err(error) = fs::copy(entry.path(), &destination) {
                            migration_complete = false;
                            log::warn!(
                                "旧图像资源迁移失败 {}: {error}",
                                entry.file_name().to_string_lossy()
                            );
                        }
                    }
                }
            }
            Err(error) => {
                migration_complete = false;
                log::warn!("无法读取旧图像资源目录: {error}");
            }
        }
        if migration_complete {
            if let Err(error) = fs::write(&migration_marker, b"migrated") {
                log::warn!("无法记录图像资源迁移状态: {error}");
            }
        }
    }
    Ok(root)
}

fn managed_data_directory(app: &AppHandle, name: &str) -> Result<PathBuf, AppError> {
    Ok(managed_data_root(app)?.join(name))
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

fn read_text_file_with_backup(path: &Path) -> Result<(String, bool), AppError> {
    match fs::read_to_string(path) {
        Ok(content) => Ok((content, false)),
        Err(primary_error) => {
            let backup_path = path.with_extension("json.bak");
            fs::read_to_string(&backup_path)
                .map(|content| (content, true))
                .map_err(|_| {
                    AppError::with_detail(
                        "behavior_file_read_failed",
                        "行为训练文件读取失败",
                        primary_error.to_string(),
                    )
                })
        }
    }
}

fn json_file_or_backup_exists(path: &Path) -> bool {
    path.exists() || path.with_extension("json.bak").exists()
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
        let indexed_name =
            if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
                Some(file_name)
            } else {
                file_name.strip_suffix(".bak")
            };
        if indexed_name.is_some_and(|name| name.ends_with(".json") && !expected.contains(name)) {
            let _ = fs::remove_file(path);
        }
    }
}

fn remove_stale_indexed_json_files<'a>(
    root: &Path,
    previous_file_names: impl Iterator<Item = &'a str>,
    expected: &HashSet<String>,
) {
    let expected_keys = expected
        .iter()
        .map(|name| name.to_lowercase())
        .collect::<HashSet<_>>();
    for file_name in previous_file_names {
        if expected_keys.contains(&file_name.to_lowercase()) {
            continue;
        }
        let Some(path) = indexed_json_path(root, file_name) else {
            continue;
        };
        for candidate in [path.clone(), path.with_extension("json.bak")] {
            if candidate.exists() {
                let _ = fs::remove_file(candidate);
            }
        }
    }
}

fn indexed_json_path(root: &Path, file_name: &str) -> Option<PathBuf> {
    let relative = Path::new(file_name);
    if relative.components().count() != 1
        || !relative
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
    {
        return None;
    }
    Some(root.join(relative))
}

fn unindexed_json_files(root: &Path, indexed_names: &HashSet<String>) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut paths = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        })
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !indexed_names.contains(&name.to_lowercase()))
        })
        .collect::<Vec<_>>();
    paths.sort_by_key(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
    });
    paths
}

fn file_stem_or(file_name: &str, fallback: &str) -> String {
    Path::new(file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn file_modified_ms(path: &Path) -> u128 {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn display_json_file_names<'a>(
    names: impl Iterator<Item = &'a str>,
    fallback: &str,
) -> Vec<String> {
    let mut used = HashSet::new();
    names
        .map(|name| {
            let stem = sanitize_display_file_stem(name, fallback);
            let mut suffix = 1_u32;
            loop {
                let candidate = if suffix == 1 {
                    format!("{stem}.json")
                } else {
                    format!("{stem} ({suffix}).json")
                };
                if used.insert(candidate.to_lowercase()) {
                    break candidate;
                }
                suffix += 1;
            }
        })
        .collect()
}

fn normalize_macro_display_names(macros: &mut [MacroRule]) -> bool {
    let normalized_names =
        display_json_file_names(macros.iter().map(|rule| rule.name.as_str()), "脚本")
            .into_iter()
            .map(|file_name| {
                Path::new(&file_name)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("脚本")
                    .to_string()
            })
            .collect::<Vec<_>>();
    let mut changed = false;
    for (rule, normalized_name) in macros.iter_mut().zip(normalized_names) {
        if rule.name != normalized_name {
            rule.name = normalized_name;
            changed = true;
        }
    }
    changed
}

fn sanitize_display_file_stem(name: &str, fallback: &str) -> String {
    let mut stem = name
        .trim()
        .chars()
        .take(96)
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    while matches!(stem.chars().last(), Some(' ' | '.')) {
        stem.pop();
    }
    if stem.is_empty() {
        stem = fallback.to_string();
    }
    if is_windows_reserved_file_stem(&stem) {
        stem.insert(0, '_');
    }
    stem
}

fn is_windows_reserved_file_stem(stem: &str) -> bool {
    let base = stem.split('.').next().unwrap_or_default().to_uppercase();
    matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || base
            .strip_prefix("COM")
            .or_else(|| base.strip_prefix("LPT"))
            .is_some_and(|number| {
                number.len() == 1 && matches!(number.as_bytes().first(), Some(b'1'..=b'9'))
            })
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

        assert!(json_file_or_backup_exists(&path));
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
    fn stale_cleanup_removes_the_stale_json_backup_too() {
        let root = test_root("stale-backup");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("keep.json"), b"keep").unwrap();
        fs::write(root.join("keep.json.bak"), b"keep backup").unwrap();
        fs::write(root.join("stale.json.bak"), b"stale backup").unwrap();

        remove_stale_json_files(&root, &HashSet::from(["keep.json".to_string()]));

        assert!(root.join("keep.json").exists());
        assert!(root.join("keep.json.bak").exists());
        assert!(!root.join("stale.json.bak").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn indexed_cleanup_preserves_unmanaged_files() {
        let root = test_root("indexed-cleanup");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("旧名称.json"), b"old").unwrap();
        fs::write(root.join("旧名称.json.bak"), b"old backup").unwrap();
        fs::write(root.join("新名称.json"), b"new").unwrap();
        fs::write(root.join("用户副本.json"), b"unmanaged").unwrap();

        remove_stale_indexed_json_files(
            &root,
            ["旧名称.json"].into_iter(),
            &HashSet::from(["新名称.json".to_string()]),
        );

        assert!(!root.join("旧名称.json").exists());
        assert!(!root.join("旧名称.json.bak").exists());
        assert!(root.join("新名称.json").exists());
        assert!(root.join("用户副本.json").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn macro_script_round_trip_preserves_advanced_rhai_source() {
        let root = test_root("macro-script");
        fs::create_dir_all(&root).unwrap();
        let file_name = display_json_file_names(std::iter::once("Advanced Rhai"), "脚本")
            .into_iter()
            .next()
            .unwrap();
        let path = root.join(&file_name);
        let rule = MacroRule {
            id: "macro/rhai".to_string(),
            name: "Advanced Rhai".to_string(),
            import_error: None,
            enabled: true,
            trigger_keys: vec!["F11".to_string()],
            mode: crate::MacroMode::Once,
            repeat_count: 1,
            speed: 1.0,
            record_mouse_move: true,
            record_mouse_clicks: true,
            target: None,
            behavior_policy: None,
            program: AutomationProgram::Rhai {
                source: "move_to(120, 80);".to_string(),
                api_version: 1,
            },
        };
        write_json_file(
            &path,
            &serde_json::to_string_pretty(&rule).unwrap(),
            "test_write",
            "test write failed",
        )
        .unwrap();

        let loaded = read_json_file::<MacroRule>(&path).unwrap();
        assert!(macro_file_validation_error(&loaded).is_none());
        assert_eq!(loaded.id, rule.id);
        assert!(matches!(
            loaded.program,
            AutomationProgram::Rhai { source, api_version: 1 }
                if source == "move_to(120, 80);"
        ));
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("Advanced Rhai.json")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_external_macro_remains_editable_but_disabled() {
        let rule = decode_macro_file("this is not json", "imported-test", "待修复脚本", false);
        assert_eq!(rule.id, "imported-test");
        assert_eq!(rule.name, "待修复脚本");
        assert!(!rule.enabled);
        assert!(rule
            .import_error
            .as_deref()
            .is_some_and(|error| error.contains("JSON 格式不合法")));
        assert!(matches!(
            rule.program,
            AutomationProgram::Rhai { source, .. } if source == "this is not json"
        ));
    }

    #[test]
    fn invalid_rhai_import_reports_compile_error_without_dropping_source() {
        let content = serde_json::json!({
            "id": "external-rhai",
            "name": "外部 Rhai",
            "enabled": true,
            "program": {
                "kind": "rhai",
                "apiVersion": 1,
                "source": "import \"fs\";"
            }
        })
        .to_string();
        let rule = decode_macro_file(&content, "fallback", "外部 Rhai", false);
        assert_eq!(rule.id, "external-rhai");
        assert!(!rule.enabled);
        assert!(rule.import_error.is_some());
        assert!(matches!(
            rule.program,
            AutomationProgram::Rhai { source, .. } if source == "import \"fs\";"
        ));
    }

    #[test]
    fn display_file_names_follow_ui_names_and_resolve_windows_conflicts() {
        let names = display_json_file_names(
            ["我的鼠标操作", "我的鼠标操作", "报告:/测试", "CON", "..."].into_iter(),
            "脚本",
        );
        assert_eq!(
            names,
            vec![
                "我的鼠标操作.json",
                "我的鼠标操作 (2).json",
                "报告__测试.json",
                "_CON.json",
                "脚本.json",
            ]
        );
    }

    #[test]
    fn indexed_json_paths_reject_directory_traversal() {
        let root = test_root("indexed-path");
        assert_eq!(
            indexed_json_path(&root, "界面名称.json"),
            Some(root.join("界面名称.json"))
        );
        assert!(indexed_json_path(&root, "../outside.json").is_none());
        assert!(indexed_json_path(&root, "nested/file.json").is_none());
        assert!(indexed_json_path(&root, "not-json.txt").is_none());
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
