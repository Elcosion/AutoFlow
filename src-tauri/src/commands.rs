use crate::automation::{asset_file_bytes, import_asset_file, AssetStore, VisionError};
use crate::hook::{MacroPlaybackStatus, MacroRecordingResult};
use crate::{
    storage, AppConfig, AppError, AppStatus, AutomationAsset, BehaviorApi, BehaviorProfileV2,
    BehaviorRecordingStatus, BehaviorSessionV2, MacroRecordingStatus, MacroRule, RuntimeState,
    SourceRetention, APP_NAME,
};
use tauri::AppHandle;
use tauri::State;

#[tauri::command]
pub fn get_app_status(state: State<'_, RuntimeState>) -> Result<AppStatus, AppError> {
    Ok(AppStatus {
        app_name: APP_NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        core_state: state.current_state()?,
    })
}

#[tauri::command]
pub fn ping() -> Result<String, AppError> {
    Ok("AutoFlow Rust Core 已连接".to_string())
}

#[tauri::command]
pub fn get_config(app: AppHandle, state: State<'_, RuntimeState>) -> Result<AppConfig, AppError> {
    let config = storage::load_config(&app)?;
    state.replace_config(config.clone())?;
    Ok(config)
}

#[tauri::command]
pub fn open_data_directory(
    app: AppHandle,
    subdirectory: Option<String>,
) -> Result<String, AppError> {
    let root = storage::managed_data_root(&app)?;
    let directory = match subdirectory.as_deref() {
        None => root,
        Some(name @ ("profiles" | "sessions" | "scripts" | "images")) => root.join(name),
        Some(_) => {
            return Err(AppError::invalid(
                "data_directory_invalid",
                "不支持的数据子目录",
            ));
        }
    };
    #[cfg(windows)]
    {
        std::process::Command::new("explorer.exe")
            .arg(&directory)
            .spawn()
            .map_err(|error| {
                AppError::with_detail(
                    "data_directory_open_failed",
                    "无法打开 AutoFlow 数据文件夹",
                    error.to_string(),
                )
            })?;
        Ok(directory.to_string_lossy().into_owned())
    }
    #[cfg(not(windows))]
    {
        let _ = directory;
        Err(AppError::invalid(
            "data_directory_unsupported",
            "当前平台暂不支持打开数据文件夹",
        ))
    }
}

#[tauri::command]
pub fn save_config(
    app: AppHandle,
    state: State<'_, RuntimeState>,
    config: AppConfig,
) -> Result<AppConfig, AppError> {
    persist_config(&app, &state, config)
}

fn persist_config(
    app: &AppHandle,
    state: &State<'_, RuntimeState>,
    config: AppConfig,
) -> Result<AppConfig, AppError> {
    let (config, _) = config.migrate()?;
    config.validate()?;
    crate::autostart::apply(config.launch_at_startup)?;
    let config = storage::save_config(app, &config)?;
    state.replace_config(config.clone())?;
    Ok(config)
}

fn asset_root(app: &AppHandle) -> Result<std::path::PathBuf, AppError> {
    storage::managed_image_directory(app)
}

fn map_vision_error(error: VisionError) -> AppError {
    AppError::invalid(error.code, error.message)
}

#[tauri::command]
pub fn import_asset(
    app: AppHandle,
    state: State<'_, RuntimeState>,
    name: String,
    file_name: String,
    bytes: Vec<u8>,
) -> Result<AutomationAsset, AppError> {
    let config = state.config()?;
    let occupied = config
        .assets
        .iter()
        .flat_map(|asset| [asset.id.clone(), asset.file_name.clone()])
        .collect::<Vec<_>>();
    let asset = import_asset_file(&asset_root(&app)?, &occupied, &name, &file_name, &bytes)
        .map_err(map_vision_error)?;
    let mut next = config;
    next.assets.push(asset.clone());
    let saved = persist_config(&app, &state, next)?;
    saved
        .assets
        .into_iter()
        .find(|saved_asset| saved_asset.file_name == asset.file_name)
        .ok_or_else(|| AppError::internal("图像资源保存后未出现在资源目录中"))
}

