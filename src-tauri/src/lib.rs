use log::{info, Level, LevelFilter, Log, Metadata, Record};
use serde::Serialize;
use std::sync::Mutex;
use tauri::Manager;

pub mod automation;
mod autostart;
mod behavior;
mod commands;
mod config;
mod hook;
mod rhai_runtime;
mod storage;

pub(crate) const APP_NAME: &str = "AutoFlow";

pub use automation::AutomationAsset;
pub use behavior::v2::*;
pub use behavior::{
    BehaviorApi, BehaviorEvent, BehaviorProfile, BehaviorProfileFile, BehaviorRecordingResult,
    BehaviorRecordingStatus, BiomimeticInput, BiomimeticInputFile, BiomimeticRuntime,
};
pub use config::{
    AppConfig, AutomationProgram, HotkeyAction, HotkeyRule, KeyAction, MacroMode, MacroRule,
    MacroRuleFile, MacroStep, MacroTarget, MouseButton, TextExpansionRule,
};
use hook::{HookService, MacroPlaybackStatus, MacroRecordingResult, MacroRecordingStatus};

struct SimpleLogger;

static LOGGER: SimpleLogger = SimpleLogger;

impl Log for SimpleLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= Level::Info
            || (metadata.level() == Level::Debug && behavior_debug_logging_enabled())
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            println!("[{}] {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

fn behavior_debug_logging_enabled() -> bool {
    std::env::var("AUTOFLOW_BEHAVIOR_DEBUG")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
        .unwrap_or(false)
}

#[derive(Debug, Clone, Serialize)]
pub struct AppError {
    pub code: String,
    pub message: String,
    pub detail: Option<String>,
}

impl AppError {
    pub fn invalid(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            detail: None,
        }
    }

    pub fn with_detail(
        code: impl Into<String>,
        message: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            detail: Some(detail.into()),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::invalid("internal_error", message)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AppStatus {
    pub app_name: String,
    pub version: String,
    pub core_state: String,
}

pub struct RuntimeState {
    core_state: Mutex<String>,
    config: Mutex<AppConfig>,
    hook: HookService,
}

impl RuntimeState {
    pub(crate) fn new() -> Result<Self, AppError> {
        let config = AppConfig::default();
        let vision = automation::VisionService::new(
            std::env::temp_dir()
                .join("AutoFlow")
                .join("data")
                .join("images"),
        );
        Ok(Self {
            core_state: Mutex::new("待机".to_string()),
            hook: HookService::start(config.clone(), vision)?,
            config: Mutex::new(config),
        })
    }

    pub(crate) fn configure_vision(&self, app: &tauri::AppHandle) -> Result<(), AppError> {
        let root = storage::managed_image_directory(app)?;
        self.hook.set_vision(automation::VisionService::new(root))
    }

    pub(crate) fn current_state(&self) -> Result<String, AppError> {
        self.core_state
            .lock()
            .map(|state| state.clone())
            .map_err(|_| AppError::internal("运行状态读取失败，请重启 AutoFlow"))
    }

    pub(crate) fn config(&self) -> Result<AppConfig, AppError> {
        self.config
            .lock()
            .map(|config| config.clone())
            .map_err(|_| AppError::internal("配置状态异常，请重启 AutoFlow"))
    }

    pub(crate) fn replace_config(&self, config: AppConfig) -> Result<(), AppError> {
        self.hook.update_config(config.clone())?;
        let mut current = self
            .config
            .lock()
            .map_err(|_| AppError::internal("配置状态异常，请重启 AutoFlow"))?;
        *current = config;
        Ok(())
    }

    pub(crate) fn emergency_stop(&self) {
        self.hook.emergency_stop();
    }

    pub(crate) fn start_recording(
        &self,
        capture_mouse_move: bool,
        capture_mouse_clicks: bool,
    ) -> Result<(), AppError> {
        self.hook
            .start_recording(capture_mouse_move, capture_mouse_clicks)
    }

    pub(crate) fn set_recording_options(
        &self,
        capture_mouse_move: bool,
        capture_mouse_clicks: bool,
    ) -> Result<(), AppError> {
        self.hook
            .set_recording_options(capture_mouse_move, capture_mouse_clicks)
    }

    pub(crate) fn stop_recording(
        &self,
        discard_trailing_mouse_input: bool,
    ) -> Result<MacroRecordingResult, AppError> {
        self.hook.stop_recording(discard_trailing_mouse_input)
    }

    pub(crate) fn play_macro(&self, macro_rule: MacroRule) -> Result<(), AppError> {
        self.hook.play_macro(macro_rule)
    }

    pub(crate) fn stop_macro(&self) {
        self.hook.stop_macro();
    }

    pub(crate) fn is_macro_playing(&self) -> bool {
        self.hook.is_playback_running()
    }

    pub(crate) fn macro_playback_status(&self) -> MacroPlaybackStatus {
        self.hook.playback_status()
    }

    pub(crate) fn macro_recording_status(&self) -> MacroRecordingStatus {
        self.hook.recording_status()
    }

    pub(crate) fn start_behavior_recording(&self, name: String) -> Result<(), AppError> {
        self.hook.start_behavior_recording(name)
    }

    pub(crate) fn stop_behavior_recording(&self) -> Result<BehaviorRecordingResult, AppError> {
        self.hook.stop_behavior_recording()
    }

    pub(crate) fn behavior_recording_status(&self) -> BehaviorRecordingStatus {
        self.hook.behavior_recording_status()
    }

    pub(crate) fn behavior_api(&self, profile_id: &str) -> Result<BehaviorApi, AppError> {
        let config = self.config()?;
        if let Some(profile) = config
            .behavior_profiles_v2
            .iter()
            .find(|profile| profile.id == profile_id)
        {
            return Ok(profile.generated_api());
        }
        let profile = config
            .behavior_profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .ok_or_else(|| AppError::invalid("behavior_profile_not_found", "找不到行为档案"))?;
        Ok(profile.generated_api())
    }
}

fn init_logging() {
    let maximum = if behavior_debug_logging_enabled() {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    };
    let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(maximum));
}

pub fn run() {
    init_logging();
    info!("启动 {APP_NAME} Rust Core");
    let runtime = RuntimeState::new().expect("初始化 AutoFlow 输入服务失败");
    tauri::Builder::default()
        .manage(runtime)
        .invoke_handler(tauri::generate_handler![
            commands::get_app_status,
            commands::ping,
            commands::get_config,
            commands::open_data_directory,
            commands::save_config,
            commands::import_asset,
            commands::read_asset,
            commands::rename_asset,
            commands::delete_asset,
            commands::run_vision_diagnostic,
            commands::emergency_stop,
            commands::start_macro_recording,
            commands::set_macro_recording_options,
            commands::stop_macro_recording,
            commands::get_macro_recording_status,
            commands::play_macro,
            commands::stop_macro,
            commands::is_macro_playing,
            commands::get_macro_playback_status,
            commands::validate_rhai_source,
            commands::start_behavior_recording,
            commands::stop_behavior_recording,
            commands::delete_behavior_profile_v2,
            commands::delete_behavior_session_v2,
            commands::retrain_behavior_profile_v2,
            commands::get_behavior_recording_status,
            commands::generate_behavior_api
        ])
        .setup(|app| {
            let state = app.state::<RuntimeState>();
            if let Err(error) = state.configure_vision(app.handle()) {
                log::warn!(
                    "视觉服务目录初始化失败，视觉功能将在使用时重试: {}",
                    error.message
                );
            }
            if let Ok(config) = storage::load_config(app.handle()) {
                if let Err(error) = state.replace_config(config) {
                    log::error!("配置应用失败: {}", error.message);
                }
            } else {
                log::error!("配置加载失败，AutoFlow 将使用安全默认配置");
            }
            info!("Tauri 桌面窗口已初始化");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("启动 AutoFlow 失败");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_error_is_structured_for_frontend() {
        let error = AppError::internal("测试错误");
        assert_eq!(error.code, "internal_error");
        assert_eq!(error.message, "测试错误");
        assert!(error.detail.is_none());
    }

    #[test]
    fn runtime_state_starts_empty_for_unit_tests() {
        let state = RuntimeState::new().expect("runtime should start");
        assert_eq!(
            state.current_state().expect("state lock should work"),
            "待机"
        );
    }
}
