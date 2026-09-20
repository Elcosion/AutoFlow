//! UI-only proxy. No hooks or injected-input ledger are constructed here.
use crate::automation::VisionService;
use crate::hook::{MacroPlaybackStatus, MacroRecordingResult, MacroRecordingStatus};
use crate::runtime_notifications::RuntimeNotification;
use crate::runtime_service::{SafetyClient, SafetyCommand};
use crate::{AppConfig, AppError, BehaviorRecordingResult, BehaviorRecordingStatus, MacroRule};
use std::sync::Arc;

pub(crate) struct RuntimeBackend(SafetyClient);
impl RuntimeBackend {
    pub fn start(config: AppConfig, vision: Arc<VisionService>) -> Result<Self, AppError> {
        let executable =
            std::env::current_exe().map_err(|_| AppError::internal("安全服务程序路径不可用"))?;
        SafetyClient::spawn(&executable, config, vision.asset_root(), false).map(Self)
    }
    pub fn set_vision(&self, vision: Arc<VisionService>) -> Result<(), AppError> {
        self.0.call(SafetyCommand::ConfigureVision {
            root: vision.asset_root(),
        })
    }
    pub fn update_config(&self, config: AppConfig) -> Result<(), AppError> {
        self.0.call(SafetyCommand::UpdateConfig {
            config: Box::new(config),
        })
    }
    pub fn start_recording(&self, moves: bool, clicks: bool) -> Result<(), AppError> {
        self.0.call(SafetyCommand::StartRecording { moves, clicks })
    }
    pub fn set_recording_options(&self, moves: bool, clicks: bool) -> Result<(), AppError> {
        self.0
            .call(SafetyCommand::RecordingOptions { moves, clicks })
    }
    pub fn stop_recording(&self, discard_tail: bool) -> Result<MacroRecordingResult, AppError> {
        self.0.call(SafetyCommand::StopRecording { discard_tail })
    }
    pub fn start_behavior_recording(&self, name: String) -> Result<(), AppError> {
        self.0.call(SafetyCommand::StartBehavior { name })
    }
    pub fn stop_behavior_recording(&self) -> Result<BehaviorRecordingResult, AppError> {
        self.0.call(SafetyCommand::StopBehavior)
    }
    pub fn discard_behavior_recording(&self) -> Result<(), AppError> {
        self.0.call(SafetyCommand::DiscardBehavior)
    }
    pub fn complete_behavior_recording_claim(&self) -> Result<(), AppError> {
        self.0.call(SafetyCommand::CompleteBehaviorClaim)
    }
    pub fn play_macro(&self, rule: MacroRule) -> Result<(), AppError> {
        self.0.call(SafetyCommand::Play {
            rule: Box::new(rule),
        })
    }
    pub fn emergency_stop(&self) {
        let _ = self.0.stop_signal();
    }
    pub fn stop_macro(&self) {
        self.emergency_stop();
    }
    pub fn recover_input_safety(&self) -> Result<(), AppError> {
        self.0.call(SafetyCommand::Recover)
    }
    pub fn shutdown(&self) -> bool {
        match self.0.shutdown_confirmed() {
            Ok(()) => true,
            Err(error) => {
                log::error!("安全停机未确认: {} ({})", error.message, error.code);
                false
            }
        }
    }
    pub fn is_playback_running(&self) -> bool {
        // Unknown authority state must not be treated as permission to start.
        self.0.call(SafetyCommand::IsPlaying).unwrap_or(true)
    }
    pub fn playback_status(&self) -> Result<MacroPlaybackStatus, AppError> {
        self.0.call(SafetyCommand::PlaybackStatus).or_else(|error| {
            if error.code == "safety_service_busy" {
                return Err(error);
            }
            Ok(MacroPlaybackStatus {
                running: false,
                current_step: 0,
                total_steps: 0,
                last_error: Some(error.message),
                playback_id: 0,
                macro_id: None,
                macro_name: None,
                program_kind: "unknown".into(),
                action_kind: None,
                action_summary: None,
                elapsed_ms: 0,
                phase: "fault_locked".into(),
                cleanup_status: "unknown".into(),
                overlay_visible: false,
            })
        })
    }
    pub fn recording_status(&self) -> Result<MacroRecordingStatus, AppError> {
        self.0.call(SafetyCommand::RecordingStatus)
    }
    pub fn behavior_recording_status(&self) -> Result<BehaviorRecordingStatus, AppError> {
        self.0.call(SafetyCommand::BehaviorStatus)
    }
    pub fn take_runtime_notification(&self) -> Option<RuntimeNotification> {
        self.0.call(SafetyCommand::Notification).unwrap_or(None)
    }
    pub fn acknowledge_runtime_notification(&self, id: u64) -> bool {
        self.0
            .call(SafetyCommand::Acknowledge { id })
            .unwrap_or(false)
    }
}
