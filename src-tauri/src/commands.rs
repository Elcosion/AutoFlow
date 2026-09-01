use crate::hook::{MacroPlaybackStatus, MacroRecordingResult};
use crate::{
    storage, AppConfig, AppError, AppStatus, MacroRecordingStatus, MacroRule, RuntimeState,
    APP_NAME,
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
pub fn get_config(state: State<'_, RuntimeState>) -> Result<AppConfig, AppError> {
    state.config()
}

#[tauri::command]
pub fn save_config(
    app: AppHandle,
    state: State<'_, RuntimeState>,
    config: AppConfig,
) -> Result<AppConfig, AppError> {
    config.validate()?;
    crate::autostart::apply(config.launch_at_startup)?;
    storage::save_config(&app, &config)?;
    state.replace_config(config.clone())?;
    Ok(config)
}

#[tauri::command]
pub fn emergency_stop(state: State<'_, RuntimeState>) -> Result<(), AppError> {
    state.emergency_stop();
    Ok(())
}

#[tauri::command]
pub fn start_macro_recording(state: State<'_, RuntimeState>) -> Result<(), AppError> {
    state.start_recording()
}

#[tauri::command]
pub fn stop_macro_recording(
    state: State<'_, RuntimeState>,
) -> Result<MacroRecordingResult, AppError> {
    state.stop_recording()
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
