use log::{info, Level, LevelFilter, Log, Metadata, Record};
use serde::Serialize;
use std::sync::Mutex;
use tauri::Manager;

mod autostart;
mod commands;
mod config;
mod hook;
mod storage;

pub(crate) const APP_NAME: &str = "AutoFlow";

pub use config::{
    AppConfig, HotkeyAction, HotkeyRule, KeyAction, MacroMode, MacroRule, MacroStep, MacroTarget,
    MouseButton, TextExpansionRule,
};
use hook::{HookService, MacroPlaybackStatus, MacroRecordingResult, MacroRecordingStatus};

struct SimpleLogger;

static LOGGER: SimpleLogger = SimpleLogger;

impl Log for SimpleLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            println!("[{}] {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
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
        Ok(Self {
            core_state: Mutex::new("待机".to_string()),
            hook: HookService::start(config.clone())?,
            config: Mutex::new(config),
        })
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

    pub(crate) fn start_recording(&self) -> Result<(), AppError> {
        self.hook.start_recording()
    }

    pub(crate) fn stop_recording(&self) -> Result<MacroRecordingResult, AppError> {
        self.hook.stop_recording()
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
}

fn init_logging() {
    let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(LevelFilter::Info));
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
            commands::save_config,
            commands::emergency_stop,
            commands::start_macro_recording,
            commands::stop_macro_recording,
            commands::get_macro_recording_status,
            commands::play_macro,
            commands::stop_macro,
            commands::is_macro_playing,
            commands::get_macro_playback_status
        ])
        .setup(|app| {
            if let Ok(config) = storage::load_config(app.handle()) {
                let state = app.state::<RuntimeState>();
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
