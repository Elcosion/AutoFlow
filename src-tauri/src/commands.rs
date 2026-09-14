use crate::automation::{asset_file_bytes, import_asset_file, AssetStore, VisionError};
use crate::hook::{MacroPlaybackStatus, MacroRecordingResult};
use crate::{
    storage, AppConfig, AppError, AppStatus, AutomationAsset, BehaviorApi, BehaviorProfileV2,
    BehaviorRecordingStatus, BehaviorSessionV2, MacroRecordingStatus, MacroRule, RuntimeState,
    SourceRetention, APP_NAME,
};
use serde::Serialize;
use tauri::AppHandle;
use tauri::Manager;
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
pub fn get_config(state: State<'_, RuntimeState>) -> Result<AppConfig, AppError> {
    state.config()
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
    storage::save_config(app, &config)?;
    state.replace_config(config.clone())?;
    Ok(config)
}

fn asset_root(app: &AppHandle) -> Result<std::path::PathBuf, AppError> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|error| {
            AppError::with_detail(
                "asset_directory_failed",
                "无法定位 AutoFlow 图像资源目录",
                error.to_string(),
            )
        })?
        .join("assets")
        .join("images"))
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
        .map(|asset| asset.id.clone())
        .collect::<Vec<_>>();
    let asset = import_asset_file(&asset_root(&app)?, &occupied, &name, &file_name, &bytes)
        .map_err(map_vision_error)?;
    let mut next = config;
    next.assets.push(asset.clone());
    let _ = persist_config(&app, &state, next)?;
    Ok(asset)
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
    name: String,
) -> Result<AutomationAsset, AppError> {
    let mut config = state.config()?;
    let asset = config
        .assets
        .iter_mut()
        .find(|asset| asset.id == asset_id)
        .ok_or_else(|| AppError::invalid("asset_not_found", "找不到图像资源"))?;
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 128 {
        return Err(AppError::invalid(
            "asset_name_invalid",
            "图像资源名称不能为空且不能超过 128 个字符",
        ));
    }
    asset.name = name.to_string();
    let result = asset.clone();
    persist_config(&app, &state, config)?;
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
            source.contains(&format!("\"{asset_id}\"")) || source.contains(&asset_id)
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
    Ok(state.macro_recording_status())
}

#[tauri::command]
pub fn play_macro(state: State<'_, RuntimeState>, macro_rule: MacroRule) -> Result<(), AppError> {
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
    Ok(state.macro_playback_status())
}

#[tauri::command]
pub fn validate_rhai_source(source: String) -> Result<(), AppError> {
    crate::rhai_runtime::validate_rhai_source(&source)
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
    Ok(profile)
}

#[tauri::command]
pub fn export_behavior_profile_v2(
    state: State<'_, RuntimeState>,
    profile_id: String,
) -> Result<String, AppError> {
    let config = state.config()?;
    let profile = config
        .behavior_profiles_v2
        .iter()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| AppError::invalid("behavior_v2_profile_not_found", "V2 行为档案不存在"))?;
    pretty_json(
        profile,
        "behavior_v2_profile_encode_failed",
        "V2 行为档案导出失败",
    )
}

#[tauri::command]
pub fn export_behavior_session_v2(
    state: State<'_, RuntimeState>,
    session_id: String,
) -> Result<String, AppError> {
    let config = state.config()?;
    let session = config
        .behavior_sessions_v2
        .iter()
        .find(|session| session.id == session_id)
        .ok_or_else(|| {
            AppError::invalid(
                "behavior_v2_session_not_retained",
                "该 V2 原始 Session 未保留在本机",
            )
        })?;
    pretty_json(
        session,
        "behavior_v2_session_encode_failed",
        "V2 原始 Session 导出失败",
    )
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

fn pretty_json<T: Serialize>(value: &T, code: &str, message: &str) -> Result<String, AppError> {
    serde_json::to_string_pretty(value)
        .map_err(|error| AppError::with_detail(code, message, error.to_string()))
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
    Ok(state.behavior_recording_status())
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
