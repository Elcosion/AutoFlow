//! UI-only proxy. No hooks or injected-input ledger are constructed here.
use crate::automation::VisionService;
use crate::hook::{MacroPlaybackStatus, MacroRecordingResult, MacroRecordingStatus};
use crate::runtime_notifications::RuntimeNotification;
use crate::runtime_service::{SafetyClient, SafetyCommand};
use crate::{AppConfig, AppError, BehaviorRecordingResult, BehaviorRecordingStatus, MacroRule};
use std::sync::Arc;

pub(crate) struct RuntimeBackend(SafetyClient);

fn preserve_playback_status_observation(
    response: Result<MacroPlaybackStatus, AppError>,
) -> Result<MacroPlaybackStatus, AppError> {
    // Transport failures have no controller provenance and must remain errors.
    response
}

fn playback_activity_fail_closed(response: Result<bool, AppError>) -> bool {
    response.unwrap_or(true)
}

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
        playback_activity_fail_closed(self.0.call(SafetyCommand::IsPlaying))
    }
    pub fn playback_status(&self) -> Result<MacroPlaybackStatus, AppError> {
        preserve_playback_status_observation(self.0.call(SafetyCommand::PlaybackStatus))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_failures_never_become_confirmed_fault_statuses() {
        for code in [
            "safety_service_busy",
            "safety_service_unavailable",
            "safety_service_stale_request",
        ] {
            let result = preserve_playback_status_observation(Err(AppError::invalid(code, "x")));
            assert_eq!(
                result.expect_err("RPC failure must remain an error").code,
                code
            );
        }
    }

    #[test]
    fn rpc_failures_remain_fail_closed_for_playback_admission() {
        assert!(!playback_activity_fail_closed(Ok(false)));
        assert!(playback_activity_fail_closed(Ok(true)));
        for code in [
            "safety_service_busy",
            "safety_service_unavailable",
            "safety_service_stale_request",
        ] {
            assert!(playback_activity_fail_closed(Err(AppError::invalid(
                code, "x"
            ))));
        }
    }
}