#[tauri::command]
pub fn read_asset(
    app: AppHandle,
    state: State<'_, RuntimeState>,
    asset_id: String,
) -> Result<Vec<u8>, AppError> {
    let asset = state
        .config()?
        .assets
        .into_iter()
        .find(|asset| asset.id == asset_id)
        .ok_or_else(|| AppError::invalid("asset_not_found", "找不到图像资源"))?;
    asset_file_bytes(&asset_root(&app)?, &asset).map_err(map_vision_error)
}

#[tauri::command]
pub fn rename_asset(
    app: AppHandle,
    state: State<'_, RuntimeState>,
    asset_id: String,
    file_name: String,
) -> Result<AutomationAsset, AppError> {
    let mut config = state.config()?;
    let index = config
        .assets
        .iter()
        .position(|asset| asset.id == asset_id)
        .ok_or_else(|| AppError::invalid("asset_not_found", "找不到图像资源"))?;
    let file_name = file_name.trim();
    if file_name.is_empty() || file_name.chars().count() > 128 {
        return Err(AppError::invalid(
            "asset_name_invalid",
            "图像资源文件名不能为空且不能超过 128 个字符",
        ));
    }
    let previous = config.assets[index].clone();
    let previous_extension = std::path::Path::new(&previous.file_name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let next_extension = std::path::Path::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if !previous_extension.eq_ignore_ascii_case(next_extension) {
        return Err(AppError::invalid(
            "asset_extension_mismatch",
            "重命名时必须保留原图像扩展名",
        ));
    }
    if previous.file_name.eq_ignore_ascii_case(file_name) {
        return Ok(previous);
    }
    let store = AssetStore::new(asset_root(&app)?);
    store
        .rename_file(&previous, file_name)
        .map_err(map_vision_error)?;
    config.assets[index].name = file_name.to_string();
    config.assets[index].file_name = file_name.to_string();
    for macro_rule in &mut config.macros {
        if let crate::AutomationProgram::Rhai { source, .. } = &mut macro_rule.program {
            *source = source.replace(
                &format!("\"{}\"", previous.file_name),
                &format!("\"{file_name}\""),
            );
        }
    }
    let result = config.assets[index].clone();
    if let Err(error) = persist_config(&app, &state, config) {
        let _ = store.rename_file(&result, &previous.file_name);
        return Err(error);
    }
    Ok(result)
}

#[tauri::command]
pub fn delete_asset(
    app: AppHandle,
    state: State<'_, RuntimeState>,
    asset_id: String,
    confirmed: bool,
) -> Result<(), AppError> {
    let mut config = state.config()?;
    let Some(index) = config.assets.iter().position(|asset| asset.id == asset_id) else {
        return Err(AppError::invalid("asset_not_found", "找不到图像资源"));
    };
    let referenced = config.macros.iter().any(|macro_rule| {
        if let crate::AutomationProgram::Rhai { source, .. } = &macro_rule.program {
            source.contains(&format!("\"{}\"", config.assets[index].file_name))
                || source.contains(&format!("\"{asset_id}\""))
        } else {
            false
        }
    });
    if referenced && !confirmed {
        return Err(AppError::invalid(
            "asset_in_use",
            "该资源仍被 Rhai 脚本引用，请确认后再删除",
        ));
    }
    let asset = config.assets[index].clone();
    AssetStore::new(asset_root(&app)?)
        .remove_file(&asset)
        .map_err(map_vision_error)?;
    config.assets.remove(index);
    let _ = persist_config(&app, &state, config)?;
    Ok(())
}

#[tauri::command]
pub fn run_vision_diagnostic(duration_ms: Option<u64>) -> Result<String, AppError> {
    let duration = std::time::Duration::from_millis(duration_ms.unwrap_or(0).min(600_000));
    Ok(crate::automation::run_vision_diagnostic(duration))
}

#[tauri::command]
pub fn emergency_stop(state: State<'_, RuntimeState>) -> Result<(), AppError> {
    state.emergency_stop();
    Ok(())
}

#[tauri::command]
pub fn start_macro_recording(
    state: State<'_, RuntimeState>,
    capture_mouse_move: bool,
    capture_mouse_clicks: bool,
) -> Result<(), AppError> {
    state.start_recording(capture_mouse_move, capture_mouse_clicks)
}

#[tauri::command]
pub fn set_macro_recording_options(
    state: State<'_, RuntimeState>,
    capture_mouse_move: bool,
    capture_mouse_clicks: bool,
) -> Result<(), AppError> {
    state.set_recording_options(capture_mouse_move, capture_mouse_clicks)
}

#[tauri::command]
pub fn stop_macro_recording(
    state: State<'_, RuntimeState>,
    discard_trailing_mouse_input: bool,
) -> Result<MacroRecordingResult, AppError> {
    state.stop_recording(discard_trailing_mouse_input)
}

#[tauri::command]
pub fn get_macro_recording_status(
    state: State<'_, RuntimeState>,
) -> Result<MacroRecordingStatus, AppError> {
    state.macro_recording_status()
}

#[tauri::command]
pub fn play_macro(state: State<'_, RuntimeState>, macro_rule: MacroRule) -> Result<(), AppError> {
    if let Some(message) = &macro_rule.import_error {
        return Err(AppError::invalid(
            "macro_import_invalid",
            format!("该宏来自不合法文件，修复并保存后才能运行：{message}"),
        ));
    }
    state.play_macro(macro_rule)
}

#[tauri::command]
pub fn stop_macro(state: State<'_, RuntimeState>) -> Result<(), AppError> {
    state.stop_macro();
    Ok(())
}

#[tauri::command]
pub fn is_macro_playing(state: State<'_, RuntimeState>) -> Result<bool, AppError> {
    Ok(state.is_macro_playing())
}

#[tauri::command]
pub fn get_macro_playback_status(
    state: State<'_, RuntimeState>,
) -> Result<MacroPlaybackStatus, AppError> {
    state.macro_playback_status()
}

#[tauri::command]
pub fn recover_input_safety(
    window: tauri::WebviewWindow,
    state: State<'_, RuntimeState>,
) -> Result<(), AppError> {
    if window.label() != "main" {
        return Err(AppError::invalid(
            "safety_recovery_forbidden",
            "只有主界面可以请求安全恢复",
        ));
    }
    state.recover_input_safety()
}

#[tauri::command]
pub fn take_runtime_notification(
    window: tauri::WebviewWindow,
    state: State<'_, RuntimeState>,
) -> Result<Option<crate::runtime_notifications::RuntimeNotification>, AppError> {
    if window.label() != "main" {
        return Err(AppError::invalid(
            "notification_access_denied",
            "仅主界面可以领取运行通知",
        ));
    }
    Ok(state.take_runtime_notification())
}

#[tauri::command]
pub fn acknowledge_runtime_notification(
    window: tauri::WebviewWindow,
    state: State<'_, RuntimeState>,
    id: u64,
) -> Result<bool, AppError> {
    if window.label() != "main" {
        return Err(AppError::invalid(
            "notification_access_denied",
            "仅主界面可以确认运行通知",
        ));
    }
    Ok(state.acknowledge_runtime_notification(id))
}

#[tauri::command]
pub fn validate_rhai_source(
    source: String,
) -> Result<crate::rhai_runtime::RhaiValidationReport, AppError> {
    crate::rhai_runtime::inspect_rhai_source(&source)
        .map_err(|message| AppError::invalid("rhai_compile_error", message))
}

#[tauri::command]
pub fn start_behavior_recording(
    state: State<'_, RuntimeState>,
    name: String,
) -> Result<(), AppError> {
    state.start_behavior_recording(name)
}

#[tauri::command]
pub fn stop_behavior_recording(
    app: AppHandle,
    state: State<'_, RuntimeState>,
) -> Result<BehaviorProfileV2, AppError> {
    let captured = state.stop_behavior_recording()?;
    let session = crate::BehaviorSessionV2::from_legacy_profile(&captured.profile)?;
    let source_retention = if captured.persist_raw_session {
        crate::SourceRetention::Persisted
    } else {
        crate::SourceRetention::Ephemeral
    };
    let profile = crate::train_behavior_profile_with_retention(&session, source_retention)?;
    let mut config = state.config()?;
    config
        .behavior_sessions_v2
        .retain(|existing| existing.id != session.id);
    if captured.persist_raw_session {
        config.behavior_sessions_v2.push(session);
    }
    config
        .behavior_profiles_v2
        .retain(|existing| existing.id != profile.id);
    config.behavior_profiles_v2.push(profile.clone());
    config.active_behavior_profile_v2_id = Some(profile.id.clone());
    config.behavior_policy.profile_id = Some(profile.id.clone());
    let _ = persist_config(&app, &state, config)?;
    state.complete_behavior_recording_claim()?;
    Ok(profile)
}

#[tauri::command]
pub fn discard_behavior_recording(state: State<'_, RuntimeState>) -> Result<(), AppError> {
    state.discard_behavior_recording()
}

#[tauri::command]
pub fn delete_behavior_profile_v2(
    app: AppHandle,
    state: State<'_, RuntimeState>,
    profile_id: String,
    confirmed: bool,
) -> Result<AppConfig, AppError> {
    let mut config = state.config()?;
    let Some(_index) = config
        .behavior_profiles_v2
        .iter()
        .position(|profile| profile.id == profile_id)
    else {
        return Err(AppError::invalid(
            "behavior_v2_profile_not_found",
            "V2 行为档案不存在",
        ));
    };
    let referenced = config.active_behavior_profile_v2_id.as_deref() == Some(&profile_id)
        || config.behavior_policy.profile_id.as_deref() == Some(&profile_id)
        || config.macros.iter().any(|macro_rule| {
            macro_rule
                .behavior_policy
                .as_ref()
                .and_then(|policy| policy.profile_id.as_deref())
                == Some(&profile_id)
        });
    if referenced && !confirmed {
        return Err(AppError::invalid(
            "behavior_v2_profile_in_use",
            "该 V2 行为档案仍被全局策略或宏引用，请确认后再删除",
        ));
    }

    clear_behavior_profile_references(&mut config, &profile_id);
    let saved = persist_config(&app, &state, config)?;
    storage::delete_behavior_profile_v2(&app, &profile_id)?;
    Ok(saved)
}

#[tauri::command]
pub fn delete_behavior_session_v2(
    app: AppHandle,
    state: State<'_, RuntimeState>,
    session_id: String,
    confirmed: bool,
) -> Result<AppConfig, AppError> {
    let mut config = state.config()?;
    let Some(index) = config
        .behavior_sessions_v2
        .iter()
        .position(|session| session.id == session_id)
    else {
        return Err(AppError::invalid(
            "behavior_v2_session_not_found",
            "V2 原始训练 Session 不存在",
        ));
    };
    let referenced = config.behavior_profiles_v2.iter().any(|profile| {
        profile
            .source_session_ids
            .iter()
            .any(|id| id == &session_id)
    });
    if referenced && !confirmed {
        return Err(AppError::invalid(
            "behavior_v2_session_in_use",
            "该 Session 仍是行为档案的训练来源，请确认后再删除",
        ));
    }
    if confirmed {
        for profile in &mut config.behavior_profiles_v2 {
            if profile
                .source_session_ids
                .iter()
                .any(|id| id == &session_id)
            {
                profile.source_retention = SourceRetention::Ephemeral;
            }
        }
    }
    config.behavior_sessions_v2.remove(index);
    let saved = persist_config(&app, &state, config)?;
    storage::delete_behavior_session_v2(&app, &session_id)?;
    Ok(saved)
}

#[tauri::command]
pub fn retrain_behavior_profile_v2(
    app: AppHandle,
    state: State<'_, RuntimeState>,
    session_id: String,
) -> Result<BehaviorProfileV2, AppError> {
    let config = state.config()?;
    let session: BehaviorSessionV2 = config
        .behavior_sessions_v2
        .iter()
        .find(|session| session.id == session_id)
        .cloned()
        .ok_or_else(|| {
            AppError::invalid(
                "behavior_v2_session_not_retained",
                "该 V2 原始 Session 未保留在本机",
            )
        })?;
    let mut profile =
        crate::train_behavior_profile_with_retention(&session, SourceRetention::Persisted)?;
    if let Some(existing) = config.behavior_profiles_v2.iter().find(|profile| {
        profile
            .source_session_ids
            .iter()
            .any(|id| id == &session_id)
    }) {
        profile.id = existing.id.clone();
    }
    let mut next = config;
    next.behavior_profiles_v2
        .retain(|existing| existing.id != profile.id);
    next.behavior_profiles_v2.push(profile.clone());
    next.active_behavior_profile_v2_id = Some(profile.id.clone());
    next.behavior_policy.profile_id = Some(profile.id.clone());
    persist_config(&app, &state, next)?;
    Ok(profile)
}

fn clear_behavior_profile_references(config: &mut AppConfig, profile_id: &str) {
    config
        .behavior_profiles_v2
        .retain(|profile| profile.id != profile_id);
    let replacement_profile_id = config
        .behavior_profiles_v2
        .first()
        .map(|profile| profile.id.clone());
    if config.active_behavior_profile_v2_id.as_deref() == Some(profile_id) {
        config.active_behavior_profile_v2_id = replacement_profile_id.clone();
    }
    if config.behavior_policy.profile_id.as_deref() == Some(profile_id) {
        config.behavior_policy.profile_id = replacement_profile_id;
    }
    for macro_rule in &mut config.macros {
        if macro_rule
            .behavior_policy
            .as_ref()
            .and_then(|policy| policy.profile_id.as_deref())
            == Some(profile_id)
        {
            // Removing the override makes the macro inherit the now-valid
            // global policy instead of leaving an enabled policy with no
            // usable profile behind.
            macro_rule.behavior_policy = None;
        }
    }
}

#[tauri::command]
pub fn get_behavior_recording_status(
    state: State<'_, RuntimeState>,
) -> Result<BehaviorRecordingStatus, AppError> {
    state.behavior_recording_status()
}

#[tauri::command]
pub fn generate_behavior_api(
    state: State<'_, RuntimeState>,
    profile_id: String,
) -> Result<BehaviorApi, AppError> {
    state.behavior_api(&profile_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        train_behavior_profile_with_retention, AppConfig, AutomationProgram,
        BehaviorCaptureMetadata, BehaviorPolicy, MacroMode, MacroStep, SourceRetention,
    };

    fn test_profile(id: &str) -> BehaviorProfileV2 {
        let session = BehaviorSessionV2 {
            id: format!("session-{id}"),
            name: format!("Session {id}"),
            api_version: crate::BEHAVIOR_V2_API_VERSION,
            created_at_ms: 1,
            duration_ms: 0,
            task_tag: None,
            capture_metadata: BehaviorCaptureMetadata::default(),
            raw_events: Vec::new(),
        };
        let mut profile =
            train_behavior_profile_with_retention(&session, SourceRetention::Persisted).unwrap();
        profile.id = id.to_string();
        profile
    }

    #[test]
    fn deleting_a_profile_clears_references_and_rebinds_global_policy() {
        let removed = test_profile("profile-removed");
        let replacement = test_profile("profile-replacement");
        let mut config = AppConfig {
            behavior_profiles_v2: vec![removed, replacement],
            active_behavior_profile_v2_id: Some("profile-removed".to_string()),
            behavior_policy: BehaviorPolicy {
                enabled: true,
                profile_id: Some("profile-removed".to_string()),
                ..BehaviorPolicy::default()
            },
            ..AppConfig::default()
        };
        config.macros.push(MacroRule {
            id: "macro-1".to_string(),
            name: "Macro".to_string(),
            import_error: None,
            enabled: false,
            trigger_keys: vec![],
            mode: MacroMode::Once,
            repeat_count: 1,
            speed: 1.0,
            record_mouse_move: true,
            record_mouse_clicks: true,
            target: None,
            behavior_policy: Some(BehaviorPolicy {
                enabled: true,
                profile_id: Some("profile-removed".to_string()),
                ..BehaviorPolicy::default()
            }),
            program: AutomationProgram::Macro {
                steps: vec![MacroStep::Delay {
                    duration_ms: 1,
                    duration_max_ms: None,
                }],
            },
        });

        clear_behavior_profile_references(&mut config, "profile-removed");

        assert_eq!(
            config
                .behavior_profiles_v2
                .iter()
                .map(|profile| profile.id.as_str())
                .collect::<Vec<_>>(),
            vec!["profile-replacement"]
        );
        assert_eq!(
            config.active_behavior_profile_v2_id.as_deref(),
            Some("profile-replacement")
        );
        assert_eq!(
            config.behavior_policy.profile_id.as_deref(),
            Some("profile-replacement")
        );
        assert!(config.macros[0].behavior_policy.is_none());
    }
}
