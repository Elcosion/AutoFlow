use crate::automation::VisionService;
use crate::behavior::v2::{BehaviorPolicy, BehaviorRuntimeV2};
use crate::behavior::{
    BehaviorRecorder, BehaviorRecordingResult, BehaviorRecordingStatus, DelayKind,
};
#[cfg(windows)]
use crate::input_safety::{
    CleanupReport, InjectedInputState, InputBroker, InputPermit, SafetyDiagnostics,
};
#[cfg(windows)]
use crate::rhai_runtime::{validate_rhai_source, AutomationInput, CANCELLED};
#[cfg(windows)]
use crate::runtime_control::{RunToken, RuntimeController, RuntimePhase, StartError};
use crate::{
    AppConfig, AppError, AutomationProgram, KeyAction, MacroMode, MacroRule, MacroStep,
    MacroTarget, MouseButton,
};
use serde::Serialize;
use std::sync::{Arc, Mutex};

#[cfg(windows)]
use std::collections::{HashMap, HashSet};
#[cfg(windows)]
use std::process::Command;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
#[cfg(windows)]
use std::thread;
#[cfg(windows)]
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct HookService {
    shared: Arc<HookShared>,
}

#[cfg(windows)]
static FRONTEND_PROCESS_ID: AtomicU32 = AtomicU32::new(0);
#[cfg(windows)]
fn frontend_process_id() -> u32 {
    let configured = FRONTEND_PROCESS_ID.load(Ordering::Acquire);
    if configured == 0 {
        std::process::id()
    } else {
        configured
    }
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MacroRecordingStatus {
    pub active: bool,
    pub capture_started: bool,
    pub step_count: usize,
    pub capture_mouse_move: bool,
    pub capture_mouse_clicks: bool,
    pub target_locked: bool,
    pub target_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MacroRecordingResult {
    pub steps: Vec<MacroStep>,
    pub target: Option<MacroTarget>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MacroPlaybackStatus {
    pub running: bool,
    pub current_step: usize,
    pub total_steps: usize,
    pub last_error: Option<String>,
    pub playback_id: u64,
    pub macro_id: Option<String>,
    pub macro_name: Option<String>,
    pub program_kind: String,
    pub action_kind: Option<String>,
    pub action_summary: Option<String>,
    pub elapsed_ms: u64,
    pub phase: String,
    pub cleanup_status: String,
    pub overlay_visible: bool,
}

#[cfg(windows)]
fn runtime_phase_name(phase: RuntimePhase) -> &'static str {
    match phase {
        RuntimePhase::Idle => "idle",
        RuntimePhase::Starting => "starting",
        RuntimePhase::Running => "running",
        RuntimePhase::Stopping => "stopping",
        RuntimePhase::Cleaning => "cleaning",
        RuntimePhase::FaultLocked => "fault_locked",
        RuntimePhase::ShuttingDown => "shutting_down",
    }
}

impl HookService {
    #[cfg(windows)]
    pub(crate) fn bind_frontend_process(pid: u32) {
        FRONTEND_PROCESS_ID.store(pid, Ordering::Release);
    }

    #[cfg(windows)]
    pub(crate) fn service_generation(&self) -> u64 {
        self.shared.controller.generation()
    }

    #[cfg(windows)]
    pub(crate) fn service_admission_revision(&self) -> u64 {
        self.shared.controller.background_generation()
    }

    #[cfg(windows)]
    pub(crate) fn play_with_generation(
        &self,
        rule: MacroRule,
        generation: u64,
        admission_revision: u64,
    ) -> Result<(), AppError> {
        self.shared
            .start_playback_at_revision(rule, generation, admission_revision)
    }

    #[cfg(windows)]
    pub(crate) fn fast_shutdown_request(&self) {
        let generation = self.shared.controller.request_shutdown();
        self.shared
            .emergency_generation
            .store(generation, Ordering::Release);
        self.shared
            .request_emergency_stop(EmergencyEntryPoint::Command, true);
    }
    pub(crate) fn take_runtime_notification(
        &self,
    ) -> Option<crate::runtime_notifications::RuntimeNotification> {
        self.shared.notifications.take()
    }

    pub(crate) fn acknowledge_runtime_notification(&self, id: u64) -> bool {
        self.shared.notifications.acknowledge(id)
    }
    /// Construct state without installing Windows hooks or injecting input.
    /// Tests supply detector health explicitly when testing admission.
    #[cfg(test)]
    pub(crate) fn isolated(config: AppConfig, vision: Arc<VisionService>) -> Self {
        // Keep the production constructor type-checked and its callback graph
        // reachable in test builds without actually installing any hooks.
        let _production_constructor: fn(AppConfig, Arc<VisionService>) -> Result<Self, AppError> =
            Self::start;
        Self {
            shared: Arc::new(HookShared::new(config, vision)),
        }
    }

    pub fn start(config: AppConfig, vision: Arc<VisionService>) -> Result<Self, AppError> {
        let shared = Arc::new(HookShared::new(config, vision));
        #[cfg(windows)]
        log::info!(
            "输入安全诊断文件: {}",
            shared.safety_diagnostics.path().display()
        );

        #[cfg(windows)]
        {
            shared.initializing.store(true, Ordering::Release);
            let initialization = (|| -> Result<(), AppError> {
                let weak = Arc::downgrade(&shared);
                let tasks = crate::bounded_worker::BoundedWorker::spawn(
                    "autoflow-text-worker",
                    8,
                    move |task: TextExpansionTask| {
                        if let Some(shared) = weak.upgrade() {
                            task(&shared);
                        }
                    },
                )
                .map_err(|error| {
                    AppError::with_detail(
                        "text_worker_start_failed",
                        "文本扩展工作线程启动失败",
                        error.to_string(),
                    )
                })?;
                shared
                    .text_tasks
                    .set(tasks)
                    .map_err(|_| AppError::internal("文本扩展工作队列重复初始化"))?;
                shared.start_trigger_worker()?;
                shared.start_remap_worker()?;
                shared.start_launch_worker()?;
                shared.start_behavior_capture_worker()?;
                shared.start_graph_capture_worker()?;
                shared.start_priority_cleanup_worker()?;
                HOOK_SHARED.set(Arc::clone(&shared)).map_err(|_| {
                    AppError::internal("当前安全进程已初始化钩子，禁止重复创建输入服务")
                })?;
                let thread_shared = Arc::clone(&shared);
                let hook_handle = thread::Builder::new()
                    .name("autoflow-keyboard-hook".to_string())
                    .spawn(move || hook_thread(thread_shared))
                    .map_err(|error| {
                        AppError::with_detail(
                            "hook_start_failed",
                            "全局键盘监听启动失败",
                            error.to_string(),
                        )
                    })?;
                shared
                    .hook_thread_handle
                    .set(hook_handle)
                    .map_err(|_| AppError::internal("钩子线程句柄重复初始化"))?;
                let emergency_shared = Arc::clone(&shared);
                let emergency_thread = thread::Builder::new()
                    .name("autoflow-emergency-stop".to_string())
                    .spawn(move || emergency_stop_thread(emergency_shared))
                    .map_err(|error| error.to_string());
                let emergency_handle = match emergency_thread {
                    Ok(handle) => handle,
                    Err(error) => {
                        return Err(AppError::with_detail(
                            "emergency_stop_start_failed",
                            "紧急停止通道启动失败，已拒绝启动输入服务",
                            error,
                        ));
                    }
                };
                shared
                    .emergency_thread_handle
                    .set(emergency_handle)
                    .map_err(|_| AppError::internal("急停线程句柄重复初始化"))?;

                // Do not expose a partially initialized service to the UI.  A
                // playback request must never win a race with creation of the
                // independent F12 polling thread and then be rejected (or worse,
                // admitted without a working stop path).
                let deadline = Instant::now() + Duration::from_millis(1000);
                while !shared.emergency_stop_ready() {
                    if Instant::now() >= deadline {
                        return Err(AppError::invalid(
                            "emergency_stop_start_failed",
                            "输入钩子或 F12 健康检测未能在启动期限内就绪，已拒绝启动输入服务",
                        ));
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Ok(())
            })();
            if let Err(error) = initialization {
                return Err(shared.rollback_failed_start(error, Duration::from_secs(2)));
            }
            shared.initializing.store(false, Ordering::Release);
        }

        Ok(Self { shared })
    }

    pub fn update_config(&self, config: AppConfig) -> Result<(), AppError> {
        let assets = config.assets.clone();
        #[cfg(windows)]
        let emergency_vk = key_to_vk(&config.emergency_stop).unwrap_or(0);
        #[cfg(windows)]
        let hotkeys_changed;
        {
            let mut current = self
                .shared
                .config
                .lock()
                .map_err(|_| AppError::internal("输入服务状态异常，请重启 AutoFlow"))?;
            #[cfg(windows)]
            {
                hotkeys_changed = key_to_vk(&current.emergency_stop)
                    != key_to_vk(&config.emergency_stop)
                    || native_registration_plan(&current) != native_registration_plan(&config);
                if trigger_permission_snapshot(&current) != trigger_permission_snapshot(&config) {
                    self.shared.advance_trigger_config_revision()?;
                }
            }
            *current = config;
        }
        if let Ok(vision) = self.shared.vision.lock() {
            vision.set_assets(&assets);
        }
        #[cfg(windows)]
        {
            self.shared
                .emergency_vk
                .store(emergency_vk, Ordering::SeqCst);
            // Configuration refreshes are also triggered when the window regains
            // focus. They must not be treated as an emergency stop: the macro
            // recorder may still be active, or it may already hold a completed
            // result that the frontend has not collected yet. Clearing the
            // transient state here used to erase both `steps` and
            // `completed_steps`, making a recording disappear on focus return.
            if hotkeys_changed {
                self.shared.request_native_hotkey_refresh();
            }
        }
        Ok(())
    }

    pub fn set_vision(&self, vision: Arc<VisionService>) -> Result<(), AppError> {
        let assets = self
            .shared
            .config
            .lock()
            .map_err(|_| AppError::internal("输入服务状态异常，请重启 AutoFlow"))?
            .assets
            .clone();
        vision.set_assets(&assets);
        let mut current = self
            .shared
            .vision
            .lock()
            .map_err(|_| AppError::internal("视觉服务状态异常，请重启 AutoFlow"))?;
        *current = vision;
        Ok(())
    }

    pub fn emergency_stop(&self) {
        #[cfg(windows)]
        self.shared
            .request_emergency_stop(EmergencyEntryPoint::Command, true);
    }

    pub fn start_recording(
        &self,
        capture_mouse_move: bool,
        capture_mouse_clicks: bool,
    ) -> Result<(), AppError> {
        #[cfg(windows)]
        {
            self.shared
                .start_recording(capture_mouse_move, capture_mouse_clicks)
        }
        #[cfg(not(windows))]
        Err(AppError::invalid(
            "recording_unsupported",
            "宏录制目前只支持 Windows 桌面端",
        ))
    }

    pub fn set_recording_options(
        &self,
        capture_mouse_move: bool,
        capture_mouse_clicks: bool,
    ) -> Result<(), AppError> {
        #[cfg(windows)]
        {
            self.shared
                .set_recording_options(capture_mouse_move, capture_mouse_clicks)
        }
        #[cfg(not(windows))]
        {
            let _ = (capture_mouse_move, capture_mouse_clicks);
            Ok(())
        }
    }

    pub fn stop_recording(
        &self,
        discard_trailing_mouse_input: bool,
    ) -> Result<MacroRecordingResult, AppError> {
        #[cfg(windows)]
        {
            self.shared.stop_recording(discard_trailing_mouse_input)
        }
        #[cfg(not(windows))]
        Err(AppError::invalid(
            "recording_unsupported",
            "宏录制目前只支持 Windows 桌面端",
        ))
    }

    pub fn start_behavior_recording(&self, name: String) -> Result<(), AppError> {
        #[cfg(windows)]
        {
            self.shared.start_behavior_recording(&name)
        }
        #[cfg(not(windows))]
        {
            let _ = name;
            Err(AppError::invalid(
                "recording_unsupported",
                "行为录制目前只支持 Windows 桌面端",
            ))
        }
    }

    pub fn stop_behavior_recording(&self) -> Result<BehaviorRecordingResult, AppError> {
        #[cfg(windows)]
        {
            self.shared.stop_behavior_recording()
        }
        #[cfg(not(windows))]
        Err(AppError::invalid(
            "recording_unsupported",
            "行为录制目前只支持 Windows 桌面端",
        ))
    }

    pub fn discard_behavior_recording(&self) -> Result<(), AppError> {
        #[cfg(windows)]
        {
            self.shared.discard_behavior_recording()
        }
        #[cfg(not(windows))]
        Err(AppError::invalid(
            "recording_unsupported",
            "behavior recording is only supported on Windows",
        ))
    }

    pub fn complete_behavior_recording_claim(&self) -> Result<(), AppError> {
        #[cfg(windows)]
        {
            self.shared.complete_behavior_recording_claim()
        }
        #[cfg(not(windows))]
        Err(AppError::invalid(
            "recording_unsupported",
            "behavior recording is only supported on Windows",
        ))
    }

    pub fn behavior_recording_status(&self) -> BehaviorRecordingStatus {
        #[cfg(windows)]
        {
            self.shared.behavior_recording_status()
        }
        #[cfg(not(windows))]
        BehaviorRecordingStatus {
            active: false,
            pending: false,
            incomplete: false,
            capture_started: false,
            duration_ms: 0,
            event_count: 0,
            keyboard_events: 0,
            mouse_events: 0,
            wheel_events: 0,
            capped: false,
            persisting_raw_session: false,
            session_name: None,
        }
    }

    #[cfg(any(not(windows), test))]
    pub fn play_macro(&self, macro_rule: MacroRule) -> Result<(), AppError> {
        #[cfg(windows)]
        {
            self.shared.start_playback(macro_rule)
        }
        #[cfg(not(windows))]
        {
            let _ = macro_rule;
            Err(AppError::invalid(
                "playback_unsupported",
                "宏播放目前只支持 Windows 桌面端",
            ))
        }
    }

    pub fn stop_macro(&self) {
        #[cfg(windows)]
        self.shared.stop_playback();
    }

    /// Stop all input activity before the application closes.  This is kept
    /// separate from `Drop` so the Tauri window-close path can request the
    /// same cleanup while the process is still alive.
    pub fn shutdown(&self) -> bool {
        #[cfg(windows)]
        {
            self.shared.request_shutdown();
            let safe = self
                .shared
                .wait_for_shutdown_cleanup(Duration::from_secs(2));
            if !self
                .shared
                .safety_diagnostics
                .flush(Duration::from_millis(500))
            {
                log::warn!("终态安全诊断刷新未确认；输入清理结果与日志持久化结果分别记录");
            }
            safe
        }
        #[cfg(not(windows))]
        {
            true
        }
    }

    #[cfg(windows)]
    pub(crate) fn recover_input_safety_at(
        &self,
        generation: u64,
        admission_revision: u64,
    ) -> Result<(), AppError> {
        if self
            .shared
            .executor_containment_unknown
            .load(Ordering::Acquire)
        {
            return Err(AppError::invalid("executor_containment_unconfirmed", "执行器或通信线程退出未确认；清空输入账本不能解除此锁定。请停止测试，确认全部 AutoFlow 进程退出后再重新启动。"));
        }
        if !self.shared.emergency_stop_ready()
            || !self.shared.playback_thread_quiescent()
            || !crate::desktop_safety::current_desktop_accepts_input()
            || !self.shared.controller.is_quiescent()
            || !self.shared.emergency_cleanup_quiescent()
            || self.shared.tracked_input_counts() != (0, 0)
            || self.shared.input_recovery_required.load(Ordering::Acquire)
            || self
                .shared
                .executor_containment_unknown
                .load(Ordering::Acquire)
            || self.shared.playback_thread_owner.load(Ordering::Acquire) != 0
        {
            return Err(AppError::invalid(
                "safety_recovery_not_ready",
                "安全通道、执行器退出或输入清理尚未确认，不能解除锁定",
            ));
        }
        if !self
            .shared
            .controller
            .recover_after_cleanup_at(generation, admission_revision)
        {
            return Err(AppError::invalid(
                "safety_recovery_rejected",
                "当前状态不允许故障恢复",
            ));
        }
        self.shared
            .record_safety_async("explicit_safety_recovery", Vec::new());
        Ok(())
    }

    pub fn is_playback_running(&self) -> bool {
        #[cfg(windows)]
        {
            self.shared.is_playback_running()
        }
        #[cfg(not(windows))]
        false
    }

    pub fn recording_status(&self) -> MacroRecordingStatus {
        #[cfg(windows)]
        {
            self.shared.recording_status()
        }
        #[cfg(not(windows))]
        MacroRecordingStatus {
            active: false,
            capture_started: false,
            step_count: 0,
            capture_mouse_move: true,
            capture_mouse_clicks: true,
            target_locked: false,
            target_name: None,
        }
    }

    pub fn playback_status(&self) -> MacroPlaybackStatus {
        #[cfg(windows)]
        {
            self.shared.playback_status()
        }
        #[cfg(not(windows))]
        MacroPlaybackStatus {
            running: false,
            current_step: 0,
            total_steps: 0,
            last_error: None,
            playback_id: 0,
            macro_id: None,
            macro_name: None,
            program_kind: "unknown".to_string(),
            action_kind: None,
            action_summary: None,
            elapsed_ms: 0,
            phase: "idle".to_string(),
            cleanup_status: "not_started".to_string(),
            overlay_visible: false,
        }
    }
}

impl Drop for HookService {
    fn drop(&mut self) {
        #[cfg(windows)]
        self.shutdown();
    }
}

#[cfg(windows)]
type TextExpansionTask = Box<dyn FnOnce(&Arc<HookShared>) + Send>;
#[cfg(windows)]
struct MacroTriggerTask {
    rule: MacroRule,
    start_timing: TriggerStartTiming,
    hold_epoch: Option<u64>,
    trigger_vk: Option<u32>,
    hold_owner_vk: Option<u32>,
    hold_modifier_vks: Vec<u32>,
    generation: u64,
    controller_generation: u64,
    admission_revision: u64,
    config_revision: u64,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HoldLifecyclePhase {
    Pending,
    Bound {
        run_token: RunToken,
    },
    Active {
        run_token: RunToken,
        instance_id: u64,
    },
    Retired,
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct HoldLifecycleIdentity {
    epoch: u64,
    macro_id: String,
    rule_snapshot: TriggerMacroSnapshot,
    owner_vk: u32,
    phase: HoldLifecyclePhase,
    cancelled: bool,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TriggerStartTiming {
    ReleaseGated,
    HoldModifierRelease,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HoldAtomicSnapshot {
    epoch: u64,
    trigger_matches: bool,
    bound_token: Option<RunToken>,
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct TriggerMacroSnapshot {
    id: String,
    enabled: bool,
    trigger_vks: Option<Vec<u32>>,
    mode: u8,
    repeat_count: u32,
    speed_bits: u32,
    import_error: bool,
    clicker: bool,
    program: Vec<u8>,
    behavior_policy: Vec<u8>,
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct TriggerPermissionSnapshot {
    global_enabled: bool,
    emergency_vk: Option<u32>,
    macros: Vec<TriggerMacroSnapshot>,
}

#[cfg(windows)]
fn macro_mode_identity(mode: MacroMode) -> u8 {
    match mode {
        MacroMode::Once => 0,
        MacroMode::Repeat => 1,
        MacroMode::Hold => 2,
        MacroMode::Toggle => 3,
    }
}

#[cfg(windows)]
fn trigger_macro_snapshot(rule: &MacroRule) -> TriggerMacroSnapshot {
    let trigger_vks = rule
        .trigger_keys
        .iter()
        .map(|key| key_to_vk(key))
        .collect::<Option<Vec<_>>>()
        .map(|mut keys| {
            keys.sort_unstable();
            keys
        });
    TriggerMacroSnapshot {
        id: rule.id.clone(),
        enabled: rule.enabled,
        trigger_vks,
        mode: macro_mode_identity(rule.mode),
        repeat_count: rule.repeat_count,
        speed_bits: rule.speed.to_bits(),
        import_error: rule.import_error.is_some(),
        clicker: is_native_clicker_rule(rule),
        program: serde_json::to_vec(&rule.program).unwrap_or_default(),
        behavior_policy: serde_json::to_vec(&rule.behavior_policy).unwrap_or_default(),
    }
}

#[cfg(windows)]
fn trigger_permission_snapshot(config: &AppConfig) -> TriggerPermissionSnapshot {
    let mut macros = config
        .macros
        .iter()
        .filter(|rule| rule.enabled)
        .map(trigger_macro_snapshot)
        .collect::<Vec<_>>();
    macros.sort_by(|left, right| left.id.cmp(&right.id));
    TriggerPermissionSnapshot {
        global_enabled: config.global_enabled,
        emergency_vk: key_to_vk(&config.emergency_stop),
        macros,
    }
}

#[cfg(windows)]
const TRIGGER_RELEASE_POLL: Duration = Duration::from_millis(3);
#[cfg(windows)]
const TRIGGER_RELEASE_STABLE: Duration = Duration::from_millis(18);
#[cfg(windows)]
const TRIGGER_RELEASE_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TriggerReleaseCancellation {
    TriggerConfiguration,
    PhysicalLedgerUncertain,
    TaskGeneration,
    ControllerGeneration,
    AdmissionRevision,
    ConfigRevision,
    HoldLifecycle,
    HoldOwnerReleased,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TriggerReleaseGateStatus {
    Current,
    Shutdown,
    Cancelled(TriggerReleaseCancellation),
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TriggerReleaseGateOutcome {
    Ready,
    Timeout,
    Shutdown,
    Cancelled(TriggerReleaseCancellation),
}

#[cfg(all(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TriggerAdmissionCheckpoint {
    AfterInitialValidation,
    BeforeActivation,
    AfterControllerActivation,
    BeforeInputRegistration,
    BeforePlaybackPublication,
}

#[cfg(all(windows, test))]
type TriggerAdmissionTestHook = Arc<dyn Fn(TriggerAdmissionCheckpoint) + Send + Sync>;

#[cfg(windows)]
#[derive(Debug)]
struct TriggerReleaseGate {
    stable_since: Option<Duration>,
    stable_window: Duration,
    timeout: Duration,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
struct TriggerReleaseSample {
    elapsed: Duration,
    any_trigger_key_down: bool,
    status: TriggerReleaseGateStatus,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
struct HoldModifierReleaseSample {
    elapsed: Duration,
    owner_down: bool,
    any_modifier_down: bool,
    status: TriggerReleaseGateStatus,
}

#[cfg(windows)]
impl TriggerReleaseGate {
    fn new(stable_window: Duration, timeout: Duration) -> Self {
        Self {
            stable_since: None,
            stable_window,
            timeout,
        }
    }

    fn observe(
        &mut self,
        elapsed: Duration,
        any_trigger_key_down: bool,
        status: TriggerReleaseGateStatus,
    ) -> Option<TriggerReleaseGateOutcome> {
        match status {
            TriggerReleaseGateStatus::Shutdown => return Some(TriggerReleaseGateOutcome::Shutdown),
            TriggerReleaseGateStatus::Cancelled(reason) => {
                return Some(TriggerReleaseGateOutcome::Cancelled(reason))
            }
            TriggerReleaseGateStatus::Current => {}
        }
        if elapsed >= self.timeout {
            return Some(TriggerReleaseGateOutcome::Timeout);
        }
        if any_trigger_key_down {
            self.stable_since = None;
            return None;
        }
        let stable_since = *self.stable_since.get_or_insert(elapsed);
        (elapsed.saturating_sub(stable_since) >= self.stable_window)
            .then_some(TriggerReleaseGateOutcome::Ready)
    }
}

#[cfg(windows)]
fn drive_trigger_release_gate(
    mut gate: TriggerReleaseGate,
    mut sample: impl FnMut() -> TriggerReleaseSample,
    mut pause: impl FnMut(Duration),
) -> TriggerReleaseGateOutcome {
    loop {
        let observation = sample();
        let Some(outcome) = gate.observe(
            observation.elapsed,
            observation.any_trigger_key_down,
            observation.status,
        ) else {
            pause(TRIGGER_RELEASE_POLL);
            continue;
        };
        if !matches!(outcome, TriggerReleaseGateOutcome::Ready) {
            return outcome;
        }

        // A key may be pressed again between the observation that completed
        // the stable window and the final lifecycle check. Resample both from
        // the same seam. A re-press resets stability and returns to waiting;
        // cancellation/shutdown remains terminal and takes precedence.
        let final_observation = sample();
        if let Some(final_outcome) = gate.observe(
            final_observation.elapsed,
            final_observation.any_trigger_key_down,
            final_observation.status,
        ) {
            return final_outcome;
        }
        pause(TRIGGER_RELEASE_POLL);
    }
}

#[cfg(windows)]
fn drive_hold_modifier_release_gate(
    has_modifiers: bool,
    gate: TriggerReleaseGate,
    mut sample: impl FnMut() -> HoldModifierReleaseSample,
    pause: impl FnMut(Duration),
) -> TriggerReleaseGateOutcome {
    if !has_modifiers {
        let observation = sample();
        return match observation.status {
            TriggerReleaseGateStatus::Shutdown => TriggerReleaseGateOutcome::Shutdown,
            TriggerReleaseGateStatus::Cancelled(reason) => {
                TriggerReleaseGateOutcome::Cancelled(reason)
            }
            TriggerReleaseGateStatus::Current if observation.owner_down => {
                TriggerReleaseGateOutcome::Ready
            }
            TriggerReleaseGateStatus::Current => {
                TriggerReleaseGateOutcome::Cancelled(TriggerReleaseCancellation::HoldOwnerReleased)
            }
        };
    }
    drive_trigger_release_gate(
        gate,
        || {
            let observation = sample();
            TriggerReleaseSample {
                elapsed: observation.elapsed,
                any_trigger_key_down: observation.any_modifier_down,
                status: if matches!(observation.status, TriggerReleaseGateStatus::Current)
                    && !observation.owner_down
                {
                    TriggerReleaseGateStatus::Cancelled(
                        TriggerReleaseCancellation::HoldOwnerReleased,
                    )
                } else {
                    observation.status
                },
            }
        },
        pause,
    )
}

#[cfg(windows)]
struct TriggerPendingReset<'a>(&'a AtomicBool);

#[cfg(windows)]
impl Drop for TriggerPendingReset<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(windows)]
struct PendingHoldLifecycleReset<'a> {
    shared: &'a HookShared,
    epoch: Option<u64>,
    transferred_to_playback: bool,
}

#[cfg(windows)]
impl PendingHoldLifecycleReset<'_> {
    fn transfer_to_playback(&mut self) {
        self.transferred_to_playback = true;
    }
}

#[cfg(windows)]
impl Drop for PendingHoldLifecycleReset<'_> {
    fn drop(&mut self) {
        if !self.transferred_to_playback {
            if let Some(epoch) = self.epoch {
                self.shared.retire_hold_lifecycle(epoch);
            }
        }
    }
}

#[cfg(windows)]
fn trigger_release_cancellation_name(reason: TriggerReleaseCancellation) -> &'static str {
    match reason {
        TriggerReleaseCancellation::TriggerConfiguration => "trigger_configuration",
        TriggerReleaseCancellation::PhysicalLedgerUncertain => "physical_ledger_uncertain",
        TriggerReleaseCancellation::TaskGeneration => "task_generation",
        TriggerReleaseCancellation::ControllerGeneration => "controller_generation",
        TriggerReleaseCancellation::AdmissionRevision => "admission_revision",
        TriggerReleaseCancellation::ConfigRevision => "config_revision",
        TriggerReleaseCancellation::HoldLifecycle => "hold_lifecycle",
        TriggerReleaseCancellation::HoldOwnerReleased => "hold_owner_released",
    }
}

#[cfg(windows)]
fn hold_diagnostic_fields(
    macro_id: Option<&str>,
    decision: &str,
    epoch: Option<u64>,
    run_token: Option<RunToken>,
    trigger_vk: Option<u32>,
    owner_vk: Option<u32>,
    released_vk: Option<u32>,
) -> Vec<(String, String)> {
    let mut fields = vec![
        ("mode".to_string(), "hold".to_string()),
        ("source".to_string(), "low_level_keyboard".to_string()),
        ("decision".to_string(), decision.to_string()),
    ];
    if let Some(macro_id) = macro_id {
        fields.push(("macro_id".to_string(), macro_id.to_string()));
    }
    if let Some(epoch) = epoch {
        fields.push(("hold_epoch".to_string(), epoch.to_string()));
    }
    if let Some(token) = run_token.filter(|token| token.id != 0) {
        fields.push(("run_id".to_string(), token.id.to_string()));
        fields.push(("run_generation".to_string(), token.generation.to_string()));
    }
    if let Some(vk) = trigger_vk {
        fields.push(("canonical_vk".to_string(), format!("{vk:#04x}")));
    }
    if let Some(vk) = owner_vk {
        fields.push(("owner_vk".to_string(), format!("{vk:#04x}")));
    }
    if let Some(vk) = released_vk {
        fields.push(("released_vk".to_string(), format!("{vk:#04x}")));
    }
    fields
}
#[cfg(windows)]
struct RemapTask {
    source: u32,
    target: u32,
    generation: u64,
    down: bool,
}

#[cfg(windows)]
struct LaunchTask {
    target: String,
    generation: u64,
}

#[cfg(windows)]
enum BehaviorCapture {
    Key(u32, u32, bool),
    Move(i32, i32),
    Button(u8, bool, i32, i32),
    Wheel(i32, i32, i32, i32),
}

#[cfg(windows)]
struct PlaybackThreadLease {
    shared: Arc<HookShared>,
    instance_id: u64,
}
#[cfg(windows)]
impl Drop for PlaybackThreadLease {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.shared.controller.lock_fault();
            self.shared
                .request_emergency_stop(EmergencyEntryPoint::Command, true);
        }
        let _ = self.shared.playback_thread_owner.compare_exchange(
            self.instance_id,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

struct HookShared {
    notifications: crate::runtime_notifications::RuntimeNotifications,
    #[cfg(windows)]
    mode_admission: Mutex<()>,
    config: Mutex<AppConfig>,
    vision: Mutex<Arc<VisionService>>,
    #[cfg(windows)]
    pressed: Mutex<HashSet<u32>>,
    #[cfg(windows)]
    physical_pressed: Mutex<HashSet<u32>>,
    #[cfg(windows)]
    physical_ledger_uncertain: AtomicBool,
    #[cfg(windows)]
    latched_hotkeys: Mutex<HashSet<String>>,
    #[cfg(windows)]
    active_remaps: Mutex<HashMap<u32, u32>>,
    #[cfg(windows)]
    text_buffer: Mutex<String>,
    #[cfg(windows)]
    text_tasks: std::sync::OnceLock<crate::bounded_worker::BoundedWorker<TextExpansionTask>>,
    #[cfg(windows)]
    trigger_tasks: std::sync::OnceLock<crate::bounded_worker::BoundedWorker<MacroTriggerTask>>,
    #[cfg(windows)]
    trigger_pending: AtomicBool,
    #[cfg(windows)]
    hold_lifecycle: Mutex<Option<HoldLifecycleIdentity>>,
    #[cfg(windows)]
    next_hold_epoch: AtomicU64,
    #[cfg(windows)]
    hold_epoch_exhausted: AtomicBool,
    #[cfg(windows)]
    hold_lifecycle_epoch: AtomicU64,
    #[cfg(windows)]
    hold_cancelled_epoch: AtomicU64,
    #[cfg(windows)]
    hold_lifecycle_phase: AtomicU32,
    #[cfg(windows)]
    hold_bound_token_id: AtomicU64,
    #[cfg(windows)]
    hold_bound_token_generation: AtomicU64,
    #[cfg(windows)]
    hold_trigger_words: [AtomicU64; 4],
    #[cfg(windows)]
    // Dedicated to hotkey permission/config identity. Background admission
    // also changes for playback lifecycle events, so it cannot distinguish an
    // equivalent focus refresh from a rule being disabled or rewritten.
    trigger_config_revision: AtomicU64,
    #[cfg(windows)]
    trigger_config_revision_exhausted: AtomicBool,
    #[cfg(all(windows, test))]
    trigger_admission_test_hook: Mutex<Option<TriggerAdmissionTestHook>>,
    #[cfg(windows)]
    remap_tasks: std::sync::OnceLock<crate::bounded_worker::BoundedWorker<RemapTask>>,
    #[cfg(windows)]
    launch_tasks: std::sync::OnceLock<crate::bounded_worker::BoundedWorker<LaunchTask>>,
    #[cfg(windows)]
    behavior_capture: std::sync::OnceLock<crate::bounded_worker::CaptureQueue<BehaviorCapture>>,
    #[cfg(windows)]
    behavior_capture_error: AtomicBool,
    #[cfg(windows)]
    graph_capture: std::sync::OnceLock<crate::bounded_worker::CaptureQueue<GraphCapture>>,
    #[cfg(windows)]
    graph_capture_error: AtomicBool,
    #[cfg(windows)]
    graph_armed: AtomicBool,
    #[cfg(windows)]
    graph_boundary_started: AtomicBool,
    #[cfg(windows)]
    graph_mouse_move_enabled: AtomicBool,
    #[cfg(windows)]
    graph_mouse_clicks_enabled: AtomicBool,
    #[cfg(windows)]
    recorder: Mutex<RecorderState>,
    #[cfg(windows)]
    behavior: Mutex<BehaviorRecorder>,
    #[cfg(windows)]
    playback: Mutex<PlaybackState>,
    #[cfg(windows)]
    injected_input: Arc<InjectedInputState>,
    #[cfg(windows)]
    text_input: Arc<InjectedInputState>,
    #[cfg(windows)]
    input_broker: Arc<InputBroker>,
    #[cfg(windows)]
    playback_inputs: Mutex<HashMap<u64, Arc<InjectedInputState>>>,
    #[cfg(windows)]
    controller: Arc<RuntimeController>,
    #[cfg(windows)]
    initializing: AtomicBool,
    #[cfg(windows)]
    startup_rollback_unconfirmed: AtomicBool,
    #[cfg(windows)]
    hook_ready: AtomicBool,
    #[cfg(windows)]
    hook_probe_pending: AtomicBool,
    #[cfg(windows)]
    hook_heartbeat_ms: AtomicU64,
    #[cfg(windows)]
    playback_instance_counter: AtomicU64,
    #[cfg(windows)]
    emergency_generation: AtomicU64,
    #[cfg(windows)]
    emergency_request_sequence: AtomicU64,
    #[cfg(windows)]
    emergency_reason: AtomicU32,
    #[cfg(windows)]
    emergency_requested_at_ms: AtomicU64,
    #[cfg(windows)]
    emergency_cleanup_scheduled: AtomicBool,
    #[cfg(windows)]
    emergency_cleanup_completed_sequence: AtomicU64,
    #[cfg(windows)]
    emergency_cleanup_thread: Mutex<Option<thread::JoinHandle<()>>>,
    #[cfg(windows)]
    priority_cleanup: std::sync::OnceLock<crate::priority_cleanup::PriorityCleanup>,
    #[cfg(windows)]
    input_recovery_required: AtomicBool,
    #[cfg(windows)]
    executor_containment_unknown: AtomicBool,
    #[cfg(windows)]
    playback_thread_owner: AtomicU64,
    #[cfg(windows)]
    playback_thread_handle: Mutex<Option<(u64, std::thread::JoinHandle<()>)>>,
    #[cfg(windows)]
    emergency_notification_failed: AtomicBool,
    #[cfg(windows)]
    thread_id: AtomicU32,
    #[cfg(windows)]
    hook_thread_handle: std::sync::OnceLock<std::thread::JoinHandle<()>>,
    #[cfg(windows)]
    emergency_thread_handle: std::sync::OnceLock<std::thread::JoinHandle<()>>,
    #[cfg(windows)]
    shutdown: AtomicBool,
    #[cfg(windows)]
    emergency_thread_id: AtomicU32,
    #[cfg(windows)]
    emergency_shutdown: AtomicBool,
    #[cfg(windows)]
    emergency_detector_ready: AtomicBool,
    #[cfg(windows)]
    emergency_clock: Instant,
    #[cfg(windows)]
    emergency_heartbeat_ms: AtomicU64,
    #[cfg(windows)]
    emergency_key_down: AtomicBool,
    #[cfg(windows)]
    trigger_rearm_required: AtomicBool,
    #[cfg(windows)]
    emergency_vk: AtomicU32,
    #[cfg(windows)]
    native_clicker_hotkey_registered: AtomicBool,
    #[cfg(windows)]
    native_macro_hotkeys: Mutex<HashMap<i32, String>>,
    #[cfg(windows)]
    native_refresh_pending: AtomicBool,
    #[cfg(windows)]
    native_refresh_message_pending: AtomicBool,
    #[cfg(windows)]
    next_native_hotkey_id: AtomicU64,
    #[cfg(windows)]
    native_teardown_failed: AtomicBool,
    #[cfg(windows)]
    safety_diagnostics: SafetyDiagnostics,
}

impl HookShared {
    fn new(config: AppConfig, vision: Arc<VisionService>) -> Self {
        #[cfg(windows)]
        let input_broker = Arc::new(InputBroker::default());
        Self {
            notifications: crate::runtime_notifications::RuntimeNotifications::default(),
            config: Mutex::new(config),
            vision: Mutex::new(vision),
            #[cfg(windows)]
            pressed: Mutex::new(HashSet::new()),
            #[cfg(windows)]
            physical_pressed: Mutex::new(HashSet::new()),
            #[cfg(windows)]
            physical_ledger_uncertain: AtomicBool::new(false),
            #[cfg(windows)]
            latched_hotkeys: Mutex::new(HashSet::new()),
            #[cfg(windows)]
            active_remaps: Mutex::new(HashMap::new()),
            #[cfg(windows)]
            text_buffer: Mutex::new(String::new()),
            #[cfg(windows)]
            text_tasks: std::sync::OnceLock::new(),
            #[cfg(windows)]
            trigger_tasks: std::sync::OnceLock::new(),
            #[cfg(windows)]
            trigger_pending: AtomicBool::new(false),
            hold_lifecycle: Mutex::new(None),
            next_hold_epoch: AtomicU64::new(0),
            hold_epoch_exhausted: AtomicBool::new(false),
            hold_lifecycle_epoch: AtomicU64::new(0),
            hold_cancelled_epoch: AtomicU64::new(0),
            hold_lifecycle_phase: AtomicU32::new(0),
            hold_bound_token_id: AtomicU64::new(0),
            hold_bound_token_generation: AtomicU64::new(0),
            hold_trigger_words: std::array::from_fn(|_| AtomicU64::new(0)),
            #[cfg(windows)]
            trigger_config_revision: AtomicU64::new(0),
            #[cfg(windows)]
            trigger_config_revision_exhausted: AtomicBool::new(false),
            #[cfg(all(windows, test))]
            trigger_admission_test_hook: Mutex::new(None),
            #[cfg(windows)]
            remap_tasks: std::sync::OnceLock::new(),
            #[cfg(windows)]
            launch_tasks: std::sync::OnceLock::new(),
            #[cfg(windows)]
            behavior_capture: std::sync::OnceLock::new(),
            #[cfg(windows)]
            behavior_capture_error: AtomicBool::new(false),
            #[cfg(windows)]
            graph_capture: std::sync::OnceLock::new(),
            #[cfg(windows)]
            graph_capture_error: AtomicBool::new(false),
            #[cfg(windows)]
            graph_armed: AtomicBool::new(false),
            #[cfg(windows)]
            graph_boundary_started: AtomicBool::new(false),
            #[cfg(windows)]
            graph_mouse_move_enabled: AtomicBool::new(true),
            #[cfg(windows)]
            graph_mouse_clicks_enabled: AtomicBool::new(true),
            #[cfg(windows)]
            mode_admission: Mutex::new(()),
            #[cfg(windows)]
            recorder: Mutex::new(RecorderState::default()),
            #[cfg(windows)]
            behavior: Mutex::new(BehaviorRecorder::default()),
            #[cfg(windows)]
            playback: Mutex::new(PlaybackState::default()),
            #[cfg(windows)]
            injected_input: Arc::new(InjectedInputState::with_broker(input_broker.clone())),
            #[cfg(windows)]
            text_input: Arc::new(InjectedInputState::with_broker(input_broker.clone())),
            #[cfg(windows)]
            input_broker,
            #[cfg(windows)]
            playback_inputs: Mutex::new(HashMap::new()),
            #[cfg(windows)]
            controller: RuntimeController::new(),
            #[cfg(windows)]
            playback_instance_counter: AtomicU64::new(0),
            #[cfg(windows)]
            emergency_generation: AtomicU64::new(0),
            #[cfg(windows)]
            initializing: AtomicBool::new(false),
            #[cfg(windows)]
            startup_rollback_unconfirmed: AtomicBool::new(false),
            #[cfg(windows)]
            hook_ready: AtomicBool::new(false),
            #[cfg(windows)]
            hook_probe_pending: AtomicBool::new(false),
            #[cfg(windows)]
            hook_heartbeat_ms: AtomicU64::new(0),
            #[cfg(windows)]
            emergency_request_sequence: AtomicU64::new(0),
            #[cfg(windows)]
            emergency_reason: AtomicU32::new(0),
            #[cfg(windows)]
            emergency_requested_at_ms: AtomicU64::new(0),
            #[cfg(windows)]
            emergency_cleanup_scheduled: AtomicBool::new(false),
            #[cfg(windows)]
            emergency_cleanup_completed_sequence: AtomicU64::new(0),
            #[cfg(windows)]
            emergency_cleanup_thread: Mutex::new(None),
            #[cfg(windows)]
            priority_cleanup: std::sync::OnceLock::new(),
            #[cfg(windows)]
            input_recovery_required: AtomicBool::new(false),
            #[cfg(windows)]
            executor_containment_unknown: AtomicBool::new(false),
            #[cfg(windows)]
            playback_thread_owner: AtomicU64::new(0),
            #[cfg(windows)]
            playback_thread_handle: Mutex::new(None),
            #[cfg(windows)]
            emergency_notification_failed: AtomicBool::new(false),
            #[cfg(windows)]
            thread_id: AtomicU32::new(0),
            #[cfg(windows)]
            hook_thread_handle: std::sync::OnceLock::new(),
            #[cfg(windows)]
            emergency_thread_handle: std::sync::OnceLock::new(),
            #[cfg(windows)]
            shutdown: AtomicBool::new(false),
            #[cfg(windows)]
            emergency_thread_id: AtomicU32::new(0),
            #[cfg(windows)]
            emergency_shutdown: AtomicBool::new(false),
            #[cfg(windows)]
            // Test fixtures explicitly inject detector health.  Production
            // readiness must come from the live polling thread; a compile-time
            // test shortcut would hide a broken emergency channel.
            emergency_detector_ready: AtomicBool::new(false),
            #[cfg(windows)]
            emergency_clock: Instant::now(),
            #[cfg(windows)]
            emergency_heartbeat_ms: AtomicU64::new(0),
            #[cfg(windows)]
            emergency_key_down: AtomicBool::new(false),
            #[cfg(windows)]
            trigger_rearm_required: AtomicBool::new(false),
            #[cfg(windows)]
            emergency_vk: AtomicU32::new(0x7B),
            #[cfg(windows)]
            native_clicker_hotkey_registered: AtomicBool::new(false),
            #[cfg(windows)]
            native_macro_hotkeys: Mutex::new(HashMap::new()),
            #[cfg(windows)]
            native_refresh_pending: AtomicBool::new(true),
            #[cfg(windows)]
            native_refresh_message_pending: AtomicBool::new(false),
            #[cfg(windows)]
            next_native_hotkey_id: AtomicU64::new(NATIVE_MACRO_HOTKEY_ID_START as u64),
            #[cfg(windows)]
            native_teardown_failed: AtomicBool::new(false),
            #[cfg(windows)]
            safety_diagnostics: SafetyDiagnostics::default(),
        }
    }

    #[cfg(windows)]
    fn request_native_hotkey_refresh(&self) {
        use windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
        self.native_refresh_pending.store(true, Ordering::Release);
        self.post_native_refresh_if_needed();
        let emergency_thread_id = self.emergency_thread_id.load(Ordering::SeqCst);
        if emergency_thread_id != 0 {
            let _ = unsafe {
                PostThreadMessageW(
                    emergency_thread_id,
                    REFRESH_EMERGENCY_HOTKEY_MESSAGE,
                    windows::Win32::Foundation::WPARAM(0),
                    windows::Win32::Foundation::LPARAM(0),
                )
            };
        }
    }

    #[cfg(windows)]
    fn post_native_refresh_if_needed(&self) {
        let thread_id = self.thread_id.load(Ordering::Acquire);
        if thread_id == 0
            || !self.native_refresh_pending.load(Ordering::Acquire)
            || self.shutdown.load(Ordering::Acquire)
            || self
                .native_refresh_message_pending
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        let posted = unsafe {
            windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                thread_id,
                REFRESH_NATIVE_HOTKEYS_MESSAGE,
                windows::Win32::Foundation::WPARAM(0),
                windows::Win32::Foundation::LPARAM(0),
            )
        };
        if posted.is_err() {
            self.native_refresh_message_pending
                .store(false, Ordering::Release);
        }
    }

    #[cfg(windows)]
    fn allocate_native_hotkey_id(&self) -> Option<i32> {
        // Never reuse an ID within this service session: an old WM_HOTKEY
        // still in the message queue must not resolve to a different macro.
        self.next_native_hotkey_id
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |id| {
                (id <= 0x7fff).then_some(id + 1)
            })
            .ok()
            .and_then(|id| i32::try_from(id).ok())
    }

    #[cfg(windows)]
    fn advance_trigger_config_revision(&self) -> Result<u64, AppError> {
        loop {
            let current = self.trigger_config_revision.load(Ordering::Acquire);
            let Some(next) = current.checked_add(1) else {
                self.trigger_config_revision_exhausted
                    .store(true, Ordering::Release);
                self.controller.lock_fault();
                return Err(AppError::invalid(
                    "trigger_config_revision_exhausted",
                    "快捷键配置版本已耗尽，已锁定新的快捷键启动；请重启 AutoFlow",
                ));
            };
            if self
                .trigger_config_revision
                .compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(next);
            }
        }
    }

    #[cfg(windows)]
    fn trigger_config_revision_is_current(&self, expected: u64) -> bool {
        !self
            .trigger_config_revision_exhausted
            .load(Ordering::Acquire)
            && self.trigger_config_revision.load(Ordering::Acquire) == expected
    }

    #[cfg(all(windows, test))]
    fn run_trigger_admission_test_hook(&self, checkpoint: TriggerAdmissionCheckpoint) {
        let hook = self
            .trigger_admission_test_hook
            .lock()
            .expect("trigger admission test hook")
            .clone();
        if let Some(hook) = hook {
            hook(checkpoint);
        }
    }

    #[cfg(windows)]
    fn request_shutdown(self: &Arc<Self>) {
        // Shutdown is idempotent.  The first caller owns the stop/cleanup
        // sequence; later callers must not reset the cancellation generation
        // or start another cleanup worker.
        if self.shutdown.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(tasks) = self.text_tasks.get() {
            tasks.close();
        }
        if let Some(tasks) = self.trigger_tasks.get() {
            tasks.close();
        }
        if let Some(tasks) = self.remap_tasks.get() {
            tasks.close();
        }
        if let Some(tasks) = self.launch_tasks.get() {
            tasks.close();
        }
        if let Some(queue) = self.behavior_capture.get() {
            queue.close();
        }
        if let Some(queue) = self.graph_capture.get() {
            queue.close();
        }

        let controller_generation = self.controller.request_shutdown();
        self.emergency_generation
            .store(controller_generation, Ordering::SeqCst);
        self.request_emergency_stop(EmergencyEntryPoint::Command, true);
        self.record_safety_async("application_shutdown_requested", Vec::new());
        self.signal_playback_stop();
        // The playback thread normally performs this cleanup.  Scheduling the
        // emergency worker as well covers an idle service and the small window
        // between a close request and playback-thread teardown.
        self.spawn_emergency_cleanup();
        self.emergency_shutdown.store(true, Ordering::SeqCst);

        use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};
        for thread_id in [
            self.thread_id.load(Ordering::SeqCst),
            self.emergency_thread_id.load(Ordering::SeqCst),
        ] {
            if thread_id != 0 {
                let _ = unsafe {
                    PostThreadMessageW(
                        thread_id,
                        WM_QUIT,
                        windows::Win32::Foundation::WPARAM(0),
                        windows::Win32::Foundation::LPARAM(0),
                    )
                };
            }
        }
    }

    #[cfg(windows)]
    fn background_workers_quiescent(&self) -> bool {
        if self
            .behavior_capture
            .get()
            .is_some_and(|queue| !queue.is_quiescent())
        {
            return false;
        }
        if self
            .graph_capture
            .get()
            .is_some_and(|queue| !queue.is_quiescent())
        {
            return false;
        }
        self.text_tasks
            .get()
            .is_none_or(|worker| worker.is_quiescent())
            && self
                .trigger_tasks
                .get()
                .is_none_or(|worker| worker.is_quiescent())
            && self
                .remap_tasks
                .get()
                .is_none_or(|worker| worker.is_quiescent())
            && self
                .launch_tasks
                .get()
                .is_none_or(|worker| worker.is_quiescent())
    }

    #[cfg(windows)]
    fn rollback_failed_start(self: &Arc<Self>, error: AppError, timeout: Duration) -> AppError {
        self.initializing.store(true, Ordering::Release);
        self.request_shutdown();
        if self.wait_for_shutdown_cleanup(timeout) {
            self.record_safety(
                "input_service_start_rolled_back",
                &[("original_error", error.code.clone())],
            );
            error
        } else {
            self.startup_rollback_unconfirmed
                .store(true, Ordering::Release);
            self.controller.lock_fault();
            self.input_recovery_required.store(true, Ordering::Release);
            self.record_safety(
                "input_service_start_rollback_unconfirmed",
                &[("original_error", error.code.clone())],
            );
            AppError::with_detail(
                "input_service_start_rollback_unconfirmed",
                "输入服务启动失败且退出清理未确认；安全进程保留控制权，禁止继续测试或重试启动",
                error.message,
            )
        }
    }

    #[cfg(windows)]
    fn wait_for_shutdown_cleanup(self: &Arc<Self>, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            // The detector may already have exited during shutdown. Continue
            // servicing coalesced requests here, without overlapping workers.
            self.spawn_emergency_cleanup();
            let safe = self.tracked_input_counts() == (0, 0)
                && !self.startup_rollback_unconfirmed.load(Ordering::Acquire)
                && self.system_threads_quiescent()
                && !self.native_teardown_failed.load(Ordering::Acquire)
                && self.background_workers_quiescent()
                && !self.input_recovery_required.load(Ordering::Acquire)
                && !self.executor_containment_unknown.load(Ordering::Acquire)
                && self.playback_thread_owner.load(Ordering::Acquire) == 0
                && self.playback_thread_quiescent()
                && self.emergency_cleanup_quiescent()
                && self.controller.is_quiescent();
            if safe {
                if let Some(worker) = self.priority_cleanup.get() {
                    worker.close();
                    if !worker.is_quiescent() {
                        if Instant::now() >= deadline {
                            return false;
                        }
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                }
                self.record_safety("application_shutdown_cleanup_completed", &[]);
                return true;
            }
            if Instant::now() >= deadline {
                self.record_safety(
                    "application_shutdown_cleanup_timeout",
                    &[
                        (
                            "tracked_input",
                            format!(
                                "keys={},buttons={}",
                                self.tracked_input_counts().0,
                                self.tracked_input_counts().1
                            ),
                        ),
                        ("status", "unsafe_manual_recovery_required".to_string()),
                    ],
                );
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(windows)]
    fn confirm_hook_removal(&self, kind: &'static str, result: Result<(), String>) -> bool {
        match result {
            Ok(()) => true,
            Err(error) => {
                self.native_teardown_failed.store(true, Ordering::Release);
                self.controller.lock_fault();
                self.record_safety(
                    "native_hook_teardown_unconfirmed",
                    &[("kind", kind.to_string()), ("error", error)],
                );
                false
            }
        }
    }

    #[cfg(windows)]
    fn system_threads_quiescent(&self) -> bool {
        self.hook_thread_handle
            .get()
            .is_none_or(crate::bounded_worker::thread_exit_confirmed)
            && self
                .emergency_thread_handle
                .get()
                .is_none_or(crate::bounded_worker::thread_exit_confirmed)
    }

    #[cfg(windows)]
    fn request_emergency_stop(
        self: &Arc<Self>,
        entry_point: EmergencyEntryPoint,
        allow_thread_fallback: bool,
    ) {
        let generation = if self.controller.is_shutting_down() {
            self.controller.generation()
        } else {
            self.controller.request_stop()
        };
        self.emergency_generation
            .store(generation, Ordering::SeqCst);
        self.trigger_rearm_required.store(true, Ordering::SeqCst);
        self.emergency_reason
            .store(entry_point as u32, Ordering::SeqCst);
        self.emergency_requested_at_ms
            .store(unix_millis(), Ordering::SeqCst);
        if let Some(worker) = self.priority_cleanup.get() {
            if let Some(sequence) = worker.request() {
                self.emergency_request_sequence
                    .fetch_max(sequence, Ordering::AcqRel);
            } else if !(worker.is_closed() && self.controller.is_shutting_down()) {
                self.controller.lock_fault();
                self.input_recovery_required.store(true, Ordering::Release);
            }
            return; // prestarted lane: no callback locks, posts or thread spawn
        }
        self.emergency_request_sequence
            .fetch_add(1, Ordering::SeqCst);
        // This path only performs atomics and a non-blocking message post.  In
        // particular, it never takes config/playback/pressed locks and never
        // shows a dialog from a Windows hook callback.
        self.notify_emergency_cleanup(allow_thread_fallback);
    }

    #[cfg(windows)]
    fn notify_emergency_cleanup(self: &Arc<Self>, allow_thread_fallback: bool) {
        // Production installs hooks only after the prestarted priority lane
        // has been stored. This legacy handoff is for partial construction
        // rollback and isolated input-free fixtures, not live hook dispatch.
        use windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW;

        let thread_id = self.emergency_thread_id.load(Ordering::SeqCst);
        if thread_id != 0 {
            let posted = unsafe {
                PostThreadMessageW(
                    thread_id,
                    EMERGENCY_STOP_MESSAGE,
                    windows::Win32::Foundation::WPARAM(0),
                    windows::Win32::Foundation::LPARAM(0),
                )
            };
            if posted.is_ok() {
                return;
            }
        }
        if allow_thread_fallback {
            self.spawn_emergency_cleanup();
        } else {
            // Fixture/partial-construction fallback; live callbacks return
            // from the prestarted-lane branch in request_emergency_stop.
            self.emergency_notification_failed
                .store(true, Ordering::SeqCst);
            self.spawn_emergency_cleanup();
        }
    }

    #[cfg(windows)]
    fn start_priority_cleanup_worker(self: &Arc<Self>) -> Result<(), AppError> {
        self.start_priority_cleanup_worker_with(emergency_cleanup_worker)
    }

    #[cfg(windows)]
    fn start_priority_cleanup_worker_with(
        self: &Arc<Self>,
        mut cleanup: impl FnMut(Arc<Self>, u64) + Send + 'static,
    ) -> Result<(), AppError> {
        let weak = Arc::downgrade(self);
        let worker = crate::priority_cleanup::PriorityCleanup::spawn(move |sequence| {
            let Some(shared) = weak.upgrade() else {
                return false;
            };
            shared
                .emergency_cleanup_scheduled
                .store(true, Ordering::Release);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cleanup(Arc::clone(&shared), sequence);
            }));
            if result.is_err() {
                shared.controller.lock_fault();
                shared
                    .input_recovery_required
                    .store(true, Ordering::Release);
                shared.record_safety("priority_cleanup_panicked", &[]);
                return false;
            }
            shared
                .emergency_cleanup_completed_sequence
                .store(sequence, Ordering::Release);
            shared
                .emergency_cleanup_scheduled
                .store(false, Ordering::Release);
            true
        })
        .map_err(|error| {
            AppError::with_detail(
                "priority_cleanup_start_failed",
                "独立急停清理线程启动失败",
                error.to_string(),
            )
        })?;
        self.priority_cleanup
            .set(worker)
            .map_err(|_| AppError::internal("独立急停清理线程重复初始化"))
    }

    #[cfg(windows)]
    fn spawn_emergency_cleanup(self: &Arc<Self>) {
        if let Some(worker) = self.priority_cleanup.get() {
            worker.wake();
            return;
        }
        self.spawn_emergency_cleanup_with(emergency_cleanup_worker);
    }

    #[cfg(windows)]
    fn emergency_cleanup_quiescent(&self) -> bool {
        if let Some(worker) = self.priority_cleanup.get() {
            return !self.emergency_cleanup_scheduled.load(Ordering::Acquire)
                && self
                    .emergency_cleanup_completed_sequence
                    .load(Ordering::Acquire)
                    == self.emergency_request_sequence.load(Ordering::Acquire)
                && (worker.is_idle() || worker.is_quiescent());
        }
        !self.emergency_cleanup_scheduled.load(Ordering::Acquire)
            && self
                .emergency_cleanup_completed_sequence
                .load(Ordering::Acquire)
                == self.emergency_request_sequence.load(Ordering::Acquire)
            && self
                .emergency_cleanup_thread
                .try_lock()
                .is_ok_and(|handle| {
                    handle
                        .as_ref()
                        .is_none_or(crate::bounded_worker::thread_exit_confirmed)
                })
    }

    #[cfg(windows)]
    fn spawn_emergency_cleanup_with(
        self: &Arc<Self>,
        cleanup: impl FnOnce(Arc<Self>, u64) + Send + 'static,
    ) {
        let sequence = self.emergency_request_sequence.load(Ordering::Acquire);
        if sequence
            == self
                .emergency_cleanup_completed_sequence
                .load(Ordering::Acquire)
        {
            return;
        }
        // Never replace a retained handle until actual OS thread exit,
        // including destructors, has been confirmed. A contended registry
        // leaves the atomic request pending for the detector/shutdown poll.
        let Ok(mut handle) = self.emergency_cleanup_thread.try_lock() else {
            return;
        };
        if handle
            .as_ref()
            .is_some_and(|old| !crate::bounded_worker::thread_exit_confirmed(old))
        {
            return;
        }
        if self
            .emergency_cleanup_scheduled
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let shared = Arc::clone(self);
        match thread::Builder::new()
            .name("autoflow-emergency-cleanup".to_string())
            .spawn(move || {
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    cleanup(Arc::clone(&shared), sequence);
                }))
                .is_err()
                {
                    // Leave scheduled/completed evidence unconfirmed. Never
                    // retry a panicked cleanup or authorize a new run from an
                    // apparently empty ledger.
                    shared.controller.lock_fault();
                    shared
                        .input_recovery_required
                        .store(true, Ordering::Release);
                    shared.record_safety("emergency_cleanup_panicked", &[]);
                    return;
                }
                shared
                    .emergency_cleanup_completed_sequence
                    .store(sequence, Ordering::Release);
                shared
                    .emergency_cleanup_scheduled
                    .store(false, Ordering::Release);
            }) {
            Ok(worker) => *handle = Some(worker),
            Err(_) => {
                self.emergency_cleanup_scheduled
                    .store(false, Ordering::SeqCst);
                self.controller.lock_fault();
                self.input_recovery_required.store(true, Ordering::SeqCst);
                self.safety_diagnostics.record(
                    "emergency_cleanup_worker_start_failed",
                    &[("status", "unsafe_manual_recovery_required".to_string())],
                );
            }
        }
    }

    #[cfg(windows)]
    fn record_safety(&self, event: &str, fields: &[(&str, String)]) {
        self.safety_diagnostics.record(event, fields);
    }

    #[cfg(windows)]
    fn record_safety_async(&self, event: &'static str, fields: Vec<(String, String)>) {
        self.safety_diagnostics.record_async(event, fields);
    }

    #[cfg(windows)]
    fn register_playback_input(
        &self,
        instance_id: u64,
        state: Arc<InjectedInputState>,
    ) -> Result<(), AppError> {
        let mut inputs = self.playback_inputs.lock().map_err(|_| {
            AppError::invalid("input_ledger_unavailable", "输入账本索引异常，已禁止启动宏")
        })?;
        inputs.insert(instance_id, state);
        Ok(())
    }

    #[cfg(windows)]
    fn unregister_playback_input(&self, instance_id: u64) {
        if let Ok(mut inputs) = self.playback_inputs.lock() {
            inputs.remove(&instance_id);
        }
    }

    #[cfg(windows)]
    fn playback_input_states(&self) -> Result<Vec<Arc<InjectedInputState>>, ()> {
        self.playback_inputs
            .try_lock()
            .map(|inputs| inputs.values().cloned().collect())
            .map_err(|_| ())
    }

    #[cfg(windows)]
    fn tracked_input_counts(&self) -> (usize, usize) {
        let (mut keys, mut buttons) = self.injected_input.counts();
        let (text_keys, text_buttons) = self.text_input.counts();
        keys = keys.saturating_add(text_keys);
        buttons = buttons.saturating_add(text_buttons);
        let Ok(states) = self.playback_inputs.try_lock() else {
            return (usize::MAX, usize::MAX);
        };
        for state in states.values() {
            let (state_keys, state_buttons) = state.counts();
            keys = keys.saturating_add(state_keys);
            buttons = buttons.saturating_add(state_buttons);
        }
        let broker_counts = self.input_broker.counts();
        if broker_counts != (keys, buttons) {
            return (usize::MAX, usize::MAX);
        }
        (keys, buttons)
    }

    #[cfg(windows)]
    fn check_recording_input_quiescence(&self) -> Result<(), AppError> {
        if self.tracked_input_counts() != (0, 0)
            || self
                .active_remaps
                .try_lock()
                .map_or(true, |remaps| !remaps.is_empty())
        {
            return Err(AppError::invalid(
                "recording_input_busy",
                "仍有程序注入或待释放的重映射输入，请松开按键并完成清理后再录制",
            ));
        }
        Ok(())
    }

    #[cfg(windows)]
    fn background_input_allowed_at(&self, generation: u64) -> bool {
        let Ok(_admission) = self.mode_admission.try_lock() else {
            return false;
        };
        let Ok(recorder) = self.recorder.try_lock() else {
            return false;
        };
        let Ok(behavior) = self.behavior.try_lock() else {
            return false;
        };
        !recorder.active
            && !self.initializing.load(Ordering::Acquire)
            && !behavior.status().active
            && self.playback_thread_owner.load(Ordering::Acquire) == 0
            && self.playback_thread_quiescent()
            && self.emergency_cleanup_quiescent()
            && self.controller.background_input_allowed_at(generation)
    }

    #[cfg(windows)]
    fn start_recording(
        &self,
        capture_mouse_move: bool,
        capture_mouse_clicks: bool,
    ) -> Result<(), AppError> {
        let _admission = self.acquire_mode_admission()?;
        if self.controller.phase() != RuntimePhase::Idle {
            return Err(AppError::invalid(
                "macro_busy",
                "运行控制器尚未空闲，请先完成停止和清理后再录制",
            ));
        }
        self.check_recording_input_quiescence()?;
        let mut recorder = self
            .recorder
            .try_lock()
            .map_err(|_| AppError::internal("录制器状态异常，请重启 AutoFlow"))?;
        if recorder.active {
            return Err(AppError::invalid("recording_active", "宏录制已经在进行中"));
        }
        if recorder.completed_steps.is_some() {
            return Err(AppError::invalid(
                "recording_result_pending",
                "上一份录制结果尚未领取，请先保存或明确丢弃",
            ));
        }
        if self
            .behavior
            .try_lock()
            .map_err(|_| {
                AppError::invalid(
                    "recording_state_unavailable",
                    "行为录制状态不可用，未开始录制",
                )
            })?
            .status()
            .active
        {
            return Err(AppError::invalid(
                "behavior_recording_active",
                "行为训练录制进行中，请先停止后再录制宏",
            ));
        }
        self.graph_capture
            .get()
            .ok_or_else(|| AppError::internal("宏录制队列尚未初始化"))?
            .begin()
            .map_err(|reason| AppError::invalid(reason, "宏录制队列不可用，未开始录制"))?;
        self.graph_capture_error.store(false, Ordering::Release);
        recorder.capture_mouse_move = capture_mouse_move;
        recorder.capture_mouse_clicks = capture_mouse_clicks;
        recorder.capture_started = false;
        self.controller.invalidate_background_admission();
        recorder.active = true;
        recorder.started_at = Some(Instant::now());
        recorder.last_event = None;
        recorder.last_mouse_move = None;
        recorder.steps.clear();
        recorder.completed_steps = None;
        recorder.pressed_keys.clear();
        recorder.pressed_buttons.clear();
        self.graph_boundary_started.store(false, Ordering::Release);
        self.graph_mouse_move_enabled
            .store(capture_mouse_move, Ordering::Release);
        self.graph_mouse_clicks_enabled
            .store(capture_mouse_clicks, Ordering::Release);
        self.graph_armed.store(true, Ordering::Release);
        Ok(())
    }

    #[cfg(windows)]
    fn set_recording_options(
        &self,
        capture_mouse_move: bool,
        capture_mouse_clicks: bool,
    ) -> Result<(), AppError> {
        let mut recorder = self
            .recorder
            .lock()
            .map_err(|_| AppError::internal("褰曞埗鍣ㄧ姸鎬佸紓甯革紝璇烽噸鍚?AutoFlow"))?;
        if !recorder.active {
            recorder.capture_mouse_move = capture_mouse_move;
            recorder.capture_mouse_clicks = capture_mouse_clicks;
            self.graph_mouse_move_enabled
                .store(capture_mouse_move, Ordering::Release);
            self.graph_mouse_clicks_enabled
                .store(capture_mouse_clicks, Ordering::Release);
        }
        Ok(())
    }

    #[cfg(windows)]
    fn start_behavior_recording(&self, name: &str) -> Result<(), AppError> {
        let _admission = self.acquire_mode_admission()?;
        if self.controller.phase() != RuntimePhase::Idle {
            return Err(AppError::invalid(
                "macro_busy",
                "宏正在运行，请先停止播放后再开始行为训练",
            ));
        }
        self.check_recording_input_quiescence()?;
        if self
            .recorder
            .try_lock()
            .map_err(|_| {
                AppError::invalid(
                    "recording_state_unavailable",
                    "宏录制状态不可用，未开始行为训练",
                )
            })?
            .active
        {
            return Err(AppError::invalid(
                "recording_busy",
                "图形宏录制正在进行中，请先停止后再开始行为训练",
            ));
        }
        let persist_raw_session = self
            .config
            .try_lock()
            .map_err(|_| AppError::internal("配置状态异常，请重启 AutoFlow"))?
            .retain_behavior_records;
        let mut behavior = self
            .behavior
            .try_lock()
            .map_err(|_| AppError::internal("行为录制器状态异常，请重启 AutoFlow"))?;
        let queue = self.behavior_capture.get().ok_or_else(|| {
            AppError::invalid("capture_not_ready", "行为事件队列未就绪，未开始录制")
        })?;
        // Validate recorder startup first, then open event admission while the
        // recorder guard still excludes worker processing.
        behavior.start(name, persist_raw_session)?;
        if let Err(code) = queue.begin() {
            behavior.reset();
            return Err(AppError::invalid(
                code,
                "行为队列存在未排空或不完整会话，请重启后再录制",
            ));
        }
        self.controller.invalidate_background_admission();
        Ok(())
    }

    #[cfg(windows)]
    fn stop_behavior_recording(&self) -> Result<BehaviorRecordingResult, AppError> {
        let queue = self
            .behavior_capture
            .get()
            .ok_or_else(|| AppError::invalid("capture_not_ready", "行为队列未就绪"))?;
        let drain_result = if queue.is_pending() {
            queue.drain(Duration::from_millis(500))
        } else if queue.is_failed() {
            Err("capture_incomplete")
        } else {
            Ok(())
        };
        drain_result.map_err(|code| {
            if let Ok(mut behavior) = self.behavior.try_lock() {
                behavior.freeze_capture(false);
            }
            let statistics = queue.statistics();
            self.record_safety("behavior_capture_stop_failed", &[
                ("reason", code.to_string()),
                ("accepted", statistics.accepted.to_string()),
                ("dropped", statistics.dropped.to_string()),
                ("processing_failed", statistics.processing_failed.to_string()),
            ]);
            AppError::invalid(
                code,
                format!("行为录制不完整或排空超时：接收 {}，丢弃 {}，处理失败 {}。未生成训练结果；请保留诊断并重启", statistics.accepted, statistics.dropped, statistics.processing_failed),
            )
        })?;
        let mut behavior = self
            .behavior
            .lock()
            .map_err(|_| AppError::internal("行为录制器状态异常，请重启 AutoFlow"))?;
        behavior.stop()
    }

    #[cfg(windows)]
    fn discard_behavior_recording(&self) -> Result<(), AppError> {
        let queue = self.behavior_capture.get().ok_or_else(|| {
            AppError::invalid("capture_not_ready", "behavior capture queue is unavailable")
        })?;
        queue
            .confirm_discard(Duration::from_millis(500))
            .map_err(|code| {
            if let Ok(mut behavior) = self.behavior.try_lock() {
                behavior.freeze_capture(false);
            }
            let statistics = queue.statistics();
            self.record_safety("behavior_capture_discard_failed", &[
                ("reason", code.to_string()),
                ("accepted", statistics.accepted.to_string()),
                ("dropped", statistics.dropped.to_string()),
                ("processing_failed", statistics.processing_failed.to_string()),
            ]);
            AppError::with_detail(
                code,
                "behavior capture could not confirm its worker barrier; the pending recording was retained",
                format!("{statistics:?}"),
            )
            })?;
        let mut behavior = self.behavior.lock().map_err(|_| {
            AppError::internal("behavior recorder state is unavailable; pending data was retained")
        })?;
        behavior.discard()?;
        queue.reset_discarded();
        self.behavior_capture_error.store(false, Ordering::Release);
        Ok(())
    }

    #[cfg(windows)]
    fn complete_behavior_recording_claim(&self) -> Result<(), AppError> {
        self.behavior
            .lock()
            .map_err(|_| AppError::internal("behavior recorder state is unavailable"))?
            .complete_claim()
    }

    #[cfg(windows)]
    fn behavior_recording_status(&self) -> BehaviorRecordingStatus {
        let mut status = self
            .behavior
            .lock()
            .map(|behavior| behavior.status())
            .unwrap_or(BehaviorRecordingStatus {
                active: false,
                pending: true,
                incomplete: true,
                capture_started: false,
                duration_ms: 0,
                event_count: 0,
                keyboard_events: 0,
                mouse_events: 0,
                wheel_events: 0,
                capped: false,
                persisting_raw_session: false,
                session_name: None,
            });
        if let Some(queue) = self.behavior_capture.get() {
            status.active &= queue.is_active();
            status.pending |= !queue.is_active() && queue.is_pending();
            if self.behavior_capture_error.load(Ordering::Acquire) || queue.is_failed() {
                status.incomplete = true;
            }
        }
        status
    }

    #[cfg(windows)]
    fn is_behavior_recording(&self) -> bool {
        self.behavior
            .try_lock()
            .map(|behavior| behavior.status().active)
            .unwrap_or(true)
    }

    #[cfg(windows)]
    fn record_behavior_key(&self, vk: u32, scan_code: u32, is_down: bool, at: Instant) {
        self.submit_behavior_capture(BehaviorCapture::Key(vk, scan_code, is_down), at);
    }

    #[cfg(windows)]
    fn record_behavior_mouse_move(&self, x: i32, y: i32, at: Instant) {
        self.submit_behavior_capture(BehaviorCapture::Move(x, y), at);
    }

    #[cfg(windows)]
    fn record_behavior_mouse_button(
        &self,
        button: MouseButton,
        is_down: bool,
        x: i32,
        y: i32,
        at: Instant,
    ) {
        self.submit_behavior_capture(
            BehaviorCapture::Button(mouse_button_id(button), is_down, x, y),
            at,
        );
    }

    #[cfg(windows)]
    fn record_behavior_wheel(&self, delta_x: i32, delta_y: i32, x: i32, y: i32, at: Instant) {
        self.submit_behavior_capture(BehaviorCapture::Wheel(delta_x, delta_y, x, y), at);
    }

    #[cfg(windows)]
    fn start_recording_from_hotkey(&self) -> Result<(), AppError> {
        self.start_recording(
            self.graph_mouse_move_enabled.load(Ordering::Acquire),
            self.graph_mouse_clicks_enabled.load(Ordering::Acquire),
        )
    }

    #[cfg(windows)]
    fn stop_recording(
        &self,
        discard_trailing_mouse_input: bool,
    ) -> Result<MacroRecordingResult, AppError> {
        self.drain_graph_capture()?;
        let mut recorder = self
            .recorder
            .lock()
            .map_err(|_| AppError::internal("录制器状态异常，请重启 AutoFlow"))?;
        if recorder.active {
            if discard_trailing_mouse_input {
                discard_trailing_mouse_input_steps(&mut recorder);
            }
            finish_recorder(&mut recorder);
        }
        let Some(steps) = recorder.completed_steps.take() else {
            return Err(AppError::invalid(
                "recording_inactive",
                "当前没有正在进行的宏录制",
            ));
        };
        Ok(MacroRecordingResult {
            steps,
            target: None,
        })
    }

    #[cfg(all(windows, test))]
    fn finish_recording(&self) {
        if self.drain_graph_capture().is_err() {
            return;
        }
        if let Ok(mut recorder) = self.recorder.lock() {
            if recorder.active {
                finish_recorder(&mut recorder);
            }
        }
    }

    #[cfg(windows)]
    fn finish_recording_from_hotkey(&self) {
        self.graph_armed.store(false, Ordering::Release);
        if let Some(queue) = self.graph_capture.get() {
            if queue.is_active()
                && !queue.submit_terminal(Instant::now(), GraphCapture::FinishShortcut)
                && !self.graph_capture_error.swap(true, Ordering::AcqRel)
            {
                self.record_safety("graph_capture_stop_admission_failed", &[]);
                self.notifications.publish(
                    "宏录制停止未完成",
                    "停止事件未能入队，本次录制不能播放。请保留诊断并停止测试。",
                );
            }
        }
    }

    #[cfg(windows)]
    fn is_recording(&self) -> bool {
        if let Some(queue) = self.graph_capture.get() {
            // Pending capture remains transparent to physical input until its
            // result is drained. Contention/failure is not an idle proof.
            return queue.is_active() || queue.is_pending() || queue.is_failed();
        }
        self.recorder
            .try_lock()
            .map_or(true, |recorder| recorder.active)
    }

    #[cfg(windows)]
    fn recording_status(&self) -> MacroRecordingStatus {
        self.recorder
            .lock()
            .map(|recorder| MacroRecordingStatus {
                active: recorder.active,
                capture_started: recorder.capture_started,
                step_count: if recorder.active {
                    recorder.steps.len()
                } else {
                    recorder.completed_steps.as_ref().map_or(0, Vec::len)
                },
                capture_mouse_move: recorder.capture_mouse_move,
                capture_mouse_clicks: recorder.capture_mouse_clicks,
                target_locked: false,
                target_name: None,
            })
            .unwrap_or(MacroRecordingStatus {
                active: false,
                capture_started: false,
                step_count: 0,
                capture_mouse_move: true,
                capture_mouse_clicks: true,
                target_locked: false,
                target_name: None,
            })
    }

    #[cfg(windows)]
    fn is_playback_running(&self) -> bool {
        self.playback
            .try_lock()
            .map_or(true, |playback| playback.running)
    }

    #[cfg(windows)]
    fn emergency_stop_ready(&self) -> bool {
        self.emergency_detector_ready.load(Ordering::Acquire)
            && self
                .priority_cleanup
                .get()
                .is_none_or(|worker| worker.is_ready())
            && self.hook_channel_ready()
            && self
                .emergency_thread_handle
                .get()
                .is_none_or(crate::bounded_worker::thread_running_confirmed)
            && self.emergency_thread_id.load(Ordering::Acquire) != 0
            && !self.emergency_shutdown.load(Ordering::Acquire)
            && detector_heartbeat_fresh(
                self.emergency_clock.elapsed().as_millis() as u64 + 1,
                self.emergency_heartbeat_ms.load(Ordering::Acquire),
            )
    }

    #[cfg(windows)]
    fn hook_channel_ready(&self) -> bool {
        self.hook_thread_handle.get().is_none_or(|handle| {
            self.hook_ready.load(Ordering::Acquire)
                && crate::bounded_worker::thread_running_confirmed(handle)
                && detector_heartbeat_fresh(
                    self.emergency_clock.elapsed().as_millis() as u64 + 1,
                    self.hook_heartbeat_ms.load(Ordering::Acquire),
                )
        })
    }

    #[cfg(windows)]
    fn post_hook_probe(&self) {
        self.post_hook_probe_with(|thread_id| unsafe {
            windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                thread_id,
                HOOK_HEALTH_PROBE_MESSAGE,
                windows::Win32::Foundation::WPARAM(0),
                windows::Win32::Foundation::LPARAM(0),
            )
            .is_ok()
        });
    }

    #[cfg(windows)]
    fn post_hook_probe_with(&self, send: impl FnOnce(u32) -> bool) {
        let thread_id = self.thread_id.load(Ordering::Acquire);
        if thread_id == 0
            || self.shutdown.load(Ordering::Acquire)
            || !self.hook_ready.load(Ordering::Acquire)
            || self
                .hook_probe_pending
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        if !send(thread_id) {
            self.hook_probe_pending.store(false, Ordering::Release);
            // A failed post must never renew health. Leave the previous
            // acknowledgment to expire; initialization remains unready.
        }
    }

    #[cfg(windows)]
    fn acknowledge_hook_probe(&self) {
        self.hook_heartbeat_ms.store(
            self.emergency_clock.elapsed().as_millis() as u64 + 1,
            Ordering::Release,
        );
        self.hook_probe_pending.store(false, Ordering::Release);
    }

    #[cfg(windows)]
    fn observe_control_channel_health(self: &Arc<Self>, desktop_available: bool) {
        let hook_available = self.hook_channel_ready();
        let ready =
            self.emergency_vk.load(Ordering::Acquire) != 0 && desktop_available && hook_available;
        let previously_ready = self.emergency_detector_ready.swap(ready, Ordering::AcqRel);
        if previously_ready && !ready {
            self.controller.lock_fault();
            self.request_emergency_stop(EmergencyEntryPoint::PhysicalDetector, true);
            self.record_safety_async(
                "input_control_channel_unavailable",
                vec![
                    ("desktop_available".into(), desktop_available.to_string()),
                    ("hook_available".into(), hook_available.to_string()),
                ],
            );
        }
    }

    #[cfg(windows)]
    fn signal_playback_stop(&self) {
        if let Ok(mut playback) = self.playback.try_lock() {
            if let Some(stop) = &playback.stop {
                stop.store(true, Ordering::SeqCst);
                if playback.running {
                    playback.phase = "stopping".to_string();
                    playback.cleanup_status = "not_started".to_string();
                }
            }
        }
    }

    #[cfg(windows)]
    fn playback_status(&self) -> MacroPlaybackStatus {
        let overlay_enabled = self
            .config
            .lock()
            .map(|config| config.show_playback_overlay)
            .unwrap_or(false);
        let recording = self.is_recording();
        let controller_phase = self.controller.phase();
        let now = Instant::now();
        self.playback
            .lock()
            .map(|mut playback| {
                if !playback.running
                    && !playback.terminal_persistent
                    && playback
                        .terminal_until
                        .is_some_and(|deadline| deadline <= now)
                {
                    playback.macro_id = None;
                    playback.macro_name = None;
                    playback.program_kind = None;
                    playback.action_summary = None;
                    playback.current_step_kind = None;
                    playback.total_steps = 0;
                    playback.last_error = None;
                    playback.phase = "idle".to_string();
                    playback.cleanup_status = "not_started".to_string();
                    playback.started_at = None;
                    playback.terminal_until = None;
                    playback.terminal_persistent = false;
                }
                let terminal_active = playback.running
                    || playback.terminal_persistent
                    || playback.terminal_until.is_some();
                MacroPlaybackStatus {
                    running: playback.running,
                    current_step: playback.current_step,
                    total_steps: playback.total_steps,
                    last_error: playback.last_error.clone(),
                    playback_id: playback.instance_id,
                    macro_id: playback.macro_id.clone(),
                    macro_name: playback.macro_name.clone(),
                    program_kind: playback
                        .program_kind
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                    action_kind: playback.current_step_kind.clone(),
                    action_summary: playback.action_summary.clone(),
                    elapsed_ms: playback
                        .started_at
                        .map(|started| now.saturating_duration_since(started).as_millis() as u64)
                        .unwrap_or(0),
                    phase: if controller_phase == RuntimePhase::FaultLocked
                        || playback.phase.is_empty()
                    {
                        runtime_phase_name(controller_phase).to_string()
                    } else {
                        playback.phase.clone()
                    },
                    cleanup_status: if playback.cleanup_status.is_empty() {
                        "not_started".to_string()
                    } else {
                        playback.cleanup_status.clone()
                    },
                    overlay_visible: overlay_enabled
                        && !recording
                        && terminal_active
                        && playback.macro_name.is_some(),
                }
            })
            .unwrap_or_else(|_| MacroPlaybackStatus {
                running: false,
                current_step: 0,
                total_steps: 0,
                last_error: Some("播放状态读取失败，请重启 AutoFlow".to_string()),
                playback_id: 0,
                macro_id: None,
                macro_name: None,
                program_kind: "unknown".to_string(),
                action_kind: None,
                action_summary: None,
                elapsed_ms: 0,
                phase: "failed".to_string(),
                cleanup_status: "failed".to_string(),
                overlay_visible: false,
            })
    }

    #[cfg(windows)]
    fn set_playback_start_error(&self, message: impl Into<String>) {
        if let Ok(mut playback) = self.playback.lock() {
            // A rejected second start must never rewrite the state of the
            // already-running instance.  The old implementation cleared
            // `stop` and `running` here, leaving the old thread injecting
            // input while allowing the next trigger to start another thread.
            if playback.running || playback.stop.is_some() {
                return;
            }
            playback.running = false;
            playback.stop = None;
            playback.last_error = Some(message.into());
            playback.phase = "failed".to_string();
            playback.cleanup_status = "safe".to_string();
            playback.terminal_until = None;
            playback.terminal_persistent = false;
        }
    }

    #[cfg(windows)]
    fn set_playback_action(
        &self,
        instance_id: u64,
        current_step: usize,
        kind: impl Into<String>,
        summary: Option<String>,
    ) {
        if let Ok(mut playback) = self.playback.lock() {
            if playback.instance_id != instance_id || !playback.running {
                return;
            }
            playback.current_step = current_step;
            playback.current_step_kind = Some(kind.into());
            playback.action_summary = summary;
        }
    }

    #[cfg(windows)]
    fn set_playback_phase(
        &self,
        instance_id: u64,
        phase: &'static str,
        cleanup_status: &'static str,
        error: Option<String>,
    ) {
        if let Ok(mut playback) = self.playback.lock() {
            if playback.instance_id != instance_id || !playback.running {
                return;
            }
            playback.phase = phase.to_string();
            playback.cleanup_status = cleanup_status.to_string();
            if let Some(error) = error {
                playback.last_error = Some(error);
            }
        }
    }

    #[cfg(windows)]
    fn finalize_playback_instance(
        playback: &mut PlaybackState,
        instance_id: u64,
        error: Option<String>,
    ) -> bool {
        if playback.instance_id != instance_id {
            return false;
        }
        playback.running = false;
        playback.stop = None;
        playback.run_token = None;
        playback.input_state = None;
        playback.restore_window = None;
        if let Some(error) = error {
            playback.last_error = Some(error);
            if playback.phase != "cleanup_failed" {
                playback.phase = "failed".to_string();
            }
        } else if playback.phase == "running" || playback.phase.is_empty() {
            playback.phase = "completed".to_string();
            playback.cleanup_status = "safe".to_string();
        }
        if playback.phase == "cleanup_failed" {
            // A cleanup failure is a safety warning, not a transient result.
            // Keep it visible until the next playback instance resets the
            // snapshot, so it cannot be mistaken for a safe stop.
            playback.terminal_persistent = true;
            playback.terminal_until = None;
        } else {
            playback.terminal_persistent = false;
            playback.terminal_until = Some(Instant::now() + Duration::from_millis(1800));
        }
        true
    }

    #[cfg(all(windows, test))]
    fn start_playback(self: &Arc<Self>, macro_rule: MacroRule) -> Result<(), AppError> {
        let expected_generation = self.emergency_generation.load(Ordering::SeqCst);
        self.start_playback_at_generation(macro_rule, expected_generation)
    }

    #[cfg(windows)]
    fn acquire_mode_admission(&self) -> Result<std::sync::MutexGuard<'_, ()>, AppError> {
        if self.initializing.load(Ordering::Acquire) {
            return Err(AppError::invalid(
                "runtime_initializing",
                "安全运行服务尚未完成初始化，未排队启动",
            ));
        }
        if self.playback_thread_owner.load(Ordering::Acquire) != 0
            || !self.playback_thread_quiescent()
            || !self.emergency_cleanup_quiescent()
        {
            return Err(AppError::invalid(
                "macro_busy",
                "上一执行或急停清理尚未完成退出收尾，未排队启动",
            ));
        }
        self.mode_admission.try_lock().map_err(|_| {
            AppError::invalid(
                "runtime_admission_busy",
                "播放或录制的启动准入正在处理中，未排队，请重新操作",
            )
        })
    }

    #[cfg(windows)]
    fn playback_thread_quiescent(&self) -> bool {
        self.playback_thread_handle.try_lock().is_ok_and(|entry| {
            entry
                .as_ref()
                .is_none_or(|(_, handle)| crate::bounded_worker::thread_exit_confirmed(handle))
        })
    }

    #[cfg(windows)]
    fn start_behavior_capture_worker(self: &Arc<Self>) -> Result<(), AppError> {
        let weak = Arc::downgrade(self);
        let queue = crate::bounded_worker::CaptureQueue::spawn(move |_session, at, event| {
            let Some(shared) = weak.upgrade() else {
                return false;
            };
            let Ok(mut behavior) = shared.behavior.lock() else {
                return false;
            };
            match event {
                BehaviorCapture::Key(vk, scan, down) => behavior.record_key_at(vk, scan, down, at),
                BehaviorCapture::Move(x, y) => behavior.record_mouse_move_at(x, y, at),
                BehaviorCapture::Button(button, down, x, y) => {
                    behavior.record_mouse_button_at(button, down, x, y, at)
                }
                BehaviorCapture::Wheel(dx, dy, x, y) => behavior.record_wheel_at(dx, dy, x, y, at),
            }
            true
        })
        .map_err(|error| {
            AppError::with_detail(
                "capture_worker_start_failed",
                "行为事件工作线程启动失败",
                error.to_string(),
            )
        })?;
        self.behavior_capture
            .set(queue)
            .map_err(|_| AppError::internal("行为队列重复初始化"))
    }

    #[cfg(windows)]
    fn start_graph_capture_worker(self: &Arc<Self>) -> Result<(), AppError> {
        let weak = Arc::downgrade(self);
        let queue = crate::bounded_worker::CaptureQueue::spawn(move |_, at, event| {
            let Some(shared) = weak.upgrade() else {
                return false;
            };
            let Ok(mut recorder) = shared.recorder.lock() else {
                return false;
            };
            apply_graph_capture(&mut recorder, event, at);
            true
        })
        .map_err(|error| {
            AppError::with_detail(
                "capture_worker_start_failed",
                "宏录制工作线程启动失败",
                error.to_string(),
            )
        })?;
        self.graph_capture
            .set(queue)
            .map_err(|_| AppError::internal("宏录制队列重复初始化"))
    }

    #[cfg(windows)]
    fn submit_graph_capture(&self, event: GraphCapture, at: Instant) {
        if let Some(queue) = self.graph_capture.get() {
            if queue.is_active()
                && !queue.submit(at, event)
                && !self.graph_capture_error.swap(true, Ordering::AcqRel)
            {
                self.record_safety("graph_capture_incomplete", &[]);
                self.notifications.publish(
                    "宏录制不完整",
                    "事件队列不可用，本次不会生成可播放录制结果。请停止并保留诊断。",
                );
            }
        }
    }

    #[cfg(windows)]
    fn track_physical_key_event(self: &Arc<Self>, raw_vk: u32, is_down: bool, is_up: bool) -> bool {
        let Ok(mut ledger) = self.physical_pressed.try_lock() else {
            self.physical_ledger_uncertain
                .store(true, Ordering::Release);
            self.trigger_rearm_required.store(true, Ordering::Release);
            self.request_emergency_stop(EmergencyEntryPoint::LowLevelHook, false);
            self.record_safety_async(
                "physical_key_ledger_unavailable",
                vec![("phase".to_string(), "hook_event".to_string())],
            );
            return false;
        };
        update_physical_pressed_ledger(&mut ledger, raw_vk, is_down, is_up, false);
        true
    }

    #[cfg(windows)]
    fn initialize_physical_key_ledger(&self) -> bool {
        match self.physical_pressed.lock() {
            Ok(mut ledger) => {
                ledger.clear();
                self.physical_ledger_uncertain
                    .store(false, Ordering::Release);
                true
            }
            Err(_) => {
                self.physical_ledger_uncertain
                    .store(true, Ordering::Release);
                self.controller.lock_fault();
                false
            }
        }
    }

    #[cfg(windows)]
    fn retire_physical_key_ledger(&self) {
        self.physical_ledger_uncertain
            .store(true, Ordering::Release);
        if let Ok(mut ledger) = self.physical_pressed.try_lock() {
            ledger.clear();
        }
    }

    #[cfg(windows)]
    fn reject_hook_event(self: &Arc<Self>, reason: &'static str, released_key: bool) {
        let graph_loss = self
            .graph_capture
            .get()
            .is_some_and(|queue| queue.note_admission_loss());
        let behavior_loss = self
            .behavior_capture
            .get()
            .is_some_and(|queue| queue.note_admission_loss());
        if graph_loss && !self.graph_capture_error.swap(true, Ordering::AcqRel) {
            self.notifications.publish(
                "宏录制不完整",
                "钩子事件状态不可用，本次录制不能生成可播放结果。请保留诊断。",
            );
        }
        if behavior_loss && !self.behavior_capture_error.swap(true, Ordering::AcqRel) {
            self.notifications.publish(
                "行为录制不完整",
                "钩子事件状态不可用，本次不会生成训练结果。请保留诊断。",
            );
        }
        // Lost physical state must not unlock a shortcut from stale pressed
        // bookkeeping. A missed up may also belong to an injected remap:
        // priority stop/ledger cleanup is safer than waiting for another up.
        self.trigger_rearm_required.store(true, Ordering::Release);
        if released_key {
            self.request_emergency_stop(EmergencyEntryPoint::LowLevelHook, false);
        }
        if graph_loss || behavior_loss {
            self.record_safety_async(
                "hook_capture_admission_failed",
                vec![("reason".to_string(), reason.to_string())],
            );
        }
    }

    #[cfg(windows)]
    fn drain_graph_capture(&self) -> Result<(), AppError> {
        self.graph_armed.store(false, Ordering::Release);
        let Some(queue) = self.graph_capture.get() else {
            return Ok(());
        };
        if !queue.is_pending() {
            if queue.is_failed() {
                return Err(AppError::invalid(
                    "capture_incomplete",
                    "宏录制已发生事件丢失，不能领取为可播放结果，请保留诊断",
                ));
            }
            return Ok(());
        }
        queue.drain(Duration::from_millis(500)).map_err(|reason| {
            let counts = queue.statistics();
            self.record_safety(
                "graph_capture_drain_failed",
                &[
                    ("reason", reason.to_string()),
                    ("accepted", counts.accepted.to_string()),
                    ("dropped", counts.dropped.to_string()),
                    ("processing_failed", counts.processing_failed.to_string()),
                ],
            );
            AppError::with_detail(
                reason,
                "宏录制事件未完整处理，结果已保留但不能播放",
                format!("{counts:?}"),
            )
        })
    }

    #[cfg(windows)]
    fn submit_behavior_capture(&self, event: BehaviorCapture, at: Instant) {
        if let Some(queue) = self.behavior_capture.get() {
            if queue.is_active()
                && !queue.submit(at, event)
                && !self.behavior_capture_error.swap(true, Ordering::AcqRel)
            {
                self.record_safety("behavior_capture_incomplete", &[]);
                self.notifications.publish(
                    "行为录制不完整",
                    "事件队列溢出或不可用，本次不会生成训练结果。请停止录制并保留诊断。",
                );
            }
        }
    }

    #[cfg(windows)]
    fn start_launch_worker(self: &Arc<Self>) -> Result<(), AppError> {
        let weak = Arc::downgrade(self);
        let worker = crate::bounded_worker::BoundedWorker::spawn(
            "autoflow-launch-worker",
            8,
            move |task: LaunchTask| {
                let Some(shared) = weak.upgrade() else {
                    return;
                };
                launch_target(&task.target, || {
                    shared.background_input_allowed_at(task.generation)
                });
            },
        )
        .map_err(|error| {
            AppError::with_detail(
                "launch_worker_start_failed",
                "快捷启动工作线程初始化失败",
                error.to_string(),
            )
        })?;
        self.launch_tasks
            .set(worker)
            .map_err(|_| AppError::internal("快捷启动队列重复初始化"))
    }

    #[cfg(windows)]
    fn submit_launch(&self, target: &str, generation: u64) -> bool {
        if target.len() > 16 * 1024
            || target
                .lines()
                .filter(|line| !line.trim().is_empty())
                .take(33)
                .count()
                > 32
            || !self.controller.background_input_allowed_at(generation)
        {
            return false;
        }
        self.launch_tasks.get().is_some_and(|worker| {
            worker
                .try_submit(LaunchTask {
                    target: target.to_owned(),
                    generation,
                })
                .is_ok()
        })
    }

    #[cfg(windows)]
    fn start_remap_worker(self: &Arc<Self>) -> Result<(), AppError> {
        let weak = Arc::downgrade(self);
        let worker = crate::bounded_worker::BoundedWorker::spawn(
            "autoflow-remap-worker",
            32,
            move |task: RemapTask| {
                let Some(shared) = weak.upgrade() else {
                    return;
                };
                let permission_denied = std::cell::Cell::new(false);
                let result = if task.down {
                    shared.injected_input.send_key_down_with_permission(
                        task.target,
                        None,
                        || {
                            if shared.background_input_allowed_at(task.generation) {
                                Ok(())
                            } else {
                                permission_denied.set(true);
                                Err("重映射准入已撤销".into())
                            }
                        },
                        || send_key(task.target, true),
                    )
                } else {
                    shared
                        .injected_input
                        .send_key_up(task.target, None, || send_key(task.target, false))
                };
                if let Err(error) = result {
                    if permission_denied.get() {
                        // No OS down occurred. Invalidated convenience work
                        // must not cancel the newer macro that superseded it.
                        return;
                    }
                    shared.record_safety("remap_worker_send_failed", &[("error", error)]);
                    shared.request_emergency_stop(EmergencyEntryPoint::Command, true);
                    return;
                }
                if !task.down {
                    if let Ok(mut remaps) = shared.active_remaps.try_lock() {
                        if remaps.get(&task.source) == Some(&task.target) {
                            remaps.remove(&task.source);
                        }
                    } else {
                        shared.request_emergency_stop(EmergencyEntryPoint::Command, true);
                    }
                }
            },
        )
        .map_err(|error| {
            AppError::with_detail(
                "remap_worker_start_failed",
                "重映射工作线程启动失败",
                error.to_string(),
            )
        })?;
        self.remap_tasks
            .set(worker)
            .map_err(|_| AppError::internal("重映射工作队列重复初始化"))
    }

    #[cfg(windows)]
    fn submit_remap_down(&self, source: u32, target: u32, generation: u64) -> bool {
        let Ok(mut remaps) = self.active_remaps.try_lock() else {
            return false;
        };
        if remaps.contains_key(&source) || remaps.values().any(|held| *held == target) {
            return false;
        }
        remaps.insert(source, target);
        let task = RemapTask {
            source,
            target,
            generation,
            down: true,
        };
        if self
            .remap_tasks
            .get()
            .is_none_or(|worker| worker.try_submit(task).is_err())
        {
            remaps.remove(&source);
            return false;
        }
        true
    }

    #[cfg(windows)]
    fn submit_remap_up(self: &Arc<Self>, source: u32) -> bool {
        let target = match self.active_remaps.try_lock() {
            Ok(remaps) => remaps.get(&source).copied(),
            Err(_) => {
                self.request_emergency_stop(EmergencyEntryPoint::Command, false);
                return false;
            }
        };
        let Some(target) = target else {
            return false;
        };
        let task = RemapTask {
            source,
            target,
            generation: self.controller.background_generation(),
            down: false,
        };
        if self
            .remap_tasks
            .get()
            .is_none_or(|worker| worker.try_submit(task).is_err())
        {
            self.request_emergency_stop(EmergencyEntryPoint::Command, false);
        }
        true
    }

    #[cfg(windows)]
    fn allocate_hold_epoch(&self) -> Option<u64> {
        loop {
            let current = self.next_hold_epoch.load(Ordering::Acquire);
            if current == u64::MAX || self.hold_epoch_exhausted.load(Ordering::Acquire) {
                self.hold_epoch_exhausted.store(true, Ordering::Release);
                return None;
            }
            let next = current + 1;
            if self
                .next_hold_epoch
                .compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(next);
            }
        }
    }

    #[cfg(windows)]
    fn publish_hold_lifecycle(&self, rule: &MacroRule, owner_vk: u32) -> Option<u64> {
        let epoch = self.allocate_hold_epoch()?;
        let Ok(mut current) = self.hold_lifecycle.try_lock() else {
            return None;
        };
        if current.is_some() || self.hold_lifecycle_epoch.load(Ordering::Acquire) != 0 {
            return None;
        }

        let mut words = [0_u64; 4];
        let index = usize::try_from(owner_vk / 64).ok()?;
        if index >= words.len() {
            return None;
        }
        words[index] |= 1_u64 << (owner_vk % 64);
        *current = Some(HoldLifecycleIdentity {
            epoch,
            macro_id: rule.id.clone(),
            rule_snapshot: trigger_macro_snapshot(rule),
            owner_vk,
            phase: HoldLifecyclePhase::Pending,
            cancelled: false,
        });
        for (target, value) in self.hold_trigger_words.iter().zip(words) {
            target.store(value, Ordering::Release);
        }
        self.hold_bound_token_id.store(0, Ordering::Release);
        self.hold_bound_token_generation.store(0, Ordering::Release);
        self.hold_lifecycle_phase.store(1, Ordering::Release);
        // Publish the epoch last. Callback readers use it as the ownership
        // fence for all other immutable/atomic identity fields.
        self.hold_lifecycle_epoch.store(epoch, Ordering::Release);
        Some(epoch)
    }

    #[cfg(windows)]
    fn validate_hold_lifecycle(
        &self,
        rule: &MacroRule,
        expected_epoch: Option<u64>,
    ) -> Result<(), AppError> {
        let Some(epoch) = expected_epoch else {
            return Ok(());
        };
        if self.hold_lifecycle_epoch.load(Ordering::Acquire) != epoch
            || self.hold_cancelled_epoch.load(Ordering::Acquire) >= epoch
        {
            return Err(AppError::invalid(
                "macro_hold_released",
                "Hold 快捷键已松开或生命周期已失效，本次启动已取消",
            ));
        }
        let current = self.hold_lifecycle.lock().map_err(|_| {
            AppError::invalid(
                "macro_hold_lifecycle_unavailable",
                "Hold 快捷键生命周期不可确认，本次启动已取消",
            )
        })?;
        let valid = current.as_ref().is_some_and(|identity| {
            identity.epoch == epoch
                && !identity.cancelled
                && identity.macro_id == rule.id
                && identity.rule_snapshot == trigger_macro_snapshot(rule)
                && !matches!(identity.phase, HoldLifecyclePhase::Retired)
        });
        if valid
            && self.hold_lifecycle_epoch.load(Ordering::Acquire) == epoch
            && self.hold_cancelled_epoch.load(Ordering::Acquire) < epoch
        {
            Ok(())
        } else {
            Err(AppError::invalid(
                "macro_hold_released",
                "Hold 快捷键已松开或生命周期已失效，本次启动已取消",
            ))
        }
    }

    #[cfg(windows)]
    fn bind_hold_lifecycle_run(
        &self,
        rule: &MacroRule,
        expected_epoch: Option<u64>,
        run_token: RunToken,
    ) -> Result<(), AppError> {
        let Some(epoch) = expected_epoch else {
            return Ok(());
        };
        self.validate_hold_lifecycle(rule, expected_epoch)?;
        let mut current = self.hold_lifecycle.lock().map_err(|_| {
            AppError::invalid(
                "macro_hold_lifecycle_unavailable",
                "Hold 快捷键生命周期不可确认，本次启动已取消",
            )
        })?;
        let Some(identity) = current.as_mut().filter(|identity| {
            identity.epoch == epoch
                && !identity.cancelled
                && matches!(identity.phase, HoldLifecyclePhase::Pending)
        }) else {
            return Err(AppError::invalid(
                "macro_hold_released",
                "Hold 快捷键已松开或生命周期已失效，本次启动已取消",
            ));
        };
        if self.hold_cancelled_epoch.load(Ordering::Acquire) >= epoch {
            identity.cancelled = true;
            return Err(AppError::invalid(
                "macro_hold_released",
                "Hold 快捷键已松开，本次启动已取消",
            ));
        }
        identity.phase = HoldLifecyclePhase::Bound { run_token };
        self.hold_bound_token_id
            .store(run_token.id, Ordering::Release);
        self.hold_bound_token_generation
            .store(run_token.generation, Ordering::Release);
        self.hold_lifecycle_phase.store(2, Ordering::Release);
        Ok(())
    }

    #[cfg(windows)]
    fn activate_hold_lifecycle(
        &self,
        rule: &MacroRule,
        expected_epoch: Option<u64>,
        run_token: RunToken,
        instance_id: u64,
    ) -> Result<(), AppError> {
        let Some(epoch) = expected_epoch else {
            return Ok(());
        };
        self.validate_hold_lifecycle(rule, expected_epoch)?;
        let mut current = self.hold_lifecycle.lock().map_err(|_| {
            AppError::invalid(
                "macro_hold_lifecycle_unavailable",
                "Hold 快捷键生命周期不可确认，本次启动已取消",
            )
        })?;
        let Some(identity) = current.as_mut().filter(|identity| {
            identity.epoch == epoch
                && !identity.cancelled
                && matches!(
                    identity.phase,
                    HoldLifecyclePhase::Bound { run_token: token } if token == run_token
                )
        }) else {
            return Err(AppError::invalid(
                "macro_hold_released",
                "Hold 快捷键已松开或生命周期已失效，本次启动已取消",
            ));
        };
        if self.hold_cancelled_epoch.load(Ordering::Acquire) >= epoch {
            identity.cancelled = true;
            return Err(AppError::invalid(
                "macro_hold_released",
                "Hold 快捷键已松开，本次启动已取消",
            ));
        }
        identity.phase = HoldLifecyclePhase::Active {
            run_token,
            instance_id,
        };
        self.hold_lifecycle_phase.store(3, Ordering::Release);
        Ok(())
    }

    #[cfg(windows)]
    fn abort_activated_hold_start(
        &self,
        expected_epoch: Option<u64>,
        run_token: RunToken,
        registered_instance: Option<u64>,
    ) {
        self.controller.revoke_token(run_token);
        if let Some(instance_id) = registered_instance {
            self.unregister_playback_input(instance_id);
        }
        if let Some(epoch) = expected_epoch {
            // Retire before making the controller idle. A stale keyup from this
            // Hold must never revoke a newer unrelated run.
            self.retire_hold_lifecycle(epoch);
        }
        let _ = self.controller.begin_cleaning(run_token);
        let _ = self.controller.finish(run_token, true);
    }

    #[cfg(windows)]
    fn retire_hold_lifecycle(&self, epoch: u64) {
        let Ok(mut current) = self.hold_lifecycle.lock() else {
            self.controller.lock_fault();
            return;
        };
        if !current
            .as_ref()
            .is_some_and(|identity| identity.epoch == epoch)
        {
            return;
        }
        if let Some(identity) = current.as_mut() {
            identity.phase = HoldLifecyclePhase::Retired;
        }
        self.hold_lifecycle_phase.store(0, Ordering::Release);
        self.hold_lifecycle_epoch.store(0, Ordering::Release);
        self.hold_bound_token_id.store(0, Ordering::Release);
        self.hold_bound_token_generation.store(0, Ordering::Release);
        for word in &self.hold_trigger_words {
            word.store(0, Ordering::Release);
        }
        *current = None;
    }

    #[cfg(windows)]
    fn revoke_current_hold_for_key_up(&self, vk: u32) -> bool {
        let mut bound_token = None;
        let mut matched_epoch = None;
        let matched = match self.hold_lifecycle.try_lock() {
            Ok(mut current) => {
                let matched = cancel_hold_identity_for_key(
                    &mut current,
                    vk,
                    &mut matched_epoch,
                    &mut bound_token,
                );
                if let Some(epoch) = matched_epoch {
                    self.hold_cancelled_epoch.fetch_max(epoch, Ordering::AcqRel);
                }
                if let Some(token) = bound_token.filter(|token| token.id != 0) {
                    self.revoke_bound_hold_token(token);
                }
                matched
            }
            Err(std::sync::TryLockError::Poisoned(error)) => {
                let mut current = error.into_inner();
                let matched = cancel_hold_identity_for_key(
                    &mut current,
                    vk,
                    &mut matched_epoch,
                    &mut bound_token,
                );
                if let Some(epoch) = matched_epoch {
                    self.hold_cancelled_epoch.fetch_max(epoch, Ordering::AcqRel);
                }
                if let Some(token) = bound_token.filter(|token| token.id != 0) {
                    self.revoke_bound_hold_token(token);
                }
                matched
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                let Some(epoch) = self.cancel_matching_hold_epoch_atomic(vk) else {
                    return false;
                };
                matched_epoch = Some(epoch);
                bound_token = self
                    .load_hold_atomic_snapshot(vk)
                    .filter(|snapshot| snapshot.epoch == epoch && snapshot.trigger_matches)
                    .and_then(|snapshot| snapshot.bound_token);

                // A Pending snapshot can become Bound immediately after it is
                // read. Re-read the same epoch after publishing cancellation
                // so that an exact token which raced with key-up is revoked as
                // well. If it remains Pending, the worker's lifecycle checks
                // observe the cancellation watermark before activation.
                if bound_token.is_none() {
                    if let Some(after_cancel) = self
                        .load_hold_atomic_snapshot(vk)
                        .filter(|after| after.epoch == epoch && after.trigger_matches)
                    {
                        bound_token = after_cancel.bound_token;
                    }
                }
                if let Some(token) = bound_token.filter(|token| token.id != 0) {
                    self.revoke_bound_hold_token(token);
                }
                true
            }
        };
        if !matched {
            return false;
        }
        // Cancellation watermark publication and any exact-token revocation
        // have already completed. Diagnostic allocation/queue loss therefore
        // cannot delay or undo Hold input-permission revocation.
        self.record_safety_async(
            "macro_hold_release_cancelled",
            hold_diagnostic_fields(
                None,
                "release_cancellation_published",
                matched_epoch,
                bound_token,
                None,
                Some(vk),
                Some(vk),
            ),
        );
        true
    }

    #[cfg(windows)]
    fn revoke_bound_hold_token(&self, expected_token: RunToken) {
        self.controller.revoke_token(expected_token);
    }

    #[cfg(windows)]
    fn load_hold_atomic_snapshot(&self, vk: u32) -> Option<HoldAtomicSnapshot> {
        // Phase transitions are monotonic within an epoch (Pending -> Bound ->
        // Active -> Retired). Reading phase and epoch again therefore rejects
        // both an in-progress transition and fields from a replacement
        // identity without ever blocking the keyboard callback.
        for _ in 0..4 {
            let epoch_before = self.hold_lifecycle_epoch.load(Ordering::Acquire);
            if epoch_before == 0 {
                return None;
            }
            let trigger_matches = hold_trigger_word_contains(&self.hold_trigger_words, vk);
            let phase_before = self.hold_lifecycle_phase.load(Ordering::Acquire);
            let token = RunToken {
                id: self.hold_bound_token_id.load(Ordering::Acquire),
                generation: self.hold_bound_token_generation.load(Ordering::Acquire),
            };
            let phase_after = self.hold_lifecycle_phase.load(Ordering::Acquire);
            let epoch_after = self.hold_lifecycle_epoch.load(Ordering::Acquire);
            if let Some(snapshot) = consistent_hold_atomic_snapshot(
                epoch_before,
                trigger_matches,
                phase_before,
                token,
                phase_after,
                epoch_after,
            ) {
                return Some(snapshot);
            }
        }
        None
    }

    #[cfg(windows)]
    fn owns_current_hold_owner_keydown(&self, vk: u32) -> bool {
        let Some(snapshot) = self.load_hold_atomic_snapshot(vk) else {
            return false;
        };
        snapshot.trigger_matches
            && self.hold_cancelled_epoch.load(Ordering::Acquire) < snapshot.epoch
            && self.hold_lifecycle_epoch.load(Ordering::Acquire) == snapshot.epoch
    }

    #[cfg(windows)]
    fn cancel_matching_hold_epoch_atomic(&self, vk: u32) -> Option<u64> {
        // Publish cancellation as soon as a matching epoch/bitmap pair is
        // observed. If the identity changes while it is read, cancelling the
        // older unique epoch is harmless and the retry handles the current
        // identity as well.
        let mut matched = None;
        for _ in 0..4 {
            let epoch_before = self.hold_lifecycle_epoch.load(Ordering::Acquire);
            if epoch_before == 0 {
                return matched;
            }
            let trigger_matches = hold_trigger_word_contains(&self.hold_trigger_words, vk);
            if trigger_matches {
                self.hold_cancelled_epoch
                    .fetch_max(epoch_before, Ordering::AcqRel);
                matched = Some(epoch_before);
            }
            let epoch_after = self.hold_lifecycle_epoch.load(Ordering::Acquire);
            if epoch_before == epoch_after {
                return trigger_matches.then_some(epoch_before).or(matched);
            }
        }
        matched
    }

    #[cfg(windows)]
    fn start_trigger_worker(self: &Arc<Self>) -> Result<(), AppError> {
        let weak = Arc::downgrade(self);
        let worker = crate::bounded_worker::BoundedWorker::spawn(
            "autoflow-trigger-worker",
            1,
            move |task: MacroTriggerTask| {
                let Some(shared) = weak.upgrade() else {
                    return;
                };
                let _reset = TriggerPendingReset(&shared.trigger_pending);
                let mut hold_reset = PendingHoldLifecycleReset {
                    shared: &shared,
                    epoch: task.hold_epoch,
                    transferred_to_playback: false,
                };
                if matches!(task.start_timing, TriggerStartTiming::HoldModifierRelease) {
                    shared.record_safety_async(
                        "macro_hold_modifier_wait",
                        hold_diagnostic_fields(
                            Some(&task.rule.id),
                            if task.hold_modifier_vks.is_empty() {
                                "owner_only_ready_check"
                            } else {
                                "waiting_for_modifier_release"
                            },
                            task.hold_epoch,
                            None,
                            task.trigger_vk,
                            task.hold_owner_vk,
                            None,
                        ),
                    );
                }
                let outcome = match task.start_timing {
                    TriggerStartTiming::ReleaseGated => wait_for_trigger_release(&shared, &task),
                    TriggerStartTiming::HoldModifierRelease => {
                        wait_for_hold_modifier_release(&shared, &task)
                    }
                };
                if !matches!(outcome, TriggerReleaseGateOutcome::Ready) {
                    let (event, reason) = match outcome {
                        TriggerReleaseGateOutcome::Timeout => (
                            if matches!(task.start_timing, TriggerStartTiming::HoldModifierRelease)
                            {
                                "macro_hold_modifier_release_timeout"
                            } else {
                                "macro_trigger_release_timeout"
                            },
                            "physical_release_timeout",
                        ),
                        TriggerReleaseGateOutcome::Shutdown => (
                            if matches!(task.start_timing, TriggerStartTiming::HoldModifierRelease)
                            {
                                "macro_hold_modifier_release_shutdown"
                            } else {
                                "macro_trigger_release_shutdown"
                            },
                            "shutdown",
                        ),
                        TriggerReleaseGateOutcome::Cancelled(reason) => (
                            if matches!(task.start_timing, TriggerStartTiming::HoldModifierRelease)
                            {
                                "macro_hold_modifier_release_cancelled"
                            } else {
                                "macro_trigger_release_cancelled"
                            },
                            trigger_release_cancellation_name(reason),
                        ),
                        TriggerReleaseGateOutcome::Ready => unreachable!(),
                    };
                    shared.record_safety(
                        event,
                        &[
                            ("macro_id", task.rule.id.clone()),
                            ("reason", reason.to_string()),
                            (
                                "start_timing",
                                match task.start_timing {
                                    TriggerStartTiming::ReleaseGated => "release_gated",
                                    TriggerStartTiming::HoldModifierRelease => {
                                        "hold_modifier_release"
                                    }
                                }
                                .to_string(),
                            ),
                        ],
                    );
                    return;
                }
                if matches!(task.start_timing, TriggerStartTiming::HoldModifierRelease) {
                    shared.record_safety_async(
                        "macro_hold_modifier_ready",
                        hold_diagnostic_fields(
                            Some(&task.rule.id),
                            "modifier_release_stable",
                            task.hold_epoch,
                            None,
                            task.trigger_vk,
                            task.hold_owner_vk,
                            None,
                        ),
                    );
                }
                if let Err(error) = shared.start_hotkey_playback_at_revision(
                    task.rule.clone(),
                    task.generation,
                    task.admission_revision,
                    task.config_revision,
                    task.hold_epoch,
                ) {
                    if let Some(epoch) = task.hold_epoch {
                        shared.record_safety_async(
                            "macro_hold_start_failed",
                            hold_diagnostic_fields(
                                Some(&task.rule.id),
                                &error.code,
                                Some(epoch),
                                None,
                                task.trigger_vk,
                                task.hold_owner_vk,
                                None,
                            ),
                        );
                    }
                    if should_show_start_error(&error.code) {
                        shared.set_playback_start_error(error.message.clone());
                        shared
                            .notifications
                            .publish(&format!("{} · 启动失败", task.rule.name), &error.message);
                    }
                    shared.record_safety("macro_trigger_failed", &[("error", error.message)]);
                } else {
                    if let Some(epoch) = task.hold_epoch {
                        let run_token = task
                            .hold_owner_vk
                            .and_then(|vk| shared.load_hold_atomic_snapshot(vk))
                            .filter(|snapshot| snapshot.epoch == epoch)
                            .and_then(|snapshot| snapshot.bound_token);
                        shared.record_safety_async(
                            "macro_hold_started",
                            hold_diagnostic_fields(
                                Some(&task.rule.id),
                                "started",
                                Some(epoch),
                                run_token,
                                task.trigger_vk,
                                task.hold_owner_vk,
                                None,
                            ),
                        );
                    }
                    hold_reset.transfer_to_playback();
                }
            },
        )
        .map_err(|error| {
            AppError::with_detail(
                "trigger_worker_start_failed",
                "宏触发工作线程启动失败",
                error.to_string(),
            )
        })?;
        self.trigger_tasks
            .set(worker)
            .map_err(|_| AppError::internal("宏触发工作队列重复初始化"))
    }

    #[cfg(windows)]
    fn submit_macro_trigger(
        &self,
        rule: &MacroRule,
        generation: u64,
        config_revision: u64,
        trigger_vk: Option<u32>,
    ) -> bool {
        let admission_revision = self.controller.background_generation();
        let controller_generation = self.controller.generation();
        let emergency_vk = self.emergency_vk.load(Ordering::Acquire);
        let start_timing = trigger_start_timing(rule);
        let hold_parts = matches!(start_timing, TriggerStartTiming::HoldModifierRelease)
            .then(|| crate::config::hold_trigger_parts(&rule.trigger_keys))
            .flatten();
        if matches!(start_timing, TriggerStartTiming::HoldModifierRelease) && hold_parts.is_none() {
            self.record_safety_async(
                "macro_hold_trigger_invalid",
                vec![("macro_id".into(), rule.id.clone())],
            );
            return false;
        }
        // At most one admitted startup, including the task currently handled.
        // This is not a deferred playback queue: busy starts are never stored.
        if self.shutdown.load(Ordering::Acquire)
            || self.emergency_generation.load(Ordering::Acquire) != generation
            || controller_generation != generation
            || !self.trigger_config_revision_is_current(config_revision)
            || self.physical_ledger_uncertain.load(Ordering::Acquire)
            || rule
                .trigger_keys
                .iter()
                .filter_map(|key| key_to_vk(key))
                .any(|vk| vk == emergency_vk)
            || self
                .trigger_pending
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return false;
        }
        let hold_epoch = if matches!(start_timing, TriggerStartTiming::HoldModifierRelease) {
            let Some(parts) = hold_parts.as_ref() else {
                self.trigger_pending.store(false, Ordering::Release);
                return false;
            };
            match self.publish_hold_lifecycle(rule, parts.owner_vk) {
                Some(epoch) => Some(epoch),
                None => {
                    self.trigger_pending.store(false, Ordering::Release);
                    self.record_safety_async(
                        "macro_hold_lifecycle_not_admitted",
                        vec![("macro_id".into(), rule.id.clone())],
                    );
                    return false;
                }
            }
        } else {
            None
        };
        let task = MacroTriggerTask {
            rule: rule.clone(),
            start_timing,
            hold_epoch,
            trigger_vk,
            hold_owner_vk: hold_parts.as_ref().map(|parts| parts.owner_vk),
            hold_modifier_vks: hold_parts
                .as_ref()
                .map(|parts| parts.modifier_vks.clone())
                .unwrap_or_default(),
            generation,
            controller_generation,
            admission_revision,
            config_revision,
        };
        if self
            .trigger_tasks
            .get()
            .is_none_or(|worker| worker.try_submit(task).is_err())
        {
            if let Some(epoch) = hold_epoch {
                self.retire_hold_lifecycle(epoch);
            }
            self.trigger_pending.store(false, Ordering::Release);
            self.record_safety_async(
                "macro_trigger_not_admitted",
                vec![("macro_id".into(), rule.id.clone())],
            );
            return false;
        }
        if let Some(epoch) = hold_epoch {
            self.record_safety_async(
                "macro_hold_trigger_admitted",
                hold_diagnostic_fields(
                    Some(&rule.id),
                    "worker_admitted",
                    Some(epoch),
                    None,
                    trigger_vk,
                    hold_parts.as_ref().map(|parts| parts.owner_vk),
                    None,
                ),
            );
        }
        true
    }

    #[cfg(all(windows, test))]
    fn start_playback_at_generation(
        self: &Arc<Self>,
        macro_rule: MacroRule,
        expected_generation: u64,
    ) -> Result<(), AppError> {
        self.start_playback_at_revision(
            macro_rule,
            expected_generation,
            self.controller.background_generation(),
        )
    }

    #[cfg(windows)]
    fn validate_pending_trigger_config(
        &self,
        task_rule: &MacroRule,
        expected_revision: u64,
    ) -> Result<(), AppError> {
        drop(self.lock_pending_trigger_config(task_rule, expected_revision)?);
        Ok(())
    }

    #[cfg(windows)]
    fn lock_pending_trigger_config<'a>(
        &'a self,
        task_rule: &MacroRule,
        expected_revision: u64,
    ) -> Result<std::sync::MutexGuard<'a, AppConfig>, AppError> {
        if !self.trigger_config_revision_is_current(expected_revision) {
            return Err(AppError::invalid(
                "macro_trigger_config_stale",
                "快捷键配置在等待期间已变化，本次启动已取消",
            ));
        }
        let config = self.config.try_lock().map_err(|_| {
            AppError::invalid(
                "macro_trigger_config_stale",
                "快捷键配置当前不可确认，本次启动已取消",
            )
        })?;
        let still_admitted = config.global_enabled
            && config
                .macros
                .iter()
                .find(|rule| rule.id == task_rule.id)
                .is_some_and(|rule| {
                    rule.enabled
                        && trigger_macro_snapshot(rule) == trigger_macro_snapshot(task_rule)
                });
        if !still_admitted || !self.trigger_config_revision_is_current(expected_revision) {
            return Err(AppError::invalid(
                "macro_trigger_config_stale",
                "快捷键配置在等待期间已变化，本次启动已取消",
            ));
        }
        Ok(config)
    }

    #[cfg(windows)]
    fn start_playback_at_revision(
        self: &Arc<Self>,
        macro_rule: MacroRule,
        expected_generation: u64,
        admission_revision: u64,
    ) -> Result<(), AppError> {
        self.start_playback_with_trigger_revision(
            macro_rule,
            expected_generation,
            admission_revision,
            None,
            None,
        )
    }

    #[cfg(windows)]
    fn start_hotkey_playback_at_revision(
        self: &Arc<Self>,
        macro_rule: MacroRule,
        expected_generation: u64,
        admission_revision: u64,
        config_revision: u64,
        expected_hold_epoch: Option<u64>,
    ) -> Result<(), AppError> {
        self.start_playback_with_trigger_revision(
            macro_rule,
            expected_generation,
            admission_revision,
            Some(config_revision),
            expected_hold_epoch,
        )
    }

    #[cfg(windows)]
    fn start_playback_with_trigger_revision(
        self: &Arc<Self>,
        macro_rule: MacroRule,
        expected_generation: u64,
        admission_revision: u64,
        trigger_config_revision: Option<u64>,
        expected_hold_epoch: Option<u64>,
    ) -> Result<(), AppError> {
        // Keep the exact rule admitted from config for every identity check.
        // Clicker normalization is execution-only and must not manufacture a
        // different config identity halfway through hotkey admission.
        let admitted_rule = macro_rule;
        self.validate_hold_lifecycle(&admitted_rule, expected_hold_epoch)?;
        if let Some(revision) = trigger_config_revision {
            self.validate_pending_trigger_config(&admitted_rule, revision)?;
        }
        #[cfg(test)]
        self.run_trigger_admission_test_hook(TriggerAdmissionCheckpoint::AfterInitialValidation);
        let _admission = self.acquire_mode_admission()?;
        let mut thread_registry = self.playback_thread_handle.try_lock().map_err(|_| {
            AppError::invalid(
                "playback_thread_state_unavailable",
                "播放线程退出状态不可用，未启动新实例",
            )
        })?;
        if thread_registry
            .as_ref()
            .is_some_and(|(_, handle)| !crate::bounded_worker::thread_exit_confirmed(handle))
        {
            return Err(AppError::invalid(
                "macro_busy",
                "上一播放线程仍未实际退出，未排队启动",
            ));
        }
        if self
            .recorder
            .try_lock()
            .map_err(|_| {
                AppError::invalid("recording_state_unavailable", "录制状态不可用，未启动播放")
            })?
            .active
        {
            return Err(AppError::invalid(
                "recording_active",
                "宏录制正在进行中，请先停止录制再播放",
            ));
        }
        if self.emergency_generation.load(Ordering::SeqCst) != expected_generation {
            return Err(AppError::invalid(
                "playback_cancelled",
                "宏启动已被 F12 取消",
            ));
        }
        self.validate_hold_lifecycle(&admitted_rule, expected_hold_epoch)?;
        if let Some(revision) = trigger_config_revision {
            self.validate_pending_trigger_config(&admitted_rule, revision)?;
        }
        let mut start_lease = self
            .controller
            .begin_start_at_revision(Some(expected_generation), admission_revision)
            .map_err(|error| match error {
                StartError::ShuttingDown => {
                    AppError::invalid("app_shutting_down", error.to_string())
                }
                StartError::SafetyLocked => {
                    AppError::invalid("input_safety_recovery_required", error.to_string())
                }
                StartError::Cancelled => AppError::invalid("playback_cancelled", error.to_string()),
                StartError::Busy(_) => AppError::invalid("macro_busy", error.to_string()),
                StartError::StatePoisoned => AppError::internal(error.to_string()),
            })?;
        let run_token = start_lease.token();
        self.bind_hold_lifecycle_run(&admitted_rule, expected_hold_epoch, run_token)?;
        let mut bound_hold_reset = PendingHoldLifecycleReset {
            shared: self,
            epoch: expected_hold_epoch,
            transferred_to_playback: false,
        };
        self.validate_hold_lifecycle(&admitted_rule, expected_hold_epoch)?;
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(AppError::invalid(
                "app_shutting_down",
                "AutoFlow 正在关闭，已拒绝启动宏",
            ));
        }
        if !self.emergency_stop_ready() {
            let message = "F12 紧急停止通道尚未就绪，已禁止启动宏；请检查急停键冲突或重新配置";
            self.set_playback_start_error(message);
            return Err(AppError::invalid("emergency_stop_unavailable", message));
        }
        if self.is_behavior_recording() {
            return Err(AppError::invalid(
                "behavior_recording_active",
                "行为训练录制进行中，请先停止录制后再播放宏",
            ));
        }
        if self.input_recovery_required.load(Ordering::SeqCst)
            || self.executor_containment_unknown.load(Ordering::Acquire)
            || self.tracked_input_counts() != (0, 0)
        {
            return Err(AppError::invalid(
                "input_safety_recovery_required",
                "上一次输入清理未确认安全，已禁止重新播放；请先按 F12 再次清理并确认安全诊断",
            ));
        }
        let execution_rule = normalize_playback_rule(admitted_rule.clone());
        let total_steps = execution_rule.macro_steps().map_or(0, |steps| steps.len());
        if total_steps == 0 && matches!(&execution_rule.program, AutomationProgram::Macro { .. }) {
            let error =
                AppError::invalid("macro_empty", "这个宏还没有步骤，录制或添加步骤后才能播放");
            self.set_playback_start_error(error.message.clone());
            return Err(error);
        }
        if let AutomationProgram::Rhai {
            source,
            api_version,
        } = &execution_rule.program
        {
            if *api_version != crate::rhai_runtime::RHAI_API_VERSION {
                let error = AppError::invalid(
                    "rhai_api_version",
                    "Rhai API 版本不受支持，请使用 apiVersion: 1",
                );
                self.set_playback_start_error(error.message.clone());
                return Err(error);
            }
            if let Err(message) = validate_rhai_source(source) {
                let error = AppError::invalid("rhai_compile_error", message);
                self.set_playback_start_error(error.message.clone());
                return Err(error);
            }
        }
        if let Some(revision) = trigger_config_revision {
            self.validate_pending_trigger_config(&admitted_rule, revision)?;
        }
        // Macros always act on the current foreground program. A saved target
        // from older versions is intentionally ignored so the same macro can
        // be used everywhere.
        let mut playback = self
            .playback
            .lock()
            .map_err(|_| AppError::internal("播放状态异常，请重启 AutoFlow"))?;
        if self.emergency_generation.load(Ordering::SeqCst) != expected_generation {
            return Err(AppError::invalid(
                "playback_cancelled",
                "宏启动已被 F12 取消",
            ));
        }
        self.validate_hold_lifecycle(&admitted_rule, expected_hold_epoch)?;
        if playback.running {
            return Err(AppError::invalid("macro_busy", "已有一个宏正在运行"));
        }
        #[cfg(test)]
        self.run_trigger_admission_test_hook(TriggerAdmissionCheckpoint::BeforeActivation);
        // update_config advances trigger_config_revision while holding this
        // same short-lived config lock. Retaining the validated guard through
        // controller activation closes the final check-to-activate race.
        let trigger_config_guard = match trigger_config_revision {
            Some(revision) => Some(self.lock_pending_trigger_config(&admitted_rule, revision)?),
            None => None,
        };
        self.validate_hold_lifecycle(&admitted_rule, expected_hold_epoch)?;
        if !self.controller.activate(run_token) {
            return Err(AppError::invalid(
                "playback_cancelled",
                "宏启动已被停止请求取消",
            ));
        }
        drop(trigger_config_guard);
        #[cfg(test)]
        self.run_trigger_admission_test_hook(TriggerAdmissionCheckpoint::AfterControllerActivation);
        if let Err(error) = self.validate_hold_lifecycle(&admitted_rule, expected_hold_epoch) {
            self.abort_activated_hold_start(expected_hold_epoch, run_token, None);
            return Err(error);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let instance_id = self
            .playback_instance_counter
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        let emergency_generation = expected_generation;
        let input_state = Arc::new(InjectedInputState::with_broker(self.input_broker.clone()));
        #[cfg(test)]
        self.run_trigger_admission_test_hook(TriggerAdmissionCheckpoint::BeforeInputRegistration);
        if let Err(error) = self.validate_hold_lifecycle(&admitted_rule, expected_hold_epoch) {
            self.abort_activated_hold_start(expected_hold_epoch, run_token, None);
            return Err(error);
        }
        if let Err(error) = self.register_playback_input(instance_id, Arc::clone(&input_state)) {
            if let Some(epoch) = expected_hold_epoch {
                self.retire_hold_lifecycle(epoch);
            }
            let _ = self.controller.finish(run_token, false);
            self.input_recovery_required.store(true, Ordering::Release);
            return Err(error);
        }
        #[cfg(test)]
        self.run_trigger_admission_test_hook(TriggerAdmissionCheckpoint::BeforePlaybackPublication);
        if let Err(error) = self.validate_hold_lifecycle(&admitted_rule, expected_hold_epoch) {
            self.unregister_playback_input(instance_id);
            self.abort_activated_hold_start(expected_hold_epoch, run_token, None);
            return Err(error);
        }
        if let Err(error) = self.activate_hold_lifecycle(
            &admitted_rule,
            expected_hold_epoch,
            run_token,
            instance_id,
        ) {
            self.unregister_playback_input(instance_id);
            self.abort_activated_hold_start(expected_hold_epoch, run_token, None);
            return Err(error);
        }
        let restore_window = foreground_window_handle();
        playback.running = true;
        playback.stop = Some(Arc::clone(&stop));
        playback.instance_id = instance_id;
        playback.run_token = Some(run_token);
        playback.emergency_generation = emergency_generation;
        playback.input_state = Some(Arc::clone(&input_state));
        playback.restore_window = restore_window;
        playback.current_step = 0;
        playback.current_step_kind = None;
        playback.total_steps = total_steps;
        playback.last_error = None;
        playback.macro_id = Some(execution_rule.id.clone());
        playback.macro_name = Some(execution_rule.name.clone());
        playback.program_kind = Some(match &execution_rule.program {
            AutomationProgram::Macro { .. } => "macro".to_string(),
            AutomationProgram::Rhai { .. } => "rhai".to_string(),
        });
        playback.action_summary = None;
        playback.phase = "running".to_string();
        playback.cleanup_status = "not_started".to_string();
        playback.started_at = Some(Instant::now());
        playback.terminal_until = None;
        playback.terminal_persistent = false;
        self.record_safety(
            "playback_started",
            &[
                ("macro_id", execution_rule.id.clone()),
                ("macro_name", execution_rule.name.clone()),
                ("instance_id", instance_id.to_string()),
                ("total_steps", total_steps.to_string()),
            ],
        );
        let shared = Arc::clone(self);
        let trigger_keys = execution_rule.trigger_keys.clone();
        let diagnostics_id = execution_rule.id.clone();
        let diagnostics_name = execution_rule.name.clone();
        self.playback_thread_owner
            .store(instance_id, Ordering::Release);
        let thread_lease = PlaybackThreadLease {
            shared: Arc::clone(self),
            instance_id,
        };
        let playback_handle = thread::Builder::new()
            .name("autoflow-macro-playback".to_string())
            .spawn(move || {
                let _thread_lease = thread_lease;
                // Give Windows a moment to finish foreground activation before
                // injecting the first recorded event.
                // Keep the foreground activation grace period short and
                // interruptible.  F12 revokes the controller permit
                // immediately; this avoids making a cancelled start wait a
                // full 120ms before the worker observes it.
                let _ = sleep_interruptible(20.0, &stop);
                let result = play_macro_thread(
                    &shared,
                    &execution_rule,
                    &stop,
                    emergency_generation,
                    instance_id,
                    Arc::clone(&input_state),
                    run_token,
                );
                let (script_stop_message, playback_error) = match result {
                    Ok(message) => (message, None),
                    Err(error) => (None, Some(error)),
                };
                if let Some(error) = &playback_error {
                    log::warn!("宏“{}”运行失败: {error}", execution_rule.name);
                }
                let cleanup_safe = input_state.counts() == (0, 0)
                    && !shared.input_recovery_required.load(Ordering::SeqCst)
                    && !shared.executor_containment_unknown.load(Ordering::Acquire);
                if let Some(epoch) = expected_hold_epoch {
                    // Cleanup has completed and no further input can be sent.
                    // Retire this exact owner before allowing a newer run.
                    shared.retire_hold_lifecycle(epoch);
                }
                let controller_finalized = shared.controller.finish(run_token, cleanup_safe);
                if !controller_finalized {
                    shared.record_safety(
                        "playback_controller_stale_finish",
                        &[("instance_id", instance_id.to_string())],
                    );
                }
                let mut current_step = 0usize;
                let mut current_step_kind = "unknown".to_string();
                let finalized = shared
                    .playback
                    .lock()
                    .map(|mut playback| {
                        current_step = playback.current_step;
                        current_step_kind = playback
                            .current_step_kind
                            .clone()
                            .unwrap_or_else(|| "unknown".to_string());
                        HookShared::finalize_playback_instance(
                            &mut playback,
                            instance_id,
                            playback_error.clone(),
                        )
                    })
                    .unwrap_or(false);
                // A failed release remains owned by the broker so subsequent
                // recovery/shutdown attempts can still see and release it.
                if cleanup_safe {
                    shared.unregister_playback_input(instance_id);
                }
                if finalized {
                    shared.release_macro_trigger_state(&trigger_keys);
                    restore_previous_window_if_own_process(restore_window);
                } else {
                    shared.record_safety(
                        "stale_playback_thread_exit",
                        &[("instance_id", instance_id.to_string())],
                    );
                }
                shared.record_safety(
                    "playback_thread_exit",
                    &[
                        ("macro_id", diagnostics_id),
                        ("macro_name", diagnostics_name),
                        ("instance_id", instance_id.to_string()),
                        ("current_step", current_step.to_string()),
                        ("step_kind", current_step_kind),
                        (
                            "last_input_send",
                            input_state
                                .last_input()
                                .unwrap_or_else(|| "none".to_string()),
                        ),
                        (
                            "last_input_send_ms",
                            input_state
                                .last_input_at_ms()
                                .map_or_else(|| "none".to_string(), |value| value.to_string()),
                        ),
                        (
                            "status",
                            if playback_error.is_some() {
                                "error".to_string()
                            } else {
                                "stopped_or_completed".to_string()
                            },
                        ),
                    ],
                );
                if let Some(error) =
                    playback_error.filter(|error| should_show_playback_error(error))
                {
                    shared
                        .notifications
                        .publish(&format!("{} · 运行失败", execution_rule.name), &error);
                } else if cleanup_safe && controller_finalized {
                    if let Some((title, message)) = script_stop_message {
                        // Presentation happens only after the run has relinquished
                        // its input permit and completed cleanup and finalization.
                        shared.notifications.publish(&title, &message);
                    }
                }
            })
            .map_err(|error| {
                self.unregister_playback_input(instance_id);
                playback.running = false;
                playback.stop = None;
                playback.run_token = None;
                playback.input_state = None;
                playback.restore_window = None;
                playback.phase = "failed".to_string();
                playback.cleanup_status = "safe".to_string();
                playback.last_error = Some(error.to_string());
                if let Some(epoch) = expected_hold_epoch {
                    self.retire_hold_lifecycle(epoch);
                }
                let _ = self.controller.finish(run_token, true);
                AppError::with_detail("playback_start_failed", "宏播放启动失败", error.to_string())
            })?;
        *thread_registry = Some((instance_id, playback_handle));
        start_lease.commit();
        bound_hold_reset.transfer_to_playback();
        Ok(())
    }

    #[cfg(windows)]
    fn stop_playback(&self) {
        let generation = self.controller.request_stop();
        self.emergency_generation
            .store(generation, Ordering::SeqCst);
        self.signal_playback_stop();
    }

    #[cfg(windows)]
    fn stop_requested(&self, stop: &AtomicBool, start_generation: u64) -> bool {
        stop.load(Ordering::SeqCst)
            || self.shutdown.load(Ordering::SeqCst)
            || self.emergency_generation.load(Ordering::SeqCst) != start_generation
            || self.controller.generation() != start_generation
    }

    #[cfg(windows)]
    fn release_macro_trigger_state(&self, trigger_keys: &[String]) {
        let Some(trigger_vks) = macro_trigger_vks(trigger_keys) else {
            return;
        };
        let Ok(mut pressed) = self.pressed.try_lock() else {
            self.trigger_rearm_required.store(true, Ordering::Release);
            return;
        };
        let Ok(mut latched) = self.latched_hotkeys.try_lock() else {
            self.trigger_rearm_required.store(true, Ordering::Release);
            return;
        };
        clear_released_macro_trigger_state(&mut pressed, &mut latched, &trigger_vks, |vk| unsafe {
            use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
            GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0
        });
    }

    #[cfg(windows)]
    fn behavior_runtime_v2(
        &self,
        macro_rule: &MacroRule,
    ) -> Result<Option<Arc<Mutex<BehaviorRuntimeV2>>>, String> {
        let config = self
            .config
            .lock()
            .map_err(|_| "行为配置状态异常".to_string())?;
        let mut policy = macro_rule
            .behavior_policy
            .clone()
            .unwrap_or_else(|| config.behavior_policy.clone());
        if macro_rule.behavior_policy.is_none() && !policy.enabled && config.biomimetic_enabled {
            policy = BehaviorPolicy::from_legacy(
                true,
                config.biomimetic_intensity,
                config.active_behavior_profile_v2_id.clone(),
            );
        }
        if !policy.enabled {
            return Ok(None);
        }
        let profile_id = policy
            .profile_id
            .clone()
            .or_else(|| config.active_behavior_profile_v2_id.clone())
            .ok_or_else(|| "行为策略未绑定 V2 行为档案".to_string())?;
        let profile = config
            .behavior_profiles_v2
            .iter()
            .find(|profile| profile.id == profile_id)
            .cloned()
            .ok_or_else(|| "行为策略绑定的 V2 行为档案不存在".to_string())?;
        BehaviorRuntimeV2::new(profile, policy)
            .map(|runtime| Some(Arc::new(Mutex::new(runtime))))
            .map_err(|error| error.message)
    }
}

#[cfg(windows)]
fn trigger_release_gate_status(
    shared: &HookShared,
    task: &MacroTriggerTask,
) -> TriggerReleaseGateStatus {
    if shared.shutdown.load(Ordering::Acquire) || shared.controller.is_shutting_down() {
        TriggerReleaseGateStatus::Shutdown
    } else if shared.emergency_generation.load(Ordering::Acquire) != task.generation {
        TriggerReleaseGateStatus::Cancelled(TriggerReleaseCancellation::TaskGeneration)
    } else if shared.controller.generation() != task.controller_generation {
        // request_emergency_stop changes the controller generation before it
        // publishes emergency_generation. This check closes that race.
        TriggerReleaseGateStatus::Cancelled(TriggerReleaseCancellation::ControllerGeneration)
    } else if shared.physical_ledger_uncertain.load(Ordering::Acquire) {
        TriggerReleaseGateStatus::Cancelled(TriggerReleaseCancellation::PhysicalLedgerUncertain)
    } else if shared.controller.background_generation() != task.admission_revision {
        TriggerReleaseGateStatus::Cancelled(TriggerReleaseCancellation::AdmissionRevision)
    } else if !shared.trigger_config_revision_is_current(task.config_revision) {
        TriggerReleaseGateStatus::Cancelled(TriggerReleaseCancellation::ConfigRevision)
    } else if task.hold_epoch.is_some_and(|epoch| {
        shared.hold_lifecycle_epoch.load(Ordering::Acquire) != epoch
            || shared.hold_cancelled_epoch.load(Ordering::Acquire) >= epoch
    }) {
        TriggerReleaseGateStatus::Cancelled(TriggerReleaseCancellation::HoldLifecycle)
    } else {
        TriggerReleaseGateStatus::Current
    }
}

#[cfg(windows)]
fn trigger_vk_is_physically_down(vk: u32) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

    let down = |key| unsafe { GetAsyncKeyState(key as i32) as u16 & 0x8000 != 0 };
    if vk == 0x5B {
        // The hook canonicalizes both Windows keys to VK_LWIN. Query both
        // physical variants before admitting a shortcut-triggered start.
        down(0x5B) || down(0x5C)
    } else {
        down(vk)
    }
}

#[cfg(windows)]
fn trigger_keys_are_physically_down(
    shared: &HookShared,
    trigger_vks: &[u32],
) -> Result<bool, TriggerReleaseCancellation> {
    if shared.physical_ledger_uncertain.load(Ordering::Acquire) {
        return Err(TriggerReleaseCancellation::PhysicalLedgerUncertain);
    }
    let ledger = shared
        .physical_pressed
        .try_lock()
        .map(|ledger| ledger.clone())
        .map_err(|_| {
            shared
                .physical_ledger_uncertain
                .store(true, Ordering::Release);
            TriggerReleaseCancellation::PhysicalLedgerUncertain
        })?;
    if shared.physical_ledger_uncertain.load(Ordering::Acquire) {
        return Err(TriggerReleaseCancellation::PhysicalLedgerUncertain);
    }
    Ok(trigger_vks
        .iter()
        .copied()
        .any(|vk| trigger_down_from_ledger_or_async(&ledger, vk, trigger_vk_is_physically_down)))
}

#[cfg(windows)]
fn hold_keys_physical_state(
    shared: &HookShared,
    owner_vk: u32,
    modifier_vks: &[u32],
) -> Result<(bool, bool), TriggerReleaseCancellation> {
    if shared.physical_ledger_uncertain.load(Ordering::Acquire) {
        return Err(TriggerReleaseCancellation::PhysicalLedgerUncertain);
    }
    let ledger = shared
        .physical_pressed
        .try_lock()
        .map(|ledger| ledger.clone())
        .map_err(|_| {
            shared
                .physical_ledger_uncertain
                .store(true, Ordering::Release);
            TriggerReleaseCancellation::PhysicalLedgerUncertain
        })?;
    if shared.physical_ledger_uncertain.load(Ordering::Acquire) {
        return Err(TriggerReleaseCancellation::PhysicalLedgerUncertain);
    }
    let owner_down =
        trigger_down_from_ledger_or_async(&ledger, owner_vk, trigger_vk_is_physically_down);
    let any_modifier_down = modifier_vks
        .iter()
        .copied()
        .any(|vk| trigger_down_from_ledger_or_async(&ledger, vk, trigger_vk_is_physically_down));
    Ok((owner_down, any_modifier_down))
}

#[cfg(windows)]
fn wait_for_trigger_release(
    shared: &HookShared,
    task: &MacroTriggerTask,
) -> TriggerReleaseGateOutcome {
    let Some(trigger_vks) = macro_trigger_vks(&task.rule.trigger_keys) else {
        return TriggerReleaseGateOutcome::Cancelled(
            TriggerReleaseCancellation::TriggerConfiguration,
        );
    };
    let started = Instant::now();
    drive_trigger_release_gate(
        TriggerReleaseGate::new(TRIGGER_RELEASE_STABLE, TRIGGER_RELEASE_TIMEOUT),
        || {
            let mut status = trigger_release_gate_status(shared, task);
            let any_trigger_key_down = if matches!(status, TriggerReleaseGateStatus::Current) {
                match trigger_keys_are_physically_down(shared, &trigger_vks) {
                    Ok(down) => down,
                    Err(reason) => {
                        status = TriggerReleaseGateStatus::Cancelled(reason);
                        false
                    }
                }
            } else {
                false
            };
            TriggerReleaseSample {
                elapsed: started.elapsed(),
                any_trigger_key_down,
                status,
            }
        },
        thread::sleep,
    )
}

#[cfg(windows)]
fn wait_for_hold_modifier_release(
    shared: &HookShared,
    task: &MacroTriggerTask,
) -> TriggerReleaseGateOutcome {
    let Some(owner_vk) = task.hold_owner_vk else {
        return TriggerReleaseGateOutcome::Cancelled(
            TriggerReleaseCancellation::TriggerConfiguration,
        );
    };
    let started = Instant::now();
    drive_hold_modifier_release_gate(
        !task.hold_modifier_vks.is_empty(),
        TriggerReleaseGate::new(TRIGGER_RELEASE_STABLE, TRIGGER_RELEASE_TIMEOUT),
        || {
            let mut status = trigger_release_gate_status(shared, task);
            let (owner_down, any_modifier_down) =
                if matches!(status, TriggerReleaseGateStatus::Current) {
                    match hold_keys_physical_state(shared, owner_vk, &task.hold_modifier_vks) {
                        Ok(state) => state,
                        Err(reason) => {
                            status = TriggerReleaseGateStatus::Cancelled(reason);
                            (false, false)
                        }
                    }
                } else {
                    (false, false)
                };
            HoldModifierReleaseSample {
                elapsed: started.elapsed(),
                owner_down,
                any_modifier_down,
                status,
            }
        },
        thread::sleep,
    )
}

#[cfg(windows)]
fn normalize_playback_rule(mut macro_rule: MacroRule) -> MacroRule {
    // The basic clicker is intentionally not recorded as arbitrary events.
    // It always clicks at the current cursor position, in whichever program is
    // active, so a bad/partial recording can never turn it into a no-op.
    if macro_rule.name.trim() == "连点器" {
        macro_rule.mode = MacroMode::Toggle;
        macro_rule.program = AutomationProgram::Macro {
            steps: vec![
                MacroStep::MouseButton {
                    button: MouseButton::Left,
                    action: KeyAction::Down,
                    x: 0,
                    y: 0,
                },
                MacroStep::Delay {
                    duration_ms: 25,
                    duration_max_ms: None,
                },
                MacroStep::MouseButton {
                    button: MouseButton::Left,
                    action: KeyAction::Up,
                    x: 0,
                    y: 0,
                },
                MacroStep::Delay {
                    duration_ms: 75,
                    duration_max_ms: None,
                },
            ],
        };
    }
    macro_rule
}

#[cfg(windows)]
struct RecorderState {
    active: bool,
    capture_started: bool,
    capture_mouse_move: bool,
    capture_mouse_clicks: bool,
    started_at: Option<Instant>,
    last_event: Option<Instant>,
    last_mouse_move: Option<(Instant, i32, i32)>,
    steps: Vec<MacroStep>,
    completed_steps: Option<Vec<MacroStep>>,
    pressed_keys: HashSet<u32>,
    pressed_buttons: HashSet<MouseButton>,
}

#[cfg(windows)]
impl Default for RecorderState {
    fn default() -> Self {
        Self {
            active: false,
            capture_started: false,
            capture_mouse_move: true,
            capture_mouse_clicks: true,
            started_at: None,
            last_event: None,
            last_mouse_move: None,
            steps: Vec::new(),
            completed_steps: None,
            pressed_keys: HashSet::new(),
            pressed_buttons: HashSet::new(),
        }
    }
}

#[cfg(windows)]
#[derive(Default)]
struct PlaybackState {
    running: bool,
    stop: Option<Arc<AtomicBool>>,
    instance_id: u64,
    run_token: Option<RunToken>,
    emergency_generation: u64,
    input_state: Option<Arc<InjectedInputState>>,
    restore_window: Option<isize>,
    current_step: usize,
    current_step_kind: Option<String>,
    total_steps: usize,
    last_error: Option<String>,
    macro_id: Option<String>,
    macro_name: Option<String>,
    program_kind: Option<String>,
    action_summary: Option<String>,
    phase: String,
    cleanup_status: String,
    started_at: Option<Instant>,
    terminal_until: Option<Instant>,
    terminal_persistent: bool,
}

#[cfg(windows)]
const EMERGENCY_STOP_MESSAGE: u32 = 0x8042;

#[cfg(windows)]
const REFRESH_EMERGENCY_HOTKEY_MESSAGE: u32 = 0x8043;

#[cfg(windows)]
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
enum EmergencyEntryPoint {
    PhysicalDetector = 1,
    LowLevelHook = 2,
    Command = 3,
}

#[cfg(windows)]
fn emergency_entry_point_name(value: u32) -> &'static str {
    match value {
        1 => "physical_key_detector",
        2 => "low_level_hook",
        3 => "command",
        _ => "unknown",
    }
}

#[cfg(windows)]
fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(windows)]
fn detector_heartbeat_fresh(now: u64, heartbeat: u64) -> bool {
    heartbeat != 0 && heartbeat <= now && now - heartbeat <= 100
}

#[cfg(windows)]
fn emergency_cleanup_worker(shared: Arc<HookShared>, sequence: u64) {
    let reason = shared.emergency_reason.load(Ordering::SeqCst);
    let requested_at = shared.emergency_requested_at_ms.load(Ordering::SeqCst);
    shared.record_safety(
        "emergency_stop_requested",
        &[
            (
                "entry_point",
                emergency_entry_point_name(reason).to_string(),
            ),
            ("request_sequence", sequence.to_string()),
            ("requested_at_ms", requested_at.to_string()),
        ],
    );

    let mut input_states = vec![
        Arc::clone(&shared.injected_input),
        Arc::clone(&shared.text_input),
    ];
    let registry_known = match shared.playback_input_states() {
        Ok(states) => {
            input_states.extend(states);
            true
        }
        Err(()) => {
            shared.controller.lock_fault();
            shared
                .input_recovery_required
                .store(true, Ordering::Release);
            false
        }
    };
    let (keys, buttons) = shared.tracked_input_counts();
    shared.record_safety(
        "emergency_cleanup_started",
        &[
            ("tracked_input", format!("keys={keys},buttons={buttons}")),
            (
                "last_input_send",
                shared
                    .injected_input
                    .last_input()
                    .unwrap_or_else(|| "none".to_string()),
            ),
            (
                "last_input_send_ms",
                shared
                    .injected_input
                    .last_input_at_ms()
                    .map_or_else(|| "none".to_string(), |value| value.to_string()),
            ),
        ],
    );

    let mut input_report = CleanupReport::default();
    for (index, input_state) in input_states.into_iter().enumerate() {
        let report = input_state.cleanup(
            |vk| send_key(vk, false),
            |button| send_mouse_button(button, KeyAction::Up),
        );
        input_report.released_keys = input_report
            .released_keys
            .saturating_add(report.released_keys);
        input_report.released_buttons = input_report
            .released_buttons
            .saturating_add(report.released_buttons);
        input_report.failures.extend(report.failures.clone());
        record_cleanup_report(&shared, &report, &format!("emergency_input:{index}"));
    }
    // Cleanup is not recovery authorization. A latched fault stays locked
    // until an explicit recovery request validates channels and all ledgers.

    let mut bookkeeping_known = cleanup_active_remaps(&shared);
    if shared.drain_graph_capture().is_ok() {
        if let Ok(mut recorder) = shared.recorder.try_lock() {
            if recorder.active {
                finish_recorder(&mut recorder);
            }
        } else {
            bookkeeping_known = false;
        }
    } else {
        bookkeeping_known = false;
    }
    let behavior_drained = shared.behavior_capture.get().is_none_or(|queue| {
        if queue.is_pending() {
            queue.drain(Duration::from_millis(500)).is_ok() && !queue.is_failed()
        } else {
            !queue.is_failed()
        }
    });
    if let Ok(mut behavior) = shared.behavior.try_lock() {
        behavior.freeze_capture(behavior_drained);
    } else {
        bookkeeping_known = false;
    }
    if !behavior_drained {
        // Do not destroy the sole recorder copy while old queue work is still
        // unconfirmed or has reported incompleteness.
        bookkeeping_known = false;
    }
    if let Ok(mut latched) = shared.latched_hotkeys.try_lock() {
        latched.clear();
    } else {
        bookkeeping_known = false;
    }
    if let Ok(mut buffer) = shared.text_buffer.try_lock() {
        buffer.clear();
    } else {
        bookkeeping_known = false;
    }
    if let Ok(mut pressed) = shared.pressed.try_lock() {
        // Preserve keys the user is physically holding.  Only stale internal
        // bookkeeping is removed; this function never releases all system
        // keys blindly.
        pressed.retain(|vk| unsafe {
            use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
            GetAsyncKeyState(*vk as i32) as u16 & 0x8000 != 0
        });
    } else {
        bookkeeping_known = false;
    }
    if !registry_known || !bookkeeping_known {
        shared.controller.lock_fault();
        shared
            .input_recovery_required
            .store(true, Ordering::Release);
    }

    shared.record_safety(
        "emergency_cleanup_finished",
        &[(
            "status",
            if registry_known
                && bookkeeping_known
                && input_report.is_safe()
                && !shared.executor_containment_unknown.load(Ordering::Acquire)
            {
                "safe".to_string()
            } else {
                "unsafe_manual_recovery_required".to_string()
            },
        )],
    );
    // Completion publication belongs to the retained worker wrapper. Never
    // recursively spawn a successor from a thread which has not yet exited.
}

#[cfg(windows)]
fn cleanup_active_remaps(shared: &HookShared) -> bool {
    let Ok(pending) = shared.active_remaps.try_lock().map(|remaps| {
        remaps
            .iter()
            .map(|(source, target)| (*source, *target))
            .collect::<Vec<_>>()
    }) else {
        return false;
    };
    let mut known = true;
    for (source, target) in pending {
        if !shared.injected_input.has_key(target) {
            if let Ok(mut remaps) = shared.active_remaps.try_lock() {
                if remaps.get(&source) == Some(&target) {
                    remaps.remove(&source);
                }
            } else {
                known = false;
            }
        } else {
            known = false;
            shared.record_safety(
                "input_cleanup_failed",
                &[
                    ("kind", "remap_key".to_string()),
                    ("source_vk", format!("{source:#x}")),
                    ("target_vk", format!("{target:#x}")),
                    (
                        "error",
                        "tracked key remains after bounded cleanup".to_string(),
                    ),
                    ("status", "unsafe_manual_recovery_required".to_string()),
                ],
            );
        }
    }
    known
}

#[cfg(windows)]
fn record_cleanup_report(shared: &HookShared, report: &CleanupReport, scope: &str) {
    if report.is_safe() {
        if shared.tracked_input_counts() == (0, 0) {
            shared
                .input_recovery_required
                .store(false, Ordering::SeqCst);
        }
        shared.record_safety(
            "input_cleanup_completed",
            &[
                ("scope", scope.to_string()),
                ("released_keys", report.released_keys.to_string()),
                ("released_buttons", report.released_buttons.to_string()),
            ],
        );
    } else {
        // A later successful release may clear the remaining-input indicator,
        // but it is not authorization to resume. Only explicit recovery may
        // clear this lifecycle fault.
        shared.controller.lock_fault();
        shared.input_recovery_required.store(true, Ordering::SeqCst);
        for failure in &report.failures {
            shared.record_safety(
                "input_cleanup_failed",
                &[
                    ("scope", scope.to_string()),
                    ("kind", failure.kind.to_string()),
                    ("value", failure.value.clone()),
                    ("attempts", failure.attempts.to_string()),
                    ("error", failure.last_error.clone()),
                    ("status", "unsafe_manual_recovery_required".to_string()),
                ],
            );
        }
    }
}

#[cfg(windows)]
fn refresh_emergency_detector(shared: &HookShared) {
    // Never pause physical F12 detection while another thread owns config.
    // Keep the last known key; a later refresh can apply the new configuration.
    let Ok(config) = shared.config.try_lock() else {
        return;
    };
    let key = key_to_vk(&config.emergency_stop);
    drop(config);
    let configured = key.is_some_and(|key| key != 0);
    shared
        .emergency_vk
        .store(key.unwrap_or(0), Ordering::Release);
    // A configuration refresh cannot manufacture readiness or erase the
    // last healthy->failed transition. Only the monitor publishes readiness
    // after checking desktop, hook acknowledgment and key configuration.
    shared.emergency_key_down.store(false, Ordering::Release);
    shared.record_safety(
        "emergency_key_detector_refresh",
        &[
            ("configured", configured.to_string()),
            (
                "key",
                key.map_or_else(|| "none".to_string(), |key| format!("{key:#x}")),
            ),
            ("method", "async_key_state_edge".to_string()),
        ],
    );
    if !configured {
        log::error!("紧急停止键配置无效，已禁止启动宏");
    }
}

#[cfg(windows)]
fn emergency_stop_thread(shared: Arc<HookShared>) {
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    use windows::Win32::UI::WindowsAndMessaging::{
        PeekMessageW, MSG, PM_REMOVE, WM_HOTKEY, WM_QUIT,
    };

    shared
        .emergency_thread_id
        .store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
    shared.record_safety(
        "emergency_thread_started",
        &[("status", "healthy".to_string())],
    );
    refresh_emergency_detector(&shared);
    shared.record_safety(
        "emergency_thread_monitor_started",
        &[("status", "awaiting_channel_probe".to_string())],
    );
    if shared.emergency_request_sequence.load(Ordering::SeqCst) != 0 {
        shared.spawn_emergency_cleanup();
    }
    if shared
        .emergency_notification_failed
        .swap(false, Ordering::SeqCst)
    {
        shared.record_safety(
            "emergency_cleanup_notification_failed",
            &[("status", "unsafe_manual_recovery_required".to_string())],
        );
    }
    let mut message = MSG::default();
    let mut last_native_refresh_retry = Instant::now();
    let mut last_hook_probe = Instant::now() - Duration::from_millis(20);
    loop {
        if shared.emergency_shutdown.load(Ordering::SeqCst) {
            break;
        }
        shared.spawn_emergency_cleanup();
        if last_hook_probe.elapsed() >= Duration::from_millis(20) {
            shared.post_hook_probe();
            last_hook_probe = Instant::now();
        }
        if last_native_refresh_retry.elapsed() >= Duration::from_millis(50) {
            shared.post_native_refresh_if_needed();
            last_native_refresh_retry = Instant::now();
        }
        shared.emergency_heartbeat_ms.store(
            shared.emergency_clock.elapsed().as_millis() as u64 + 1,
            Ordering::Release,
        );
        let key = shared.emergency_vk.load(Ordering::Acquire);
        let desktop_available = crate::desktop_safety::current_desktop_accepts_input();
        shared.observe_control_channel_health(desktop_available);
        let physically_down =
            key != 0 && unsafe { GetAsyncKeyState(key as i32) as u16 & 0x8000 != 0 };
        if physically_down {
            if !shared.emergency_key_down.swap(true, Ordering::AcqRel) {
                shared.request_emergency_stop(EmergencyEntryPoint::PhysicalDetector, true);
            }
        } else {
            shared.emergency_key_down.store(false, Ordering::Release);
        }
        if let Ok(config) = shared.config.try_lock().map(|config| config.clone()) {
            rearm_macro_triggers_if_released(&shared, &config);
        }

        let mut quit_requested = false;
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() } {
            if message.message == WM_QUIT {
                quit_requested = true;
                break;
            }
            if message.message == REFRESH_EMERGENCY_HOTKEY_MESSAGE {
                refresh_emergency_detector(&shared);
            } else if message.message == EMERGENCY_STOP_MESSAGE {
                shared.spawn_emergency_cleanup();
            } else if message.message == WM_HOTKEY {
                // F12 is intentionally not registered with RegisterHotKey:
                // Windows reserves it for debuggers.  Ignore stale messages
                // from older instances rather than treating them as a second
                // emergency source.
            }
        }
        if quit_requested {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    shared
        .emergency_detector_ready
        .store(false, Ordering::Release);
    shared.emergency_key_down.store(false, Ordering::Release);
    shared.emergency_thread_id.store(0, Ordering::SeqCst);
    shared.record_safety(
        "emergency_thread_exit",
        &[("status", "stopped".to_string())],
    );
}

#[cfg(windows)]
fn hook_thread(shared: Arc<HookShared>) {
    use windows::Win32::Foundation::HINSTANCE;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_CONTROL, MOD_NOREPEAT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
        MSG, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_HOTKEY,
    };

    shared
        .thread_id
        .store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
    shared.record_safety("hook_thread_started", &[("status", "starting".to_string())]);
    if !shared.initialize_physical_key_ledger() {
        shared.record_safety(
            "hook_thread_start_failed",
            &[("kind", "physical_key_ledger".to_string())],
        );
        return;
    }
    let hook = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(keyboard_hook),
            Some(HINSTANCE::default()),
            0,
        )
    };
    let hook = match hook {
        Ok(hook) => hook,
        Err(error) => {
            shared.retire_physical_key_ledger();
            log::error!("全局键盘监听启动失败: {error}");
            shared.record_safety(
                "hook_thread_start_failed",
                &[
                    ("kind", "keyboard_hook".to_string()),
                    ("error", error.to_string()),
                ],
            );
            return;
        }
    };
    let mouse_hook =
        unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), Some(HINSTANCE::default()), 0) };
    let mouse_hook = match mouse_hook {
        Ok(hook) => hook,
        Err(error) => {
            shared.retire_physical_key_ledger();
            log::error!("全局鼠标监听启动失败: {error}");
            shared.record_safety(
                "hook_thread_start_failed",
                &[
                    ("kind", "mouse_hook".to_string()),
                    ("error", error.to_string()),
                ],
            );
            shared.confirm_hook_removal(
                "keyboard_startup_rollback",
                unsafe { UnhookWindowsHookEx(hook) }.map_err(|error| error.to_string()),
            );
            return;
        }
    };

    // Use Windows' native hotkey delivery for the clicker. Low-level keyboard
    // hooks are still needed for F12, recording and general macros, but they
    // are not a dependable foundation for a Ctrl+function-key toggle.
    let native_clicker_allowed = shared
        .config
        .try_lock()
        .is_ok_and(|config| native_clicker_registration_allowed(&config));
    let native_clicker_hotkey = native_clicker_allowed
        && unsafe {
            RegisterHotKey(
                None,
                CLICKER_HOTKEY_ID,
                HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_NOREPEAT.0),
                0x77,
            )
        }
        .is_ok();
    shared
        .native_clicker_hotkey_registered
        .store(native_clicker_hotkey, Ordering::SeqCst);
    if native_clicker_allowed && !native_clicker_hotkey {
        log::warn!("Ctrl+F8 原生热键注册失败，将使用兼容监听方式");
    }
    refresh_native_macro_hotkeys(&shared);
    shared.hook_ready.store(true, Ordering::Release);
    shared.record_safety("hook_thread_ready", &[("status", "healthy".to_string())]);

    let mut message = MSG::default();
    loop {
        if shared.shutdown.load(Ordering::SeqCst) {
            break;
        }
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if result.0 <= 0 {
            break;
        }
        if message.message == HOOK_HEALTH_PROBE_MESSAGE {
            shared.acknowledge_hook_probe();
            continue;
        }
        if message.message == WM_HOTKEY && message.wParam.0 as i32 == CLICKER_HOTKEY_ID {
            process_native_clicker_hotkey(&shared);
            continue;
        }
        if message.message == REFRESH_NATIVE_HOTKEYS_MESSAGE {
            shared
                .native_refresh_message_pending
                .store(false, Ordering::Release);
            refresh_native_macro_hotkeys(&shared);
            continue;
        }
        if message.message == WM_HOTKEY {
            process_native_macro_hotkey(&shared, message.wParam.0 as i32);
            continue;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    shared.hook_ready.store(false, Ordering::Release);
    shared.retire_physical_key_ledger();
    let keyboard_removed = shared.confirm_hook_removal(
        "keyboard",
        unsafe { UnhookWindowsHookEx(hook) }.map_err(|error| error.to_string()),
    );
    let mouse_removed = shared.confirm_hook_removal(
        "mouse",
        unsafe { UnhookWindowsHookEx(mouse_hook) }.map_err(|error| error.to_string()),
    );
    let clicker_retired =
        !native_clicker_hotkey || unsafe { UnregisterHotKey(None, CLICKER_HOTKEY_ID).is_ok() };
    let macros_retired = unregister_native_macro_hotkeys(&shared);
    if !clicker_retired || !macros_retired {
        shared.native_teardown_failed.store(true, Ordering::Release);
        shared.controller.lock_fault();
        shared.record_safety(
            "native_hotkey_teardown_unconfirmed",
            &[
                ("clicker_retired", clicker_retired.to_string()),
                ("macros_retired", macros_retired.to_string()),
            ],
        );
    }
    if clicker_retired {
        shared
            .native_clicker_hotkey_registered
            .store(false, Ordering::SeqCst);
    }
    shared.record_safety(
        "hook_thread_exit",
        &[(
            "status",
            if clicker_retired && macros_retired && keyboard_removed && mouse_removed {
                "thread_body_returned"
            } else {
                "thread_body_returned_teardown_unconfirmed"
            }
            .to_string(),
        )],
    );
}

#[cfg(windows)]
unsafe extern "system" fn keyboard_hook(
    code: i32,
    message: windows::Win32::Foundation::WPARAM,
    data: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, KBDLLHOOKSTRUCT, KBDLLHOOKSTRUCT_FLAGS, WM_KEYDOWN, WM_KEYUP,
        WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    if code < 0 || data.0 == 0 {
        return CallNextHookEx(None, code, message, data);
    }
    let info = *(data.0 as *const KBDLLHOOKSTRUCT);
    let captured_at = Instant::now();
    if info.flags & KBDLLHOOKSTRUCT_FLAGS(0x00000010) != KBDLLHOOKSTRUCT_FLAGS(0) {
        return CallNextHookEx(None, code, message, data);
    }

    let is_down = matches!(message.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
    let is_up = matches!(message.0 as u32, WM_KEYUP | WM_SYSKEYUP);
    if !is_down && !is_up {
        return CallNextHookEx(None, code, message, data);
    }

    let Some(shared) = HOOK_SHARED.get() else {
        return CallNextHookEx(None, code, message, data);
    };
    if !shared.track_physical_key_event(info.vkCode, is_down, is_up) {
        return CallNextHookEx(None, code, message, data);
    }
    // Low-level hooks report left/right modifiers as distinct keys. Rules are
    // configured with the user-facing Ctrl / Alt / Shift names, so normalize
    // before tracking or matching a combination.
    let vk = canonical_virtual_key(info.vkCode);

    // Emergency handling must precede every normal hook lock.  If the native
    // hotkey thread is unavailable, this fallback still only performs an
    // atomic request and returns immediately.
    if shared.emergency_vk.load(Ordering::SeqCst) == vk {
        if is_down {
            if !shared.emergency_key_down.swap(true, Ordering::AcqRel) {
                shared.request_emergency_stop(EmergencyEntryPoint::LowLevelHook, false);
            }
        } else if is_up {
            shared.emergency_key_down.store(false, Ordering::Release);
        }
        return LRESULT(1);
    }

    // Once a Hold chord has been admitted, its ordinary owner key belongs to
    // that lifecycle until owner key-up or retirement. Modifier release makes
    // the full chord stop matching, so suppress physical auto-repeat here,
    // before configuration routing, using only the published atomic identity.
    if is_down && shared.owns_current_hold_owner_keydown(vk) {
        return LRESULT(1);
    }

    // Hold release is a lifecycle revocation, not configuration dispatch.
    // Resolve it before config/playback/pressed locks so disable/delete/rekey
    // and callback lock contention cannot delay or lose the stop.
    if is_up {
        shared.revoke_current_hold_for_key_up(vk);
    }

    let (config, trigger_config_revision) = match shared.config.try_lock() {
        Ok(config) => (
            config.clone(),
            shared.trigger_config_revision.load(Ordering::Acquire),
        ),
        Err(_) => {
            shared.reject_hook_event("configuration_unavailable", is_up);
            return CallNextHookEx(None, code, message, data);
        }
    };
    if is_up {
        rearm_macro_triggers_if_released(shared, &config);
    }
    let mut pressed = match shared.pressed.try_lock() {
        Ok(pressed) => pressed,
        Err(_) => {
            shared.reject_hook_event("pressed_state_unavailable", is_up);
            return CallNextHookEx(None, code, message, data);
        }
    };

    let was_pressed = pressed.contains(&vk);
    if is_down {
        pressed.insert(vk);
    } else {
        pressed.remove(&vk);
        if let Ok(mut latched) = shared.latched_hotkeys.try_lock() {
            latched.retain(|signature| !latched_signature_contains_vk(signature, vk));
        }
    }

    // Ctrl+Shift+F9 is reserved for starting/stopping recording. Handle it
    // before the recorder and consume it so no part of the shortcut reaches
    // the macro or the foreground application.
    if is_down && !was_pressed && is_recording_shortcut_key(vk, &pressed) {
        if shared.is_recording() {
            shared.finish_recording_from_hotkey();
        } else if let Err(error) = shared.start_recording_from_hotkey() {
            log::warn!("录制快捷键启动录制失败: {}", error.message);
        }
        return LRESULT(1);
    }

    if behavior_input_is_allowed(shared) {
        shared.record_behavior_key(vk, info.scanCode, is_down, captured_at);
    }

    if shared.is_recording() && is_focus_switch_key(vk, &pressed) {
        discard_focus_switch_steps(shared);
        return CallNextHookEx(None, code, message, data);
    }

    if recording_input_is_allowed(shared) {
        record_keyboard_event(shared, vk, is_down, captured_at);
    }

    // A recording is a transparent capture session: do not let configured
    // hotkeys or text expansions consume the keys that the user is recording.
    if shared.is_recording() {
        return CallNextHookEx(None, code, message, data);
    }

    if is_up {
        if shared.submit_remap_up(vk) {
            return LRESULT(1);
        }
        return CallNextHookEx(None, code, message, data);
    }

    if !config.global_enabled {
        return CallNextHookEx(None, code, message, data);
    }

    if shared.trigger_rearm_required.load(Ordering::Acquire) {
        if should_suppress_for_trigger_rearm(shared, &config, vk) {
            return LRESULT(1);
        }
        return CallNextHookEx(None, code, message, data);
    }

    if process_macro_key_down(
        shared,
        &config,
        trigger_config_revision,
        vk,
        was_pressed,
        &pressed,
    ) {
        return LRESULT(1);
    }

    // Remaps, launch shortcuts, and text expansion are background producers.
    // They must not inject while a macro owns the input broker; otherwise a
    // playback cleanup could release another feature's key or a stale worker
    // could resume after playback ends.
    let background_generation = shared.controller.background_generation();
    if !shared
        .controller
        .background_input_allowed_at(background_generation)
    {
        return CallNextHookEx(None, code, message, data);
    }

    if let Some(rule) = config.hotkeys.iter().find(|rule| {
        rule.enabled
            && rule.action.action_type == "remap"
            && rule.trigger_keys.len() == 1
            && key_to_vk(&rule.trigger_keys[0]) == Some(vk)
    }) {
        if let Some(target_vk) = key_to_vk(&rule.action.target) {
            if was_pressed
                && !shared
                    .active_remaps
                    .try_lock()
                    .is_ok_and(|remaps| remaps.get(&vk) == Some(&target_vk))
            {
                return CallNextHookEx(None, code, message, data);
            }
            if !was_pressed && !shared.submit_remap_down(vk, target_vk, background_generation) {
                return CallNextHookEx(None, code, message, data);
            }
            return LRESULT(1);
        }
    }

    for rule in config
        .hotkeys
        .iter()
        .filter(|rule| rule.enabled && rule.action.action_type == "launch")
    {
        let trigger_vks = rule
            .trigger_keys
            .iter()
            .filter_map(|key| key_to_vk(key))
            .collect::<Vec<_>>();
        if trigger_vks.len() != rule.trigger_keys.len()
            || was_pressed
            || !trigger_vks.contains(&vk)
            || !trigger_vks.iter().all(|key| pressed.contains(key))
        {
            continue;
        }
        let signature = trigger_vks
            .iter()
            .map(|key| key.to_string())
            .collect::<Vec<_>>()
            .join("+");
        let mut latched = match shared.latched_hotkeys.try_lock() {
            Ok(latched) => latched,
            Err(_) => continue,
        };
        if latched.insert(signature.clone()) {
            if shared.submit_launch(&rule.action.target, background_generation) {
                return LRESULT(1);
            }
            latched.remove(&signature);
        }
    }

    let text_modifier_pressed = pressed.iter().any(|key| is_text_modifier(*key));
    if text_modifier_pressed && !is_text_modifier(vk) {
        if let Ok(mut buffer) = shared.text_buffer.lock() {
            buffer.clear();
        }
        return CallNextHookEx(None, code, message, data);
    }

    let Some(character) = key_to_character(vk, info.scanCode, &pressed) else {
        let is_modifier = is_keyboard_modifier(vk);
        if !is_modifier {
            if let Ok(mut buffer) = shared.text_buffer.lock() {
                buffer.clear();
            }
        }
        return CallNextHookEx(None, code, message, data);
    };
    if character.chars().any(char::is_whitespace) {
        if let Ok(mut buffer) = shared.text_buffer.lock() {
            buffer.clear();
        }
        return CallNextHookEx(None, code, message, data);
    }
    let mut buffer = match shared.text_buffer.lock() {
        Ok(buffer) => buffer,
        Err(_) => return CallNextHookEx(None, code, message, data),
    };
    buffer.push_str(&character);
    if buffer.chars().count() > 64 {
        let keep = buffer.chars().rev().take(64).collect::<Vec<_>>();
        *buffer = keep.into_iter().rev().collect();
    }

    if let Some(rule) = config.text_expansions.iter().find(|rule| {
        if !rule.enabled {
            return false;
        }
        let abbreviation = if rule.case_sensitive {
            rule.abbreviation.clone()
        } else {
            rule.abbreviation.to_ascii_lowercase()
        };
        let current = if rule.case_sensitive {
            buffer.clone()
        } else {
            buffer.to_ascii_lowercase()
        };
        current.ends_with(&abbreviation)
    }) {
        let length = rule.abbreviation.chars().count();
        if rule.replacement.len() > 16 * 1024 {
            shared.notifications.publish(
                "文本扩展未执行",
                "替换内容超过后台任务安全上限，本次保留原始输入。",
            );
            return CallNextHookEx(None, code, message, data);
        }
        let replacement = rule.replacement.clone();
        buffer.clear();
        drop(buffer);
        let task: TextExpansionTask = Box::new(move |expansion_shared| {
            for _ in 0..length {
                if let Err(error) = expansion_shared.text_input.send_key_down_with_permission(
                    0x08,
                    None,
                    move || {
                        if expansion_shared.background_input_allowed_at(background_generation) {
                            Ok(())
                        } else {
                            Err("宏播放或输入恢复期间禁止文本扩展注入".to_string())
                        }
                    },
                    || send_key(0x08, true),
                ) {
                    expansion_shared.record_safety(
                        "text_expansion_send_failed",
                        &[("phase", "backspace_down".to_string()), ("error", error)],
                    );
                    let cleanup = expansion_shared.text_input.cleanup(
                        |vk| send_key(vk, false),
                        |button| send_mouse_button(button, KeyAction::Up),
                    );
                    record_cleanup_report(expansion_shared, &cleanup, "text_expansion");
                    return;
                }
                if let Err(error) = expansion_shared
                    .text_input
                    .send_key_up(0x08, None, || send_key(0x08, false))
                {
                    expansion_shared.record_safety(
                        "text_expansion_send_failed",
                        &[("phase", "backspace_up".to_string()), ("error", error)],
                    );
                    let cleanup = expansion_shared.text_input.cleanup(
                        |vk| send_key(vk, false),
                        |button| send_mouse_button(button, KeyAction::Up),
                    );
                    record_cleanup_report(expansion_shared, &cleanup, "text_expansion");
                    return;
                }
            }
            if let Err(error) = expansion_shared.text_input.send_text_with_permission(
                &replacement,
                None,
                move || {
                    if expansion_shared.background_input_allowed_at(background_generation) {
                        Ok(())
                    } else {
                        Err("宏播放或输入恢复期间禁止文本扩展注入".to_string())
                    }
                },
                send_key,
            ) {
                expansion_shared.record_safety("text_expansion_send_failed", &[("error", error)]);
                let cleanup = expansion_shared.text_input.cleanup(
                    |vk| send_key(vk, false),
                    |button| send_mouse_button(button, KeyAction::Up),
                );
                record_cleanup_report(expansion_shared, &cleanup, "text_expansion");
            }
        });
        if shared
            .text_tasks
            .get()
            .is_none_or(|tasks| tasks.try_submit(task).is_err())
        {
            shared.notifications.publish(
                "文本扩展未执行",
                "后台工作队列繁忙或已经停止，本次保留原始输入。",
            );
            return CallNextHookEx(None, code, message, data);
        }
        return LRESULT(1);
    }

    CallNextHookEx(None, code, message, data)
}

#[cfg(windows)]
unsafe extern "system" fn mouse_hook(
    code: i32,
    message: windows::Win32::Foundation::WPARAM,
    data: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, MSLLHOOKSTRUCT, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
        WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN,
        WM_XBUTTONUP,
    };

    if code < 0 || data.0 == 0 {
        return CallNextHookEx(None, code, message, data);
    }
    let info = *(data.0 as *const MSLLHOOKSTRUCT);
    let captured_at = Instant::now();
    if info.flags & 0x00000001 != 0 {
        return CallNextHookEx(None, code, message, data);
    }
    let Some(shared) = HOOK_SHARED.get() else {
        return CallNextHookEx(None, code, message, data);
    };
    let message_id = message.0 as u32;
    let x = info.pt.x;
    let y = info.pt.y;
    if behavior_mouse_input_is_allowed(shared, x, y) {
        match message_id {
            WM_MOUSEMOVE => shared.record_behavior_mouse_move(x, y, captured_at),
            WM_LBUTTONDOWN => {
                shared.record_behavior_mouse_button(MouseButton::Left, true, x, y, captured_at)
            }
            WM_LBUTTONUP => {
                shared.record_behavior_mouse_button(MouseButton::Left, false, x, y, captured_at)
            }
            WM_RBUTTONDOWN => {
                shared.record_behavior_mouse_button(MouseButton::Right, true, x, y, captured_at)
            }
            WM_RBUTTONUP => {
                shared.record_behavior_mouse_button(MouseButton::Right, false, x, y, captured_at)
            }
            WM_MBUTTONDOWN => {
                shared.record_behavior_mouse_button(MouseButton::Middle, true, x, y, captured_at)
            }
            WM_MBUTTONUP => {
                shared.record_behavior_mouse_button(MouseButton::Middle, false, x, y, captured_at)
            }
            WM_XBUTTONDOWN => shared.record_behavior_mouse_button(
                x_button(info.mouseData),
                true,
                x,
                y,
                captured_at,
            ),
            WM_XBUTTONUP => shared.record_behavior_mouse_button(
                x_button(info.mouseData),
                false,
                x,
                y,
                captured_at,
            ),
            WM_MOUSEWHEEL => shared.record_behavior_wheel(
                0,
                i32::from((info.mouseData >> 16) as i16),
                x,
                y,
                captured_at,
            ),
            WM_MOUSEHWHEEL => shared.record_behavior_wheel(
                i32::from((info.mouseData >> 16) as i16),
                0,
                x,
                y,
                captured_at,
            ),
            _ => {}
        }
    }
    if shared.is_recording() && is_own_process_at_point(x, y) && is_mouse_button_message(message_id)
    {
        // A click on AutoFlow is the UI stop action. Remove the cursor path
        // leading to that button, but leave the same final mouse move intact
        // when recording is stopped with the keyboard shortcut.
        discard_trailing_mouse_actions(shared);
        return CallNextHookEx(None, code, message, data);
    }
    // Motion is meaningful even before the user has clicked into the target
    // application. Capture it as soon as recording starts, but use the window
    // under the pointer rather than the current foreground window so moving
    // back to AutoFlow cannot become part of the macro.
    if message_id == WM_MOUSEMOVE && shared.is_recording() {
        if recording_mouse_input_is_allowed(shared, x, y) && recording_mouse_move_enabled(shared) {
            record_mouse_move(shared, x, y, captured_at);
        }
        return CallNextHookEx(None, code, message, data);
    }
    if !recording_mouse_input_is_allowed(shared, x, y) {
        return CallNextHookEx(None, code, message, data);
    }
    if !recording_mouse_clicks_enabled(shared) {
        return CallNextHookEx(None, code, message, data);
    }

    match message_id {
        WM_LBUTTONDOWN => record_mouse_button(
            shared,
            MouseButton::Left,
            KeyAction::Down,
            x,
            y,
            captured_at,
        ),
        WM_LBUTTONUP => {
            record_mouse_button(shared, MouseButton::Left, KeyAction::Up, x, y, captured_at)
        }
        WM_RBUTTONDOWN => record_mouse_button(
            shared,
            MouseButton::Right,
            KeyAction::Down,
            x,
            y,
            captured_at,
        ),
        WM_RBUTTONUP => {
            record_mouse_button(shared, MouseButton::Right, KeyAction::Up, x, y, captured_at)
        }
        WM_MBUTTONDOWN => record_mouse_button(
            shared,
            MouseButton::Middle,
            KeyAction::Down,
            x,
            y,
            captured_at,
        ),
        WM_MBUTTONUP => record_mouse_button(
            shared,
            MouseButton::Middle,
            KeyAction::Up,
            x,
            y,
            captured_at,
        ),
        WM_XBUTTONDOWN => record_mouse_button(
            shared,
            x_button(info.mouseData),
            KeyAction::Down,
            x,
            y,
            captured_at,
        ),
        WM_XBUTTONUP => record_mouse_button(
            shared,
            x_button(info.mouseData),
            KeyAction::Up,
            x,
            y,
            captured_at,
        ),
        WM_MOUSEWHEEL => record_mouse_wheel(
            shared,
            0,
            i32::from((info.mouseData >> 16) as i16),
            captured_at,
        ),
        WM_MOUSEHWHEEL => record_mouse_wheel(
            shared,
            i32::from((info.mouseData >> 16) as i16),
            0,
            captured_at,
        ),
        _ => {}
    }
    CallNextHookEx(None, code, message, data)
}

#[cfg(windows)]
fn x_button(mouse_data: u32) -> MouseButton {
    if (mouse_data >> 16) as u16 == 1 {
        MouseButton::X1
    } else {
        MouseButton::X2
    }
}

#[cfg(windows)]
fn mouse_button_id(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 3,
        MouseButton::X1 => 4,
        MouseButton::X2 => 5,
    }
}

#[cfg(windows)]
fn canonical_virtual_key(vk: u32) -> u32 {
    match vk {
        0xA0 | 0xA1 => 0x10, // left / right Shift
        0xA2 | 0xA3 => 0x11, // left / right Ctrl
        0xA4 | 0xA5 => 0x12, // left / right Alt
        0x5B | 0x5C => 0x5B, // left / right Windows key
        _ => vk,
    }
}

#[cfg(windows)]
fn update_physical_pressed_ledger(
    ledger: &mut HashSet<u32>,
    raw_vk: u32,
    is_down: bool,
    is_up: bool,
    injected: bool,
) {
    if injected {
        return;
    }
    if is_down {
        ledger.insert(raw_vk);
    } else if is_up {
        ledger.remove(&raw_vk);
    }
}

#[cfg(windows)]
fn physical_ledger_contains_trigger(ledger: &HashSet<u32>, trigger_vk: u32) -> bool {
    match trigger_vk {
        0x10 => [0x10, 0xA0, 0xA1].iter().any(|vk| ledger.contains(vk)),
        0x11 => [0x11, 0xA2, 0xA3].iter().any(|vk| ledger.contains(vk)),
        0x12 => [0x12, 0xA4, 0xA5].iter().any(|vk| ledger.contains(vk)),
        0x5B => [0x5B, 0x5C].iter().any(|vk| ledger.contains(vk)),
        vk => ledger.contains(&vk),
    }
}

#[cfg(windows)]
fn trigger_down_from_ledger_or_async(
    ledger: &HashSet<u32>,
    trigger_vk: u32,
    is_async_down: impl FnOnce(u32) -> bool,
) -> bool {
    physical_ledger_contains_trigger(ledger, trigger_vk) || is_async_down(trigger_vk)
}

#[cfg(windows)]
fn foreground_process_id() -> Option<u32> {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    unsafe {
        let window = GetForegroundWindow();
        if window.0.is_null() {
            return None;
        }
        let mut process_id = 0;
        GetWindowThreadProcessId(window, Some(&mut process_id));
        Some(process_id)
    }
}

#[cfg(windows)]
fn is_own_process_foreground() -> bool {
    foreground_process_id() == Some(frontend_process_id())
}

#[cfg(windows)]
fn foreground_window_handle() -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    let window = unsafe { GetForegroundWindow() };
    (!window.0.is_null()).then_some(window.0 as isize)
}

#[cfg(windows)]
fn restore_previous_window_if_own_process(previous_window: Option<isize>) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{IsWindow, SetForegroundWindow};

    if !is_own_process_foreground() {
        return;
    }
    let Some(previous_window) = previous_window else {
        return;
    };
    let window = HWND(previous_window as *mut _);
    unsafe {
        if IsWindow(Some(window)).as_bool() {
            let _ = SetForegroundWindow(window);
        }
    }
}

#[cfg(windows)]
fn is_recording_shortcut_key(vk: u32, pressed: &HashSet<u32>) -> bool {
    matches!(vk, 0x10 | 0x11 | 0x78)
        && [0x10_u32, 0x11_u32, 0x78_u32]
            .iter()
            .all(|key| pressed.contains(key))
}

#[cfg(windows)]
fn latched_signature_contains_vk(signature: &str, vk: u32) -> bool {
    signature
        .split('+')
        .any(|key| key_to_vk(key) == Some(vk) || key.parse::<u32>().ok() == Some(vk))
}

#[cfg(windows)]
fn is_focus_switch_key(vk: u32, pressed: &HashSet<u32>) -> bool {
    matches!(vk, 0x09 | 0x1B) && (pressed.contains(&0x12) || pressed.contains(&0x5B))
}

#[cfg(windows)]
fn is_mouse_button_message(message_id: u32) -> bool {
    matches!(
        message_id,
        windows::Win32::UI::WindowsAndMessaging::WM_LBUTTONDOWN
            | windows::Win32::UI::WindowsAndMessaging::WM_LBUTTONUP
            | windows::Win32::UI::WindowsAndMessaging::WM_RBUTTONDOWN
            | windows::Win32::UI::WindowsAndMessaging::WM_RBUTTONUP
            | windows::Win32::UI::WindowsAndMessaging::WM_MBUTTONDOWN
            | windows::Win32::UI::WindowsAndMessaging::WM_MBUTTONUP
            | windows::Win32::UI::WindowsAndMessaging::WM_XBUTTONDOWN
            | windows::Win32::UI::WindowsAndMessaging::WM_XBUTTONUP
    )
}

#[cfg(windows)]
fn discard_focus_switch_steps(shared: &HookShared) {
    shared.submit_graph_capture(GraphCapture::DiscardFocus, Instant::now());
}

#[cfg(windows)]
fn discard_focus_switch_state(recorder: &mut RecorderState) {
    for vk in [0x09_u32, 0x12_u32, 0x5B_u32] {
        recorder.pressed_keys.remove(&vk);
    }
    while let Some(step) = recorder.steps.last() {
        let is_focus_switch_step = match step {
            MacroStep::Delay { .. } => true,
            MacroStep::Key { key, action } => {
                matches!(action, KeyAction::Down)
                    && key_to_vk(key).is_some_and(|vk| matches!(vk, 0x09 | 0x12 | 0x5B))
            }
            _ => false,
        };
        if !is_focus_switch_step {
            break;
        }
        recorder.steps.pop();
    }
    recorder.last_event = None;
}

#[cfg(windows)]
fn discard_trailing_mouse_actions(shared: &HookShared) {
    shared.submit_graph_capture(GraphCapture::DiscardMouse, Instant::now());
}

#[cfg(windows)]
fn discard_trailing_mouse_input_steps(recorder: &mut RecorderState) {
    let mut removed_mouse_input = false;

    // If the stop-button click made it into the recorder, remove only that
    // final down/up sequence and its delays. The previous click remains.
    for _ in 0..2 {
        let Some(MacroStep::MouseButton { button, .. }) = recorder.steps.last().cloned() else {
            break;
        };
        recorder.steps.pop();
        recorder.pressed_buttons.remove(&button);
        removed_mouse_input = true;
        if matches!(recorder.steps.last(), Some(MacroStep::Delay { .. })) {
            recorder.steps.pop();
        }
    }

    // The button click may have been filtered by the hook, leaving the cursor
    // path as the final steps. Remove all contiguous move samples and their
    // delays, but stop at the previous keyboard or mouse-button action.
    while matches!(recorder.steps.last(), Some(MacroStep::MouseMove { .. })) {
        recorder.steps.pop();
        if matches!(recorder.steps.last(), Some(MacroStep::Delay { .. })) {
            recorder.steps.pop();
        }
        removed_mouse_input = true;
    }

    if removed_mouse_input {
        recorder.last_event = None;
        recorder.last_mouse_move = None;
    }
}

#[cfg(windows)]
fn is_own_process_at_point(x: i32, y: i32) -> bool {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, WindowFromPoint};

    unsafe {
        let window = WindowFromPoint(POINT { x, y });
        if window.0.is_null() {
            return false;
        }
        let mut process_id = 0;
        GetWindowThreadProcessId(window, Some(&mut process_id));
        process_id == frontend_process_id()
    }
}

#[cfg(windows)]
fn recording_input_is_allowed(shared: &HookShared) -> bool {
    admit_recording_focus(shared, is_own_process_foreground())
}

#[cfg(windows)]
fn admit_recording_focus(shared: &HookShared, own_foreground: bool) -> bool {
    if own_foreground
        || !shared.graph_armed.load(Ordering::Acquire)
        || !shared
            .graph_capture
            .get()
            .is_some_and(|queue| queue.is_active())
    {
        return false;
    }

    // Starting recording while AutoFlow is focused arms the recorder. The
    // first hook event after AutoFlow loses focus only marks the boundary;
    // this keeps the click or Alt+Tab used to focus the target out of the
    // macro. Subsequent events are the actual macro input.
    if !shared.graph_boundary_started.swap(true, Ordering::AcqRel) {
        shared.submit_graph_capture(GraphCapture::BeginBoundary, Instant::now());
        return false;
    }
    true
}

#[cfg(windows)]
fn behavior_input_is_allowed(shared: &HookShared) -> bool {
    shared.is_behavior_recording() && !is_own_process_foreground()
}

#[cfg(windows)]
fn behavior_mouse_input_is_allowed(shared: &HookShared, x: i32, y: i32) -> bool {
    behavior_input_is_allowed(shared) && !is_own_process_at_point(x, y)
}

#[cfg(windows)]
fn recording_mouse_input_is_allowed(shared: &HookShared, x: i32, y: i32) -> bool {
    recording_input_is_allowed(shared) && !is_own_process_at_point(x, y)
}

#[cfg(windows)]
fn recording_mouse_move_enabled(shared: &HookShared) -> bool {
    shared.graph_mouse_move_enabled.load(Ordering::Acquire)
}

#[cfg(windows)]
fn recording_mouse_clicks_enabled(shared: &HookShared) -> bool {
    shared.graph_mouse_clicks_enabled.load(Ordering::Acquire)
}

#[cfg(windows)]
fn record_keyboard_event(shared: &HookShared, vk: u32, is_down: bool, captured_at: Instant) {
    shared.submit_graph_capture(GraphCapture::Key(vk, is_down), captured_at);
}

#[cfg(windows)]
fn record_mouse_button(
    shared: &HookShared,
    button: MouseButton,
    action: KeyAction,
    x: i32,
    y: i32,
    captured_at: Instant,
) {
    shared.submit_graph_capture(GraphCapture::Button(button, action, x, y), captured_at);
}

#[cfg(windows)]
fn record_mouse_move(shared: &HookShared, x: i32, y: i32, now: Instant) {
    shared.submit_graph_capture(GraphCapture::Move(x, y), now);
}

#[cfg(windows)]
fn record_mouse_wheel(shared: &HookShared, delta_x: i32, delta_y: i32, captured_at: Instant) {
    shared.submit_graph_capture(GraphCapture::Wheel(delta_x, delta_y), captured_at);
}

/// Value-only event payload for the ordered graph capture worker. Processing
/// depends solely on captured time and recorder state, never current focus or
/// an OS input backend. Focus/boundary decisions belong to event admission.
#[cfg(windows)]
enum GraphCapture {
    BeginBoundary,
    FinishShortcut,
    DiscardFocus,
    DiscardMouse,
    Key(u32, bool),
    Button(MouseButton, KeyAction, i32, i32),
    Move(i32, i32),
    Wheel(i32, i32),
}

#[cfg(windows)]
fn apply_graph_capture(recorder: &mut RecorderState, event: GraphCapture, at: Instant) {
    if !recorder.active {
        return;
    }
    match event {
        GraphCapture::BeginBoundary => {
            recorder.capture_started = true;
            recorder.last_event = None;
            recorder.last_mouse_move = None;
            recorder.pressed_keys.clear();
            recorder.pressed_buttons.clear();
        }
        GraphCapture::DiscardFocus => discard_focus_switch_state(recorder),
        GraphCapture::DiscardMouse => discard_trailing_mouse_input_steps(recorder),
        GraphCapture::FinishShortcut => {
            discard_recording_shortcut_steps(recorder);
            finish_recorder(recorder);
        }
        GraphCapture::Key(vk, down) => {
            // Repeated down and unmatched up are not replayable actions.
            if down == recorder.pressed_keys.contains(&vk) {
                return;
            }
            push_record_step_at(
                recorder,
                MacroStep::Key {
                    key: key_name_from_vk(vk),
                    action: if down { KeyAction::Down } else { KeyAction::Up },
                },
                at,
            );
            if down {
                recorder.pressed_keys.insert(vk);
            } else {
                recorder.pressed_keys.remove(&vk);
            }
        }
        GraphCapture::Button(button, action, x, y) => {
            if matches!(action, KeyAction::Up) && !recorder.pressed_buttons.contains(&button) {
                return;
            }
            push_record_step_at(
                recorder,
                MacroStep::MouseButton {
                    button,
                    action,
                    x,
                    y,
                },
                at,
            );
            if matches!(action, KeyAction::Down) {
                recorder.pressed_buttons.insert(button);
            } else {
                recorder.pressed_buttons.remove(&button);
            }
        }
        GraphCapture::Move(x, y) => {
            if let Some((previous_at, previous_x, previous_y)) = recorder.last_mouse_move {
                let elapsed = at.saturating_duration_since(previous_at).as_millis();
                // Promote before subtraction: extreme signed screen origins
                // must not overflow while classifying the captured motion.
                let distance = (i64::from(x) - i64::from(previous_x))
                    .abs()
                    .max((i64::from(y) - i64::from(previous_y)).abs());
                if elapsed < 16 && distance <= 3 {
                    return;
                }
            }
            recorder.last_mouse_move = Some((at, x, y));
            push_record_step_at(recorder, MacroStep::MouseMove { x, y }, at);
        }
        GraphCapture::Wheel(delta_x, delta_y) => {
            push_record_step_at(recorder, MacroStep::Wheel { delta_x, delta_y }, at);
        }
    }
}

#[cfg(windows)]
fn push_record_step_at(recorder: &mut RecorderState, step: MacroStep, now: Instant) {
    if let Some(last_event) = recorder.last_event {
        let elapsed = now.duration_since(last_event).as_millis() as u64;
        // MacroStep stores delays as whole milliseconds. Preserve every
        // representable interval instead of dropping the 1-7 ms gaps between
        // high-frequency mouse samples. Dropping those gaps makes playback
        // collapse a recorded path into a burst of consecutive move_to calls.
        if elapsed > 0 {
            recorder.steps.push(MacroStep::Delay {
                duration_ms: elapsed.min(60_000),
                duration_max_ms: None,
            });
        }
    }
    recorder.steps.push(step);
    recorder.last_event = Some(now);
}

#[cfg(windows)]
fn discard_recording_shortcut_steps(recorder: &mut RecorderState) {
    for vk in [0x10_u32, 0x11_u32, 0x78_u32] {
        recorder.pressed_keys.remove(&vk);
    }

    while let Some(step) = recorder.steps.last() {
        let is_recording_shortcut_step = match step {
            MacroStep::Delay { .. } => true,
            MacroStep::Key { key, action } => {
                matches!(action, KeyAction::Down)
                    && key_to_vk(key).is_some_and(|vk| matches!(vk, 0x10 | 0x11 | 0x78))
            }
            _ => false,
        };
        if !is_recording_shortcut_step {
            break;
        }
        recorder.steps.pop();
    }
    recorder.last_event = None;
}

#[cfg(windows)]
fn finish_recorder(recorder: &mut RecorderState) {
    let pressed_keys = recorder.pressed_keys.drain().collect::<Vec<_>>();
    for vk in pressed_keys {
        recorder.steps.push(MacroStep::Key {
            key: key_name_from_vk(vk),
            action: KeyAction::Up,
        });
    }
    let pressed_buttons = recorder.pressed_buttons.drain().collect::<Vec<_>>();
    for button in pressed_buttons {
        recorder.steps.push(MacroStep::MouseButton {
            button,
            action: KeyAction::Up,
            x: 0,
            y: 0,
        });
    }
    recorder.active = false;
    recorder.capture_started = false;
    recorder.started_at = None;
    recorder.last_event = None;
    recorder.last_mouse_move = None;
    recorder.completed_steps = Some(std::mem::take(&mut recorder.steps));
}

#[cfg(windows)]
fn key_name_from_vk(vk: u32) -> String {
    let name = match vk {
        0x10 => "Shift".to_string(),
        0x11 => "Ctrl".to_string(),
        0x12 => "Alt".to_string(),
        0x5B => "Win".to_string(),
        0x1B => "Esc".to_string(),
        0x0D => "Enter".to_string(),
        0x20 => "Space".to_string(),
        0x09 => "Tab".to_string(),
        0x08 => "Backspace".to_string(),
        0x14 => "CapsLock".to_string(),
        0x25 => "Left".to_string(),
        0x26 => "Up".to_string(),
        0x27 => "Right".to_string(),
        0x28 => "Down".to_string(),
        0x41..=0x5A | 0x30..=0x39 => char::from_u32(vk).unwrap_or('?').to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        _ => format!("VK{vk}"),
    };
    name
}

#[cfg(windows)]
fn macro_rule_matches_key_down(rule: &MacroRule, current_vk: u32, pressed: &HashSet<u32>) -> bool {
    let trigger_vks = rule
        .trigger_keys
        .iter()
        .filter_map(|key| key_to_vk(key))
        .collect::<Vec<_>>();
    !trigger_vks.is_empty()
        && trigger_vks.len() == rule.trigger_keys.len()
        && trigger_vks.contains(&current_vk)
        && trigger_vks.iter().all(|key| pressed.contains(key))
}

#[cfg(windows)]
fn effective_hotkey_mode(rule: &MacroRule) -> MacroMode {
    if rule.name.trim() == "连点器" {
        MacroMode::Toggle
    } else {
        rule.mode
    }
}

#[cfg(windows)]
fn process_macro_key_down(
    shared: &Arc<HookShared>,
    config: &AppConfig,
    config_revision: u64,
    current_vk: u32,
    was_pressed: bool,
    pressed: &HashSet<u32>,
) -> bool {
    let request_generation = shared.emergency_generation.load(Ordering::SeqCst);
    if shared.is_recording() {
        return false;
    }
    // Stopping an already-running Toggle/clicker is a revocation operation,
    // not a new start. Derive it entirely from the immutable config snapshot
    // and pressed-key snapshot before touching start-only registry/latch locks.
    // Rearm fencing prevents a native WM_HOTKEY for the same physical press
    // from becoming a fresh start if the playback thread exits immediately.
    let immediate_stop = !was_pressed
        && config
            .macros
            .iter()
            .filter(|rule| rule.enabled)
            .any(|rule| {
                matches!(effective_hotkey_mode(rule), MacroMode::Toggle)
                    && macro_rule_matches_key_down(rule, current_vk, pressed)
            });
    if immediate_stop && shared.is_playback_running() {
        shared.trigger_rearm_required.store(true, Ordering::Release);
        shared.stop_playback();
        return true;
    }
    let native_macro_ids = match shared.native_macro_hotkeys.try_lock() {
        Ok(registered) => registered.values().cloned().collect::<HashSet<_>>(),
        Err(error) => {
            // This function runs directly on the low-level keyboard callback.
            // If registration identity cannot be read immediately, never risk
            // submitting the same trigger through both native and fallback
            // paths. Consume only a configured macro key and fail closed.
            let consume = is_macro_trigger_key(config, current_vk);
            if consume {
                let kind = match error {
                    std::sync::TryLockError::WouldBlock => "would_block",
                    std::sync::TryLockError::Poisoned(_) => "poisoned",
                };
                shared.record_safety_async(
                    "fallback_macro_lock_unavailable",
                    vec![
                        ("lock".to_string(), "native_macro_hotkeys".to_string()),
                        ("kind".to_string(), kind.to_string()),
                    ],
                );
            }
            return consume;
        }
    };
    for rule in config.macros.iter().filter(|rule| rule.enabled) {
        if shared
            .native_clicker_hotkey_registered
            .load(Ordering::SeqCst)
            && is_native_clicker_rule(rule)
        {
            continue;
        }
        if native_macro_ids.contains(&rule.id) {
            continue;
        }
        if !macro_rule_matches_key_down(rule, current_vk, pressed) {
            continue;
        }
        if was_pressed {
            return true;
        }
        let Some(signature) = macro_latch_signature(&rule.trigger_keys) else {
            continue;
        };
        let mut latched = match shared.latched_hotkeys.try_lock() {
            Ok(latched) => latched,
            Err(error) => {
                let kind = match error {
                    std::sync::TryLockError::WouldBlock => "would_block",
                    std::sync::TryLockError::Poisoned(_) => "poisoned",
                };
                shared.record_safety_async(
                    "fallback_macro_lock_unavailable",
                    vec![
                        ("lock".to_string(), "latched_hotkeys".to_string()),
                        ("kind".to_string(), kind.to_string()),
                        ("macro_id".to_string(), rule.id.clone()),
                    ],
                );
                // The full combination matched, but it cannot be latched
                // without waiting. Consume this keydown and submit nothing;
                // the next physical press may retry after normal keyup.
                return true;
            }
        };
        if !latched.insert(signature) {
            continue;
        }
        drop(latched);
        match hotkey_playback_action(effective_hotkey_mode(rule), shared.is_playback_running()) {
            HotkeyPlaybackAction::Ignore => {
                shared.record_safety_async(
                    "macro_trigger_ignored_while_running",
                    vec![("macro_id".to_string(), rule.id.clone())],
                );
                return true;
            }
            HotkeyPlaybackAction::StopImmediate => {
                shared.stop_playback();
                return true;
            }
            HotkeyPlaybackAction::StartAfterRelease | HotkeyPlaybackAction::StartImmediate => {}
        }
        if matches!(effective_hotkey_mode(rule), MacroMode::Hold) {
            shared.record_safety_async(
                "macro_hold_trigger_matched",
                hold_diagnostic_fields(
                    Some(&rule.id),
                    "matched",
                    None,
                    None,
                    Some(current_vk),
                    crate::config::hold_trigger_parts(&rule.trigger_keys)
                        .map(|parts| parts.owner_vk),
                    None,
                ),
            );
        }
        shared.submit_macro_trigger(rule, request_generation, config_revision, Some(current_vk));
        return true;
    }
    false
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HotkeyPlaybackAction {
    StartAfterRelease,
    StartImmediate,
    StopImmediate,
    Ignore,
}

#[cfg(windows)]
fn hotkey_playback_action(mode: MacroMode, playback_running: bool) -> HotkeyPlaybackAction {
    match (mode, playback_running) {
        (MacroMode::Hold, true) => HotkeyPlaybackAction::Ignore,
        (MacroMode::Hold, false) => HotkeyPlaybackAction::StartImmediate,
        (MacroMode::Toggle, true) => HotkeyPlaybackAction::StopImmediate,
        (MacroMode::Once | MacroMode::Repeat, true) => HotkeyPlaybackAction::Ignore,
        (MacroMode::Once | MacroMode::Repeat | MacroMode::Toggle, false) => {
            HotkeyPlaybackAction::StartAfterRelease
        }
    }
}

#[cfg(windows)]
fn trigger_start_timing(rule: &MacroRule) -> TriggerStartTiming {
    if matches!(effective_hotkey_mode(rule), MacroMode::Hold) {
        TriggerStartTiming::HoldModifierRelease
    } else {
        TriggerStartTiming::ReleaseGated
    }
}

#[cfg(windows)]
fn hold_trigger_word_contains(words: &[AtomicU64; 4], vk: u32) -> bool {
    let Ok(index) = usize::try_from(vk / 64) else {
        return false;
    };
    index < words.len() && words[index].load(Ordering::Acquire) & (1_u64 << (vk % 64)) != 0
}

#[cfg(windows)]
fn consistent_hold_atomic_snapshot(
    epoch_before: u64,
    trigger_matches: bool,
    phase_before: u32,
    token: RunToken,
    phase_after: u32,
    epoch_after: u64,
) -> Option<HoldAtomicSnapshot> {
    if epoch_before == 0
        || epoch_before != epoch_after
        || phase_before != phase_after
        || !(1..=3).contains(&phase_before)
    {
        return None;
    }
    Some(HoldAtomicSnapshot {
        epoch: epoch_before,
        trigger_matches,
        bound_token: (phase_before >= 2 && token.id != 0).then_some(token),
    })
}

#[cfg(windows)]
fn cancel_hold_identity_for_key(
    current: &mut Option<HoldLifecycleIdentity>,
    vk: u32,
    matched_epoch: &mut Option<u64>,
    bound_token: &mut Option<RunToken>,
) -> bool {
    let Some(identity) = current.as_mut().filter(|identity| identity.owner_vk == vk) else {
        return false;
    };
    *matched_epoch = Some(identity.epoch);
    if identity.cancelled || matches!(identity.phase, HoldLifecyclePhase::Retired) {
        return true;
    }
    identity.cancelled = true;
    *bound_token = match identity.phase {
        HoldLifecyclePhase::Bound { run_token } | HoldLifecyclePhase::Active { run_token, .. } => {
            Some(run_token)
        }
        HoldLifecyclePhase::Pending | HoldLifecyclePhase::Retired => None,
    };
    true
}

#[cfg(windows)]
fn macro_latch_signature(trigger_keys: &[String]) -> Option<String> {
    let trigger_vks = macro_trigger_vks(trigger_keys)?;
    Some(format!(
        "macro+{}",
        trigger_vks
            .iter()
            .map(|key| key.to_string())
            .collect::<Vec<_>>()
            .join("+")
    ))
}

#[cfg(windows)]
fn macro_trigger_vks(trigger_keys: &[String]) -> Option<Vec<u32>> {
    let trigger_vks = trigger_keys
        .iter()
        .map(|key| key_to_vk(key))
        .collect::<Option<Vec<_>>>()?;
    (!trigger_vks.is_empty()).then_some(trigger_vks)
}

#[cfg(windows)]
fn clear_released_macro_trigger_state(
    pressed: &mut HashSet<u32>,
    latched: &mut HashSet<String>,
    trigger_vks: &[u32],
    is_physically_down: impl Fn(u32) -> bool,
) -> bool {
    if trigger_vks.iter().copied().any(is_physically_down) {
        return false;
    }
    for vk in trigger_vks {
        pressed.remove(vk);
    }
    let signature = format!(
        "macro+{}",
        trigger_vks
            .iter()
            .map(|key| key.to_string())
            .collect::<Vec<_>>()
            .join("+")
    );
    latched.remove(&signature);
    true
}

#[cfg(windows)]
fn is_macro_trigger_key(config: &AppConfig, vk: u32) -> bool {
    config
        .macros
        .iter()
        .filter(|rule| rule.enabled)
        .any(|rule| {
            rule.trigger_keys
                .iter()
                .filter_map(|key| key_to_vk(key))
                .any(|trigger_vk| trigger_vk == vk)
        })
}

#[cfg(windows)]
fn should_suppress_for_trigger_rearm(shared: &HookShared, config: &AppConfig, vk: u32) -> bool {
    shared.trigger_rearm_required.load(Ordering::Acquire) && is_macro_trigger_key(config, vk)
}

#[cfg(windows)]
fn rearm_macro_triggers_if_released_with(
    shared: &HookShared,
    config: &AppConfig,
    mut is_async_down: impl FnMut(u32) -> bool,
) {
    if !shared.trigger_rearm_required.load(Ordering::Acquire) {
        return;
    }
    if shared.physical_ledger_uncertain.load(Ordering::Acquire) {
        return;
    }
    let ledger = match shared.physical_pressed.try_lock() {
        Ok(ledger) => ledger.clone(),
        Err(_) => {
            shared
                .physical_ledger_uncertain
                .store(true, Ordering::Release);
            return;
        }
    };
    if shared.physical_ledger_uncertain.load(Ordering::Acquire) {
        return;
    }
    let trigger_vks = config
        .macros
        .iter()
        .filter(|rule| rule.enabled)
        .flat_map(|rule| rule.trigger_keys.iter())
        .filter_map(|key| key_to_vk(key))
        .collect::<HashSet<_>>();
    if trigger_vks
        .iter()
        .copied()
        .any(|vk| trigger_down_from_ledger_or_async(&ledger, vk, &mut is_async_down))
    {
        return;
    }
    let Ok(mut pressed) = shared.pressed.try_lock() else {
        return;
    };
    let Ok(mut latched) = shared.latched_hotkeys.try_lock() else {
        return;
    };
    // Async state can change after the first sample even though hook callbacks
    // are serialized. Recheck immediately before clearing logical identity.
    if shared.physical_ledger_uncertain.load(Ordering::Acquire)
        || trigger_vks.iter().copied().any(is_async_down)
    {
        return;
    }
    for vk in trigger_vks {
        pressed.remove(&vk);
    }
    latched.retain(|signature| !signature.starts_with("macro+"));
    shared
        .trigger_rearm_required
        .store(false, Ordering::Release);
}

#[cfg(windows)]
fn rearm_macro_triggers_if_released(shared: &HookShared, config: &AppConfig) {
    rearm_macro_triggers_if_released_with(shared, config, trigger_vk_is_physically_down);
}

#[cfg(windows)]
fn should_show_playback_error(error: &str) -> bool {
    error != CANCELLED
}

#[cfg(windows)]
fn should_show_start_error(code: &str) -> bool {
    !matches!(
        code,
        "macro_busy"
            | "runtime_admission_busy"
            | "app_shutting_down"
            | "playback_cancelled"
            | "emergency_stop_unavailable"
            | "macro_trigger_config_stale"
            | "macro_hold_released"
    )
}

#[cfg(windows)]
static HOOK_SHARED: std::sync::OnceLock<Arc<HookShared>> = std::sync::OnceLock::new();

#[cfg(windows)]
const CLICKER_HOTKEY_ID: i32 = 0x4155;

#[cfg(windows)]
const NATIVE_MACRO_HOTKEY_ID_START: i32 = 0x5000;

#[cfg(windows)]
const REFRESH_NATIVE_HOTKEYS_MESSAGE: u32 = 0x8041;

#[cfg(windows)]
const HOOK_HEALTH_PROBE_MESSAGE: u32 = 0x8042;

#[cfg(windows)]
fn native_hotkey_spec(trigger_keys: &[String]) -> Option<(u32, u32)> {
    let mut modifiers = 0u32;
    let mut primary_key = None;
    for key in trigger_keys {
        match key_to_vk(key)? {
            0x10 => modifiers |= 0x0004,
            0x11 => modifiers |= 0x0002,
            0x12 => modifiers |= 0x0001,
            0x5B => modifiers |= 0x0008,
            key => {
                if primary_key.is_some() {
                    return None;
                }
                primary_key = Some(key);
            }
        }
    }
    primary_key.map(|key| (modifiers, key))
}

#[cfg(windows)]
fn refresh_native_macro_hotkeys(shared: &HookShared) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
    };

    let Ok(config_guard) = shared.config.try_lock() else {
        return;
    };
    // Clear only while config is held: any later update publishes another
    // pending request after its write, so it cannot be lost during OS work.
    shared
        .native_refresh_pending
        .store(false, Ordering::Release);
    let plan = native_registration_plan(&config_guard);
    drop(config_guard);
    let Ok(mut registered) = shared.native_macro_hotkeys.try_lock() else {
        shared.native_refresh_pending.store(true, Ordering::Release);
        return;
    };
    if !retire_native_registration_map(&mut registered, |id| unsafe {
        UnregisterHotKey(None, id).is_ok()
    }) {
        shared.native_refresh_pending.store(true, Ordering::Release);
        shared.trigger_rearm_required.store(true, Ordering::Release);
        shared.record_safety_async("native_hotkey_unregister_failed", Vec::new());
        return;
    }

    for (macro_id, modifiers, primary_key) in plan {
        let Some(hotkey_id) = shared.allocate_native_hotkey_id() else {
            shared.record_safety_async("native_hotkey_identity_exhausted", Vec::new());
            break;
        };
        let succeeded = unsafe {
            RegisterHotKey(
                None,
                hotkey_id,
                HOT_KEY_MODIFIERS(modifiers | MOD_NOREPEAT.0),
                primary_key,
            )
        }
        .is_ok();
        if succeeded {
            registered.insert(hotkey_id, macro_id);
        } else {
            log::warn!("宏 {macro_id} 的原生快捷键注册失败，将使用兼容监听方式");
        }
    }
}

/// Only properties that affect RegisterHotKey belong here. Script edits,
/// progress UI, global gating and policy changes are read at dispatch time;
/// they must not tear down registrations on every focus/config refresh.
#[cfg(windows)]
fn native_registration_plan(config: &AppConfig) -> Vec<(String, u32, u32)> {
    let emergency_vk = key_to_vk(&config.emergency_stop);
    config
        .macros
        .iter()
        .filter(|rule| {
            rule.enabled
                && !matches!(rule.mode, MacroMode::Hold)
                && !is_native_clicker_rule(rule)
                && rule.import_error.is_none()
        })
        .filter_map(|rule| {
            let (modifiers, key) = native_hotkey_spec(&rule.trigger_keys)?;
            if rule
                .trigger_keys
                .iter()
                .filter_map(|trigger| key_to_vk(trigger))
                .any(|trigger_vk| Some(trigger_vk) == emergency_vk)
            {
                return None;
            }
            Some((rule.id.clone(), modifiers, key))
        })
        .collect()
}

#[cfg(windows)]
fn unregister_native_macro_hotkeys(shared: &HookShared) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey;

    unregister_native_macro_hotkeys_with(shared, |id| unsafe { UnregisterHotKey(None, id).is_ok() })
}

#[cfg(windows)]
fn unregister_native_macro_hotkeys_with(
    shared: &HookShared,
    unregister: impl FnMut(i32) -> bool,
) -> bool {
    let Ok(mut registered) = shared.native_macro_hotkeys.try_lock() else {
        return false;
    };
    retire_native_registration_map(&mut registered, unregister)
}

#[cfg(windows)]
fn retire_native_registration_map(
    registered: &mut HashMap<i32, String>,
    mut unregister: impl FnMut(i32) -> bool,
) -> bool {
    // A failed OS release remains in the ledger for diagnosis/retry.
    registered.retain(|id, _| !unregister(*id));
    registered.is_empty()
}

#[cfg(windows)]
fn process_native_macro_hotkey(shared: &Arc<HookShared>, hotkey_id: i32) {
    let request_generation = shared.emergency_generation.load(Ordering::SeqCst);
    if shared.is_recording() {
        return;
    }
    if shared.trigger_rearm_required.load(Ordering::Acquire)
        || shared.native_refresh_pending.load(Ordering::Acquire)
    {
        return;
    }
    let macro_id = shared
        .native_macro_hotkeys
        .try_lock()
        .ok()
        .and_then(|registered| registered.get(&hotkey_id).cloned());
    let Some(macro_id) = macro_id else {
        return;
    };
    let Ok((config, config_revision)) = shared.config.try_lock().map(|config| {
        (
            config.clone(),
            shared.trigger_config_revision.load(Ordering::Acquire),
        )
    }) else {
        return;
    };
    if !config.global_enabled {
        return;
    }
    let Some(rule) = config
        .macros
        .iter()
        .find(|rule| rule.enabled && rule.id == macro_id)
    else {
        return;
    };
    match hotkey_playback_action(effective_hotkey_mode(rule), shared.is_playback_running()) {
        HotkeyPlaybackAction::Ignore => {
            shared.record_safety_async(
                "native_macro_trigger_ignored_while_running",
                vec![("macro_id".to_string(), rule.id.clone())],
            );
            return;
        }
        HotkeyPlaybackAction::StopImmediate => {
            shared.stop_playback();
            return;
        }
        HotkeyPlaybackAction::StartAfterRelease | HotkeyPlaybackAction::StartImmediate => {}
    }
    shared.submit_macro_trigger(rule, request_generation, config_revision, None);
}

#[cfg(windows)]
fn is_native_clicker_rule(rule: &MacroRule) -> bool {
    rule.name.trim() == "连点器"
        && rule.trigger_keys.len() == 2
        && rule
            .trigger_keys
            .iter()
            .any(|key| key_to_vk(key) == Some(0x11))
        && rule
            .trigger_keys
            .iter()
            .any(|key| key_to_vk(key) == Some(0x77))
}

#[cfg(windows)]
fn native_clicker_registration_allowed(config: &AppConfig) -> bool {
    let emergency_vk = key_to_vk(&config.emergency_stop);
    [0x11, 0x77].iter().all(|vk| Some(*vk) != emergency_vk)
}

#[cfg(windows)]
fn process_native_clicker_hotkey(shared: &Arc<HookShared>) {
    let request_generation = shared.emergency_generation.load(Ordering::SeqCst);
    if shared.is_recording() {
        return;
    }
    if shared.trigger_rearm_required.load(Ordering::Acquire)
        || shared.native_refresh_pending.load(Ordering::Acquire)
    {
        return;
    }
    let Ok((config, config_revision)) = shared.config.try_lock().map(|config| {
        (
            config.clone(),
            shared.trigger_config_revision.load(Ordering::Acquire),
        )
    }) else {
        return;
    };
    if !config.global_enabled {
        return;
    }
    let Some(rule) = config
        .macros
        .iter()
        .find(|rule| rule.enabled && is_native_clicker_rule(rule))
    else {
        return;
    };
    match hotkey_playback_action(MacroMode::Toggle, shared.is_playback_running()) {
        HotkeyPlaybackAction::StopImmediate => {
            shared.stop_playback();
            return;
        }
        HotkeyPlaybackAction::StartAfterRelease => {}
        HotkeyPlaybackAction::StartImmediate | HotkeyPlaybackAction::Ignore => return,
    }
    shared.submit_macro_trigger(rule, request_generation, config_revision, None);
}

#[cfg(windows)]
fn key_to_vk(key: &str) -> Option<u32> {
    crate::config::normalized_virtual_key(key)
}

#[cfg(windows)]
fn is_shift_key(vk: u32) -> bool {
    matches!(vk, 0x10 | 0xA0 | 0xA1)
}

#[cfg(windows)]
fn is_text_modifier(vk: u32) -> bool {
    matches!(vk, 0x11 | 0xA2 | 0xA3 | 0x12 | 0xA4 | 0xA5 | 0x5B | 0x5C)
}

#[cfg(windows)]
fn is_keyboard_modifier(vk: u32) -> bool {
    is_shift_key(vk) || is_text_modifier(vk)
}

#[cfg(windows)]
fn shifted_printable_character(vk: u32, pressed: &HashSet<u32>) -> Option<&'static str> {
    if !pressed.contains(&0x10) {
        return None;
    }

    // ToUnicodeEx normally converts this using the active keyboard layout. Low-level hooks
    // sometimes receive the generic Shift virtual key, however, which some layouts do not
    // apply to the synthetic keyboard-state array. These symbols keep "@@" reliable.
    match vk {
        0x30 => Some(")"),
        0x31 => Some("!"),
        0x32 => Some("@"),
        0x33 => Some("#"),
        0x34 => Some("$"),
        0x35 => Some("%"),
        0x36 => Some("^"),
        0x37 => Some("&"),
        0x38 => Some("*"),
        0x39 => Some("("),
        0xBA => Some(":"),
        0xBB => Some("+"),
        0xBC => Some("<"),
        0xBD => Some("_"),
        0xBE => Some(">"),
        0xBF => Some("?"),
        0xC0 => Some("~"),
        0xDB => Some("{"),
        0xDC => Some("|"),
        0xDD => Some("}"),
        0xDE => Some("\""),
        _ => None,
    }
}

#[cfg(windows)]
fn key_to_character(vk: u32, scan_code: u32, pressed: &HashSet<u32>) -> Option<String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardLayout, ToUnicodeEx};
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    if let Some(character) = shifted_printable_character(vk, pressed) {
        return Some(character.to_string());
    }

    let window = unsafe { GetForegroundWindow() };
    if window.0.is_null() {
        return None;
    }
    let thread_id = unsafe { GetWindowThreadProcessId(window, None) };
    let layout = unsafe { GetKeyboardLayout(thread_id) };
    let mut keyboard_state = [0u8; 256];
    for key in pressed {
        if *key < keyboard_state.len() as u32 {
            keyboard_state[*key as usize] = 0x80;
        }
        match *key {
            0xA0 | 0xA1 => keyboard_state[0x10] = 0x80,
            0xA2 | 0xA3 => keyboard_state[0x11] = 0x80,
            0xA4 | 0xA5 => keyboard_state[0x12] = 0x80,
            _ => {}
        }
    }
    if pressed.contains(&0x10) {
        keyboard_state[0x10] = 0x80;
        keyboard_state[0xA0] = 0x80;
        keyboard_state[0xA1] = 0x80;
    }
    if pressed.contains(&0x14) {
        keyboard_state[0x14] |= 0x01;
    }

    let mut output = [0u16; 8];
    let length = unsafe {
        ToUnicodeEx(
            vk,
            scan_code,
            &keyboard_state,
            &mut output,
            0x0004,
            Some(layout),
        )
    };
    if length <= 0 {
        return None;
    }
    String::from_utf16(&output[..length as usize]).ok()
}

#[cfg(windows)]
#[derive(Debug)]
enum AutomationProgramResult {
    Continue,
    Stopped,
    StoppedWithMessage { title: String, message: String },
}

#[cfg(windows)]
fn play_macro_thread(
    shared: &Arc<HookShared>,
    macro_rule: &MacroRule,
    stop: &Arc<AtomicBool>,
    emergency_generation: u64,
    instance_id: u64,
    input_state: Arc<InjectedInputState>,
    run_token: RunToken,
) -> Result<Option<(String, String)>, String> {
    let behavior = match shared.behavior_runtime_v2(macro_rule) {
        Ok(behavior) => behavior,
        Err(error) => {
            shared.set_playback_phase(instance_id, "failed", "safe", Some(error.clone()));
            return Err(error);
        }
    };
    let max_iterations = match macro_rule.mode {
        MacroMode::Once => Some(1),
        MacroMode::Repeat => Some(macro_rule.repeat_count.max(1)),
        MacroMode::Hold | MacroMode::Toggle => None,
    };
    let mut iterations = 0;
    let mut held_keys = HashSet::new();
    let mut held_buttons = HashSet::new();

    let watcher_stop = Arc::clone(stop);
    let watcher_shared = Arc::clone(shared);
    let watcher = thread::Builder::new()
        .name("autoflow-playback-cancel-watch".to_string())
        .spawn(move || {
            while !watcher_stop.load(Ordering::SeqCst)
                && !watcher_shared.shutdown.load(Ordering::SeqCst)
                && watcher_shared.emergency_generation.load(Ordering::SeqCst)
                    == emergency_generation
                && !watcher_shared.controller.token_revoked(run_token)
            {
                if !watcher_shared.emergency_stop_ready() {
                    watcher_shared
                        .emergency_detector_ready
                        .store(false, Ordering::Release);
                    watcher_shared.request_emergency_stop(EmergencyEntryPoint::Command, true);
                    watcher_shared.notifications.publish(
                        "急停检测不可用",
                        "急停线程心跳失效，已请求停止并撤销输入许可；请勿继续播放宏。",
                    );
                    break;
                }
                thread::sleep(Duration::from_millis(2));
            }
            watcher_stop.store(true, Ordering::SeqCst);
        })
        .map_err(|error| format!("无法启动播放取消监视器: {error}"))?;

    let mut script_stop_message = None;
    let playback_result = loop {
        if shared.stop_requested(stop, emergency_generation) {
            break Ok(());
        }
        let iteration_result = play_automation_program(
            shared,
            macro_rule,
            macro_rule.speed,
            stop,
            &mut held_keys,
            &mut held_buttons,
            behavior.clone(),
            Arc::clone(&input_state),
            emergency_generation,
            instance_id,
            run_token,
        );
        match iteration_result {
            Ok(AutomationProgramResult::Continue) => {}
            Ok(AutomationProgramResult::Stopped) => break Ok(()),
            Ok(AutomationProgramResult::StoppedWithMessage { title, message }) => {
                script_stop_message = Some((title, message));
                break Ok(());
            }
            Err(error) => break Err(error),
        }
        iterations += 1;
        if max_iterations.is_some_and(|max| iterations >= max) {
            break Ok(());
        }
    };

    let stop_requested = shared.stop_requested(stop, emergency_generation);
    if stop_requested && playback_result.is_ok() {
        shared.set_playback_phase(instance_id, "stopping", "not_started", None);
    } else if let Err(error) = &playback_result {
        shared.set_playback_phase(instance_id, "failed", "not_started", Some(error.clone()));
        let (current_step, step_kind) = shared
            .playback
            .lock()
            .map(|playback| {
                (
                    playback.current_step,
                    playback
                        .current_step_kind
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                )
            })
            .unwrap_or((0, "unknown".to_string()));
        shared.record_safety(
            "playback_input_error",
            &[
                ("macro_id", macro_rule.id.clone()),
                ("macro_name", macro_rule.name.clone()),
                ("current_step", current_step.to_string()),
                ("step_kind", step_kind),
                ("error", error.clone()),
                (
                    "last_input_send",
                    input_state
                        .last_input()
                        .unwrap_or_else(|| "none".to_string()),
                ),
            ],
        );
    }

    shared.controller.begin_cleaning(run_token);
    shared.set_playback_phase(instance_id, "cleaning", "pending", None);
    stop.store(true, Ordering::SeqCst);
    let _ = watcher.join();
    let cleanup = input_state.cleanup(
        |vk| send_key(vk, false),
        |button| send_mouse_button(button, KeyAction::Up),
    );
    record_cleanup_report(shared, &cleanup, &format!("playback:{instance_id}"));
    held_keys.clear();
    held_buttons.clear();
    if cleanup.is_safe() {
        if script_stop_message.is_some() {
            shared.set_playback_phase(instance_id, "stopped", "safe", None);
        } else if playback_result.is_err() {
            shared.set_playback_phase(instance_id, "failed", "safe", None);
        } else if stop_requested {
            shared.set_playback_phase(instance_id, "stopped", "safe", None);
        } else {
            shared.set_playback_phase(instance_id, "completed", "safe", None);
        }
        playback_result.map(|()| script_stop_message)
    } else {
        let failures = cleanup
            .failures
            .iter()
            .map(|failure| format!("{} {}: {}", failure.kind, failure.value, failure.last_error))
            .collect::<Vec<_>>()
            .join("; ");
        let error = format!("输入清理失败，当前状态可能不安全: {failures}");
        shared.set_playback_phase(instance_id, "cleanup_failed", "failed", Some(error.clone()));
        Err(error)
    }
}

#[cfg(windows)]
struct WindowsAutomationInput {
    behavior: Option<Arc<Mutex<BehaviorRuntimeV2>>>,
    cursor: Mutex<Option<(i32, i32)>>,
    cancel: Arc<AtomicBool>,
    input_state: Arc<InjectedInputState>,
    permit: InputPermit,
}

#[cfg(windows)]
impl WindowsAutomationInput {
    fn new(
        behavior: Option<Arc<Mutex<BehaviorRuntimeV2>>>,
        cancel: Arc<AtomicBool>,
        input_state: Arc<InjectedInputState>,
        controller: Arc<RuntimeController>,
        run_token: RunToken,
    ) -> Self {
        let permit = InputPermit::new(Arc::clone(&input_state), controller, run_token);
        Self {
            behavior,
            cursor: Mutex::new(current_cursor_position()),
            cancel,
            input_state,
            permit,
        }
    }

    fn cancellation_requested(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn tracked_key_down(&self, vk: u32) -> Result<(), String> {
        self.permit
            .send_key_down(vk, Some(self.cancel.as_ref()), || send_key(vk, true))
    }

    fn tracked_key_up(&self, vk: u32) -> Result<(), String> {
        self.permit
            .send_key_up(vk, Some(self.cancel.as_ref()), || send_key(vk, false))
    }

    fn forced_key_up(&self, vk: u32) -> Result<(), String> {
        self.permit.force_key_up(vk, || send_key(vk, false))
    }

    fn tracked_button_down(&self, button: MouseButton) -> Result<(), String> {
        self.permit
            .send_button_down(button, Some(self.cancel.as_ref()), || {
                send_mouse_button(button, KeyAction::Down)
            })
    }

    fn forced_button_up(&self, button: MouseButton) -> Result<(), String> {
        self.permit
            .force_button_up(button, || send_mouse_button(button, KeyAction::Up))
    }

    fn human_pause(&self, kind: DelayKind) {
        let Some(behavior) = &self.behavior else {
            return;
        };
        let delay = behavior
            .lock()
            .map(|mut runtime| runtime.adjust_delay_ms(0, kind))
            .unwrap_or(0);
        if delay > 0 {
            let _ = sleep_interruptible(delay as f32, &self.cancel);
        }
    }

    fn synchronized_cursor_position(&self) -> Option<(i32, i32)> {
        let cached = self.cursor.lock().ok().and_then(|cursor| *cursor);
        let current = current_cursor_position();
        let start = resolve_cursor_start(current, cached);
        if current.is_some() {
            if let Ok(mut cursor) = self.cursor.lock() {
                *cursor = current;
            }
        }
        start
    }

    fn move_to_raw_with_cancel(
        &self,
        x: i32,
        y: i32,
        cancel: Option<&AtomicBool>,
    ) -> Result<(), String> {
        if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            return Err("脚本已被 F12 停止".to_string());
        }
        self.permit
            .perform(format!("mouse_move:{x},{y}"), cancel, || {
                send_mouse_move(x, y)
            })?;
        if let Ok(mut cursor) = self.cursor.lock() {
            *cursor = Some((x, y));
        }
        Ok(())
    }

    fn move_to_with_cancel(
        &self,
        x: i32,
        y: i32,
        cancel: Option<&AtomicBool>,
    ) -> Result<(), String> {
        let start = self.synchronized_cursor_position();
        let Some(behavior) = &self.behavior else {
            if cancel.is_some_and(|cancel| cancel.load(Ordering::SeqCst)) {
                return Err("脚本已被 F12 停止".to_string());
            }
            self.permit
                .perform(format!("mouse_move:{x},{y}"), cancel, || {
                    send_mouse_move(x, y)
                })?;
            if let Ok(mut cursor) = self.cursor.lock() {
                *cursor = Some((x, y));
            }
            return Ok(());
        };
        let points = behavior
            .lock()
            .map_err(|_| "仿生运行时状态异常".to_string())?
            .plan_mouse_move(start.unwrap_or((x, y)), (x, y));
        for point in points {
            if cancel.is_some_and(|cancel| cancel.load(Ordering::SeqCst)) {
                return Err("脚本已被 F12 停止".to_string());
            }
            self.permit.perform(
                format!("mouse_move:{},{}", point.x, point.y),
                cancel,
                || send_mouse_move(point.x, point.y),
            )?;
            if point.delay_ms > 0 {
                if let Some(cancel) = cancel {
                    if !sleep_interruptible(point.delay_ms as f32, cancel) {
                        return Err("脚本已被 F12 停止".to_string());
                    }
                } else {
                    thread::sleep(Duration::from_millis(point.delay_ms));
                }
            }
        }
        if let Ok(mut cursor) = self.cursor.lock() {
            *cursor = Some((x, y));
        }
        Ok(())
    }

    fn execute_pointer_trajectory(
        &self,
        trajectory: crate::behavior::v2::PointerTrajectory,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        self.execute_pointer_points_with_writer(&trajectory.points, cancel, send_mouse_move)?;
        if let Some(last) = trajectory.points.last() {
            if let Ok(mut cursor) = self.cursor.lock() {
                *cursor = Some((last.x, last.y));
            }
        }
        Ok(())
    }

    fn execute_pointer_points_with_writer(
        &self,
        points: &[crate::behavior::v2::PointerTrajectoryPoint],
        cancel: &AtomicBool,
        mut writer: impl FnMut(i32, i32) -> Result<(), String>,
    ) -> Result<(), String> {
        for point in points {
            if cancel.load(Ordering::SeqCst) {
                return Err("脚本已被 F12 停止".to_string());
            }
            self.permit.perform(
                format!("mouse_move:{},{}", point.x, point.y),
                Some(cancel),
                || writer(point.x, point.y),
            )?;
            if point.delay_ms > 0 && !sleep_interruptible(point.delay_ms as f32, cancel) {
                return Err("脚本已被 F12 停止".to_string());
            }
        }
        Ok(())
    }

    fn bio_move_to_with_cancel(
        &self,
        x: i32,
        y: i32,
        target_width: Option<f32>,
        followed_by_click: bool,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        if cancel.load(Ordering::SeqCst) {
            return Err("脚本已被 F12 停止".to_string());
        }
        let Some(behavior) = &self.behavior else {
            return self.move_to_raw_with_cancel(x, y, Some(cancel));
        };
        let start = self.synchronized_cursor_position();
        let Some(start) = start else {
            return self.move_to_raw_with_cancel(x, y, Some(cancel));
        };
        let mut runtime = behavior
            .lock()
            .map_err(|_| "V2 仿生运行时状态异常".to_string())?;
        if !runtime.applies_pointer_behavior() {
            drop(runtime);
            return self.move_to_raw_with_cancel(x, y, Some(cancel));
        }
        let trajectory = runtime
            .pointer_trajectory(start, (x, y), target_width, followed_by_click, Some(cancel))
            .map_err(|error| error.message)?;
        if let Some(diagnostic) = trajectory.diagnostic.as_ref() {
            log::debug!(
                "behavior_v2 pointer action={} seed={} action_seed={} bucket={} trained={} fallback_level={} reason={:?} movement_ms={:?} target_width={:?} time_to_peak={:?} path_efficiency={:?} overshoot_enabled={} correction_enabled={}",
                diagnostic.action_index,
                diagnostic.runtime_seed,
                diagnostic.action_seed,
                diagnostic.bucket,
                diagnostic.trained,
                diagnostic.fallback_level,
                diagnostic.fallback_reason,
                diagnostic.movement_time_ms,
                diagnostic.target_width,
                diagnostic.time_to_peak_ratio,
                diagnostic.path_efficiency,
                diagnostic.overshoot_enabled,
                diagnostic.correction_enabled,
            );
        }
        self.execute_pointer_trajectory(trajectory, cancel)
    }

    fn bio_click_with_cancel(
        &self,
        button: &str,
        x: i32,
        y: i32,
        target_width: Option<f32>,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        let button = parse_mouse_button(button)?;
        let Some(behavior) = &self.behavior else {
            self.move_to_with_cancel(x, y, Some(cancel))?;
            self.tracked_button_down(button)?;
            if !sleep_interruptible(1.0, cancel) {
                return Err("脚本已被 F12 停止".to_string());
            }
            return self.forced_button_up(button);
        };
        let applies_behavior = behavior
            .lock()
            .map_err(|_| "V2 仿生运行时状态异常".to_string())?
            .applies_behavior();
        if !applies_behavior {
            self.move_to_raw_with_cancel(x, y, Some(cancel))?;
            self.tracked_button_down(button)?;
            return self.forced_button_up(button);
        }
        self.bio_move_to_with_cancel(x, y, target_width, true, cancel)?;
        let plan = behavior
            .lock()
            .map_err(|_| "V2 仿生运行时状态异常".to_string())?
            .click_plan(button, true, Some(cancel))
            .map_err(|error| error.message)?;
        if let Some(diagnostic) = plan.diagnostic.as_ref() {
            log::debug!(
                "behavior_v2 click action={} seed={} action_seed={} bucket={} trained={} coverage={} fallback_level={} reason={:?} pre_ms={} hold_ms={} post_ms={}",
                diagnostic.action_index,
                diagnostic.runtime_seed,
                diagnostic.action_seed,
                diagnostic.bucket,
                diagnostic.trained,
                diagnostic.coverage,
                diagnostic.fallback_level,
                diagnostic.fallback_reason,
                plan.pre_click_dwell_ms,
                plan.hold_ms,
                plan.post_click_dwell_ms,
            );
        }
        if !sleep_interruptible(plan.pre_click_dwell_ms as f32, cancel) {
            return Err("脚本已被 F12 停止".to_string());
        }
        self.tracked_button_down(button)?;
        if !sleep_interruptible(plan.hold_ms as f32, cancel) {
            self.forced_button_up(button)?;
            return Err("脚本已被 F12 停止".to_string());
        }
        self.forced_button_up(button)?;
        if !sleep_interruptible(plan.post_click_dwell_ms as f32, cancel) {
            return Err("脚本已被 F12 停止".to_string());
        }
        Ok(())
    }

    fn type_text_with_cancel(&self, text: &str, cancel: Option<&AtomicBool>) -> Result<(), String> {
        if self.behavior.is_none() {
            return self.permit.send_text(text, cancel, send_key);
        }
        for character in text.chars() {
            if cancel.is_some_and(|cancel| cancel.load(Ordering::SeqCst)) {
                return Err("脚本已被 F12 停止".to_string());
            }
            self.permit
                .send_text(&character.to_string(), cancel, send_key)?;
            self.human_pause(DelayKind::KeyInterval);
        }
        Ok(())
    }
}

#[cfg(windows)]
fn release_after_best_effort_move<Move, Release>(
    move_action: Option<Move>,
    release_action: Release,
) -> Result<(), String>
where
    Move: FnOnce() -> Result<(), String>,
    Release: FnOnce() -> Result<(), String>,
{
    let move_error = move_action.and_then(|action| action().err());
    release_action()?;
    if let Some(error) = move_error {
        Err(error)
    } else {
        Ok(())
    }
}

#[cfg(windows)]
impl AutomationInput for WindowsAutomationInput {
    fn wait_ms(&self, milliseconds: u64, speed: f32, cancel: &AtomicBool) -> Result<(), String> {
        let milliseconds =
            self.behavior
                .as_ref()
                .and_then(|behavior| {
                    behavior.lock().ok().map(|mut runtime| {
                        runtime.adjust_delay_ms(milliseconds, DelayKind::General)
                    })
                })
                .unwrap_or(milliseconds);
        if sleep_interruptible(milliseconds as f32 / speed.max(0.05), cancel) {
            Ok(())
        } else {
            Err("脚本已被 F12 停止".to_string())
        }
    }

    fn wait_random_ms(
        &self,
        minimum: u64,
        maximum: u64,
        speed: f32,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        self.wait_ms(randomized_delay_ms(minimum, Some(maximum)), speed, cancel)
    }

    fn key_down(&self, key: &str) -> Result<(), String> {
        let vk = key_to_vk(key).ok_or_else(|| format!("未知按键：{key}"))?;
        self.human_pause(DelayKind::KeyInterval);
        self.tracked_key_down(vk)
    }

    fn key_up(&self, key: &str) -> Result<(), String> {
        let vk = key_to_vk(key).ok_or_else(|| format!("未知按键：{key}"))?;
        self.human_pause(DelayKind::KeyHold);
        self.tracked_key_up(vk)
    }

    fn force_key_up(&self, key: &str) -> Result<(), String> {
        let vk = key_to_vk(key).ok_or_else(|| format!("unknown key: {key}"))?;
        self.forced_key_up(vk)
    }

    fn move_to(&self, x: i32, y: i32) -> Result<(), String> {
        self.move_to_raw_with_cancel(x, y, Some(self.cancel.as_ref()))
    }

    fn click(&self, button: &str, x: i32, y: i32) -> Result<(), String> {
        if x != 0 || y != 0 {
            self.move_to(x, y)?;
        }
        let button = parse_mouse_button(button)?;
        self.tracked_button_down(button)?;
        self.forced_button_up(button)
    }

    fn bio_move_to(
        &self,
        x: i32,
        y: i32,
        target_width: Option<f32>,
        followed_by_click: bool,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        self.bio_move_to_with_cancel(x, y, target_width, followed_by_click, cancel)
    }

    fn bio_click(
        &self,
        button: &str,
        x: i32,
        y: i32,
        target_width: Option<f32>,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        self.bio_click_with_cancel(button, x, y, target_width, cancel)
    }

    fn mouse_down(&self, button: &str, x: i32, y: i32) -> Result<(), String> {
        if x != 0 || y != 0 {
            self.move_to(x, y)?;
        }
        self.human_pause(DelayKind::ClickInterval);
        self.tracked_button_down(parse_mouse_button(button)?)
    }

    fn mouse_up(&self, button: &str, x: i32, y: i32) -> Result<(), String> {
        let button = parse_mouse_button(button)?;
        // A cursor move is best effort here.  Releasing the button must still
        // happen when SetCursorPos is rejected or cancellation races the move.
        release_after_best_effort_move((x != 0 || y != 0).then_some(|| self.move_to(x, y)), || {
            self.human_pause(DelayKind::ClickHold);
            self.forced_button_up(button)
        })
    }

    fn force_mouse_up(&self, button: &str) -> Result<(), String> {
        self.forced_button_up(parse_mouse_button(button)?)
    }

    fn cleanup_injected_input(&self) -> Result<(), String> {
        let report = self.input_state.cleanup(
            |vk| send_key(vk, false),
            |button| send_mouse_button(button, KeyAction::Up),
        );
        if report.is_safe() {
            Ok(())
        } else {
            Err(format!(
                "仍有 {} 个注入输入未释放",
                report.unreleased_count()
            ))
        }
    }

    fn scroll(&self, delta_x: i32, delta_y: i32) -> Result<(), String> {
        if self.cancellation_requested() {
            return Err("脚本已被 F12 停止".to_string());
        }
        self.human_pause(DelayKind::MousePause);
        self.permit.perform(
            format!("mouse_wheel:{delta_x},{delta_y}"),
            Some(&self.cancel),
            || send_mouse_wheel(delta_x, delta_y),
        )
    }

    fn type_text(&self, text: &str) -> Result<(), String> {
        self.type_text_with_cancel(text, Some(self.cancel.as_ref()))
    }
}

#[cfg(windows)]
fn parse_mouse_button(button: &str) -> Result<MouseButton, String> {
    match button {
        "left" => Ok(MouseButton::Left),
        "right" => Ok(MouseButton::Right),
        "middle" => Ok(MouseButton::Middle),
        "x1" => Ok(MouseButton::X1),
        "x2" => Ok(MouseButton::X2),
        _ => Err("鼠标按钮必须是 left、right、middle、x1 或 x2".to_string()),
    }
}

#[cfg(windows)]
fn mouse_button_name(button: MouseButton) -> &'static str {
    match button {
        MouseButton::Left => "left",
        MouseButton::Right => "right",
        MouseButton::Middle => "middle",
        MouseButton::X1 => "x1",
        MouseButton::X2 => "x2",
    }
}

#[cfg(windows)]
fn current_cursor_position() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point).is_ok() }.then_some((point.x, point.y))
}

#[cfg(windows)]
fn resolve_cursor_start(
    current: Option<(i32, i32)>,
    cached: Option<(i32, i32)>,
) -> Option<(i32, i32)> {
    current.or(cached)
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn play_automation_program(
    shared: &Arc<HookShared>,
    macro_rule: &MacroRule,
    speed: f32,
    stop: &Arc<AtomicBool>,
    _held_keys: &mut HashSet<u32>,
    _held_buttons: &mut HashSet<MouseButton>,
    behavior: Option<Arc<Mutex<BehaviorRuntimeV2>>>,
    input_state: Arc<InjectedInputState>,
    _emergency_generation: u64,
    instance_id: u64,
    run_token: RunToken,
) -> Result<AutomationProgramResult, String> {
    let behavior_enabled = behavior.is_some();
    let initial_held_buttons = input_state.buttons_snapshot()?;
    let input: Arc<dyn AutomationInput> = Arc::new(WindowsAutomationInput::new(
        behavior,
        Arc::clone(stop),
        input_state,
        Arc::clone(&shared.controller),
        run_token,
    ));
    let vision = shared
        .vision
        .lock()
        .map_err(|_| "视觉服务状态异常")?
        .clone();
    let assets = shared
        .config
        .lock()
        .map_err(|_| "配置状态异常")?
        .assets
        .clone();
    let bootstrap = crate::runtime_executor::ExecutorBootstrap {
        version: crate::runtime_protocol::PROTOCOL_VERSION,
        identity: crate::runtime_protocol::RunIdentity {
            session: format!("{}", std::process::id()),
            run: run_token.id,
            generation: run_token.generation,
        },
        secret: crate::runtime_executor::session_secret()?,
        program: macro_rule.program.clone(),
        speed,
        behavior_enabled,
        initial_held_buttons,
        image_root: vision.asset_root(),
        assets,
    };
    let executable =
        std::env::current_exe().map_err(|error| format!("执行器路径不可用: {error}"))?;
    let progress = |step: usize, action: String| {
        if let AutomationProgram::Macro { steps } = &macro_rule.program {
            if let Some(original) = steps.get(step.saturating_sub(1)) {
                shared.set_playback_action(
                    instance_id,
                    step,
                    macro_step_kind(original),
                    Some(macro_step_summary(original)),
                );
            }
        } else {
            shared.set_playback_action(
                instance_id,
                step,
                "rhai",
                Some(format!("Rhai API：{action}")),
            );
        }
    };
    let revoke = |cause| {
        if cause == crate::runtime_executor::ExecutorStopCause::ContainmentUnconfirmed {
            shared
                .executor_containment_unknown
                .store(true, Ordering::Release);
            shared.controller.lock_fault();
            shared.record_safety(
                "executor_containment_unconfirmed",
                &[("status", "restart_required_no_cleanup_override".into())],
            );
        }
        shared.stop_playback();
    };
    let result = crate::runtime_executor::run_parent_supervised(
        &executable,
        bootstrap,
        input,
        stop,
        &progress,
        &revoke,
    );
    match result {
        Ok(Some((title, message))) => {
            Ok(AutomationProgramResult::StoppedWithMessage { title, message })
        }
        Ok(None) => Ok(AutomationProgramResult::Continue),
        Err(error) if error == CANCELLED => Ok(AutomationProgramResult::Stopped),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn macro_step_kind(step: &MacroStep) -> &'static str {
    match step {
        MacroStep::Delay { .. } => "delay",
        MacroStep::Key { action, .. } => match action {
            KeyAction::Down => "key_down",
            KeyAction::Up => "key_up",
        },
        MacroStep::MouseMove { .. } => "mouse_move",
        MacroStep::MouseButton { action, .. } => match action {
            KeyAction::Down => "mouse_button_down",
            KeyAction::Up => "mouse_button_up",
        },
        MacroStep::Wheel { .. } => "wheel",
        MacroStep::Text { .. } => "text",
    }
}

#[cfg(windows)]
fn macro_step_summary(step: &MacroStep) -> String {
    match step {
        MacroStep::Delay {
            duration_ms,
            duration_max_ms,
        } => match duration_max_ms {
            Some(maximum) => format!("等待 {duration_ms}–{maximum} ms"),
            None => format!("等待 {duration_ms} ms"),
        },
        MacroStep::Key { key, action } => match action {
            KeyAction::Down => format!("按下 {key}"),
            KeyAction::Up => format!("释放 {key}"),
        },
        MacroStep::MouseButton { button, action, .. } => match action {
            KeyAction::Down => format!("鼠标{}按下", mouse_button_name(*button)),
            KeyAction::Up => format!("鼠标{}释放", mouse_button_name(*button)),
        },
        MacroStep::MouseMove { x, y } => format!("移动鼠标至 ({x}, {y})"),
        MacroStep::Wheel { delta_x, delta_y } => format!("滚轮 ({delta_x}, {delta_y})"),
        MacroStep::Text { .. } => "输入文本".to_string(),
    }
}

#[cfg(windows)]
fn randomized_delay_ms(minimum: u64, maximum: Option<u64>) -> u64 {
    let Some(maximum) = maximum else {
        return minimum;
    };
    if maximum <= minimum {
        return minimum;
    }

    // A small non-cryptographic sample is enough here: it only varies macro
    // timing, and retaining no state makes simultaneous macro runs independent.
    let clock = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mut value = clock ^ (minimum.rotate_left(17)) ^ maximum.rotate_right(9);
    value ^= value << 13;
    value ^= value >> 7;
    value ^= value << 17;
    let span = u128::from(maximum) - u128::from(minimum) + 1;
    minimum.saturating_add((u128::from(value) % span) as u64)
}

#[cfg(windows)]
fn sleep_interruptible(duration_ms: f32, stop: &AtomicBool) -> bool {
    let duration = Duration::from_secs_f32((duration_ms.max(0.0)) / 1000.0);
    let started = Instant::now();
    while started.elapsed() < duration {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        thread::sleep(Duration::from_millis(8).min(duration.saturating_sub(started.elapsed())));
    }
    true
}

#[cfg(windows)]
fn send_key(vk: u32, key_down: bool) -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_TYPE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
        VIRTUAL_KEY,
    };
    let mut flags = if key_down {
        Default::default()
    } else {
        KEYEVENTF_KEYUP
    };
    let unicode = vk & crate::input_safety::UNICODE_INPUT_TAG != 0;
    if unicode {
        flags |= KEYEVENTF_UNICODE;
    }
    let input = INPUT {
        r#type: INPUT_TYPE(1),
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(if unicode { 0 } else { vk as u16 }),
                wScan: if unicode { vk as u16 } else { 0 },
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
    if sent != 1 {
        return Err(
            "Windows 拒绝了键盘输入。若目标软件以管理员身份运行，请也以管理员身份启动 AutoFlow。"
                .to_string(),
        );
    }
    Ok(())
}

#[cfg(windows)]
fn send_mouse_button(button: MouseButton, action: KeyAction) -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_TYPE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
        MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
    };
    let is_down = matches!(action, KeyAction::Down);
    let (flags, data) = match (button, is_down) {
        (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
        (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (MouseButton::X1, true) => (MOUSEEVENTF_XDOWN, 1),
        (MouseButton::X1, false) => (MOUSEEVENTF_XUP, 1),
        (MouseButton::X2, true) => (MOUSEEVENTF_XDOWN, 2),
        (MouseButton::X2, false) => (MOUSEEVENTF_XUP, 2),
    };
    let input = INPUT {
        r#type: INPUT_TYPE(0),
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
    if sent != 1 {
        return Err(
            "Windows 拒绝了鼠标输入。若目标软件以管理员身份运行，请也以管理员身份启动 AutoFlow。"
                .to_string(),
        );
    }
    Ok(())
}

#[cfg(windows)]
fn send_mouse_move(x: i32, y: i32) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
    unsafe { SetCursorPos(x, y) }.map_err(|_| "Windows 拒绝了鼠标移动输入。".to_string())
}

#[cfg(windows)]
fn send_mouse_wheel(delta_x: i32, delta_y: i32) -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_TYPE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_WHEEL, MOUSEINPUT,
    };
    let (flags, data) = if delta_x != 0 {
        (MOUSEEVENTF_HWHEEL, delta_x as u32)
    } else {
        (MOUSEEVENTF_WHEEL, delta_y as u32)
    };
    let input = INPUT {
        r#type: INPUT_TYPE(0),
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
    if sent != 1 {
        return Err(
            "Windows 拒绝了滚轮输入。若目标软件以管理员身份运行，请也以管理员身份启动 AutoFlow。"
                .to_string(),
        );
    }
    Ok(())
}

#[cfg(windows)]
fn launch_target(target: &str, permitted: impl FnMut() -> bool) {
    visit_launch_commands(target, permitted, |program, args| {
        if let Err(error) = Command::new(program).args(args).spawn() {
            log::error!("启动快捷动作失败: {error}");
        }
    });
}

#[cfg(windows)]
fn visit_launch_commands(
    target: &str,
    mut permitted: impl FnMut() -> bool,
    mut launch: impl FnMut(&str, &[String]),
) {
    for command_line in target
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if !permitted() {
            break;
        }
        let parts = split_command_line(command_line);
        let Some((program, args)) = parts.split_first() else {
            continue;
        };
        launch(program, args);
    }
}

#[cfg(windows)]
fn split_command_line(command_line: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in command_line.chars() {
        match character {
            '"' => quoted = !quoted,
            character if character.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            character => current.push(character),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

#[cfg(all(test, windows))]
mod tests {
    use super::{
        canonical_virtual_key, clear_released_macro_trigger_state,
        discard_recording_shortcut_steps, is_keyboard_modifier, is_recording_shortcut_key,
        is_shift_key, is_text_modifier, latched_signature_contains_vk, native_hotkey_spec,
        push_record_step_at, randomized_delay_ms, release_after_best_effort_move,
        resolve_cursor_start, shifted_printable_character, should_show_playback_error,
        split_command_line, HookService, HookShared, PlaybackState, RecorderState,
    };
    use crate::MouseButton;
    use crate::{AppConfig, AutomationProgram, KeyAction, MacroMode, MacroRule, MacroStep};
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn emergency_heartbeat_requires_a_recent_monotonic_observation() {
        assert!(!super::detector_heartbeat_fresh(1000, 0));
        assert!(super::detector_heartbeat_fresh(1000, 900));
        assert!(!super::detector_heartbeat_fresh(1000, 899));
        assert!(!super::detector_heartbeat_fresh(1000, 1001));
    }

    #[test]
    fn late_biomimetic_trajectory_cannot_write_after_permit_revocation() {
        let controller = crate::runtime_control::RuntimeController::new();
        let mut lease = controller.begin_start(None).expect("admit playback");
        let token = lease.token();
        assert!(controller.activate(token));
        lease.commit();

        let cancel = Arc::new(AtomicBool::new(false));
        let input_state = Arc::new(crate::input_safety::InjectedInputState::default());
        let input = super::WindowsAutomationInput {
            behavior: None,
            cursor: std::sync::Mutex::new(None),
            cancel: Arc::clone(&cancel),
            input_state: Arc::clone(&input_state),
            permit: crate::input_safety::InputPermit::new(
                input_state,
                Arc::clone(&controller),
                token,
            ),
        };
        let planned = [crate::behavior::v2::PointerTrajectoryPoint {
            x: 640,
            y: 480,
            delay_ms: 0,
        }];

        // Model a planner returning after F12 changed the controller
        // generation. The trajectory executor must recheck the permit before
        // invoking its platform writer.
        controller.request_stop();
        let writes = std::sync::atomic::AtomicUsize::new(0);
        assert!(input
            .execute_pointer_points_with_writer(&planned, cancel.as_ref(), |_, _| {
                writes.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .is_err());
        assert_eq!(writes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn explicit_recovery_cannot_bypass_unavailable_emergency_channel() {
        let hook = HookService::isolated(
            AppConfig::default(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        hook.shared.controller.lock_fault();
        let error = hook
            .recover_input_safety_at(hook.service_generation(), hook.service_admission_revision())
            .expect_err("channel is deliberately unavailable");
        assert_eq!(error.code, "safety_recovery_not_ready");
        assert_eq!(
            hook.shared.controller.phase(),
            crate::runtime_control::RuntimePhase::FaultLocked
        );
        assert!(!hook.shared.controller.background_input_allowed());
    }

    #[test]
    fn cleanup_registry_contention_is_unknown_not_an_empty_snapshot() {
        let hook = HookService::isolated(
            AppConfig::default(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let held = hook.shared.playback_inputs.lock().expect("test registry");
        assert!(hook.shared.playback_input_states().is_err());
        assert_eq!(hook.shared.tracked_input_counts(), (usize::MAX, usize::MAX));
        drop(held);
        assert!(hook
            .shared
            .playback_input_states()
            .expect("known snapshot")
            .is_empty());
    }

    #[test]
    fn a_created_detector_thread_without_heartbeat_is_not_ready() {
        let service = test_hook_service();
        assert!(service.shared.emergency_stop_ready());
        service
            .shared
            .emergency_heartbeat_ms
            .store(0, Ordering::Release);
        assert!(!service.shared.emergency_stop_ready());
    }

    fn test_hook_service() -> HookService {
        let shared = Arc::new(HookShared::new(
            AppConfig::default(),
            crate::automation::VisionService::new(
                std::env::temp_dir()
                    .join("AutoFlow")
                    .join("data")
                    .join("images"),
            ),
        ));
        shared
            .start_behavior_capture_worker()
            .expect("input-free capture worker");
        shared
            .start_graph_capture_worker()
            .expect("input-free graph worker");
        // Unit tests inject the health of the emergency detector instead of
        // pretending that a real Windows polling thread is running.
        shared
            .emergency_detector_ready
            .store(true, Ordering::Release);
        shared.emergency_thread_id.store(1, Ordering::Release);
        shared.emergency_heartbeat_ms.store(
            shared.emergency_clock.elapsed().as_millis() as u64 + 1,
            Ordering::Release,
        );
        HookService { shared }
    }

    #[test]
    fn remap_reservation_preserves_down_up_order_and_full_release_queue_requests_stop() {
        use std::time::Duration;
        let service = HookService::isolated(
            AppConfig::default(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let generation = service.shared.controller.background_generation();
        assert!(!service.shared.submit_remap_down(0x41, 0x42, generation));
        assert!(service.shared.active_remaps.lock().expect("map").is_empty());
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let (task_tx, task_rx) = std::sync::mpsc::sync_channel(2);
        let mut first = true;
        let worker = crate::bounded_worker::BoundedWorker::spawn(
            "input-free-remap-fixture",
            1,
            move |task: super::RemapTask| {
                task_tx
                    .send((task.source, task.target, task.down))
                    .expect("task");
                if first {
                    first = false;
                    entered_tx.send(()).expect("entered");
                    release_rx
                        .recv_timeout(Duration::from_secs(2))
                        .expect("release");
                }
            },
        )
        .expect("worker");
        assert!(service.shared.remap_tasks.set(worker).is_ok());
        assert!(service.shared.submit_remap_down(0x41, 0x42, generation));
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("down handling");
        assert_eq!(
            service.shared.active_remaps.lock().expect("map").get(&0x41),
            Some(&0x42)
        );
        assert!(service.shared.submit_remap_up(0x41));
        assert!(!service.shared.submit_remap_down(0x43, 0x44, generation));
        assert!(!service
            .shared
            .active_remaps
            .lock()
            .expect("map")
            .contains_key(&0x43));
        let before_stop = service.shared.controller.generation();
        assert!(service.shared.submit_remap_up(0x41)); // full queue: independent stop
        assert!(service.shared.controller.generation() > before_stop);
        release_tx.send(()).expect("release fixture");
        assert_eq!(
            task_rx.recv_timeout(Duration::from_secs(1)).expect("down"),
            (0x41, 0x42, true)
        );
        assert_eq!(
            task_rx.recv_timeout(Duration::from_secs(1)).expect("up"),
            (0x41, 0x42, false)
        );
        assert_eq!(service.shared.injected_input.counts(), (0, 0));
    }

    #[test]
    fn idle_controller_cannot_admit_work_until_previous_thread_lease_is_released() {
        let service = test_hook_service();
        service
            .shared
            .playback_thread_owner
            .store(17, Ordering::Release);
        let lease = super::PlaybackThreadLease {
            shared: service.shared.clone(),
            instance_id: 17,
        };
        assert!(service.shared.controller.is_quiescent());
        assert!(matches!(
            service.shared.controller.phase(),
            crate::runtime_control::RuntimePhase::Idle
        ));
        assert_eq!(
            service
                .start_recording(true, true)
                .expect_err("thread still alive")
                .code,
            "macro_busy"
        );
        assert_eq!(
            service
                .start_behavior_recording("fixture".into())
                .expect_err("thread still alive")
                .code,
            "macro_busy"
        );
        service.shared.controller.lock_fault();
        assert_eq!(
            service
                .recover_input_safety_at(
                    service.service_generation(),
                    service.service_admission_revision(),
                )
                .expect_err("thread still alive")
                .code,
            "safety_recovery_not_ready"
        );
        drop(lease);
        assert_eq!(
            service.shared.playback_thread_owner.load(Ordering::Acquire),
            0
        );
    }

    #[test]
    fn stale_thread_lease_cannot_clear_current_thread_ownership() {
        let service = test_hook_service();
        service
            .shared
            .playback_thread_owner
            .store(23, Ordering::Release);
        drop(super::PlaybackThreadLease {
            shared: service.shared.clone(),
            instance_id: 22,
        });
        assert_eq!(
            service.shared.playback_thread_owner.load(Ordering::Acquire),
            23
        );
        drop(super::PlaybackThreadLease {
            shared: service.shared.clone(),
            instance_id: 23,
        });
        assert_eq!(
            service.shared.playback_thread_owner.load(Ordering::Acquire),
            0
        );
    }

    #[test]
    fn safe_input_cleanup_cannot_clear_unconfirmed_executor_containment() {
        let service = test_hook_service();
        service
            .shared
            .executor_containment_unknown
            .store(true, Ordering::Release);
        service
            .shared
            .input_recovery_required
            .store(true, Ordering::Release);
        service.shared.controller.lock_fault();
        super::record_cleanup_report(
            &service.shared,
            &crate::input_safety::CleanupReport::default(),
            "input-free-fixture",
        );
        assert_eq!(service.shared.tracked_input_counts(), (0, 0));
        assert!(!service
            .shared
            .input_recovery_required
            .load(Ordering::Acquire));
        assert!(service
            .shared
            .executor_containment_unknown
            .load(Ordering::Acquire));
        assert_eq!(
            service
                .recover_input_safety_at(
                    service.service_generation(),
                    service.service_admission_revision(),
                )
                .expect_err("containment is separate from cleanup")
                .code,
            "executor_containment_unconfirmed"
        );
        assert_eq!(
            service.shared.controller.phase(),
            crate::runtime_control::RuntimePhase::FaultLocked
        );
        assert!(!service.shared.controller.background_input_allowed());
    }

    #[test]
    fn recording_refuses_pending_remap_and_invalidates_old_background_work() {
        let service = test_hook_service();
        let revision = service.shared.controller.background_generation();
        assert!(service.shared.background_input_allowed_at(revision));
        service
            .shared
            .active_remaps
            .lock()
            .expect("reservation")
            .insert(0x41, 0x42);
        assert_eq!(
            service
                .start_recording(true, true)
                .expect_err("pending remap")
                .code,
            "recording_input_busy"
        );
        assert_eq!(
            service
                .start_behavior_recording("test".into())
                .expect_err("pending remap")
                .code,
            "recording_input_busy"
        );
        service
            .shared
            .active_remaps
            .lock()
            .expect("reservation")
            .clear();
        service
            .start_recording(true, true)
            .expect("quiescent recording");
        let current = service.shared.controller.background_generation();
        assert_ne!(revision, current);
        assert!(!service.shared.background_input_allowed_at(current));
        service.shared.finish_recording();
        assert!(!service.shared.background_input_allowed_at(revision));
        assert!(service.shared.background_input_allowed_at(current));
    }

    #[test]
    fn background_injection_is_denied_during_mode_admission_or_unknown_recording_state() {
        let service = test_hook_service();
        let revision = service.shared.controller.background_generation();
        let admission = service.shared.mode_admission.lock().expect("admission");
        assert!(!service.shared.background_input_allowed_at(revision));
        drop(admission);
        let recorder = service.shared.recorder.lock().expect("recorder");
        assert!(!service.shared.background_input_allowed_at(revision));
        drop(recorder);
        let behavior = service.shared.behavior.lock().expect("behavior");
        assert!(!service.shared.background_input_allowed_at(revision));
        drop(behavior);
        assert!(service.shared.background_input_allowed_at(revision));
    }

    #[test]
    fn unavailable_hook_state_invalidates_capture_without_waiting_for_business_locks() {
        let service = test_hook_service();
        service.start_recording(true, true).expect("capture");
        let config = service.shared.config.lock().expect("hold config");
        let pressed = service.shared.pressed.lock().expect("hold pressed");
        service
            .shared
            .reject_hook_event("configuration_unavailable", false);
        service
            .shared
            .reject_hook_event("pressed_state_unavailable", false);
        assert!(service
            .shared
            .trigger_rearm_required
            .load(Ordering::Acquire));
        assert!(service.shared.graph_capture_error.load(Ordering::Acquire));
        assert_eq!(
            service
                .shared
                .graph_capture
                .get()
                .expect("queue")
                .statistics()
                .dropped,
            2
        );
        drop(pressed);
        drop(config);
        assert_eq!(
            service.stop_recording(false).expect_err("incomplete").code,
            "capture_incomplete"
        );
        assert_eq!(
            service
                .stop_recording(false)
                .expect_err("still incomplete")
                .code,
            "capture_incomplete"
        );
    }

    #[test]
    fn rejected_physical_release_requests_priority_stop_without_business_locks() {
        let service = test_hook_service();
        service
            .shared
            .emergency_thread_id
            .store(0, Ordering::Release);
        let config = service.shared.config.lock().expect("hold config");
        let pressed = service.shared.pressed.lock().expect("hold pressed");
        let generation = service.shared.controller.generation();
        service
            .shared
            .reject_hook_event("pressed_state_unavailable", true);
        assert!(service.shared.controller.generation() > generation);
        assert_eq!(
            service
                .shared
                .emergency_request_sequence
                .load(Ordering::Acquire),
            1
        );
        assert!(service
            .shared
            .trigger_rearm_required
            .load(Ordering::Acquire));
        drop(pressed);
        drop(config);
    }

    #[test]
    fn native_unregister_failures_remain_tracked_and_retry_only_remaining_ids() {
        let mut registered =
            std::collections::HashMap::from([(0x5000, "one".into()), (0x5001, "two".into())]);
        let mut calls = Vec::new();
        assert!(!super::retire_native_registration_map(
            &mut registered,
            |id| {
                calls.push(id);
                id == 0x5001
            }
        ));
        calls.sort_unstable();
        assert_eq!(calls, [0x5000, 0x5001]);
        assert_eq!(registered.len(), 1);
        assert_eq!(registered.get(&0x5000).map(String::as_str), Some("one"));
        calls.clear();
        assert!(super::retire_native_registration_map(
            &mut registered,
            |id| {
                calls.push(id);
                true
            }
        ));
        assert_eq!(calls, [0x5000]);
        assert!(registered.is_empty());
    }

    #[test]
    fn native_teardown_does_not_wait_or_claim_success_with_unavailable_registry() {
        let service = test_hook_service();
        let held = service
            .shared
            .native_macro_hotkeys
            .lock()
            .expect("hold registry");
        assert!(!super::unregister_native_macro_hotkeys_with(
            &service.shared,
            |_| panic!("unavailable registry must not issue OS calls")
        ));
        drop(held);
        assert!(super::unregister_native_macro_hotkeys_with(
            &service.shared,
            |_| panic!("empty registry must not issue OS calls")
        ));
    }

    #[test]
    fn idle_controller_and_released_frame_do_not_bypass_a_live_playback_handle() {
        let service = test_hook_service();
        let generation = service.shared.controller.background_generation();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let handle = std::thread::spawn(move || {
            let _ = release_rx.recv_timeout(std::time::Duration::from_secs(2));
        });
        *service
            .shared
            .playback_thread_handle
            .lock()
            .expect("registry") = Some((37, handle));
        assert_eq!(
            service.shared.playback_thread_owner.load(Ordering::Acquire),
            0
        );
        assert_eq!(
            service.shared.controller.phase(),
            crate::runtime_control::RuntimePhase::Idle
        );
        assert!(!service.shared.playback_thread_quiescent());
        assert!(!service.shared.background_input_allowed_at(generation));
        assert_eq!(
            service
                .start_recording(true, true)
                .expect_err("old thread live")
                .code,
            "macro_busy"
        );
        assert_eq!(
            service
                .recover_input_safety_at(
                    service.service_generation(),
                    service.service_admission_revision(),
                )
                .expect_err("recovery cannot bypass live thread")
                .code,
            "safety_recovery_not_ready"
        );
        release_tx.send(()).expect("release input-free fixture");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !service.shared.playback_thread_quiescent() {
            assert!(std::time::Instant::now() < deadline, "fixture did not exit");
            std::thread::yield_now();
        }
        assert!(service.shared.background_input_allowed_at(generation));
        service
            .start_recording(true, true)
            .expect("next mode only after actual exit");
        service.stop_recording(false).expect("empty mock recording");
    }

    #[test]
    fn unavailable_playback_handle_registry_cannot_prove_quiescence() {
        let service = test_hook_service();
        let held = service
            .shared
            .playback_thread_handle
            .lock()
            .expect("hold registry");
        assert!(!service.shared.playback_thread_quiescent());
        assert_eq!(
            service
                .start_recording(true, true)
                .expect_err("unavailable registry")
                .code,
            "macro_busy"
        );
        drop(held);
        assert!(service.shared.playback_thread_quiescent());
    }

    #[test]
    fn failed_release_report_latches_fault_even_after_a_later_empty_cleanup() {
        let service = test_hook_service();
        let shared = &service.shared;
        let report = crate::input_safety::CleanupReport {
            released_keys: 0,
            released_buttons: 0,
            failures: vec![crate::input_safety::CleanupFailure {
                kind: "mouse_button",
                value: "mock_left".into(),
                attempts: 3,
                last_error: "mock release refused".into(),
            }],
        };
        super::record_cleanup_report(shared, &report, "mock_priority_release");
        assert_eq!(
            shared.controller.phase(),
            crate::runtime_control::RuntimePhase::FaultLocked
        );
        assert!(shared.input_recovery_required.load(Ordering::Acquire));
        super::record_cleanup_report(
            shared,
            &crate::input_safety::CleanupReport::default(),
            "mock_late_success",
        );
        assert_eq!(
            shared.controller.phase(),
            crate::runtime_control::RuntimePhase::FaultLocked
        );
        assert!(!shared.background_input_allowed_at(shared.controller.background_generation()));
    }

    #[test]
    fn priority_cleanup_panic_fault_locks_hook_service_without_false_completion() {
        let service = test_hook_service();
        service
            .shared
            .start_priority_cleanup_worker_with(|_, _| panic!("mock priority cleanup failure"))
            .expect("mock lane");
        service.emergency_stop();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !service
            .shared
            .input_recovery_required
            .load(Ordering::Acquire)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "panic fault publication"
            );
            std::thread::yield_now();
        }
        assert_eq!(
            service.shared.controller.phase(),
            crate::runtime_control::RuntimePhase::FaultLocked
        );
        assert!(!service.shared.emergency_cleanup_quiescent());
        assert_eq!(
            service
                .shared
                .emergency_cleanup_completed_sequence
                .load(Ordering::Acquire),
            0
        );
        assert!(service.shared.acquire_mode_admission().is_err());
        assert!(!service
            .shared
            .wait_for_shutdown_cleanup(std::time::Duration::ZERO));
    }

    #[test]
    fn prestarted_priority_stop_bypasses_business_locks_and_never_spawns_legacy_cleanup() {
        let service = test_hook_service();
        let shared = &service.shared;
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        shared
            .start_priority_cleanup_worker_with(move |_, sequence| {
                if sequence == 1 {
                    entered_tx
                        .send(std::thread::current().id())
                        .expect("mock cleanup");
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .expect("release");
                } else {
                    entered_tx
                        .send(std::thread::current().id())
                        .expect("coalesced cleanup");
                }
            })
            .expect("prestarted mock lane");
        let config = shared.config.lock().expect("hold config");
        let pressed = shared.pressed.lock().expect("hold pressed");
        let registry = shared
            .emergency_cleanup_thread
            .lock()
            .expect("hold legacy registry");
        let generation = shared.controller.generation();
        shared.request_emergency_stop(super::EmergencyEntryPoint::LowLevelHook, false);
        let first = entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("priority dispatch");
        shared.request_emergency_stop(super::EmergencyEntryPoint::LowLevelHook, false);
        assert!(shared.controller.generation() > generation);
        assert!(registry.is_none(), "no fallback thread created");
        assert_eq!(shared.emergency_request_sequence.load(Ordering::Acquire), 2);
        assert!(!shared.emergency_cleanup_quiescent());
        drop(registry);
        drop(pressed);
        drop(config);
        release_tx.send(()).expect("release first pass");
        assert_eq!(
            entered_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("next pass"),
            first
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !shared.emergency_cleanup_quiescent() {
            assert!(
                std::time::Instant::now() < deadline,
                "priority idle confirmation"
            );
            std::thread::yield_now();
        }
        assert!(
            shared.priority_cleanup.get().expect("worker").is_ready(),
            "idle lane stays alive"
        );
        assert!(
            service.shutdown(),
            "shutdown confirms actual priority thread exit"
        );
        assert!(shared
            .priority_cleanup
            .get()
            .expect("worker")
            .is_quiescent());
    }

    #[test]
    fn closing_priority_lane_fences_late_shutdown_requests_without_replay() {
        let service = test_hook_service();
        service
            .shared
            .start_priority_cleanup_worker_with(|_, _| {})
            .expect("mock lane");
        assert!(service.shutdown());
        let sequence = service
            .shared
            .emergency_request_sequence
            .load(Ordering::Acquire);
        service.emergency_stop();
        assert_eq!(
            service
                .shared
                .emergency_request_sequence
                .load(Ordering::Acquire),
            sequence
        );
        assert!(service.shared.emergency_cleanup_quiescent());
        assert!(
            service.shutdown(),
            "idempotent close cannot enqueue post-exit work"
        );
    }

    #[test]
    fn emergency_config_refresh_cannot_promote_unacknowledged_startup_health() {
        let service = test_hook_service();
        let shared = &service.shared;
        shared
            .emergency_detector_ready
            .store(false, Ordering::Release);
        super::refresh_emergency_detector(shared);
        assert!(!shared.emergency_detector_ready.load(Ordering::Acquire));
        assert_eq!(
            shared.controller.phase(),
            crate::runtime_control::RuntimePhase::Idle
        );
        assert_eq!(shared.emergency_request_sequence.load(Ordering::Acquire), 0);
    }

    #[test]
    fn hook_health_probe_round_trip_uses_private_windows_thread_messages_without_input() {
        use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, MSG, PM_NOREMOVE, PM_REMOVE};
        let service = test_hook_service();
        let shared = &service.shared;
        let worker_shared = Arc::clone(shared);
        let (id_tx, id_rx) = std::sync::mpsc::sync_channel(1);
        let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let mut message = MSG::default();
            unsafe {
                let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
            }
            id_tx
                .send(unsafe { windows::Win32::System::Threading::GetCurrentThreadId() })
                .expect("queue exists");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                if unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() } {
                    assert_eq!(message.message, super::HOOK_HEALTH_PROBE_MESSAGE);
                    worker_shared.acknowledge_hook_probe();
                    ack_tx.send(()).expect("acknowledged private probe");
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "private message timeout"
                );
                std::thread::yield_now();
            }
            let _ = release_rx.recv_timeout(std::time::Duration::from_secs(2));
        });
        shared
            .hook_thread_handle
            .set(worker)
            .expect("ordinary private message thread");
        let id = id_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("private queue ID");
        shared.thread_id.store(id, Ordering::Release);
        shared.hook_ready.store(true, Ordering::Release);
        shared.post_hook_probe();
        ack_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("actual Windows message acknowledged");
        assert!(shared.hook_heartbeat_ms.load(Ordering::Acquire) > 0);
        assert!(!shared.hook_probe_pending.load(Ordering::Acquire));
        assert!(crate::bounded_worker::thread_running_confirmed(
            shared.hook_thread_handle.get().expect("retained fixture")
        ));
        shared.thread_id.store(0, Ordering::Release);
        release_tx.send(()).expect("release fixture, no OS input");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !shared.system_threads_quiescent() {
            assert!(
                std::time::Instant::now() < deadline,
                "private message thread exit"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn hook_health_probe_is_coalesced_and_failed_posts_cannot_renew_health() {
        let service = test_hook_service();
        let shared = &service.shared;
        shared.thread_id.store(123, Ordering::Release);
        shared.hook_ready.store(true, Ordering::Release);
        let config = shared.config.lock().expect("hold business config");
        let pressed = shared.pressed.lock().expect("hold business pressed state");
        shared.post_hook_probe_with(|id| {
            assert_eq!(id, 123);
            true
        });
        assert!(shared.hook_probe_pending.load(Ordering::Acquire));
        for _ in 0..1000 {
            shared.post_hook_probe_with(|_| panic!("one outstanding probe only"));
        }
        assert_eq!(shared.hook_heartbeat_ms.load(Ordering::Acquire), 0);
        shared.acknowledge_hook_probe();
        assert!(!shared.hook_probe_pending.load(Ordering::Acquire));
        let acknowledged = shared.hook_heartbeat_ms.load(Ordering::Acquire);
        assert!(acknowledged > 0);
        shared.post_hook_probe_with(|_| false);
        assert!(!shared.hook_probe_pending.load(Ordering::Acquire));
        assert_eq!(
            shared.hook_heartbeat_ms.load(Ordering::Acquire),
            acknowledged
        );
        drop(pressed);
        drop(config);
        // The fake ID was used only with injected send closures, never Win32.
        shared.thread_id.store(0, Ordering::Release);
    }

    #[test]
    fn stalled_hook_health_revokes_permission_without_business_locks_or_auto_recovery() {
        let service = test_hook_service();
        let shared = &service.shared;
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        shared
            .hook_thread_handle
            .set(std::thread::spawn(move || {
                let _ = release_rx.recv_timeout(std::time::Duration::from_secs(2));
            }))
            .expect("ordinary blocked thread, not native hook");
        shared.hook_ready.store(true, Ordering::Release);
        shared.acknowledge_hook_probe();
        shared.observe_control_channel_health(true);
        assert!(shared.emergency_detector_ready.load(Ordering::Acquire));
        let generation = shared.controller.generation();
        let config = shared.config.lock().expect("hold config");
        let pressed = shared.pressed.lock().expect("hold pressed");
        // Missing/expired acknowledgment, despite a running OS thread.
        shared.hook_heartbeat_ms.store(0, Ordering::Release);
        shared.observe_control_channel_health(true);
        assert!(!shared.emergency_detector_ready.load(Ordering::Acquire));
        assert!(shared.controller.generation() > generation);
        assert_eq!(
            shared.controller.phase(),
            crate::runtime_control::RuntimePhase::FaultLocked
        );
        let stopped_generation = shared.controller.generation();
        shared.observe_control_channel_health(true);
        assert_eq!(
            shared.controller.generation(),
            stopped_generation,
            "failure transition is coalesced"
        );
        shared.acknowledge_hook_probe();
        shared.observe_control_channel_health(true);
        assert_eq!(
            shared.controller.phase(),
            crate::runtime_control::RuntimePhase::FaultLocked,
            "late pong cannot recover or replay"
        );
        drop(pressed);
        drop(config);
        release_tx.send(()).expect("release private thread");
    }

    #[test]
    fn retained_hook_thread_requires_initialized_channel_and_actual_liveness() {
        let service = test_hook_service();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        service
            .shared
            .hook_thread_handle
            .set(std::thread::spawn(move || {
                let _ = release_rx.recv_timeout(std::time::Duration::from_secs(2));
            }))
            .expect("private ordinary thread");
        assert!(
            !service.shared.hook_channel_ready(),
            "thread spawn alone is not readiness"
        );
        assert!(!service.shared.emergency_stop_ready());
        service.shared.hook_ready.store(true, Ordering::Release);
        assert!(
            !service.shared.hook_channel_ready(),
            "no message acknowledgment yet"
        );
        service.shared.acknowledge_hook_probe();
        assert!(service.shared.hook_channel_ready());
        assert!(service.shared.emergency_stop_ready());
        release_tx.send(()).expect("release fixture");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !service.shared.system_threads_quiescent() {
            assert!(std::time::Instant::now() < deadline, "fixture exit");
            std::thread::yield_now();
        }
        assert!(
            !service.shared.hook_channel_ready(),
            "stale ready bit cannot authorize dead hook"
        );
        assert!(!service.shared.emergency_stop_ready());
    }

    #[test]
    fn initialization_blocks_all_mode_and_background_admission() {
        let service = test_hook_service();
        let shared = &service.shared;
        shared.initializing.store(true, Ordering::Release);
        assert_eq!(
            shared
                .acquire_mode_admission()
                .expect_err("initializing")
                .code,
            "runtime_initializing"
        );
        assert_eq!(
            service
                .start_recording(true, true)
                .expect_err("capture initializing")
                .code,
            "runtime_initializing"
        );
        assert_eq!(
            service
                .start_behavior_recording("fixture".into())
                .expect_err("behavior initializing")
                .code,
            "runtime_initializing"
        );
        assert!(!shared.background_input_allowed_at(shared.controller.background_generation()));
        assert_eq!(
            shared.controller.phase(),
            crate::runtime_control::RuntimePhase::Idle
        );
        shared.initializing.store(false, Ordering::Release);
        assert!(shared.acquire_mode_admission().is_ok());
    }

    #[test]
    fn confirmed_startup_rollback_preserves_original_error_and_closes_capture_workers() {
        let service = test_hook_service();
        let error = service.shared.rollback_failed_start(
            crate::AppError::invalid("mock_start_failure", "fixture failure"),
            std::time::Duration::from_secs(1),
        );
        assert_eq!(error.code, "mock_start_failure");
        assert!(service.shared.background_workers_quiescent());
        assert!(service.shared.emergency_cleanup_quiescent());
        assert!(service.shared.initializing.load(Ordering::Acquire));
        assert!(service.shared.acquire_mode_admission().is_err());
    }

    #[test]
    fn startup_rollback_timeout_retains_failure_even_after_old_hook_exits() {
        let service = test_hook_service();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        service
            .shared
            .hook_thread_handle
            .set(std::thread::spawn(move || {
                let _ = release_rx.recv_timeout(std::time::Duration::from_secs(2));
            }))
            .expect("private ordinary thread, not a Windows hook");
        let error = service.shared.rollback_failed_start(
            crate::AppError::invalid("mock_detector_start_failed", "fixture detector failure"),
            std::time::Duration::ZERO,
        );
        assert_eq!(error.code, "input_service_start_rollback_unconfirmed");
        assert!(service
            .shared
            .startup_rollback_unconfirmed
            .load(Ordering::Acquire));
        assert!(service.shared.initializing.load(Ordering::Acquire));
        release_tx.send(()).expect("release private thread");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !service.shared.system_threads_quiescent() {
            assert!(std::time::Instant::now() < deadline, "fixture thread exit");
            std::thread::yield_now();
        }
        assert!(service.shared.acquire_mode_admission().is_err());
        super::record_cleanup_report(
            &service.shared,
            &crate::input_safety::CleanupReport::default(),
            "mock_late_empty_cleanup",
        );
        assert!(service
            .shared
            .startup_rollback_unconfirmed
            .load(Ordering::Acquire));
        assert!(!service
            .shared
            .wait_for_shutdown_cleanup(std::time::Duration::ZERO));
    }

    #[test]
    fn emergency_cleanup_body_completion_does_not_bypass_thread_destructors() {
        struct Finalizer {
            entered: std::sync::mpsc::SyncSender<()>,
            release: std::sync::mpsc::Receiver<()>,
        }
        impl Drop for Finalizer {
            fn drop(&mut self) {
                self.entered.send(()).expect("finalizer entered");
                self.release
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .expect("finalizer release");
            }
        }
        std::thread_local! {
            static FINALIZER: std::cell::RefCell<Option<Finalizer>> = const { std::cell::RefCell::new(None) };
        }
        let service = test_hook_service();
        let shared = &service.shared;
        shared
            .emergency_request_sequence
            .store(1, Ordering::Release);
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        shared.spawn_emergency_cleanup_with(move |_, _| {
            FINALIZER.with(|slot| {
                *slot.borrow_mut() = Some(Finalizer {
                    entered: entered_tx,
                    release: release_rx,
                })
            });
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("body returned");
        assert!(!shared.emergency_cleanup_scheduled.load(Ordering::Acquire));
        assert_eq!(
            shared
                .emergency_cleanup_completed_sequence
                .load(Ordering::Acquire),
            1
        );
        assert!(
            !shared.emergency_cleanup_quiescent(),
            "OS thread is still in its destructor"
        );
        assert!(shared.acquire_mode_admission().is_err());
        shared
            .emergency_request_sequence
            .store(2, Ordering::Release);
        shared.spawn_emergency_cleanup_with(|_, _| panic!("old destructor still live"));
        release_tx.send(()).expect("release finalizer");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            let exited = shared
                .emergency_cleanup_thread
                .lock()
                .expect("registry")
                .as_ref()
                .is_some_and(crate::bounded_worker::thread_exit_confirmed);
            if exited {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "mock finalizer exit");
            std::thread::yield_now();
        }
        shared.spawn_emergency_cleanup_with(|_, sequence| assert_eq!(sequence, 2));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !shared.emergency_cleanup_quiescent() {
            assert!(std::time::Instant::now() < deadline, "successor exit");
            std::thread::yield_now();
        }
    }

    #[test]
    fn panicked_emergency_cleanup_remains_locked_and_is_not_automatically_retried() {
        let service = test_hook_service();
        let shared = &service.shared;
        shared
            .emergency_request_sequence
            .store(1, Ordering::Release);
        shared.spawn_emergency_cleanup_with(|_, _| panic!("mock cleanup failure"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            let exited = shared
                .emergency_cleanup_thread
                .lock()
                .expect("registry")
                .as_ref()
                .is_some_and(crate::bounded_worker::thread_exit_confirmed);
            if exited {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "panicked fixture exit"
            );
            std::thread::yield_now();
        }
        assert!(shared.input_recovery_required.load(Ordering::Acquire));
        assert_eq!(
            shared.controller.phase(),
            crate::runtime_control::RuntimePhase::FaultLocked
        );
        assert!(!shared.emergency_cleanup_quiescent());
        shared.spawn_emergency_cleanup_with(|_, _| panic!("must not retry panicked cleanup"));
        assert_eq!(
            shared
                .emergency_cleanup_completed_sequence
                .load(Ordering::Acquire),
            0
        );
    }

    #[test]
    fn failed_hook_removal_cannot_report_safe_shutdown_with_empty_ledgers() {
        let service = test_hook_service();
        assert_eq!(service.shared.tracked_input_counts(), (0, 0));
        assert!(service.shared.confirm_hook_removal("keyboard", Ok(())));
        assert!(!service
            .shared
            .confirm_hook_removal("mouse", Err("mock unhook failure".into())));
        assert!(service
            .shared
            .native_teardown_failed
            .load(Ordering::Acquire));
        assert_eq!(
            service.shared.controller.phase(),
            crate::runtime_control::RuntimePhase::FaultLocked
        );
        assert!(!service
            .shared
            .wait_for_shutdown_cleanup(std::time::Duration::ZERO));
        assert!(service.shared.confirm_hook_removal("mouse", Ok(())));
        assert!(
            service
                .shared
                .native_teardown_failed
                .load(Ordering::Acquire),
            "a later success cannot clear unconfirmed teardown"
        );
    }

    #[test]
    fn emergency_cleanup_coalesces_late_requests_without_overlapping_threads() {
        let service = test_hook_service();
        let shared = &service.shared;
        shared
            .emergency_request_sequence
            .store(1, Ordering::Release);
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        shared.spawn_emergency_cleanup_with(move |_, sequence| {
            assert_eq!(sequence, 1);
            entered_tx.send(()).expect("entered mock cleanup");
            release_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("release");
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("worker entered");
        shared
            .emergency_request_sequence
            .store(3, Ordering::Release);
        shared.spawn_emergency_cleanup_with(|_, _| panic!("overlapping cleanup"));
        assert!(!shared.emergency_cleanup_quiescent());
        assert!(shared.acquire_mode_admission().is_err());
        release_tx.send(()).expect("release fixture");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            let exited = shared
                .emergency_cleanup_thread
                .lock()
                .expect("registry")
                .as_ref()
                .is_some_and(crate::bounded_worker::thread_exit_confirmed);
            if exited {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "mock worker exit");
            std::thread::yield_now();
        }
        assert_eq!(
            shared
                .emergency_cleanup_completed_sequence
                .load(Ordering::Acquire),
            1
        );
        assert!(
            !shared.emergency_cleanup_quiescent(),
            "late requests remain pending"
        );
        shared.spawn_emergency_cleanup_with(|_, sequence| assert_eq!(sequence, 3));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !shared.emergency_cleanup_quiescent() {
            assert!(
                std::time::Instant::now() < deadline,
                "coalesced cleanup exit"
            );
            std::thread::yield_now();
        }
        assert!(shared.acquire_mode_admission().is_ok());
    }

    #[test]
    fn contended_emergency_cleanup_registry_preserves_pending_stop() {
        let service = test_hook_service();
        let shared = &service.shared;
        shared
            .emergency_request_sequence
            .store(1, Ordering::Release);
        let guard = shared
            .emergency_cleanup_thread
            .lock()
            .expect("hold registry");
        shared.spawn_emergency_cleanup_with(|_, _| panic!("contended registry must defer"));
        assert!(!shared.emergency_cleanup_quiescent());
        assert!(shared.acquire_mode_admission().is_err());
        assert_eq!(
            shared
                .emergency_cleanup_completed_sequence
                .load(Ordering::Acquire),
            0
        );
        drop(guard);
        shared.spawn_emergency_cleanup_with(|_, sequence| assert_eq!(sequence, 1));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !shared.emergency_cleanup_quiescent() {
            assert!(std::time::Instant::now() < deadline, "deferred worker exit");
            std::thread::yield_now();
        }
    }

    #[test]
    fn shutdown_confirmation_waits_for_retained_hook_and_emergency_thread_handles() {
        let service = HookService::isolated(
            AppConfig::default(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        assert!(service
            .shared
            .wait_for_shutdown_cleanup(std::time::Duration::ZERO));
        let (hook_release_tx, hook_release_rx) = std::sync::mpsc::sync_channel(1);
        let hook = std::thread::spawn(move || {
            let _ = hook_release_rx.recv_timeout(std::time::Duration::from_secs(2));
        });
        service
            .shared
            .hook_thread_handle
            .set(hook)
            .expect("mock hook handle");
        let (emergency_release_tx, emergency_release_rx) = std::sync::mpsc::sync_channel(1);
        let emergency = std::thread::spawn(move || {
            let _ = emergency_release_rx.recv_timeout(std::time::Duration::from_secs(2));
        });
        service
            .shared
            .emergency_thread_handle
            .set(emergency)
            .expect("mock detector handle");
        assert!(!service.shared.system_threads_quiescent());
        assert!(!service
            .shared
            .wait_for_shutdown_cleanup(std::time::Duration::ZERO));
        hook_release_tx.send(()).expect("release hook fixture");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !crate::bounded_worker::thread_exit_confirmed(
            service.shared.hook_thread_handle.get().expect("handle"),
        ) {
            assert!(
                std::time::Instant::now() < deadline,
                "fixture hook did not exit"
            );
            std::thread::yield_now();
        }
        assert!(
            !service
                .shared
                .wait_for_shutdown_cleanup(std::time::Duration::ZERO),
            "detector is still live"
        );
        emergency_release_tx
            .send(())
            .expect("release detector fixture");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !service.shared.system_threads_quiescent() {
            assert!(
                std::time::Instant::now() < deadline,
                "fixture detector did not exit"
            );
            std::thread::yield_now();
        }
        assert!(service
            .shared
            .wait_for_shutdown_cleanup(std::time::Duration::ZERO));
    }

    #[test]
    fn exited_detector_handle_cannot_be_ready_with_a_fresh_heartbeat() {
        let service = test_hook_service();
        let detector = std::thread::spawn(|| {});
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !crate::bounded_worker::thread_exit_confirmed(&detector) {
            assert!(
                std::time::Instant::now() < deadline,
                "fixture detector did not exit"
            );
            std::thread::yield_now();
        }
        service
            .shared
            .emergency_thread_handle
            .set(detector)
            .expect("mock detector");
        service.shared.emergency_heartbeat_ms.store(
            service.shared.emergency_clock.elapsed().as_millis() as u64 + 1,
            Ordering::Release,
        );
        assert!(!service.shared.emergency_stop_ready());
    }

    #[test]
    fn unconfirmed_hotkey_teardown_blocks_otherwise_empty_shutdown_confirmation() {
        let service = HookService::isolated(
            AppConfig::default(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        assert!(service
            .shared
            .wait_for_shutdown_cleanup(std::time::Duration::ZERO));
        service
            .shared
            .native_teardown_failed
            .store(true, Ordering::Release);
        assert_eq!(service.shared.tracked_input_counts(), (0, 0));
        assert!(service.shared.background_workers_quiescent());
        assert!(!service
            .shared
            .wait_for_shutdown_cleanup(std::time::Duration::ZERO));
    }

    #[test]
    fn native_registration_ids_never_reuse_or_wrap_after_exhaustion() {
        let service = test_hook_service();
        assert_eq!(service.shared.allocate_native_hotkey_id(), Some(0x5000));
        assert_eq!(service.shared.allocate_native_hotkey_id(), Some(0x5001));
        service
            .shared
            .next_native_hotkey_id
            .store(0x7fff, Ordering::Release);
        assert_eq!(service.shared.allocate_native_hotkey_id(), Some(0x7fff));
        assert_eq!(service.shared.allocate_native_hotkey_id(), None);
        assert_eq!(service.shared.allocate_native_hotkey_id(), None);
        assert_eq!(
            service.shared.next_native_hotkey_id.load(Ordering::Acquire),
            0x8000
        );
    }

    #[test]
    fn registration_refresh_preserves_retry_on_business_lock_contention() {
        let service = test_hook_service();
        let held = service.shared.config.lock().expect("hold config");
        let shared = service.shared.clone();
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        let caller = std::thread::spawn(move || {
            // Must return before any RegisterHotKey/UnregisterHotKey call.
            super::refresh_native_macro_hotkeys(&shared);
            let _ = done_tx.send(());
        });
        let done = done_rx.recv_timeout(std::time::Duration::from_secs(1));
        drop(held);
        caller.join().expect("refresh fixture");
        done.expect("refresh must not wait for config");
        assert!(service
            .shared
            .native_refresh_pending
            .load(Ordering::Acquire));

        let held = service
            .shared
            .native_macro_hotkeys
            .lock()
            .expect("hold registry");
        let shared = service.shared.clone();
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        let caller = std::thread::spawn(move || {
            super::refresh_native_macro_hotkeys(&shared);
            let _ = done_tx.send(());
        });
        let done = done_rx.recv_timeout(std::time::Duration::from_secs(1));
        drop(held);
        caller.join().expect("refresh fixture");
        done.expect("refresh must not wait for registry");
        assert!(service
            .shared
            .native_refresh_pending
            .load(Ordering::Acquire));
        // The isolated fixture has no hook thread; keep the request pending
        // rather than falsely claiming an OS refresh message was delivered.
        service.shared.thread_id.store(0, Ordering::Release);
        service.shared.post_native_refresh_if_needed();
        assert!(service
            .shared
            .native_refresh_pending
            .load(Ordering::Acquire));
        assert!(!service
            .shared
            .native_refresh_message_pending
            .load(Ordering::Acquire));
    }

    #[test]
    fn registration_plan_ignores_script_and_ui_edits_but_detects_trigger_changes() {
        let mut config = AppConfig {
            macros: vec![MacroRule {
                id: "registration-fixture".into(),
                name: "fixture".into(),
                import_error: None,
                enabled: true,
                trigger_keys: vec!["F10".into()],
                mode: MacroMode::Once,
                repeat_count: 1,
                speed: 1.0,
                record_mouse_move: true,
                record_mouse_clicks: true,
                target: None,
                behavior_policy: None,
                program: AutomationProgram::Macro { steps: Vec::new() },
            }],
            ..AppConfig::default()
        };
        let expected = super::native_registration_plan(&config);
        assert_eq!(expected, [("registration-fixture".into(), 0, 0x79)]);
        config.global_enabled = false;
        config.show_playback_overlay = true;
        config.macros[0].name = "renamed".into();
        config.macros[0].mode = MacroMode::Toggle;
        config.macros[0].speed = 2.0;
        config.macros[0].program = AutomationProgram::Macro {
            steps: vec![MacroStep::Delay {
                duration_ms: 37,
                duration_max_ms: None,
            }],
        };
        assert_eq!(super::native_registration_plan(&config), expected);
        config.macros[0].trigger_keys = vec!["Ctrl".into(), "F10".into()];
        assert_ne!(super::native_registration_plan(&config), expected);
        config.macros[0].mode = MacroMode::Hold;
        assert!(super::native_registration_plan(&config).is_empty());
        config.macros[0].mode = MacroMode::Once;
        config.macros[0].enabled = false;
        assert!(super::native_registration_plan(&config).is_empty());
        config.macros[0].enabled = true;
        config.macros[0].trigger_keys = vec!["F12".into()];
        assert!(super::native_registration_plan(&config).is_empty());
        config.macros[0].trigger_keys = vec!["Ctrl".into(), "F12".into()];
        assert!(super::native_registration_plan(&config).is_empty());
        config.emergency_stop = "F11".into();
        config.macros[0].trigger_keys = vec!["Shift".into(), "F11".into()];
        assert!(super::native_registration_plan(&config).is_empty());
        config.macros[0].trigger_keys = vec!["Shift".into(), "F10".into()];
        assert!(!super::native_registration_plan(&config).is_empty());
    }

    #[test]
    fn native_clicker_registration_rejects_any_emergency_key_in_ctrl_f8() {
        let mut config = AppConfig::default();
        assert!(super::native_clicker_registration_allowed(&config));
        config.emergency_stop = "F8".into();
        assert!(!super::native_clicker_registration_allowed(&config));
        config.emergency_stop = "Control".into();
        assert!(!super::native_clicker_registration_allowed(&config));
    }

    #[test]
    fn native_trigger_dispatch_discards_unavailable_registry_and_configuration() {
        let service = test_hook_service();
        service
            .shared
            .native_refresh_pending
            .store(false, Ordering::Release);
        service
            .shared
            .native_macro_hotkeys
            .lock()
            .expect("registry")
            .insert(0x5000, "fixture".into());
        let held = service
            .shared
            .native_macro_hotkeys
            .lock()
            .expect("hold registry");
        let shared = service.shared.clone();
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        let dispatcher = std::thread::spawn(move || {
            super::process_native_macro_hotkey(&shared, 0x5000);
            let _ = done_tx.send(());
        });
        let result = done_rx.recv_timeout(std::time::Duration::from_secs(1));
        drop(held);
        dispatcher.join().expect("input-free dispatcher");
        result.expect("native registry contention must not block dispatch");

        let held = service.shared.config.lock().expect("hold configuration");
        let shared = service.shared.clone();
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        let dispatcher = std::thread::spawn(move || {
            super::process_native_macro_hotkey(&shared, 0x5000);
            super::process_native_clicker_hotkey(&shared);
            let _ = done_tx.send(());
        });
        let result = done_rx.recv_timeout(std::time::Duration::from_secs(1));
        drop(held);
        dispatcher.join().expect("input-free dispatcher");
        result.expect("configuration contention must not defer a native trigger");
        assert!(!service.shared.trigger_pending.load(Ordering::Acquire));
        assert_eq!(
            service.shared.playback_thread_owner.load(Ordering::Acquire),
            0
        );
        assert_eq!(
            service.shared.controller.phase(),
            crate::runtime_control::RuntimePhase::Idle
        );
    }

    #[test]
    fn unavailable_playback_snapshot_is_busy_without_waiting_or_mutating_run() {
        let service = test_hook_service();
        let mut held = service.shared.playback.lock().expect("hold playback");
        held.running = false;
        assert!(
            service.shared.is_playback_running(),
            "unavailable state cannot prove idle"
        );
        assert!(
            !held.running,
            "busy observation must not mutate the active state"
        );
        drop(held);
        assert!(!service.shared.is_playback_running());
    }

    #[test]
    fn production_graph_submission_and_shortcut_stop_preserve_queued_events() {
        let service = test_hook_service();
        service.start_recording(true, true).expect("record");
        let guard = service
            .shared
            .recorder
            .lock()
            .expect("block worker processing");
        let shared = service.shared.clone();
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        let producer = std::thread::spawn(move || {
            let at = std::time::Instant::now();
            super::record_keyboard_event(&shared, 0x41, true, at);
            super::record_keyboard_event(
                &shared,
                0x41,
                false,
                at + std::time::Duration::from_millis(37),
            );
            super::record_mouse_move(&shared, 100, 200, at + std::time::Duration::from_millis(74));
            super::discard_trailing_mouse_actions(&shared);
            shared.finish_recording_from_hotkey();
            // Stop admission fences events even while processing is blocked.
            super::record_mouse_move(&shared, 900, 800, at);
            let _ = done_tx.send(());
        });
        let submitted = done_rx.recv_timeout(std::time::Duration::from_secs(1));
        assert!(guard.steps.is_empty());
        drop(guard);
        producer.join().expect("input-free producer");
        submitted.expect("submission and shortcut stop do not wait for the recorder lock");
        assert_eq!(
            service
                .shared
                .graph_capture
                .get()
                .expect("queue")
                .statistics()
                .accepted,
            5
        );
        let result = service.stop_recording(false).expect("ordered result");
        assert_eq!(result.steps.len(), 3);
        assert!(
            matches!(&result.steps[0], MacroStep::Key { key, action: KeyAction::Down } if key == "A")
        );
        assert!(matches!(
            &result.steps[1],
            MacroStep::Delay {
                duration_ms: 37,
                ..
            }
        ));
        assert!(
            matches!(&result.steps[2], MacroStep::Key { key, action: KeyAction::Up } if key == "A")
        );
        assert!(!service.shared.is_recording());
        assert!(!service
            .shared
            .graph_capture
            .get()
            .expect("queue")
            .is_pending());
        service
            .start_recording(true, true)
            .expect("next session after result retrieval");
        service.stop_recording(false).expect("empty second session");
    }

    #[test]
    fn recording_focus_boundary_and_options_do_not_wait_for_processing_lock() {
        let service = test_hook_service();
        service.start_recording(true, false).expect("record");
        let guard = service.shared.recorder.lock().expect("hold processing");
        let shared = service.shared.clone();
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        let producer = std::thread::spawn(move || {
            assert!(shared.is_recording());
            assert!(super::recording_mouse_move_enabled(&shared));
            assert!(!super::recording_mouse_clicks_enabled(&shared));
            assert!(!super::admit_recording_focus(&shared, true));
            assert!(!super::admit_recording_focus(&shared, false));
            assert!(super::admit_recording_focus(&shared, false));
            let at = std::time::Instant::now();
            super::record_keyboard_event(&shared, 0x41, true, at);
            super::record_keyboard_event(
                &shared,
                0x41,
                false,
                at + std::time::Duration::from_millis(37),
            );
            shared.finish_recording_from_hotkey();
            assert!(!super::admit_recording_focus(&shared, false));
            assert!(shared.is_recording(), "pending result remains transparent");
            let _ = done_tx.send(());
        });
        let submitted = done_rx.recv_timeout(std::time::Duration::from_secs(1));
        assert!(!guard.capture_started);
        drop(guard);
        producer.join().expect("input-free focus producer");
        submitted.expect("focus/options checks must not wait for processing");
        assert_eq!(
            service
                .shared
                .graph_capture
                .get()
                .expect("queue")
                .statistics()
                .accepted,
            4
        );
        let result = service.stop_recording(false).expect("drained capture");
        assert_eq!(result.steps.len(), 3);
        assert!(!service.shared.is_recording());
    }

    #[test]
    fn incomplete_graph_capture_cannot_be_claimed_by_retrying_stop() {
        let service = test_hook_service();
        service.start_recording(true, true).expect("record");
        let guard = service.shared.recorder.lock().expect("hold processing");
        let at = std::time::Instant::now();
        for index in 0..1100 {
            super::record_mouse_move(&service.shared, index * 10, 0, at);
        }
        assert!(service.shared.graph_capture_error.load(Ordering::Acquire));
        assert!(
            service
                .shared
                .graph_capture
                .get()
                .expect("queue")
                .statistics()
                .dropped
                > 0
        );
        drop(guard);
        assert_eq!(
            service
                .stop_recording(false)
                .expect_err("lost capture")
                .code,
            "capture_incomplete"
        );
        assert_eq!(
            service
                .stop_recording(false)
                .expect_err("repeat must not bypass failure")
                .code,
            "capture_incomplete"
        );
        let recorder = service.shared.recorder.lock().expect("retained recorder");
        assert!(recorder.active);
        assert!(!recorder.steps.is_empty());
        assert!(recorder.completed_steps.is_none());
        drop(recorder);
        assert!(service.start_recording(true, true).is_err());
    }

    #[test]
    fn graph_capture_processing_is_ordered_behind_a_drain_barrier() {
        let recorder = std::sync::Arc::new(std::sync::Mutex::new(RecorderState {
            active: true,
            ..RecorderState::default()
        }));
        let worker_recorder = recorder.clone();
        let queue = crate::bounded_worker::CaptureQueue::spawn(move |_, at, event| {
            let mut state = worker_recorder.lock().expect("input-free recorder");
            super::apply_graph_capture(&mut state, event, at);
            true
        })
        .expect("graph capture fixture");
        queue.begin().expect("session");
        let at = std::time::Instant::now();
        let guard = recorder.lock().expect("block statistics processing");
        let events = [
            super::GraphCapture::Key(0x41, false), // unmatched up: ignored
            super::GraphCapture::Key(0x41, true),
            super::GraphCapture::Key(0x41, true), // repeat: ignored
            super::GraphCapture::Key(0x41, false),
            super::GraphCapture::Move(i32::MIN, i32::MIN),
            super::GraphCapture::Move(i32::MAX, i32::MAX),
            super::GraphCapture::Button(MouseButton::Left, KeyAction::Up, 10, 20),
            super::GraphCapture::Button(MouseButton::Left, KeyAction::Down, 10, 20),
            super::GraphCapture::Button(MouseButton::Left, KeyAction::Up, 10, 20),
            super::GraphCapture::Wheel(0, 120),
        ];
        for (index, event) in events.into_iter().enumerate() {
            assert!(queue.submit(
                at + std::time::Duration::from_millis(index as u64 * 37),
                event,
            ));
        }
        assert!(
            guard.steps.is_empty(),
            "submission does not process under the caller lock"
        );
        drop(guard);
        queue
            .drain(std::time::Duration::from_millis(500))
            .expect("all admitted graph events processed");
        let mut state = recorder.lock().expect("drained recorder");
        assert!(state.pressed_keys.is_empty());
        assert!(state.pressed_buttons.is_empty());
        let delays = state
            .steps
            .iter()
            .filter_map(|step| match step {
                MacroStep::Delay { duration_ms, .. } => Some(*duration_ms),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(delays, [74, 37, 37, 74, 37, 37]);
        assert_eq!(state.steps.len(), 13);
        super::finish_recorder(&mut state);
        assert_eq!(
            state
                .completed_steps
                .as_ref()
                .expect("pending result")
                .len(),
            13
        );
        assert!(!state.active);
        queue.close();
    }

    #[test]
    fn graph_recording_uses_supplied_event_clock_for_all_action_kinds() {
        let service = test_hook_service();
        service.start_recording(true, true).expect("record");
        let base = std::time::Instant::now();
        let at = |index: u64| base + std::time::Duration::from_millis(index * 37);
        super::record_keyboard_event(&service.shared, 0x41, true, at(0));
        super::record_keyboard_event(&service.shared, 0x41, false, at(1));
        super::record_mouse_move(&service.shared, 10, 20, at(2));
        super::record_mouse_move(&service.shared, 50, 60, at(3));
        super::record_mouse_button(
            &service.shared,
            MouseButton::Left,
            KeyAction::Down,
            50,
            60,
            at(4),
        );
        super::record_mouse_button(
            &service.shared,
            MouseButton::Left,
            KeyAction::Up,
            50,
            60,
            at(5),
        );
        super::record_mouse_wheel(&service.shared, 0, 120, at(6));
        let result = service.stop_recording(false).expect("result");
        assert_eq!(result.steps.len(), 13);
        let delays: Vec<u64> = result
            .steps
            .iter()
            .filter_map(|step| match step {
                MacroStep::Delay { duration_ms, .. } => Some(*duration_ms),
                _ => None,
            })
            .collect();
        assert_eq!(delays, [37; 6]);
    }

    #[test]
    fn behavior_capture_is_nonblocking_and_stop_drains_all_accepted_events() {
        use std::time::{Duration, Instant};
        let service = test_hook_service();
        service
            .start_behavior_recording("queued fixture".into())
            .expect("start");
        let recorder = service
            .shared
            .behavior
            .lock()
            .expect("block statistics processing");
        let shared = service.shared.clone();
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        let producer = std::thread::spawn(move || {
            let captured_at = Instant::now();
            for (index, key) in (0x41..0x45).enumerate() {
                shared.record_behavior_key(
                    key,
                    0,
                    true,
                    captured_at + Duration::from_millis(index as u64 * 74),
                );
                shared.record_behavior_key(
                    key,
                    0,
                    false,
                    captured_at + Duration::from_millis(index as u64 * 74 + 37),
                );
            }
            shared.record_behavior_mouse_move(0, 0, captured_at + Duration::from_millis(296));
            shared.record_behavior_mouse_move(40, 50, captured_at + Duration::from_millis(333));
            let _ = done_tx.send(());
        });
        let admitted = done_rx.recv_timeout(std::time::Duration::from_secs(1));
        drop(recorder);
        producer.join().expect("producer exit");
        admitted.expect("hook-side submission must not wait for statistics lock");
        let result = service
            .stop_behavior_recording()
            .expect("drained training result");
        assert_eq!(result.profile.sample_count, 10);
        assert_eq!(result.profile.raw_events.len(), 10);
        let events = serde_json::to_value(&result.profile.raw_events).expect("events");
        let timestamps: Vec<u64> = events
            .as_array()
            .expect("raw events")
            .iter()
            .map(|event| event["timestampMs"].as_u64().expect("capture timestamp"))
            .collect();
        assert!(
            timestamps.windows(2).all(|pair| pair[1] - pair[0] == 37),
            "queue processing must not replace the supplied hook clock"
        );
        assert!(!service.behavior_recording_status().active);
        assert!(!service
            .shared
            .behavior_capture_error
            .load(Ordering::Acquire));
    }

    #[test]
    fn behavior_f12_preserves_pending_claim_once() {
        let service = test_hook_service();
        service
            .shared
            .start_behavior_recording("fixture")
            .expect("start behavior recording");
        for index in 0..8 {
            service.shared.record_behavior_mouse_move(
                index * 10,
                index * 5,
                std::time::Instant::now(),
            );
        }
        super::emergency_cleanup_worker(std::sync::Arc::clone(&service.shared), 1);
        let status = service.behavior_recording_status();
        assert!(!status.active, "cleanup must stop the recording");
        assert_eq!(status.event_count, 8, "cleanup keeps pending events");
        super::emergency_cleanup_worker(std::sync::Arc::clone(&service.shared), 2);
        service
            .update_config(AppConfig::default())
            .expect("config update");
        assert_eq!(
            service.behavior_recording_status().event_count,
            8,
            "repeat cleanup must not drop the pending claim"
        );
        assert!(
            service.shared.start_behavior_recording("again").is_err(),
            "new start denied before the pending claim is drained"
        );
        let result = service
            .shared
            .stop_behavior_recording()
            .expect("successful stop drains the claim once");
        assert_eq!(
            result.profile.sample_count, 8,
            "stop drains all pending samples"
        );
        assert_eq!(
            result.profile.raw_events.len(),
            8,
            "stop emits the pending raw events once"
        );
        let retried = service
            .shared
            .stop_behavior_recording()
            .expect("uncommitted claim remains retryable");
        assert_eq!(retried.profile.id, result.profile.id);
        service
            .shared
            .complete_behavior_recording_claim()
            .expect("commit claim once");
        assert!(service.shared.stop_behavior_recording().is_err());
        assert!(service.shared.complete_behavior_recording_claim().is_err());
    }

    #[test]
    fn behavior_f12_failed_drain_preserves_unusable_events() {
        let service = test_hook_service();
        service
            .shared
            .start_behavior_recording("fixture")
            .expect("start behavior recording");
        for index in 0..8 {
            service.shared.record_behavior_mouse_move(
                index * 10,
                index * 5,
                std::time::Instant::now(),
            );
        }
        let queue = service.shared.behavior_capture.get().expect("queue");
        assert!(
            queue.note_admission_loss(),
            "queue must register an admission loss before cleanup"
        );
        super::emergency_cleanup_worker(std::sync::Arc::clone(&service.shared), 1);
        let status = service.behavior_recording_status();
        assert!(!status.active, "cleanup must stop the recording");
        assert_eq!(status.event_count, 8, "cleanup keeps the events");
        assert!(!queue.is_active(), "queue is inactive after cleanup");
        assert!(
            service.shared.stop_behavior_recording().is_err(),
            "stop rejected while drain is unusable"
        );
        assert!(
            service.shared.start_behavior_recording("again").is_err(),
            "new start denied while unusable events remain"
        );
        super::emergency_cleanup_worker(std::sync::Arc::clone(&service.shared), 2);
        assert_eq!(
            service.behavior_recording_status().event_count,
            8,
            "repeat cleanup retains the unusable events"
        );
    }

    #[test]
    fn behavior_normal_failed_stop_preserves_unusable_events() {
        let service = test_hook_service();
        service
            .shared
            .start_behavior_recording("fixture")
            .expect("start behavior recording");
        for index in 0..8 {
            service.shared.record_behavior_mouse_move(
                index * 10,
                index * 5,
                std::time::Instant::now(),
            );
        }
        let queue = service.shared.behavior_capture.get().expect("queue");
        queue.note_admission_loss();
        assert!(
            service.shared.stop_behavior_recording().is_err(),
            "normal stop rejected while events are unusable"
        );
        let status = service.behavior_recording_status();
        assert!(!status.active, "rejected stop still clears active state");
        assert!(status.pending, "failed stop retains an explicit decision");
        assert!(status.incomplete, "failed stop cannot be trained");
        assert_eq!(status.event_count, 8, "rejected stop preserves events");
        assert!(
            service.shared.stop_behavior_recording().is_err(),
            "repeated stop stays denied"
        );
        assert!(
            service.shared.start_behavior_recording("again").is_err(),
            "new start denied while unusable events remain"
        );
        service
            .shared
            .discard_behavior_recording()
            .expect("explicit discard after confirmed barrier");
        let discarded = service.behavior_recording_status();
        assert!(!discarded.pending);
        assert!(!discarded.incomplete);
        service
            .shared
            .start_behavior_recording("after discard")
            .expect("new session after explicit discard");
    }

    #[test]
    fn behavior_discard_timeout_retains_pending_incomplete_state() {
        let service = test_hook_service();
        service
            .shared
            .start_behavior_recording("fixture")
            .expect("start behavior recording");
        let behavior = service.shared.behavior.lock().expect("block worker");
        service
            .shared
            .record_behavior_mouse_move(10, 20, std::time::Instant::now());
        let error = service
            .shared
            .discard_behavior_recording()
            .expect_err("blocked worker barrier must preserve capture");
        assert_eq!(error.code, "capture_drain_timeout");
        drop(behavior);
        let status = service.behavior_recording_status();
        assert!(!status.active);
        assert!(status.pending);
        assert!(status.incomplete);
        assert!(service
            .shared
            .start_behavior_recording("too early")
            .is_err());
        service
            .shared
            .discard_behavior_recording()
            .expect("later barrier confirms explicit discard");
        service
            .shared
            .start_behavior_recording("after barrier")
            .expect("new session after confirmed discard");
    }

    #[test]
    fn empty_input_ledger_does_not_confirm_shutdown_with_a_blocked_background_worker() {
        let service = test_hook_service();
        service
            .shared
            .graph_capture
            .get()
            .expect("graph capture worker")
            .close();
        service
            .shared
            .behavior_capture
            .get()
            .expect("capture worker")
            .close();
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let worker = crate::bounded_worker::BoundedWorker::spawn(
            "mock-shutdown-worker",
            1,
            move |_: super::LaunchTask| {
                let _ = entered_tx.send(());
                let _ = release_rx.recv_timeout(std::time::Duration::from_secs(2));
            },
        )
        .expect("worker");
        assert!(service.shared.launch_tasks.set(worker).is_ok());
        assert!(service.shared.submit_launch(
            "never-executed",
            service.shared.controller.background_generation()
        ));
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("handler entered");
        service.shared.launch_tasks.get().expect("worker").close();
        assert_eq!(service.shared.tracked_input_counts(), (0, 0));
        assert!(!service.shared.background_workers_quiescent());
        assert!(!service
            .shared
            .wait_for_shutdown_cleanup(std::time::Duration::ZERO));
        release_tx.send(()).expect("release fixture");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !service.shared.background_workers_quiescent() {
            assert!(
                std::time::Instant::now() < deadline,
                "worker still executing"
            );
            std::thread::yield_now();
        }
        assert!(service
            .shared
            .wait_for_shutdown_cleanup(std::time::Duration::ZERO));
    }

    #[test]
    fn launch_queue_is_bounded_and_rejects_stale_or_oversized_admission() {
        let service = test_hook_service();
        let revision = service.shared.controller.background_generation();
        assert!(!service.shared.submit_launch("not-executed", revision));
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let worker = crate::bounded_worker::BoundedWorker::spawn(
            "mock-launch-queue",
            1,
            move |_: super::LaunchTask| {
                let _ = entered_tx.send(());
                let _ = release_rx.recv_timeout(std::time::Duration::from_secs(2));
            },
        )
        .expect("mock worker");
        assert!(service.shared.launch_tasks.set(worker).is_ok());
        assert!(service.shared.submit_launch("not-executed", revision));
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("blocked worker");
        assert!(service
            .shared
            .submit_launch("queued-not-executed", revision));
        assert!(!service.shared.submit_launch("queue-full", revision));
        assert!(!service
            .shared
            .submit_launch(&"x".repeat(16 * 1024 + 1), revision));
        assert!(!service.shared.submit_launch(&"a\n".repeat(33), revision));
        service.shared.controller.request_stop();
        assert!(!service.shared.submit_launch("stale", revision));
        service.shared.launch_tasks.get().expect("worker").close();
        release_tx.send(()).expect("release fixture");
    }

    #[test]
    fn launch_command_dispatch_rechecks_permission_between_commands() {
        let mut checks = 0;
        let mut seen = Vec::new();
        super::visit_launch_commands(
            "\"test program\" a\nsecond b\nthird c",
            || {
                checks += 1;
                checks == 1
            },
            |program, args| seen.push((program.to_string(), args.to_vec())),
        );
        assert_eq!(checks, 2);
        assert_eq!(seen, [("test program".into(), vec!["a".into()])]);
        super::visit_launch_commands("never-executed", || false, |_, _| panic!("revoked launch"));
    }

    #[test]
    fn recording_rejects_playback_without_destroying_captured_steps() {
        let service = test_hook_service();
        service.start_recording(true, true).expect("record");
        service
            .shared
            .recorder
            .lock()
            .expect("recorder")
            .steps
            .push(MacroStep::MouseMove { x: 12, y: 34 });
        let rule: MacroRule = serde_json::from_value(serde_json::json!({"id":"blocked", "name":"blocked", "program":{"kind":"macro","steps":[]}})).expect("fixture");
        assert_eq!(
            service
                .shared
                .start_playback(rule)
                .expect_err("recording blocks playback")
                .code,
            "recording_active"
        );
        assert!(service.recording_status().active);
        assert_eq!(service.recording_status().step_count, 1);
        assert!(service.shared.controller.is_quiescent());
        let recorded = service.stop_recording(false).expect("preserved result");
        assert_eq!(recorded.steps.len(), 1);
    }

    #[test]
    fn new_recording_cannot_overwrite_uncollected_completed_result() {
        let service = test_hook_service();
        service.start_recording(true, true).expect("record");
        service
            .shared
            .recorder
            .lock()
            .expect("recorder")
            .steps
            .push(MacroStep::MouseMove { x: 12, y: 34 });
        service.shared.finish_recording();
        assert_eq!(
            service
                .start_recording(true, true)
                .expect_err("pending result")
                .code,
            "recording_result_pending"
        );
        assert_eq!(
            service
                .stop_recording(false)
                .expect("preserved result")
                .steps
                .len(),
            1
        );
        service
            .start_recording(true, true)
            .expect("explicit next recording");
    }

    #[test]
    fn recording_modes_share_nonblocking_admission_and_do_not_block_stop_signal() {
        let service = test_hook_service();
        let held = service.shared.mode_admission.lock().expect("held startup");
        assert_eq!(
            service
                .start_recording(true, true)
                .expect_err("admission busy")
                .code,
            "runtime_admission_busy"
        );
        assert_eq!(
            service
                .start_behavior_recording("fixture".into())
                .expect_err("admission busy")
                .code,
            "runtime_admission_busy"
        );
        let before = service.shared.controller.generation();
        assert!(service.shared.controller.request_stop() > before);
        drop(held);
        service
            .start_behavior_recording("fixture".into())
            .expect("behavior record");
        assert_eq!(
            service
                .start_recording(true, true)
                .expect_err("mode conflict")
                .code,
            "behavior_recording_active"
        );
        assert!(service.behavior_recording_status().active);
    }

    #[test]
    fn trigger_worker_admission_is_single_pending_and_rejects_old_generation() {
        use std::time::Duration;
        let service = HookService::isolated(
            AppConfig::default(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let rule = MacroRule {
            id: "trigger-test".into(),
            name: "trigger-test".into(),
            import_error: None,
            enabled: true,
            trigger_keys: vec!["F10".into()],
            mode: MacroMode::Once,
            repeat_count: 1,
            speed: 1.0,
            record_mouse_move: true,
            record_mouse_clicks: true,
            target: None,
            behavior_policy: None,
            program: AutomationProgram::Macro { steps: Vec::new() },
        };
        let generation = service.shared.emergency_generation.load(Ordering::Acquire);
        let config_revision = service
            .shared
            .trigger_config_revision
            .load(Ordering::Acquire);
        assert!(!service
            .shared
            .submit_macro_trigger(&rule, generation, config_revision, None));
        assert!(!service.shared.trigger_pending.load(Ordering::Acquire));
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let worker = crate::bounded_worker::BoundedWorker::spawn(
            "input-free-trigger-fixture",
            1,
            move |_: super::MacroTriggerTask| {
                entered_tx.send(()).expect("entered");
                release_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("release");
            },
        )
        .expect("worker");
        assert!(service.shared.trigger_tasks.set(worker).is_ok());
        assert!(service
            .shared
            .submit_macro_trigger(&rule, generation, config_revision, None));
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("handling");
        for _ in 0..100 {
            assert!(!service
                .shared
                .submit_macro_trigger(&rule, generation, config_revision, None));
        }
        service
            .shared
            .emergency_generation
            .fetch_add(1, Ordering::AcqRel);
        assert!(!service
            .shared
            .submit_macro_trigger(&rule, generation, config_revision, None));
        service.shared.trigger_tasks.get().expect("queue").close();
        release_tx.send(()).expect("release fixture");
        assert!(service.shared.controller.is_quiescent());
        assert_eq!(service.shared.injected_input.counts(), (0, 0));
    }

    #[test]
    fn ctrl_f9_gate_waits_for_every_key_and_a_stable_release_window() {
        let mut gate = super::TriggerReleaseGate::new(
            std::time::Duration::from_millis(18),
            std::time::Duration::from_secs(5),
        );
        assert_eq!(
            gate.observe(
                std::time::Duration::ZERO,
                true,
                super::TriggerReleaseGateStatus::Current,
            ),
            None
        );
        // F9 is up, but Ctrl remains down: startup is still forbidden.
        assert_eq!(
            gate.observe(
                std::time::Duration::from_millis(12),
                true,
                super::TriggerReleaseGateStatus::Current,
            ),
            None
        );
        assert_eq!(
            gate.observe(
                std::time::Duration::from_millis(20),
                false,
                super::TriggerReleaseGateStatus::Current,
            ),
            None
        );
        assert_eq!(
            gate.observe(
                std::time::Duration::from_millis(37),
                false,
                super::TriggerReleaseGateStatus::Current,
            ),
            None
        );
        assert_eq!(
            gate.observe(
                std::time::Duration::from_millis(38),
                false,
                super::TriggerReleaseGateStatus::Current,
            ),
            Some(super::TriggerReleaseGateOutcome::Ready)
        );
    }

    #[test]
    fn single_key_gate_never_starts_before_key_up() {
        let mut gate = super::TriggerReleaseGate::new(
            std::time::Duration::from_millis(18),
            std::time::Duration::from_secs(5),
        );
        for millis in [0, 20, 200] {
            assert_eq!(
                gate.observe(
                    std::time::Duration::from_millis(millis),
                    true,
                    super::TriggerReleaseGateStatus::Current,
                ),
                None
            );
        }
        assert_eq!(
            gate.observe(
                std::time::Duration::from_millis(201),
                false,
                super::TriggerReleaseGateStatus::Current,
            ),
            None
        );
        assert_eq!(
            gate.observe(
                std::time::Duration::from_millis(219),
                false,
                super::TriggerReleaseGateStatus::Current,
            ),
            Some(super::TriggerReleaseGateOutcome::Ready)
        );
    }

    #[test]
    fn hold_combo_matches_in_both_entry_orders_and_waits_for_all_modifiers() {
        use std::collections::VecDeque;

        let mut rule = pending_trigger_task(&test_hook_service()).rule;
        rule.mode = MacroMode::Hold;
        rule.trigger_keys = vec!["Ctrl".into(), "Shift".into(), "7".into()];
        let pressed = HashSet::from([0x11, 0x10, 0x37]);
        assert!(super::macro_rule_matches_key_down(&rule, 0x37, &pressed));
        assert!(super::macro_rule_matches_key_down(&rule, 0x11, &pressed));

        let mut samples = VecDeque::from([
            super::HoldModifierReleaseSample {
                elapsed: std::time::Duration::ZERO,
                owner_down: true,
                any_modifier_down: true,
                status: super::TriggerReleaseGateStatus::Current,
            },
            // Only one modifier was released; the aggregate remains down.
            super::HoldModifierReleaseSample {
                elapsed: std::time::Duration::from_millis(12),
                owner_down: true,
                any_modifier_down: true,
                status: super::TriggerReleaseGateStatus::Current,
            },
            super::HoldModifierReleaseSample {
                elapsed: std::time::Duration::from_millis(20),
                owner_down: true,
                any_modifier_down: false,
                status: super::TriggerReleaseGateStatus::Current,
            },
            // Re-press resets the 18 ms stability window.
            super::HoldModifierReleaseSample {
                elapsed: std::time::Duration::from_millis(30),
                owner_down: true,
                any_modifier_down: true,
                status: super::TriggerReleaseGateStatus::Current,
            },
            super::HoldModifierReleaseSample {
                elapsed: std::time::Duration::from_millis(40),
                owner_down: true,
                any_modifier_down: false,
                status: super::TriggerReleaseGateStatus::Current,
            },
            super::HoldModifierReleaseSample {
                elapsed: std::time::Duration::from_millis(58),
                owner_down: true,
                any_modifier_down: false,
                status: super::TriggerReleaseGateStatus::Current,
            },
            super::HoldModifierReleaseSample {
                elapsed: std::time::Duration::from_millis(59),
                owner_down: true,
                any_modifier_down: false,
                status: super::TriggerReleaseGateStatus::Current,
            },
        ]);
        assert_eq!(
            super::drive_hold_modifier_release_gate(
                true,
                super::TriggerReleaseGate::new(
                    std::time::Duration::from_millis(18),
                    std::time::Duration::from_secs(5),
                ),
                || samples.pop_front().expect("Hold gate sample"),
                |_| {},
            ),
            super::TriggerReleaseGateOutcome::Ready
        );
        assert!(samples.is_empty());
    }

    #[test]
    fn hold_owner_release_is_terminal_but_owner_only_hold_is_immediate() {
        use std::collections::VecDeque;

        let mut released = VecDeque::from([
            super::HoldModifierReleaseSample {
                elapsed: std::time::Duration::ZERO,
                owner_down: true,
                any_modifier_down: true,
                status: super::TriggerReleaseGateStatus::Current,
            },
            super::HoldModifierReleaseSample {
                elapsed: std::time::Duration::from_millis(3),
                owner_down: false,
                any_modifier_down: true,
                status: super::TriggerReleaseGateStatus::Current,
            },
            // A re-press must not create a new start in this epoch.
            super::HoldModifierReleaseSample {
                elapsed: std::time::Duration::from_millis(6),
                owner_down: true,
                any_modifier_down: false,
                status: super::TriggerReleaseGateStatus::Current,
            },
        ]);
        assert_eq!(
            super::drive_hold_modifier_release_gate(
                true,
                super::TriggerReleaseGate::new(
                    std::time::Duration::from_millis(18),
                    std::time::Duration::from_secs(5),
                ),
                || released.pop_front().expect("Hold release sample"),
                |_| {},
            ),
            super::TriggerReleaseGateOutcome::Cancelled(
                super::TriggerReleaseCancellation::HoldOwnerReleased
            )
        );
        assert_eq!(released.len(), 1);

        let mut samples = 0;
        assert_eq!(
            super::drive_hold_modifier_release_gate(
                false,
                super::TriggerReleaseGate::new(
                    std::time::Duration::from_millis(18),
                    std::time::Duration::from_secs(5),
                ),
                || {
                    samples += 1;
                    super::HoldModifierReleaseSample {
                        elapsed: std::time::Duration::ZERO,
                        owner_down: true,
                        any_modifier_down: false,
                        status: super::TriggerReleaseGateStatus::Current,
                    }
                },
                |_| {},
            ),
            super::TriggerReleaseGateOutcome::Ready
        );
        assert_eq!(samples, 1);
    }

    #[test]
    fn final_admission_repress_resets_stability_and_never_returns_ready() {
        use std::collections::VecDeque;

        let mut samples = VecDeque::from([
            super::TriggerReleaseSample {
                elapsed: std::time::Duration::ZERO,
                any_trigger_key_down: true,
                status: super::TriggerReleaseGateStatus::Current,
            },
            super::TriggerReleaseSample {
                elapsed: std::time::Duration::from_millis(20),
                any_trigger_key_down: false,
                status: super::TriggerReleaseGateStatus::Current,
            },
            super::TriggerReleaseSample {
                elapsed: std::time::Duration::from_millis(38),
                any_trigger_key_down: false,
                status: super::TriggerReleaseGateStatus::Current,
            },
            // This is the final-admission resample after the stable window.
            // Re-pressing F9 must reset the window, not return Ready.
            super::TriggerReleaseSample {
                elapsed: std::time::Duration::from_millis(39),
                any_trigger_key_down: true,
                status: super::TriggerReleaseGateStatus::Current,
            },
            // Prove the driver re-entered its wait loop. A later shutdown is
            // terminal, so the earlier Ready candidate cannot leak through.
            super::TriggerReleaseSample {
                elapsed: std::time::Duration::from_millis(40),
                any_trigger_key_down: false,
                status: super::TriggerReleaseGateStatus::Shutdown,
            },
        ]);
        let outcome = super::drive_trigger_release_gate(
            super::TriggerReleaseGate::new(
                std::time::Duration::from_millis(18),
                std::time::Duration::from_secs(5),
            ),
            || samples.pop_front().expect("deterministic gate sample"),
            |_| {},
        );
        assert_eq!(outcome, super::TriggerReleaseGateOutcome::Shutdown);
        assert!(samples.is_empty());
    }

    fn pending_trigger_task(service: &HookService) -> super::MacroTriggerTask {
        super::MacroTriggerTask {
            rule: MacroRule {
                id: "pending-gate".into(),
                name: "pending-gate".into(),
                import_error: None,
                enabled: true,
                trigger_keys: vec!["Ctrl".into(), "F9".into()],
                mode: MacroMode::Repeat,
                repeat_count: 7,
                speed: 1.0,
                record_mouse_move: true,
                record_mouse_clicks: true,
                target: None,
                behavior_policy: None,
                program: AutomationProgram::Macro { steps: Vec::new() },
            },
            start_timing: super::TriggerStartTiming::ReleaseGated,
            hold_epoch: None,
            trigger_vk: Some(0x78),
            hold_owner_vk: None,
            hold_modifier_vks: Vec::new(),
            generation: service.shared.emergency_generation.load(Ordering::Acquire),
            controller_generation: service.shared.controller.generation(),
            admission_revision: service.shared.controller.background_generation(),
            config_revision: service
                .shared
                .trigger_config_revision
                .load(Ordering::Acquire),
        }
    }

    fn trigger_config_fixture() -> AppConfig {
        AppConfig {
            macros: vec![MacroRule {
                id: "revision-gate".into(),
                name: "revision-gate".into(),
                import_error: None,
                enabled: true,
                trigger_keys: vec!["Ctrl".into(), "F9".into()],
                mode: MacroMode::Once,
                repeat_count: 1,
                speed: 1.0,
                record_mouse_move: true,
                record_mouse_clicks: true,
                target: None,
                behavior_policy: None,
                program: AutomationProgram::Macro {
                    steps: vec![MacroStep::Delay {
                        duration_ms: 10,
                        duration_max_ms: None,
                    }],
                },
            }],
            ..AppConfig::default()
        }
    }

    fn configured_trigger_task(service: &HookService) -> super::MacroTriggerTask {
        let rule = service.shared.config.lock().expect("config").macros[0].clone();
        let start_timing = super::trigger_start_timing(&rule);
        let hold_parts = crate::config::hold_trigger_parts(&rule.trigger_keys);
        let hold_epoch = matches!(start_timing, super::TriggerStartTiming::HoldModifierRelease)
            .then(|| {
                service
                    .shared
                    .publish_hold_lifecycle(
                        &rule,
                        hold_parts.as_ref().expect("valid Hold trigger").owner_vk,
                    )
                    .expect("publish test Hold lifecycle")
            });
        super::MacroTriggerTask {
            rule,
            start_timing,
            hold_epoch,
            trigger_vk: Some(0x78),
            hold_owner_vk: hold_parts.as_ref().map(|parts| parts.owner_vk),
            hold_modifier_vks: hold_parts
                .map(|parts| parts.modifier_vks)
                .unwrap_or_default(),
            generation: service.shared.emergency_generation.load(Ordering::Acquire),
            controller_generation: service.shared.controller.generation(),
            admission_revision: service.shared.controller.background_generation(),
            config_revision: service
                .shared
                .trigger_config_revision
                .load(Ordering::Acquire),
        }
    }

    fn publish_test_hold_lifecycle(service: &HookService, rule: &MacroRule) -> u64 {
        let owner_vk = crate::config::hold_trigger_parts(&rule.trigger_keys)
            .expect("valid test Hold trigger")
            .owner_vk;
        service
            .shared
            .publish_hold_lifecycle(rule, owner_vk)
            .expect("publish test Hold lifecycle")
    }

    fn assert_trigger_config_change_cancels(mut change: impl FnMut(&mut AppConfig)) {
        let service = HookService::isolated(
            trigger_config_fixture(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let task = configured_trigger_task(&service);
        service
            .shared
            .validate_pending_trigger_config(&task.rule, task.config_revision)
            .expect("initial trigger permission");
        let mut next = service.shared.config.lock().expect("config").clone();
        change(&mut next);
        service.update_config(next).expect("valid config update");
        assert_eq!(
            super::trigger_release_gate_status(&service.shared, &task),
            super::TriggerReleaseGateStatus::Cancelled(
                super::TriggerReleaseCancellation::ConfigRevision
            )
        );
        assert_eq!(service.shared.injected_input.counts(), (0, 0));
        assert_eq!(
            service
                .shared
                .validate_pending_trigger_config(&task.rule, task.config_revision)
                .expect_err("final admission must reject stale config")
                .code,
            "macro_trigger_config_stale"
        );
    }

    #[test]
    fn permission_relevant_config_changes_cancel_pending_trigger() {
        assert_trigger_config_change_cancels(|config| config.global_enabled = false);
        assert_trigger_config_change_cancels(|config| config.macros[0].enabled = false);
        assert_trigger_config_change_cancels(|config| config.macros.clear());
        assert_trigger_config_change_cancels(|config| {
            config.macros[0].trigger_keys = vec!["Ctrl".into(), "F10".into()]
        });
        assert_trigger_config_change_cancels(|config| config.macros[0].mode = MacroMode::Toggle);
        assert_trigger_config_change_cancels(|config| {
            config.macros[0].program = AutomationProgram::Macro {
                steps: vec![MacroStep::Delay {
                    duration_ms: 11,
                    duration_max_ms: None,
                }],
            }
        });
        assert_trigger_config_change_cancels(|config| config.emergency_stop = "F11".into());
        assert_trigger_config_change_cancels(|config| {
            config.macros[0].trigger_keys = vec!["Ctrl".into(), "F8".into()];
            config.macros[0].name = "连点器".into();
            config.macros[0].mode = MacroMode::Toggle;
        });
    }

    #[test]
    fn equivalent_and_non_trigger_config_updates_preserve_pending_revision() {
        let service = HookService::isolated(
            trigger_config_fixture(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let task = configured_trigger_task(&service);
        let unchanged = service.shared.config.lock().expect("config").clone();
        service
            .update_config(unchanged.clone())
            .expect("identical refresh");
        assert_eq!(
            super::trigger_release_gate_status(&service.shared, &task),
            super::TriggerReleaseGateStatus::Current
        );

        let mut presentation_only = unchanged;
        presentation_only.navigation_auto_collapse = !presentation_only.navigation_auto_collapse;
        presentation_only.show_playback_overlay = !presentation_only.show_playback_overlay;
        let mut disabled = presentation_only.macros[0].clone();
        disabled.id = "disabled-editor-only".into();
        disabled.enabled = false;
        disabled.trigger_keys = vec!["F7".into()];
        presentation_only.macros.push(disabled);
        service
            .update_config(presentation_only)
            .expect("presentation-only refresh");
        assert_eq!(
            super::trigger_release_gate_status(&service.shared, &task),
            super::TriggerReleaseGateStatus::Current
        );
        service
            .shared
            .validate_pending_trigger_config(&task.rule, task.config_revision)
            .expect("same trigger permission remains admitted");
    }

    #[test]
    fn legacy_clicker_keeps_raw_config_identity_and_uses_normalized_execution() {
        let raw_rule = MacroRule {
            id: "legacy-clicker".into(),
            name: "连点器".into(),
            import_error: None,
            enabled: true,
            trigger_keys: vec!["Ctrl".into(), "F8".into()],
            mode: MacroMode::Once,
            repeat_count: 1,
            speed: 1.0,
            record_mouse_move: true,
            record_mouse_clicks: true,
            target: None,
            behavior_policy: None,
            program: AutomationProgram::Macro { steps: Vec::new() },
        };
        let execution_rule = super::normalize_playback_rule(raw_rule.clone());
        assert!(matches!(execution_rule.mode, MacroMode::Toggle));
        assert_eq!(
            execution_rule
                .macro_steps()
                .expect("normalized steps")
                .len(),
            4
        );
        assert_ne!(
            super::trigger_macro_snapshot(&raw_rule),
            super::trigger_macro_snapshot(&execution_rule),
            "execution normalization must not become config identity"
        );

        let service = test_hook_service();
        service
            .update_config(AppConfig {
                macros: vec![raw_rule.clone()],
                ..AppConfig::default()
            })
            .expect("install legacy clicker");
        let task = configured_trigger_task(&service);
        service
            .shared
            .validate_pending_trigger_config(&raw_rule, task.config_revision)
            .expect("unchanged raw config identity remains admitted");
        assert_eq!(
            service
                .shared
                .validate_pending_trigger_config(&execution_rule, task.config_revision)
                .expect_err("normalized execution clone is not config identity")
                .code,
            "macro_trigger_config_stale"
        );

        let reached_post_normalization = Arc::new(AtomicBool::new(false));
        let reached_for_hook = Arc::clone(&reached_post_normalization);
        let weak = Arc::downgrade(&service.shared);
        *service
            .shared
            .trigger_admission_test_hook
            .lock()
            .expect("test hook") = Some(Arc::new(move |checkpoint| {
            if checkpoint != super::TriggerAdmissionCheckpoint::BeforeActivation
                || reached_for_hook.swap(true, Ordering::AcqRel)
            {
                return;
            }
            let shared = weak.upgrade().expect("service remains alive");
            let mut config = shared.config.lock().expect("config");
            config.macros[0].speed = 2.0;
            shared
                .advance_trigger_config_revision()
                .expect("changed clicker config revision");
        }));
        let error = service
            .shared
            .start_hotkey_playback_at_revision(
                task.rule,
                task.generation,
                task.admission_revision,
                task.config_revision,
                None,
            )
            .expect_err("changed config must cancel before activation");
        assert!(reached_post_normalization.load(Ordering::Acquire));
        assert_eq!(error.code, "macro_trigger_config_stale");
        assert_eq!(
            service.shared.controller.phase(),
            crate::runtime_control::RuntimePhase::Idle
        );
        assert_eq!(service.shared.injected_input.counts(), (0, 0));
        *service
            .shared
            .trigger_admission_test_hook
            .lock()
            .expect("test hook") = None;
    }

    #[test]
    fn config_revision_changes_in_final_start_windows_never_activate_playback() {
        fn assert_rejected_at(checkpoint: super::TriggerAdmissionCheckpoint) {
            let service = test_hook_service();
            service
                .update_config(trigger_config_fixture())
                .expect("install trigger config");
            let task = configured_trigger_task(&service);
            let fired = Arc::new(AtomicBool::new(false));
            let fired_for_hook = Arc::clone(&fired);
            let weak = Arc::downgrade(&service.shared);
            *service
                .shared
                .trigger_admission_test_hook
                .lock()
                .expect("test hook") = Some(Arc::new(move |observed| {
                if observed != checkpoint || fired_for_hook.swap(true, Ordering::AcqRel) {
                    return;
                }
                let shared = weak.upgrade().expect("service remains alive");
                let mut current = shared.config.lock().expect("config");
                let mut changed = current.clone();
                changed.macros[0].program = AutomationProgram::Macro {
                    steps: vec![MacroStep::Delay {
                        duration_ms: 11,
                        duration_max_ms: None,
                    }],
                };
                shared
                    .advance_trigger_config_revision()
                    .expect("permission-relevant config revision");
                *current = changed;
            }));

            let error = service
                .shared
                .start_hotkey_playback_at_revision(
                    task.rule,
                    task.generation,
                    task.admission_revision,
                    task.config_revision,
                    None,
                )
                .expect_err("stale hotkey start must not activate");
            assert_eq!(
                error.code, "macro_trigger_config_stale",
                "checkpoint: {checkpoint:?}"
            );
            assert!(fired.load(Ordering::Acquire));
            assert_eq!(
                service.shared.controller.phase(),
                crate::runtime_control::RuntimePhase::Idle
            );
            assert!(!service.shared.playback.lock().expect("playback").running);
            assert_eq!(
                service
                    .shared
                    .playback_instance_counter
                    .load(Ordering::Acquire),
                0
            );
            assert!(service
                .shared
                .playback_inputs
                .lock()
                .expect("playback inputs")
                .is_empty());
            assert_eq!(service.shared.injected_input.counts(), (0, 0));
            *service
                .shared
                .trigger_admission_test_hook
                .lock()
                .expect("test hook") = None;
        }

        assert_rejected_at(super::TriggerAdmissionCheckpoint::AfterInitialValidation);
        assert_rejected_at(super::TriggerAdmissionCheckpoint::BeforeActivation);
    }

    #[test]
    fn clicker_identity_is_trigger_permission_relevant() {
        let mut ordinary = trigger_config_fixture();
        ordinary.macros[0].trigger_keys = vec!["Ctrl".into(), "F8".into()];
        ordinary.macros[0].mode = MacroMode::Toggle;
        let mut clicker = ordinary.clone();
        clicker.macros[0].name = "连点器".into();
        assert_ne!(
            super::trigger_permission_snapshot(&ordinary),
            super::trigger_permission_snapshot(&clicker)
        );
    }

    #[test]
    fn trigger_config_revision_exhaustion_fails_closed_without_wrap() {
        let service = HookService::isolated(
            trigger_config_fixture(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let original = service.shared.config.lock().expect("config").clone();
        service
            .shared
            .trigger_config_revision
            .store(u64::MAX, Ordering::Release);
        let mut changed = original.clone();
        changed.global_enabled = false;
        let error = service
            .update_config(changed)
            .expect_err("revision exhaustion must reject update");
        assert_eq!(error.code, "trigger_config_revision_exhausted");
        assert!(service
            .shared
            .trigger_config_revision_exhausted
            .load(Ordering::Acquire));
        assert_eq!(
            service
                .shared
                .trigger_config_revision
                .load(Ordering::Acquire),
            u64::MAX
        );
        assert!(service.shared.config.lock().expect("config").global_enabled);
        assert_eq!(
            service.shared.controller.phase(),
            crate::runtime_control::RuntimePhase::FaultLocked
        );
    }

    #[test]
    fn f12_controller_generation_race_cancels_pending_gate_and_revokes_old_token() {
        let service = test_hook_service();
        let mut lease = service
            .shared
            .controller
            .begin_start(None)
            .expect("start token");
        let old_token = lease.token();
        assert!(service.shared.controller.activate(old_token));
        lease.commit();
        assert!(service.shared.controller.input_allowed(old_token));
        let task = pending_trigger_task(&service);
        assert_eq!(
            super::trigger_release_gate_status(&service.shared, &task),
            super::TriggerReleaseGateStatus::Current
        );

        // Model the exact request_emergency_stop interval where the controller
        // changed but emergency_generation has not yet been published.
        let before = service.shared.controller.generation();
        assert!(service.shared.controller.request_stop() > before);
        assert!(!service.shared.controller.input_allowed(old_token));
        assert_eq!(
            super::trigger_release_gate_status(&service.shared, &task),
            super::TriggerReleaseGateStatus::Cancelled(
                super::TriggerReleaseCancellation::ControllerGeneration
            )
        );
        let mut gate = super::TriggerReleaseGate::new(
            std::time::Duration::from_millis(18),
            std::time::Duration::from_secs(5),
        );
        assert_eq!(
            gate.observe(
                std::time::Duration::from_millis(30),
                false,
                super::trigger_release_gate_status(&service.shared, &task),
            ),
            Some(super::TriggerReleaseGateOutcome::Cancelled(
                super::TriggerReleaseCancellation::ControllerGeneration
            ))
        );

        let mut hold_config = trigger_config_fixture();
        hold_config.macros[0].mode = MacroMode::Hold;
        let hold_service = HookService::isolated(
            hold_config,
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let hold_task = configured_trigger_task(&hold_service);
        hold_service.shared.controller.request_stop();
        assert_eq!(
            super::trigger_release_gate_status(&hold_service.shared, &hold_task),
            super::TriggerReleaseGateStatus::Cancelled(
                super::TriggerReleaseCancellation::ControllerGeneration
            )
        );
        hold_service
            .shared
            .retire_hold_lifecycle(hold_task.hold_epoch.expect("Hold epoch"));
    }

    #[test]
    fn release_gate_distinguishes_timeout_shutdown_and_stale_revisions() {
        let mut gate = super::TriggerReleaseGate::new(
            std::time::Duration::from_millis(18),
            std::time::Duration::from_millis(50),
        );
        assert_eq!(
            gate.observe(
                std::time::Duration::from_millis(50),
                true,
                super::TriggerReleaseGateStatus::Current,
            ),
            Some(super::TriggerReleaseGateOutcome::Timeout)
        );

        let service = test_hook_service();
        let task = pending_trigger_task(&service);
        service.shared.controller.invalidate_background_admission();
        assert_eq!(
            super::trigger_release_gate_status(&service.shared, &task),
            super::TriggerReleaseGateStatus::Cancelled(
                super::TriggerReleaseCancellation::AdmissionRevision
            )
        );

        let service = test_hook_service();
        let task = pending_trigger_task(&service);
        service
            .shared
            .emergency_generation
            .fetch_add(1, Ordering::AcqRel);
        assert_eq!(
            super::trigger_release_gate_status(&service.shared, &task),
            super::TriggerReleaseGateStatus::Cancelled(
                super::TriggerReleaseCancellation::TaskGeneration
            )
        );

        let service = test_hook_service();
        let task = pending_trigger_task(&service);
        service.shared.shutdown.store(true, Ordering::Release);
        assert_eq!(
            super::trigger_release_gate_status(&service.shared, &task),
            super::TriggerReleaseGateStatus::Shutdown
        );
    }

    #[test]
    fn pending_claim_clears_on_every_gate_exit_and_can_be_reclaimed() {
        let terminal_statuses = [
            super::TriggerReleaseGateStatus::Current,
            super::TriggerReleaseGateStatus::Shutdown,
            super::TriggerReleaseGateStatus::Cancelled(
                super::TriggerReleaseCancellation::TaskGeneration,
            ),
            super::TriggerReleaseGateStatus::Cancelled(
                super::TriggerReleaseCancellation::ControllerGeneration,
            ),
            super::TriggerReleaseGateStatus::Cancelled(
                super::TriggerReleaseCancellation::AdmissionRevision,
            ),
            super::TriggerReleaseGateStatus::Cancelled(
                super::TriggerReleaseCancellation::ConfigRevision,
            ),
            super::TriggerReleaseGateStatus::Cancelled(
                super::TriggerReleaseCancellation::PhysicalLedgerUncertain,
            ),
        ];
        for status in terminal_statuses {
            let pending = AtomicBool::new(true);
            {
                let _reset = super::TriggerPendingReset(&pending);
                let mut gate = super::TriggerReleaseGate::new(
                    std::time::Duration::from_millis(18),
                    std::time::Duration::from_millis(10),
                );
                assert!(gate
                    .observe(std::time::Duration::from_millis(10), true, status)
                    .is_some());
                assert!(pending.load(Ordering::Acquire));
            }
            assert!(!pending.load(Ordering::Acquire));
            assert!(pending
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok());
        }
    }

    #[test]
    fn pending_hold_lifecycle_clears_on_terminal_gate_exit_and_can_retrigger() {
        let terminal_statuses = [
            super::TriggerReleaseGateStatus::Current,
            super::TriggerReleaseGateStatus::Shutdown,
            super::TriggerReleaseGateStatus::Cancelled(
                super::TriggerReleaseCancellation::TaskGeneration,
            ),
            super::TriggerReleaseGateStatus::Cancelled(
                super::TriggerReleaseCancellation::ControllerGeneration,
            ),
            super::TriggerReleaseGateStatus::Cancelled(
                super::TriggerReleaseCancellation::AdmissionRevision,
            ),
            super::TriggerReleaseGateStatus::Cancelled(
                super::TriggerReleaseCancellation::ConfigRevision,
            ),
            super::TriggerReleaseGateStatus::Cancelled(
                super::TriggerReleaseCancellation::PhysicalLedgerUncertain,
            ),
        ];
        for status in terminal_statuses {
            let mut config = trigger_config_fixture();
            config.macros[0].mode = MacroMode::Hold;
            let service = HookService::isolated(
                config,
                crate::automation::VisionService::new(std::env::temp_dir()),
            );
            let task = configured_trigger_task(&service);
            service
                .shared
                .trigger_pending
                .store(true, Ordering::Release);
            {
                let _pending_reset = super::TriggerPendingReset(&service.shared.trigger_pending);
                let _hold_reset = super::PendingHoldLifecycleReset {
                    shared: &service.shared,
                    epoch: task.hold_epoch,
                    transferred_to_playback: false,
                };
                let mut gate = super::TriggerReleaseGate::new(
                    std::time::Duration::from_millis(18),
                    std::time::Duration::from_millis(10),
                );
                assert!(gate
                    .observe(std::time::Duration::from_millis(10), true, status)
                    .is_some());
            }
            assert!(!service.shared.trigger_pending.load(Ordering::Acquire));
            assert_eq!(
                service.shared.hold_lifecycle_epoch.load(Ordering::Acquire),
                0
            );
            let rule = service.shared.config.lock().expect("config").macros[0].clone();
            let epoch = publish_test_hold_lifecycle(&service, &rule);
            service.shared.retire_hold_lifecycle(epoch);
        }
    }

    #[test]
    fn hold_submits_immediately_while_existing_toggle_and_release_gate_semantics_remain() {
        use super::HotkeyPlaybackAction::{
            Ignore, StartAfterRelease, StartImmediate, StopImmediate,
        };

        assert_eq!(
            super::hotkey_playback_action(MacroMode::Toggle, false),
            StartAfterRelease
        );
        assert_eq!(
            super::hotkey_playback_action(MacroMode::Toggle, true),
            StopImmediate
        );
        // The clicker is a Toggle and uses this same dispatch decision.
        assert_eq!(
            super::hotkey_playback_action(MacroMode::Toggle, true),
            StopImmediate
        );
        assert_eq!(
            super::hotkey_playback_action(MacroMode::Repeat, false),
            StartAfterRelease
        );
        assert_eq!(
            super::hotkey_playback_action(MacroMode::Repeat, true),
            Ignore
        );
        assert_eq!(
            super::hotkey_playback_action(MacroMode::Hold, false),
            StartImmediate
        );
        assert_eq!(super::hotkey_playback_action(MacroMode::Hold, true), Ignore);

        let service = test_hook_service();
        let mut task = pending_trigger_task(&service);
        assert_eq!(
            task.rule.repeat_count, 7,
            "gate must not alter Repeat count"
        );
        task.rule.mode = MacroMode::Hold;
        assert_eq!(
            super::trigger_start_timing(&task.rule),
            super::TriggerStartTiming::HoldModifierRelease
        );
        assert_eq!(service.shared.injected_input.counts(), (0, 0));
    }

    #[test]
    fn hold_diagnostics_are_correlatable_and_do_not_include_typed_content() {
        let fields = super::hold_diagnostic_fields(
            Some("hold-diagnostic-fixture"),
            "input_permission_revoked",
            Some(17),
            Some(crate::runtime_control::RunToken {
                id: 23,
                generation: 29,
            }),
            Some(0x41),
            Some(0x41),
            Some(0x41),
        )
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();

        assert_eq!(fields.get("mode").map(String::as_str), Some("hold"));
        assert_eq!(
            fields.get("source").map(String::as_str),
            Some("low_level_keyboard")
        );
        assert_eq!(
            fields.get("decision").map(String::as_str),
            Some("input_permission_revoked")
        );
        assert_eq!(fields.get("hold_epoch").map(String::as_str), Some("17"));
        assert_eq!(fields.get("run_id").map(String::as_str), Some("23"));
        assert_eq!(fields.get("run_generation").map(String::as_str), Some("29"));
        assert_eq!(fields.get("canonical_vk").map(String::as_str), Some("0x41"));
        assert_eq!(fields.get("owner_vk").map(String::as_str), Some("0x41"));
        assert_eq!(fields.get("released_vk").map(String::as_str), Some("0x41"));
        assert!(!fields.contains_key("key"));
        assert!(!fields.contains_key("text"));
        assert!(!fields.contains_key("content"));
    }

    #[test]
    fn direct_ui_play_never_claims_the_hotkey_release_gate() {
        let service = test_hook_service();
        let task = pending_trigger_task(&service);
        assert!(!service.shared.trigger_pending.load(Ordering::Acquire));
        let _ = service.play_with_generation(
            task.rule,
            service.service_generation(),
            service.service_admission_revision(),
        );
        assert!(!service.shared.trigger_pending.load(Ordering::Acquire));
    }

    #[test]
    fn hold_keydown_submits_immediately_but_only_owner_keyup_cancels_pending() {
        let mut config = trigger_config_fixture();
        config.macros[0].mode = MacroMode::Hold;
        let service = HookService::isolated(
            config.clone(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        let worker = crate::bounded_worker::BoundedWorker::spawn(
            "input-free-hold-immediate-capture",
            1,
            move |task| tx.send(task).expect("capture Hold task"),
        )
        .expect("capture worker");
        service
            .shared
            .trigger_tasks
            .set(worker)
            .map_err(drop)
            .expect("set capture worker");

        let revision = service
            .shared
            .trigger_config_revision
            .load(Ordering::Acquire);
        let pressed = HashSet::from([0x11, 0x78]);
        assert!(super::process_macro_key_down(
            &service.shared,
            &config,
            revision,
            0x78,
            false,
            &pressed,
        ));
        let task = rx
            .recv_timeout(std::time::Duration::from_millis(500))
            .expect("Hold keydown must submit without waiting for keyup");
        assert_eq!(
            task.start_timing,
            super::TriggerStartTiming::HoldModifierRelease
        );
        assert_eq!(
            super::trigger_release_gate_status(&service.shared, &task),
            super::TriggerReleaseGateStatus::Current
        );

        assert!(super::process_macro_key_down(
            &service.shared,
            &config,
            revision,
            0x78,
            true,
            &pressed,
        ));
        assert!(matches!(
            rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));

        assert!(!service.shared.revoke_current_hold_for_key_up(0x11));
        assert_eq!(
            super::trigger_release_gate_status(&service.shared, &task),
            super::TriggerReleaseGateStatus::Current
        );
        assert!(service.shared.revoke_current_hold_for_key_up(0x78));
        assert_eq!(
            super::trigger_release_gate_status(&service.shared, &task),
            super::TriggerReleaseGateStatus::Cancelled(
                super::TriggerReleaseCancellation::HoldLifecycle
            )
        );
        service
            .shared
            .retire_hold_lifecycle(task.hold_epoch.expect("Hold epoch"));
        service
            .shared
            .trigger_pending
            .store(false, Ordering::Release);
        service.shared.trigger_tasks.get().expect("worker").close();
    }

    #[test]
    fn hold_owner_keydown_suppression_survives_modifier_release_and_active_transition() {
        let mut config = trigger_config_fixture();
        config.macros[0].mode = MacroMode::Hold;
        let service = HookService::isolated(
            config.clone(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let epoch = publish_test_hold_lifecycle(&service, &config.macros[0]);

        assert!(service.shared.owns_current_hold_owner_keydown(0x78));
        assert!(!service.shared.owns_current_hold_owner_keydown(0x11));
        assert!(!service.shared.owns_current_hold_owner_keydown(0x77));
        assert!(!service.shared.revoke_current_hold_for_key_up(0x11));
        assert!(
            service.shared.owns_current_hold_owner_keydown(0x78),
            "owner autorepeat remains consumed after Ctrl release"
        );

        let mut lease = service
            .shared
            .controller
            .begin_start(None)
            .expect("admit Hold");
        let token = lease.token();
        service
            .shared
            .bind_hold_lifecycle_run(&config.macros[0], Some(epoch), token)
            .expect("bind Hold");
        assert!(service.shared.controller.activate(token));
        lease.commit();
        service
            .shared
            .activate_hold_lifecycle(&config.macros[0], Some(epoch), token, 1)
            .expect("activate Hold");
        assert!(service.shared.owns_current_hold_owner_keydown(0x78));

        assert!(service.shared.revoke_current_hold_for_key_up(0x78));
        assert!(!service.shared.owns_current_hold_owner_keydown(0x78));
        assert!(!service.shared.controller.input_allowed(token));
        service.shared.retire_hold_lifecycle(epoch);
        let _ = service.shared.controller.begin_cleaning(token);
        let _ = service.shared.controller.finish(token, true);
    }

    #[test]
    fn hold_owner_suppression_ends_on_cancel_timeout_and_stale_repress() {
        let mut config = trigger_config_fixture();
        config.macros[0].mode = MacroMode::Hold;
        let service = HookService::isolated(
            config.clone(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let cancelled_epoch = publish_test_hold_lifecycle(&service, &config.macros[0]);
        assert!(service.shared.owns_current_hold_owner_keydown(0x78));
        assert!(service.shared.revoke_current_hold_for_key_up(0x78));
        assert!(
            !service.shared.owns_current_hold_owner_keydown(0x78),
            "a re-press cannot be consumed as ownership of the cancelled epoch"
        );
        service.shared.retire_hold_lifecycle(cancelled_epoch);
        assert!(!service.shared.owns_current_hold_owner_keydown(0x78));

        let timeout_epoch = publish_test_hold_lifecycle(&service, &config.macros[0]);
        assert!(service.shared.owns_current_hold_owner_keydown(0x78));
        {
            let _timeout_cleanup = super::PendingHoldLifecycleReset {
                shared: &service.shared,
                epoch: Some(timeout_epoch),
                transferred_to_playback: false,
            };
        }
        assert!(!service.shared.owns_current_hold_owner_keydown(0x78));

        let fresh_epoch = publish_test_hold_lifecycle(&service, &config.macros[0]);
        assert!(fresh_epoch > timeout_epoch);
        assert!(service.shared.owns_current_hold_owner_keydown(0x78));
        service.shared.retire_hold_lifecycle(fresh_epoch);
        assert!(!service.shared.owns_current_hold_owner_keydown(0x78));
    }

    #[test]
    fn active_hold_ignores_modifier_keyup_and_stops_on_owner_keyup() {
        let mut config = trigger_config_fixture();
        config.macros[0].mode = MacroMode::Hold;
        let service = HookService::isolated(
            config.clone(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let epoch = publish_test_hold_lifecycle(&service, &config.macros[0]);
        let mut lease = service
            .shared
            .controller
            .begin_start(None)
            .expect("admit active Hold");
        let token = lease.token();
        service
            .shared
            .bind_hold_lifecycle_run(&config.macros[0], Some(epoch), token)
            .expect("bind Hold token");
        assert!(service.shared.controller.activate(token));
        lease.commit();
        service
            .shared
            .activate_hold_lifecycle(&config.macros[0], Some(epoch), token, 1)
            .expect("activate Hold identity");
        let playback_guard = service.shared.playback.lock().expect("playback lock");
        assert!(!service.shared.revoke_current_hold_for_key_up(0x11));
        assert!(service.shared.controller.input_allowed(token));
        drop(playback_guard);
        service.shared.playback.lock().expect("playback").running = true;
        let revision = service
            .shared
            .trigger_config_revision
            .load(Ordering::Acquire);
        assert!(super::process_macro_key_down(
            &service.shared,
            &config,
            revision,
            0x11,
            false,
            &HashSet::from([0x11, 0x78]),
        ));
        assert!(service.shared.controller.input_allowed(token));
        assert!(service.shared.revoke_current_hold_for_key_up(0x78));
        assert!(!service.shared.controller.input_allowed(token));
        service.shared.playback.lock().expect("playback").running = false;
        service.shared.retire_hold_lifecycle(epoch);
        let _ = service.shared.controller.begin_cleaning(token);
        let _ = service.shared.controller.finish(token, true);

        for unrelated_mode in [MacroMode::Once, MacroMode::Repeat, MacroMode::Toggle] {
            let mut config = trigger_config_fixture();
            config.macros[0].mode = MacroMode::Hold;
            let service = HookService::isolated(
                config,
                crate::automation::VisionService::new(std::env::temp_dir()),
            );
            let mut lease = service
                .shared
                .controller
                .begin_start(None)
                .expect("admit unrelated playback");
            let token = lease.token();
            assert!(service.shared.controller.activate(token));
            lease.commit();
            {
                let mut playback = service.shared.playback.lock().expect("playback lock");
                playback.running = true;
                playback.macro_id = Some(format!("unrelated-{unrelated_mode:?}"));
                assert!(!service.shared.revoke_current_hold_for_key_up(0x78));
                assert!(
                    service.shared.controller.input_allowed(token),
                    "configured Hold keyup must not stop unrelated {unrelated_mode:?}"
                );
            }
            service.shared.controller.request_stop();
            let _ = service.shared.controller.begin_cleaning(token);
            let _ = service.shared.controller.finish(token, true);
        }
    }

    #[test]
    fn hold_config_change_cancels_stale_modifier_release_task() {
        let mut config = trigger_config_fixture();
        config.macros[0].mode = MacroMode::Hold;
        let service = HookService::isolated(
            config,
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let task = configured_trigger_task(&service);
        assert_eq!(
            task.start_timing,
            super::TriggerStartTiming::HoldModifierRelease
        );
        {
            let mut current = service.shared.config.lock().expect("config");
            current.macros[0].enabled = false;
            service
                .shared
                .advance_trigger_config_revision()
                .expect("disable Hold");
        }
        assert_eq!(
            super::trigger_release_gate_status(&service.shared, &task),
            super::TriggerReleaseGateStatus::Cancelled(
                super::TriggerReleaseCancellation::ConfigRevision
            )
        );
        service
            .shared
            .retire_hold_lifecycle(task.hold_epoch.expect("Hold epoch"));
    }

    #[test]
    fn active_hold_uses_original_identity_after_disable_delete_or_rekey() {
        for mutation in 0..3 {
            let mut config = trigger_config_fixture();
            config.macros[0].mode = MacroMode::Hold;
            let service = HookService::isolated(
                config.clone(),
                crate::automation::VisionService::new(std::env::temp_dir()),
            );
            let rule = config.macros[0].clone();
            let epoch = publish_test_hold_lifecycle(&service, &rule);
            let mut lease = service
                .shared
                .controller
                .begin_start(None)
                .expect("admit Hold");
            let token = lease.token();
            service
                .shared
                .bind_hold_lifecycle_run(&rule, Some(epoch), token)
                .expect("bind Hold");
            assert!(service.shared.controller.activate(token));
            lease.commit();
            service
                .shared
                .activate_hold_lifecycle(&rule, Some(epoch), token, 41)
                .expect("activate Hold identity");
            {
                let mut current = service.shared.config.lock().expect("config");
                match mutation {
                    0 => current.macros[0].enabled = false,
                    1 => current.macros.clear(),
                    2 => current.macros[0].trigger_keys = vec!["F8".into()],
                    _ => unreachable!(),
                }
                service
                    .shared
                    .advance_trigger_config_revision()
                    .expect("mutate active Hold config");
            }
            let playback_guard = service.shared.playback.lock().expect("playback lock");
            assert!(service.shared.revoke_current_hold_for_key_up(0x78));
            assert!(!service.shared.controller.input_allowed(token));
            drop(playback_guard);
            service.shared.retire_hold_lifecycle(epoch);
            let _ = service.shared.controller.begin_cleaning(token);
            let _ = service.shared.controller.finish(token, true);
        }
    }

    #[test]
    fn pending_hold_identity_ignores_other_rules_and_modifier_releases() {
        let mut config = trigger_config_fixture();
        config.macros[0].mode = MacroMode::Hold;
        let rule_a = config.macros[0].clone();
        let mut rule_b = rule_a.clone();
        rule_b.id = "other-hold".into();
        rule_b.name = "other-hold".into();
        rule_b.trigger_keys = vec!["Ctrl".into(), "F8".into()];
        config.macros.push(rule_b);
        let service = HookService::isolated(
            config,
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let epoch = publish_test_hold_lifecycle(&service, &rule_a);

        assert!(!service.shared.revoke_current_hold_for_key_up(0x77));
        service
            .shared
            .validate_hold_lifecycle(&rule_a, Some(epoch))
            .expect("Hold B unique key must not cancel Hold A");
        assert!(!service.shared.revoke_current_hold_for_key_up(0x11));
        assert!(service.shared.revoke_current_hold_for_key_up(0x78));
        assert_eq!(
            service
                .shared
                .validate_hold_lifecycle(&rule_a, Some(epoch))
                .expect_err("owner F9 release cancels current Hold A")
                .code,
            "macro_hold_released"
        );
        service.shared.retire_hold_lifecycle(epoch);
    }

    #[test]
    fn hold_keyup_is_nonblocking_while_lifecycle_and_playback_locks_are_held() {
        let mut config = trigger_config_fixture();
        config.macros[0].mode = MacroMode::Hold;
        let service = HookService::isolated(
            config.clone(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let epoch = publish_test_hold_lifecycle(&service, &config.macros[0]);
        let lifecycle_guard = service
            .shared
            .hold_lifecycle
            .lock()
            .expect("lifecycle lock");
        let playback_guard = service.shared.playback.lock().expect("playback lock");
        let shared = Arc::clone(&service.shared);
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        let callback = std::thread::spawn(move || {
            done_tx
                .send(shared.revoke_current_hold_for_key_up(0x78))
                .expect("report Hold callback");
        });
        assert!(done_rx
            .recv_timeout(std::time::Duration::from_millis(500))
            .expect("Hold keyup callback must not block"));
        callback.join().expect("Hold callback");
        drop(playback_guard);
        drop(lifecycle_guard);
        assert_eq!(
            service
                .shared
                .validate_hold_lifecycle(&config.macros[0], Some(epoch))
                .expect_err("contended callback still cancels Hold")
                .code,
            "macro_hold_released"
        );
        service.shared.retire_hold_lifecycle(epoch);
    }

    #[test]
    fn hold_atomic_snapshot_rejects_mixed_epoch_phase_and_token_reads() {
        let old = crate::runtime_control::RunToken {
            id: 7,
            generation: 3,
        };
        let stable = super::consistent_hold_atomic_snapshot(11, true, 2, old, 2, 11)
            .expect("stable Bound snapshot");
        assert_eq!(stable.epoch, 11);
        assert!(stable.trigger_matches);
        assert_eq!(stable.bound_token, Some(old));

        // Retirement/replacement between dependent field loads must not
        // combine an old trigger bitmap with a new lifecycle token.
        let replacement = crate::runtime_control::RunToken {
            id: 8,
            generation: 3,
        };
        assert!(super::consistent_hold_atomic_snapshot(11, true, 2, replacement, 2, 12).is_none());
        // A Pending -> Bound publication observed halfway through is retried,
        // rather than accepting the token under the old phase.
        assert!(super::consistent_hold_atomic_snapshot(11, true, 1, old, 2, 11).is_none());
        assert!(super::consistent_hold_atomic_snapshot(11, true, 3, old, 0, 0).is_none());
    }

    #[test]
    fn contended_active_hold_keyup_revokes_the_exact_published_token() {
        let mut config = trigger_config_fixture();
        config.macros[0].mode = MacroMode::Hold;
        let service = HookService::isolated(
            config.clone(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let epoch = publish_test_hold_lifecycle(&service, &config.macros[0]);
        let mut lease = service
            .shared
            .controller
            .begin_start(None)
            .expect("admit Hold");
        let token = lease.token();
        service
            .shared
            .bind_hold_lifecycle_run(&config.macros[0], Some(epoch), token)
            .expect("bind Hold");
        assert!(service.shared.controller.activate(token));
        lease.commit();
        service
            .shared
            .activate_hold_lifecycle(&config.macros[0], Some(epoch), token, 9)
            .expect("activate Hold lifecycle");

        let lifecycle_guard = service
            .shared
            .hold_lifecycle
            .lock()
            .expect("lifecycle lock");
        assert!(service.shared.revoke_current_hold_for_key_up(0x78));
        assert!(service.shared.controller.token_revoked(token));
        drop(lifecycle_guard);
        assert!(!service.shared.controller.input_allowed(token));
        service.shared.retire_hold_lifecycle(epoch);
        let _ = service.shared.controller.begin_cleaning(token);
        let _ = service.shared.controller.finish(token, true);
    }

    #[test]
    fn hold_keyup_at_every_admission_checkpoint_never_leaves_permission_or_identity() {
        for checkpoint in [
            super::TriggerAdmissionCheckpoint::AfterInitialValidation,
            super::TriggerAdmissionCheckpoint::BeforeActivation,
            super::TriggerAdmissionCheckpoint::AfterControllerActivation,
            super::TriggerAdmissionCheckpoint::BeforeInputRegistration,
            super::TriggerAdmissionCheckpoint::BeforePlaybackPublication,
        ] {
            let mut config = trigger_config_fixture();
            config.macros[0].mode = MacroMode::Hold;
            let service = HookService::isolated(
                config,
                crate::automation::VisionService::new(std::env::temp_dir()),
            );
            service
                .shared
                .emergency_detector_ready
                .store(true, Ordering::Release);
            service
                .shared
                .emergency_thread_id
                .store(1, Ordering::Release);
            service.shared.emergency_heartbeat_ms.store(
                service.shared.emergency_clock.elapsed().as_millis() as u64 + 1,
                Ordering::Release,
            );
            let task = configured_trigger_task(&service);
            let epoch = task.hold_epoch.expect("Hold epoch");
            let reset = super::PendingHoldLifecycleReset {
                shared: &service.shared,
                epoch: Some(epoch),
                transferred_to_playback: false,
            };
            let weak = Arc::downgrade(&service.shared);
            *service
                .shared
                .trigger_admission_test_hook
                .lock()
                .expect("test hook") = Some(Arc::new(move |observed| {
                if observed == checkpoint {
                    let shared = weak.upgrade().expect("service remains alive");
                    assert!(shared.revoke_current_hold_for_key_up(0x78));
                }
            }));
            let error = service
                .shared
                .start_hotkey_playback_at_revision(
                    task.rule,
                    task.generation,
                    task.admission_revision,
                    task.config_revision,
                    task.hold_epoch,
                )
                .expect_err("checkpoint keyup must cancel Hold start");
            assert!(
                matches!(
                    error.code.as_str(),
                    "macro_hold_released" | "playback_cancelled"
                ),
                "checkpoint {checkpoint:?} returned {}",
                error.code
            );
            drop(reset);
            assert_eq!(
                service.shared.hold_lifecycle_epoch.load(Ordering::Acquire),
                0
            );
            assert!(!service
                .shared
                .controller
                .input_allowed(crate::runtime_control::RunToken {
                    id: 1,
                    generation: task.generation,
                }));
            assert_eq!(service.shared.tracked_input_counts(), (0, 0));
            assert!(service
                .shared
                .playback_inputs
                .lock()
                .expect("playback inputs")
                .is_empty());
        }
    }

    #[test]
    fn stale_hold_owner_cannot_clear_new_identity_and_epoch_exhaustion_fails_closed() {
        let mut config = trigger_config_fixture();
        config.macros[0].mode = MacroMode::Hold;
        let rule = config.macros[0].clone();
        let service = HookService::isolated(
            config,
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let old_epoch = publish_test_hold_lifecycle(&service, &rule);
        service.shared.retire_hold_lifecycle(old_epoch);
        let new_epoch = publish_test_hold_lifecycle(&service, &rule);
        service.shared.retire_hold_lifecycle(old_epoch);
        assert_eq!(
            service.shared.hold_lifecycle_epoch.load(Ordering::Acquire),
            new_epoch
        );
        service
            .shared
            .validate_hold_lifecycle(&rule, Some(new_epoch))
            .expect("stale retirement must not clear newer Hold");
        service.shared.retire_hold_lifecycle(new_epoch);

        service
            .shared
            .next_hold_epoch
            .store(u64::MAX, Ordering::Release);
        let owner_vk = crate::config::hold_trigger_parts(&rule.trigger_keys)
            .expect("valid Hold trigger")
            .owner_vk;
        assert!(service
            .shared
            .publish_hold_lifecycle(&rule, owner_vk)
            .is_none());
        assert!(service.shared.hold_epoch_exhausted.load(Ordering::Acquire));
        assert_eq!(
            service.shared.hold_lifecycle_epoch.load(Ordering::Acquire),
            0
        );
    }

    #[test]
    fn stale_bound_hold_token_cannot_revoke_a_newer_unrelated_run() {
        let mut config = trigger_config_fixture();
        config.macros[0].mode = MacroMode::Hold;
        let rule = config.macros[0].clone();
        let service = HookService::isolated(
            config,
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let epoch = publish_test_hold_lifecycle(&service, &rule);
        let mut old_lease = service
            .shared
            .controller
            .begin_start(None)
            .expect("admit old Hold");
        let old_token = old_lease.token();
        service
            .shared
            .bind_hold_lifecycle_run(&rule, Some(epoch), old_token)
            .expect("bind old Hold");
        assert!(service.shared.controller.activate(old_token));
        old_lease.commit();
        service.shared.controller.request_stop();
        let _ = service.shared.controller.begin_cleaning(old_token);
        assert!(service.shared.controller.finish(old_token, true));

        let mut new_lease = service
            .shared
            .controller
            .begin_start(None)
            .expect("admit newer unrelated run");
        let new_token = new_lease.token();
        assert!(service.shared.controller.activate(new_token));
        new_lease.commit();
        assert!(service.shared.revoke_current_hold_for_key_up(0x78));
        assert!(
            service.shared.controller.input_allowed(new_token),
            "stale Hold identity must not revoke a different active token"
        );
        service.shared.retire_hold_lifecycle(epoch);
        service.shared.controller.request_stop();
        let _ = service.shared.controller.begin_cleaning(new_token);
        let _ = service.shared.controller.finish(new_token, true);
    }

    #[test]
    fn native_and_fallback_hotkeys_submit_to_the_same_release_gate_worker() {
        fn config_with_macro() -> AppConfig {
            AppConfig {
                macros: vec![MacroRule {
                    id: "shared-gate".into(),
                    name: "shared-gate".into(),
                    import_error: None,
                    enabled: true,
                    trigger_keys: vec!["F9".into()],
                    mode: MacroMode::Once,
                    repeat_count: 1,
                    speed: 1.0,
                    record_mouse_move: true,
                    record_mouse_clicks: true,
                    target: None,
                    behavior_policy: None,
                    program: AutomationProgram::Macro { steps: Vec::new() },
                }],
                ..AppConfig::default()
            }
        }

        fn capture_worker(
            service: &HookService,
        ) -> std::sync::mpsc::Receiver<super::MacroTriggerTask> {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let worker = crate::bounded_worker::BoundedWorker::spawn(
                "input-free-release-gate-capture",
                1,
                move |task| tx.send(task).expect("capture trigger task"),
            )
            .expect("capture worker");
            service
                .shared
                .trigger_tasks
                .set(worker)
                .map_err(drop)
                .expect("set capture worker");
            rx
        }

        let fallback = HookService::isolated(
            config_with_macro(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let fallback_rx = capture_worker(&fallback);
        let fallback_config = fallback.shared.config.lock().expect("config").clone();
        assert!(super::process_macro_key_down(
            &fallback.shared,
            &fallback_config,
            fallback
                .shared
                .trigger_config_revision
                .load(Ordering::Acquire),
            0x78,
            false,
            &HashSet::from([0x78]),
        ));
        let fallback_task = fallback_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("fallback task");

        let native = HookService::isolated(
            config_with_macro(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        native
            .shared
            .native_refresh_pending
            .store(false, Ordering::Release);
        native
            .shared
            .native_macro_hotkeys
            .lock()
            .expect("native registry")
            .insert(0x5000, "shared-gate".into());
        let native_rx = capture_worker(&native);
        super::process_native_macro_hotkey(&native.shared, 0x5000);
        let native_task = native_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("native task");

        assert_eq!(fallback_task.rule.id, native_task.rule.id);
        assert_eq!(
            fallback_task.rule.trigger_keys,
            native_task.rule.trigger_keys
        );
        assert_eq!(fallback_task.generation, native_task.generation);
        assert_eq!(
            fallback_task.controller_generation,
            native_task.controller_generation
        );
        assert_eq!(
            fallback_task.admission_revision,
            native_task.admission_revision
        );
        assert_eq!(fallback_task.config_revision, native_task.config_revision);
        fallback.shared.trigger_tasks.get().expect("worker").close();
        native.shared.trigger_tasks.get().expect("worker").close();
    }

    #[test]
    fn fallback_macro_callback_fails_closed_when_registry_or_latch_is_busy() {
        fn capture_worker(
            service: &HookService,
        ) -> std::sync::mpsc::Receiver<super::MacroTriggerTask> {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let worker = crate::bounded_worker::BoundedWorker::spawn(
                "input-free-fallback-lock-capture",
                1,
                move |task| tx.send(task).expect("capture trigger task"),
            )
            .expect("capture worker");
            service
                .shared
                .trigger_tasks
                .set(worker)
                .map_err(drop)
                .expect("set capture worker");
            rx
        }

        fn invoke_while_lock_is_held(
            shared: Arc<HookShared>,
            config: AppConfig,
            revision: u64,
        ) -> (std::thread::JoinHandle<()>, std::sync::mpsc::Receiver<bool>) {
            let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
            let handle = std::thread::spawn(move || {
                let consumed = super::process_macro_key_down(
                    &shared,
                    &config,
                    revision,
                    0x78,
                    false,
                    &HashSet::from([0x11, 0x78]),
                );
                done_tx.send(consumed).expect("report callback result");
            });
            (handle, done_rx)
        }

        let registry_busy = HookService::isolated(
            trigger_config_fixture(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let registry_rx = capture_worker(&registry_busy);
        let registry_config = registry_busy.shared.config.lock().expect("config").clone();
        let registry_revision = registry_busy
            .shared
            .trigger_config_revision
            .load(Ordering::Acquire);
        registry_busy
            .shared
            .latched_hotkeys
            .lock()
            .expect("latch")
            .insert("sentinel".into());
        let registry_guard = registry_busy
            .shared
            .native_macro_hotkeys
            .lock()
            .expect("native registry");
        let (registry_call, registry_done) = invoke_while_lock_is_held(
            Arc::clone(&registry_busy.shared),
            registry_config,
            registry_revision,
        );
        assert!(registry_done
            .recv_timeout(std::time::Duration::from_millis(500))
            .expect("callback seam must return while registry lock is held"));
        registry_call.join().expect("registry contention callback");
        assert!(matches!(
            registry_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        assert!(!registry_busy.shared.trigger_pending.load(Ordering::Acquire));
        drop(registry_guard);
        let registry_latch = registry_busy.shared.latched_hotkeys.lock().expect("latch");
        assert_eq!(registry_latch.len(), 1);
        assert!(registry_latch.contains("sentinel"));
        drop(registry_latch);
        registry_busy
            .shared
            .trigger_tasks
            .get()
            .expect("worker")
            .close();

        let latch_busy = HookService::isolated(
            trigger_config_fixture(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let latch_rx = capture_worker(&latch_busy);
        let latch_config = latch_busy.shared.config.lock().expect("config").clone();
        let latch_revision = latch_busy
            .shared
            .trigger_config_revision
            .load(Ordering::Acquire);
        let mut latch_guard = latch_busy.shared.latched_hotkeys.lock().expect("latch");
        latch_guard.insert("sentinel".into());
        let (latch_call, latch_done) =
            invoke_while_lock_is_held(Arc::clone(&latch_busy.shared), latch_config, latch_revision);
        assert!(latch_done
            .recv_timeout(std::time::Duration::from_millis(500))
            .expect("callback seam must return while latch lock is held"));
        latch_call.join().expect("latch contention callback");
        assert_eq!(latch_guard.len(), 1);
        assert!(latch_guard.contains("sentinel"));
        assert!(matches!(
            latch_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        assert!(!latch_busy.shared.trigger_pending.load(Ordering::Acquire));
        drop(latch_guard);
        latch_busy
            .shared
            .trigger_tasks
            .get()
            .expect("worker")
            .close();
    }

    #[test]
    fn running_toggle_and_clicker_stop_before_contended_start_only_locks() {
        fn capture_worker(
            service: &HookService,
        ) -> std::sync::mpsc::Receiver<super::MacroTriggerTask> {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let worker = crate::bounded_worker::BoundedWorker::spawn(
                "input-free-immediate-stop-capture",
                1,
                move |task| tx.send(task).expect("capture trigger task"),
            )
            .expect("capture worker");
            service
                .shared
                .trigger_tasks
                .set(worker)
                .map_err(drop)
                .expect("set capture worker");
            rx
        }

        fn mark_playback_running(service: &HookService) -> crate::runtime_control::RunToken {
            let mut lease = service
                .shared
                .controller
                .begin_start(None)
                .expect("admit active playback");
            let token = lease.token();
            assert!(service.shared.controller.activate(token));
            lease.commit();
            let mut playback = service.shared.playback.lock().expect("playback");
            playback.running = true;
            playback.stop = Some(Arc::new(AtomicBool::new(false)));
            playback.run_token = Some(token);
            token
        }

        fn invoke(
            shared: Arc<HookShared>,
            config: AppConfig,
            current_vk: u32,
            pressed: HashSet<u32>,
        ) -> (std::thread::JoinHandle<()>, std::sync::mpsc::Receiver<bool>) {
            let revision = shared.trigger_config_revision.load(Ordering::Acquire);
            let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
            let handle = std::thread::spawn(move || {
                let consumed = super::process_macro_key_down(
                    &shared, &config, revision, current_vk, false, &pressed,
                );
                done_tx.send(consumed).expect("report callback result");
            });
            (handle, done_rx)
        }

        let mut toggle_config = trigger_config_fixture();
        toggle_config.macros[0].mode = MacroMode::Toggle;
        let toggle = HookService::isolated(
            toggle_config,
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let toggle_rx = capture_worker(&toggle);
        let toggle_token = mark_playback_running(&toggle);
        assert!(toggle.shared.controller.input_allowed(toggle_token));
        let toggle_config = toggle.shared.config.lock().expect("config").clone();
        let registry_guard = toggle
            .shared
            .native_macro_hotkeys
            .lock()
            .expect("native registry");
        let (toggle_call, toggle_done) = invoke(
            Arc::clone(&toggle.shared),
            toggle_config,
            0x78,
            HashSet::from([0x11, 0x78]),
        );
        assert!(toggle_done
            .recv_timeout(std::time::Duration::from_millis(500))
            .expect("Toggle stop must not wait for registry lock"));
        toggle_call.join().expect("Toggle stop callback");
        assert!(!toggle.shared.controller.input_allowed(toggle_token));
        assert!(toggle.shared.trigger_rearm_required.load(Ordering::Acquire));
        assert!(!toggle.shared.trigger_pending.load(Ordering::Acquire));
        assert!(matches!(
            toggle_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        drop(registry_guard);
        toggle.shared.trigger_tasks.get().expect("worker").close();

        let clicker_config = AppConfig {
            macros: vec![MacroRule {
                id: "legacy-clicker-stop".into(),
                name: "连点器".into(),
                import_error: None,
                enabled: true,
                trigger_keys: vec!["Ctrl".into(), "F8".into()],
                mode: MacroMode::Once,
                repeat_count: 1,
                speed: 1.0,
                record_mouse_move: true,
                record_mouse_clicks: true,
                target: None,
                behavior_policy: None,
                program: AutomationProgram::Macro { steps: Vec::new() },
            }],
            ..AppConfig::default()
        };
        let clicker = HookService::isolated(
            clicker_config,
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let clicker_rx = capture_worker(&clicker);
        let clicker_token = mark_playback_running(&clicker);
        assert!(clicker.shared.controller.input_allowed(clicker_token));
        let clicker_config = clicker.shared.config.lock().expect("config").clone();
        let latch_guard = clicker.shared.latched_hotkeys.lock().expect("latch");
        let (clicker_call, clicker_done) = invoke(
            Arc::clone(&clicker.shared),
            clicker_config,
            0x77,
            HashSet::from([0x11, 0x77]),
        );
        assert!(clicker_done
            .recv_timeout(std::time::Duration::from_millis(500))
            .expect("clicker stop must not wait for latch lock"));
        clicker_call.join().expect("clicker stop callback");
        assert!(!clicker.shared.controller.input_allowed(clicker_token));
        assert!(clicker
            .shared
            .trigger_rearm_required
            .load(Ordering::Acquire));
        assert!(!clicker.shared.trigger_pending.load(Ordering::Acquire));
        assert!(matches!(
            clicker_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        drop(latch_guard);
        clicker.shared.trigger_tasks.get().expect("worker").close();

        let partial = HookService::isolated(
            {
                let mut config = trigger_config_fixture();
                config.macros[0].mode = MacroMode::Toggle;
                config
            },
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let partial_token = mark_playback_running(&partial);
        let partial_config = partial.shared.config.lock().expect("config").clone();
        let registry_guard = partial
            .shared
            .native_macro_hotkeys
            .lock()
            .expect("native registry");
        assert!(super::process_macro_key_down(
            &partial.shared,
            &partial_config,
            partial
                .shared
                .trigger_config_revision
                .load(Ordering::Acquire),
            0x78,
            false,
            &HashSet::from([0x78]),
        ));
        assert!(partial.shared.controller.input_allowed(partial_token));
        assert!(!super::process_macro_key_down(
            &partial.shared,
            &partial_config,
            partial
                .shared
                .trigger_config_revision
                .load(Ordering::Acquire),
            0x41,
            false,
            &HashSet::from([0x41]),
        ));
        assert!(partial.shared.controller.input_allowed(partial_token));
        assert!(!partial
            .shared
            .trigger_rearm_required
            .load(Ordering::Acquire));
        drop(registry_guard);
    }

    #[test]
    fn runtime_admission_rejects_default_and_custom_emergency_key_combinations() {
        let service = test_hook_service();
        let mut task = pending_trigger_task(&service);
        for keys in [vec!["F12".into()], vec!["Ctrl".into(), "F12".into()]] {
            task.rule.trigger_keys = keys;
            assert!(!service.shared.submit_macro_trigger(
                &task.rule,
                task.generation,
                task.config_revision,
                Some(0x7B),
            ));
            assert!(!service.shared.trigger_pending.load(Ordering::Acquire));
        }
        service.shared.emergency_vk.store(0x7A, Ordering::Release);
        task.rule.trigger_keys = vec!["Shift".into(), "F11".into()];
        assert!(!service.shared.submit_macro_trigger(
            &task.rule,
            task.generation,
            task.config_revision,
            Some(0x7A),
        ));
        assert!(!service.shared.trigger_pending.load(Ordering::Acquire));
        assert_eq!(service.shared.injected_input.counts(), (0, 0));
    }

    #[test]
    fn native_clicker_start_submits_ctrl_f8_to_release_gate_worker() {
        let config = AppConfig {
            macros: vec![MacroRule {
                id: "clicker-release-gate".into(),
                name: "连点器".into(),
                import_error: None,
                enabled: true,
                trigger_keys: vec!["Ctrl".into(), "F8".into()],
                mode: MacroMode::Toggle,
                repeat_count: 1,
                speed: 1.0,
                record_mouse_move: true,
                record_mouse_clicks: true,
                target: None,
                behavior_policy: None,
                program: AutomationProgram::Macro { steps: Vec::new() },
            }],
            ..AppConfig::default()
        };
        let service = HookService::isolated(
            config,
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        service
            .shared
            .native_refresh_pending
            .store(false, Ordering::Release);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let worker = crate::bounded_worker::BoundedWorker::spawn(
            "input-free-clicker-gate-capture",
            1,
            move |task| tx.send(task).expect("capture clicker task"),
        )
        .expect("capture worker");
        service
            .shared
            .trigger_tasks
            .set(worker)
            .map_err(drop)
            .expect("set capture worker");

        super::process_native_clicker_hotkey(&service.shared);
        let task = rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("clicker trigger task");
        assert_eq!(task.rule.trigger_keys, ["Ctrl", "F8"]);
        assert!(service.shared.trigger_pending.load(Ordering::Acquire));
        assert_eq!(service.shared.injected_input.counts(), (0, 0));
        service.shared.trigger_tasks.get().expect("worker").close();
    }

    #[test]
    fn swallowed_toggle_stop_stays_fenced_until_real_keyup_and_async_release() {
        let config = AppConfig {
            macros: vec![MacroRule {
                id: "single-f9-toggle".into(),
                name: "single-f9-toggle".into(),
                import_error: None,
                enabled: true,
                trigger_keys: vec!["F9".into()],
                mode: MacroMode::Toggle,
                repeat_count: 1,
                speed: 1.0,
                record_mouse_move: true,
                record_mouse_clicks: true,
                target: None,
                behavior_policy: None,
                program: AutomationProgram::Macro { steps: Vec::new() },
            }],
            ..AppConfig::default()
        };
        let service = HookService::isolated(
            config.clone(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        let (task_tx, task_rx) = std::sync::mpsc::sync_channel(1);
        let worker = crate::bounded_worker::BoundedWorker::spawn(
            "input-free-rearm-capture",
            1,
            move |task| task_tx.send(task).expect("capture fresh trigger"),
        )
        .expect("capture worker");
        service
            .shared
            .trigger_tasks
            .set(worker)
            .map_err(drop)
            .expect("set capture worker");

        let mut lease = service
            .shared
            .controller
            .begin_start(None)
            .expect("admit active Toggle");
        let token = lease.token();
        assert!(service.shared.controller.activate(token));
        lease.commit();
        {
            let mut playback = service.shared.playback.lock().expect("playback");
            playback.running = true;
            playback.stop = Some(Arc::new(AtomicBool::new(false)));
            playback.run_token = Some(token);
        }
        super::update_physical_pressed_ledger(
            &mut service
                .shared
                .physical_pressed
                .lock()
                .expect("physical ledger"),
            0x78,
            true,
            false,
            false,
        );
        service.shared.pressed.lock().expect("pressed").insert(0x78);

        assert!(super::process_macro_key_down(
            &service.shared,
            &config,
            service
                .shared
                .trigger_config_revision
                .load(Ordering::Acquire),
            0x78,
            false,
            &HashSet::from([0x78]),
        ));
        assert!(!service.shared.controller.input_allowed(token));
        assert!(service
            .shared
            .trigger_rearm_required
            .load(Ordering::Acquire));
        assert!(matches!(
            task_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));

        super::update_physical_pressed_ledger(
            &mut service
                .shared
                .physical_pressed
                .lock()
                .expect("physical ledger"),
            0x41,
            false,
            true,
            false,
        );
        super::rearm_macro_triggers_if_released_with(&service.shared, &config, |_| false);
        assert!(service
            .shared
            .trigger_rearm_required
            .load(Ordering::Acquire));
        assert!(service
            .shared
            .pressed
            .lock()
            .expect("pressed")
            .contains(&0x78));
        assert!(super::should_suppress_for_trigger_rearm(
            &service.shared,
            &config,
            0x78
        ));
        assert!(matches!(
            task_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));

        super::update_physical_pressed_ledger(
            &mut service
                .shared
                .physical_pressed
                .lock()
                .expect("physical ledger"),
            0x78,
            false,
            true,
            false,
        );
        super::rearm_macro_triggers_if_released_with(&service.shared, &config, |_| true);
        assert!(service
            .shared
            .trigger_rearm_required
            .load(Ordering::Acquire));
        super::rearm_macro_triggers_if_released_with(&service.shared, &config, |_| false);
        assert!(!service
            .shared
            .trigger_rearm_required
            .load(Ordering::Acquire));
        assert!(!service
            .shared
            .pressed
            .lock()
            .expect("pressed")
            .contains(&0x78));

        assert!(service.shared.controller.begin_cleaning(token));
        assert!(service.shared.controller.finish(token, true));
        service.shared.playback.lock().expect("playback").running = false;
        super::update_physical_pressed_ledger(
            &mut service
                .shared
                .physical_pressed
                .lock()
                .expect("physical ledger"),
            0x78,
            true,
            false,
            false,
        );
        assert!(super::process_macro_key_down(
            &service.shared,
            &config,
            service
                .shared
                .trigger_config_revision
                .load(Ordering::Acquire),
            0x78,
            false,
            &HashSet::from([0x78]),
        ));
        assert_eq!(
            task_rx
                .recv_timeout(std::time::Duration::from_millis(500))
                .expect("later fresh press may submit")
                .rule
                .id,
            "single-f9-toggle"
        );
        service
            .shared
            .trigger_pending
            .store(false, Ordering::Release);
        service.shared.trigger_tasks.get().expect("worker").close();
    }

    #[test]
    fn rearm_requires_all_modifier_sides_and_rwin_to_be_released() {
        let config = AppConfig {
            macros: vec![MacroRule {
                id: "modifier-rearm".into(),
                name: "modifier-rearm".into(),
                import_error: None,
                enabled: true,
                trigger_keys: vec!["Ctrl".into(), "Win".into(), "F9".into()],
                mode: MacroMode::Toggle,
                repeat_count: 1,
                speed: 1.0,
                record_mouse_move: true,
                record_mouse_clicks: true,
                target: None,
                behavior_policy: None,
                program: AutomationProgram::Macro { steps: Vec::new() },
            }],
            ..AppConfig::default()
        };
        let service = HookService::isolated(
            config.clone(),
            crate::automation::VisionService::new(std::env::temp_dir()),
        );
        service
            .shared
            .trigger_rearm_required
            .store(true, Ordering::Release);
        service
            .shared
            .pressed
            .lock()
            .expect("pressed")
            .extend([0x11, 0x5B, 0x78]);
        service
            .shared
            .physical_pressed
            .lock()
            .expect("physical ledger")
            .extend([0xA3, 0x5C, 0x78]);
        super::rearm_macro_triggers_if_released_with(&service.shared, &config, |_| false);
        assert!(service
            .shared
            .trigger_rearm_required
            .load(Ordering::Acquire));
        service
            .shared
            .physical_pressed
            .lock()
            .expect("physical ledger")
            .clear();
        super::rearm_macro_triggers_if_released_with(&service.shared, &config, |vk| vk == 0x5B);
        assert!(service
            .shared
            .trigger_rearm_required
            .load(Ordering::Acquire));
        super::rearm_macro_triggers_if_released_with(&service.shared, &config, |_| false);
        assert!(!service
            .shared
            .trigger_rearm_required
            .load(Ordering::Acquire));
        assert!(service.shared.pressed.lock().expect("pressed").is_empty());

        service
            .shared
            .trigger_rearm_required
            .store(true, Ordering::Release);
        service.shared.pressed.lock().expect("pressed").insert(0x78);
        let ledger_guard = service
            .shared
            .physical_pressed
            .lock()
            .expect("physical ledger");
        super::rearm_macro_triggers_if_released_with(&service.shared, &config, |_| false);
        assert!(service
            .shared
            .trigger_rearm_required
            .load(Ordering::Acquire));
        assert!(service
            .shared
            .pressed
            .lock()
            .expect("pressed")
            .contains(&0x78));
        assert!(service
            .shared
            .physical_ledger_uncertain
            .load(Ordering::Acquire));
        drop(ledger_guard);
    }

    #[test]
    fn left_and_right_modifier_vk_codes_are_supported() {
        assert!(is_shift_key(0xA0));
        assert!(is_shift_key(0xA1));
        assert!(is_text_modifier(0xA2));
        assert!(is_text_modifier(0xA5));
        assert!(is_keyboard_modifier(0x5B));
    }

    #[test]
    fn modifier_keys_are_canonicalized_for_combo_matching() {
        assert_eq!(canonical_virtual_key(0xA2), 0x11);
        assert_eq!(canonical_virtual_key(0xA5), 0x12);
        assert_eq!(canonical_virtual_key(0xA1), 0x10);
        assert_eq!(canonical_virtual_key(0x5C), 0x5B);

        for (raw, canonical) in [
            (0xA0, 0x10),
            (0xA1, 0x10),
            (0xA2, 0x11),
            (0xA3, 0x11),
            (0xA4, 0x12),
            (0xA5, 0x12),
            (0x5B, 0x5B),
            (0x5C, 0x5B),
        ] {
            let ledger = HashSet::from([raw]);
            assert!(super::physical_ledger_contains_trigger(&ledger, canonical));
        }
    }

    #[test]
    fn hook_physical_ledger_is_conservative_when_async_state_is_false() {
        let mut ledger = HashSet::new();
        super::update_physical_pressed_ledger(&mut ledger, 0x78, true, false, false);
        assert!(super::trigger_down_from_ledger_or_async(
            &ledger,
            0x78,
            |_| false
        ));

        let mut gate = super::TriggerReleaseGate::new(
            std::time::Duration::from_millis(18),
            std::time::Duration::from_secs(5),
        );
        assert_eq!(
            gate.observe(
                std::time::Duration::from_millis(100),
                super::trigger_down_from_ledger_or_async(&ledger, 0x78, |_| false),
                super::TriggerReleaseGateStatus::Current,
            ),
            None
        );
        super::update_physical_pressed_ledger(&mut ledger, 0x78, false, true, false);
        assert!(!super::trigger_down_from_ledger_or_async(
            &ledger,
            0x78,
            |_| false
        ));
        assert_eq!(
            gate.observe(
                std::time::Duration::from_millis(101),
                false,
                super::TriggerReleaseGateStatus::Current,
            ),
            None
        );
        assert_eq!(
            gate.observe(
                std::time::Duration::from_millis(119),
                false,
                super::TriggerReleaseGateStatus::Current,
            ),
            Some(super::TriggerReleaseGateOutcome::Ready)
        );
    }

    #[test]
    fn swallowed_combo_key_and_modifier_sides_require_real_hook_keyups() {
        let mut ledger = HashSet::new();
        super::update_physical_pressed_ledger(&mut ledger, 0xA2, true, false, false);
        super::update_physical_pressed_ledger(&mut ledger, 0xA3, true, false, false);
        super::update_physical_pressed_ledger(&mut ledger, 0x78, true, false, false);
        assert!(super::physical_ledger_contains_trigger(&ledger, 0x11));
        assert!(super::physical_ledger_contains_trigger(&ledger, 0x78));

        // The swallowed final F9 key cannot disappear merely because the
        // async provider reports false. Its physical keyup is authoritative.
        assert!(super::trigger_down_from_ledger_or_async(
            &ledger,
            0x78,
            |_| false
        ));
        super::update_physical_pressed_ledger(&mut ledger, 0x78, false, true, false);
        assert!(!super::physical_ledger_contains_trigger(&ledger, 0x78));
        assert!(super::physical_ledger_contains_trigger(&ledger, 0x11));

        // Releasing one side must not clear the other physical Ctrl.
        super::update_physical_pressed_ledger(&mut ledger, 0xA2, false, true, false);
        assert!(super::physical_ledger_contains_trigger(&ledger, 0x11));
        super::update_physical_pressed_ledger(&mut ledger, 0xA3, false, true, false);
        assert!(!super::physical_ledger_contains_trigger(&ledger, 0x11));
    }

    #[test]
    fn injected_and_repeat_events_do_not_corrupt_physical_ledger() {
        let mut ledger = HashSet::new();
        super::update_physical_pressed_ledger(&mut ledger, 0x78, true, false, true);
        assert!(
            ledger.is_empty(),
            "injected keydown is never physical state"
        );

        super::update_physical_pressed_ledger(&mut ledger, 0x78, true, false, false);
        super::update_physical_pressed_ledger(&mut ledger, 0x78, true, false, false);
        assert_eq!(ledger, HashSet::from([0x78]));
        super::update_physical_pressed_ledger(&mut ledger, 0x78, false, true, true);
        assert_eq!(ledger, HashSet::from([0x78]));
        super::update_physical_pressed_ledger(&mut ledger, 0x78, false, true, false);
        assert!(ledger.is_empty());
    }

    #[test]
    fn hook_teardown_ledger_is_fail_closed_until_explicit_startup_reset() {
        let service = test_hook_service();
        service
            .shared
            .physical_pressed
            .lock()
            .expect("physical ledger")
            .insert(0x78);
        service.shared.retire_physical_key_ledger();
        assert!(service
            .shared
            .physical_ledger_uncertain
            .load(Ordering::Acquire));
        assert!(service
            .shared
            .physical_pressed
            .lock()
            .expect("physical ledger")
            .is_empty());
        let task = pending_trigger_task(&service);
        assert_eq!(
            super::trigger_release_gate_status(&service.shared, &task),
            super::TriggerReleaseGateStatus::Cancelled(
                super::TriggerReleaseCancellation::PhysicalLedgerUncertain
            )
        );

        assert!(service.shared.initialize_physical_key_ledger());
        assert!(!service
            .shared
            .physical_ledger_uncertain
            .load(Ordering::Acquire));
    }

    #[test]
    fn shifted_number_keys_produce_symbols_for_text_expansions() {
        let pressed = HashSet::from([0x10]);
        assert_eq!(shifted_printable_character(0x32, &pressed), Some("@"));
        assert_eq!(shifted_printable_character(0x31, &pressed), Some("!"));
        assert_eq!(shifted_printable_character(0x32, &HashSet::new()), None);
    }

    #[test]
    fn random_delay_stays_in_the_requested_range() {
        assert_eq!(randomized_delay_ms(300, None), 300);
        assert_eq!(randomized_delay_ms(300, Some(100)), 300);
        for _ in 0..16 {
            let value = randomized_delay_ms(120, Some(360));
            assert!((120..=360).contains(&value));
        }
    }

    #[test]
    fn live_cursor_position_takes_priority_over_cached_automation_endpoint() {
        assert_eq!(
            resolve_cursor_start(Some((420, 260)), Some((100, 80))),
            Some((420, 260))
        );
        assert_eq!(resolve_cursor_start(None, Some((100, 80))), Some((100, 80)));
    }

    #[test]
    fn mouse_button_release_is_attempted_when_cursor_move_fails() {
        let released = AtomicBool::new(false);
        let result =
            release_after_best_effort_move(Some(|| Err("move rejected".to_string())), || {
                released.store(true, Ordering::SeqCst);
                Ok(())
            });
        assert_eq!(result, Err("move rejected".to_string()));
        assert!(released.load(Ordering::SeqCst));
    }

    #[test]
    fn mouse_button_release_failure_is_not_hidden_by_move_success() {
        let result = release_after_best_effort_move(Some(|| Ok(())), || {
            Err("button release rejected".to_string())
        });
        assert_eq!(result, Err("button release rejected".to_string()));
    }

    #[test]
    fn mouse_button_release_reports_move_failure_after_successful_release() {
        let result =
            release_after_best_effort_move(Some(|| Err("move rejected".to_string())), || Ok(()));
        assert_eq!(result, Err("move rejected".to_string()));
    }

    #[test]
    fn split_command_line_preserves_quoted_paths_and_arguments() {
        assert_eq!(
            split_command_line(r#""C:\Program Files\Demo\demo.exe" --open"#),
            vec![
                r#"C:\Program Files\Demo\demo.exe"#.to_string(),
                "--open".to_string(),
            ]
        );
    }

    #[test]
    fn recording_shortcut_is_detected_and_removed_from_recording() {
        let pressed = HashSet::from([0x10_u32, 0x11_u32, 0x78_u32]);
        assert!(is_recording_shortcut_key(0x78, &pressed));

        let mut recorder = RecorderState {
            steps: vec![
                MacroStep::Key {
                    key: "Ctrl".to_string(),
                    action: KeyAction::Down,
                },
                MacroStep::Delay {
                    duration_ms: 20,
                    duration_max_ms: None,
                },
                MacroStep::Key {
                    key: "Shift".to_string(),
                    action: KeyAction::Down,
                },
                MacroStep::Key {
                    key: "F9".to_string(),
                    action: KeyAction::Down,
                },
            ],
            pressed_keys: pressed,
            ..Default::default()
        };
        discard_recording_shortcut_steps(&mut recorder);
        assert!(recorder.steps.is_empty());
        assert!(recorder.pressed_keys.is_empty());
    }

    #[test]
    fn macro_and_hotkey_latches_are_released_by_key_up() {
        assert!(latched_signature_contains_vk("macro+119", 0x77));
        assert!(latched_signature_contains_vk("17+119", 0x11));
        assert!(latched_signature_contains_vk("Ctrl+F8", 0x77));
        assert!(!latched_signature_contains_vk("macro+119", 0x78));
    }

    #[test]
    fn completed_macro_reconciles_stale_pressed_and_latched_trigger_state() {
        let trigger_vks = [0x77_u32];
        let signature = "macro+119".to_string();
        let mut pressed = HashSet::from(trigger_vks);
        let mut latched = HashSet::from([signature.clone()]);

        assert!(clear_released_macro_trigger_state(
            &mut pressed,
            &mut latched,
            &trigger_vks,
            |_| false,
        ));
        assert!(!pressed.contains(&0x77));
        assert!(!latched.contains(&signature));
    }

    #[test]
    fn completed_macro_keeps_trigger_state_while_key_is_physically_held() {
        let trigger_vks = [0x77_u32];
        let signature = "macro+119".to_string();
        let mut pressed = HashSet::from(trigger_vks);
        let mut latched = HashSet::from([signature.clone()]);

        assert!(!clear_released_macro_trigger_state(
            &mut pressed,
            &mut latched,
            &trigger_vks,
            |_| true,
        ));
        assert!(pressed.contains(&0x77));
        assert!(latched.contains(&signature));
    }

    #[test]
    fn playback_errors_are_visible_but_f12_cancellation_stays_silent() {
        assert!(should_show_playback_error("找不到图像文件"));
        assert!(!should_show_playback_error(crate::rhai_runtime::CANCELLED));
    }

    #[test]
    fn native_hotkey_specs_support_single_function_keys_and_modifier_combos() {
        assert_eq!(native_hotkey_spec(&["F9".to_string()]), Some((0, 0x78)));
        assert_eq!(
            native_hotkey_spec(&["Ctrl".to_string(), "F9".to_string()]),
            Some((0x0002, 0x78))
        );
        assert_eq!(native_hotkey_spec(&["Ctrl".to_string()]), None);
        assert_eq!(
            native_hotkey_spec(&["F8".to_string(), "F9".to_string()]),
            None
        );
    }

    #[test]
    fn stale_playback_thread_cannot_clear_a_newer_instance() {
        let mut playback = PlaybackState {
            running: true,
            instance_id: 2,
            last_error: None,
            ..Default::default()
        };

        assert!(!HookShared::finalize_playback_instance(
            &mut playback,
            1,
            Some("stale".to_string())
        ));
        assert!(playback.running);
        assert_eq!(playback.instance_id, 2);
        assert_eq!(playback.last_error, None);

        assert!(HookShared::finalize_playback_instance(
            &mut playback,
            2,
            Some("current".to_string())
        ));
        assert!(!playback.running);
        assert_eq!(playback.last_error.as_deref(), Some("current"));
    }

    #[test]
    fn rejected_start_error_cannot_clear_the_active_playback_instance() {
        let service = test_hook_service();
        let stop = Arc::new(AtomicBool::new(false));
        {
            let mut playback = service
                .shared
                .playback
                .lock()
                .expect("playback should lock");
            playback.running = true;
            playback.stop = Some(Arc::clone(&stop));
            playback.instance_id = 21;
            playback.phase = "running".to_string();
        }

        service
            .shared
            .set_playback_start_error("a second trigger was rejected");

        let playback = service
            .shared
            .playback
            .lock()
            .expect("playback should lock");
        assert!(playback.running);
        assert!(playback.stop.is_some());
        assert_eq!(playback.instance_id, 21);
        assert_eq!(playback.phase, "running");
        assert_eq!(playback.last_error, None);
        assert!(!stop.load(Ordering::SeqCst));
    }

    #[test]
    fn emergency_generation_cancels_a_queued_start_before_validation() {
        let service = test_hook_service();
        let queued_generation = service.shared.emergency_generation.load(Ordering::SeqCst);
        service
            .shared
            .emergency_generation
            .fetch_add(1, Ordering::SeqCst);
        let rule = MacroRule {
            id: "cancelled-before-start".to_string(),
            name: "cancelled-before-start".to_string(),
            import_error: None,
            enabled: true,
            trigger_keys: Vec::new(),
            mode: MacroMode::Once,
            repeat_count: 1,
            speed: 1.0,
            record_mouse_move: true,
            record_mouse_clicks: true,
            target: None,
            behavior_policy: None,
            program: AutomationProgram::Macro { steps: Vec::new() },
        };

        let error = service
            .shared
            .start_playback_at_generation(rule, queued_generation)
            .expect_err("an emergency request must cancel a queued start");
        assert_eq!(error.code, "playback_cancelled");
        assert!(!service.shared.is_playback_running());
    }

    #[test]
    fn stale_playback_progress_cannot_overwrite_a_newer_instance() {
        let service = test_hook_service();
        {
            let mut playback = service
                .shared
                .playback
                .lock()
                .expect("playback should lock");
            playback.running = true;
            playback.instance_id = 9;
        }

        service
            .shared
            .set_playback_action(8, 3, "stale", Some("旧线程".to_string()));
        let status = service.shared.playback_status();
        assert_eq!(status.current_step, 0);
        assert_eq!(status.action_summary, None);

        service.shared.set_playback_action(
            9,
            3,
            "mouse_move",
            Some("移动鼠标至 (10, 20)".to_string()),
        );
        let status = service.shared.playback_status();
        assert_eq!(status.current_step, 3);
        assert_eq!(status.action_kind.as_deref(), Some("mouse_move"));
        assert_eq!(
            status.action_summary.as_deref(),
            Some("移动鼠标至 (10, 20)")
        );
    }

    #[test]
    fn playback_status_keeps_cleanup_phase_until_cleanup_finishes() {
        let service = test_hook_service();
        {
            let mut playback = service
                .shared
                .playback
                .lock()
                .expect("playback should lock");
            playback.running = true;
            playback.instance_id = 12;
            playback.macro_name = Some("清理测试".to_string());
            playback.phase = "stopping".to_string();
        }

        service
            .shared
            .set_playback_phase(12, "cleaning", "pending", None);
        let status = service.shared.playback_status();
        assert!(status.running);
        assert_eq!(status.phase, "cleaning");
        assert_eq!(status.cleanup_status, "pending");
    }

    #[test]
    fn cleanup_failure_remains_visible_until_a_new_playback_instance() {
        let service = test_hook_service();
        service
            .shared
            .config
            .lock()
            .expect("config should lock")
            .show_playback_overlay = true;
        {
            let mut playback = service
                .shared
                .playback
                .lock()
                .expect("playback should lock");
            playback.running = true;
            playback.instance_id = 13;
            playback.macro_name = Some("清理失败测试".to_string());
            playback.phase = "cleanup_failed".to_string();
            playback.cleanup_status = "failed".to_string();
        }

        assert!(HookShared::finalize_playback_instance(
            &mut service
                .shared
                .playback
                .lock()
                .expect("playback should lock"),
            13,
            Some("输入清理失败".to_string()),
        ));
        let status = service.shared.playback_status();
        assert!(!status.running);
        assert_eq!(status.phase, "cleanup_failed");
        assert_eq!(status.cleanup_status, "failed");
        assert!(status.overlay_visible);
    }

    #[test]
    fn playback_overlay_is_hidden_while_macro_recording() {
        let service = test_hook_service();
        service
            .start_recording(true, true)
            .expect("input-free recording admission");
        service
            .shared
            .config
            .lock()
            .expect("config should lock")
            .show_playback_overlay = true;
        {
            let mut playback = service
                .shared
                .playback
                .lock()
                .expect("playback should lock");
            playback.running = true;
            playback.instance_id = 14;
            playback.macro_name = Some("录制期间隐藏".to_string());
            playback.phase = "running".to_string();
        }
        assert!(!service.shared.playback_status().overlay_visible);
    }

    #[test]
    fn emergency_generation_interrupts_a_playback_without_touching_input_locks() {
        let service = test_hook_service();
        let stop = AtomicBool::new(false);
        let generation = service.shared.emergency_generation.load(Ordering::SeqCst);

        assert!(!service.shared.stop_requested(&stop, generation));
        service
            .shared
            .emergency_generation
            .fetch_add(1, Ordering::SeqCst);
        assert!(service.shared.stop_requested(&stop, generation));
        assert_eq!(service.shared.injected_input.counts(), (0, 0));
    }

    #[test]
    fn emergency_generation_is_monotonic_for_repeated_stop_requests() {
        let service = test_hook_service();
        let first = service
            .shared
            .emergency_generation
            .fetch_add(1, Ordering::SeqCst)
            + 1;
        let second = service
            .shared
            .emergency_generation
            .fetch_add(1, Ordering::SeqCst)
            + 1;
        assert!(second > first);
        assert_eq!(
            service
                .shared
                .emergency_request_sequence
                .load(Ordering::SeqCst),
            0
        );
    }

    #[test]
    fn playback_is_blocked_until_failed_input_cleanup_is_recovered() {
        let service = test_hook_service();
        service
            .shared
            .input_recovery_required
            .store(true, Ordering::SeqCst);
        let rule = MacroRule {
            id: "safety-test".to_string(),
            name: "safety-test".to_string(),
            import_error: None,
            enabled: true,
            trigger_keys: Vec::new(),
            mode: MacroMode::Once,
            repeat_count: 1,
            speed: 1.0,
            record_mouse_move: true,
            record_mouse_clicks: true,
            target: None,
            behavior_policy: None,
            program: AutomationProgram::Macro { steps: Vec::new() },
        };

        let error = service
            .shared
            .start_playback(rule)
            .expect_err("unsafe input state must block a new playback");
        assert_eq!(error.code, "input_safety_recovery_required");
    }

    #[test]
    fn config_refresh_keeps_an_active_macro_recording() {
        let service = test_hook_service();
        service
            .start_recording(true, true)
            .expect("recording should start");
        {
            let mut recorder = service
                .shared
                .recorder
                .lock()
                .expect("recorder should lock");
            recorder.capture_started = true;
            recorder.steps.push(MacroStep::MouseMove { x: 120, y: 80 });
        }

        service
            .update_config(AppConfig::default())
            .expect("config refresh should succeed");

        let status = service.recording_status();
        assert!(status.active);
        assert!(status.capture_started);
        assert_eq!(status.step_count, 1);
    }

    #[test]
    fn config_refresh_keeps_a_completed_macro_recording_until_collected() {
        let service = test_hook_service();
        service
            .start_recording(true, true)
            .expect("recording should start");
        {
            let mut recorder = service
                .shared
                .recorder
                .lock()
                .expect("recorder should lock");
            recorder.steps.push(MacroStep::MouseMove { x: 240, y: 160 });
        }
        service.shared.finish_recording();

        service
            .update_config(AppConfig::default())
            .expect("config refresh should succeed");

        let status = service.recording_status();
        assert!(!status.active);
        assert_eq!(status.step_count, 1);
        let result = service
            .stop_recording(false)
            .expect("completed recording should still be available");
        assert_eq!(result.steps, vec![MacroStep::MouseMove { x: 240, y: 160 }]);
    }

    #[test]
    fn recording_preserves_sub_eight_millisecond_step_intervals() {
        let mut recorder = RecorderState::default();
        let started = std::time::Instant::now();
        push_record_step_at(
            &mut recorder,
            MacroStep::MouseMove { x: 10, y: 20 },
            started,
        );
        push_record_step_at(
            &mut recorder,
            MacroStep::MouseMove { x: 30, y: 40 },
            started + std::time::Duration::from_millis(4),
        );

        assert_eq!(
            recorder.steps,
            vec![
                MacroStep::MouseMove { x: 10, y: 20 },
                MacroStep::Delay {
                    duration_ms: 4,
                    duration_max_ms: None,
                },
                MacroStep::MouseMove { x: 30, y: 40 },
            ]
        );
    }

    #[test]
    fn stopping_by_click_removes_trailing_mouse_input() {
        let shared = HookShared::new(
            crate::AppConfig::default(),
            crate::automation::VisionService::new(
                std::env::temp_dir()
                    .join("AutoFlow")
                    .join("data")
                    .join("images"),
            ),
        );
        let mut recorder = shared.recorder.lock().expect("recorder should lock");
        recorder.steps = vec![
            MacroStep::Key {
                key: "A".to_string(),
                action: KeyAction::Down,
            },
            MacroStep::Delay {
                duration_ms: 20,
                duration_max_ms: None,
            },
            MacroStep::MouseButton {
                button: crate::MouseButton::Left,
                action: KeyAction::Down,
                x: 50,
                y: 50,
            },
            MacroStep::MouseButton {
                button: crate::MouseButton::Left,
                action: KeyAction::Up,
                x: 50,
                y: 50,
            },
            MacroStep::Delay {
                duration_ms: 20,
                duration_max_ms: None,
            },
            MacroStep::MouseMove { x: 100, y: 100 },
            MacroStep::Delay {
                duration_ms: 20,
                duration_max_ms: None,
            },
            MacroStep::MouseMove { x: 200, y: 200 },
            MacroStep::Delay {
                duration_ms: 20,
                duration_max_ms: None,
            },
            MacroStep::MouseButton {
                button: crate::MouseButton::Left,
                action: KeyAction::Down,
                x: 300,
                y: 300,
            },
            MacroStep::MouseButton {
                button: crate::MouseButton::Left,
                action: KeyAction::Up,
                x: 300,
                y: 300,
            },
        ];
        drop(recorder);

        super::discard_trailing_mouse_input_steps(
            &mut shared
                .recorder
                .lock()
                .expect("input-free processing fixture"),
        );

        let recorder = shared.recorder.lock().expect("recorder should lock");
        assert_eq!(recorder.steps.len(), 4);
        assert!(matches!(
            recorder.steps.first(),
            Some(MacroStep::Key { key, .. }) if key == "A"
        ));
        assert!(matches!(
            recorder.steps.get(2),
            Some(MacroStep::MouseButton {
                action: KeyAction::Down,
                ..
            })
        ));
        assert!(matches!(
            recorder.steps.get(3),
            Some(MacroStep::MouseButton {
                action: KeyAction::Up,
                ..
            })
        ));
        assert!(recorder
            .steps
            .iter()
            .all(|step| !matches!(step, MacroStep::MouseMove { .. })));
    }
}
