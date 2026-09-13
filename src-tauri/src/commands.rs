use crate::automation::{asset_file_bytes, import_asset_file, AssetStore, VisionError};
use crate::hook::{MacroPlaybackStatus, MacroRecordingResult};
use crate::{
    storage, AppConfig, AppError, AppStatus, AutomationAsset, MacroRecordingStatus, MacroRule,
    RuntimeState, APP_NAME,
};
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
