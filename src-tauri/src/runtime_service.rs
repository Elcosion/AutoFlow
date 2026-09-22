//! Private safety-service control protocol. UI exit does NOT kill this service
//! via a UI-owned Job: the independent lease watcher must clean input first.
use crate::hook::HookService;
use crate::runtime_control::{RuntimeController, RuntimePhaseObservation, RuntimePhaseProvenance};
use crate::service_lease::{claim_authority, ControllerLease, ParentWatch};
use crate::service_transport::{read_document, write_document, DocumentWriter};
use crate::{AppConfig, AppError, MacroRule};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SafetyCommand {
    UpdateConfig { config: Box<AppConfig> },
    ConfigureVision { root: PathBuf },
    StartRecording { moves: bool, clicks: bool },
    RecordingOptions { moves: bool, clicks: bool },
    StopRecording { discard_tail: bool },
    RecordingStatus,
    StartBehavior { name: String },
    StopBehavior,
    DiscardBehavior,
    CompleteBehaviorClaim,
    BehaviorStatus,
    Play { rule: Box<MacroRule> },
    Stop,
    PlaybackStatus,
    IsPlaying,
    Notification,
    Acknowledge { id: u64 },
    Shutdown,
    Recover,
}

#[derive(Serialize, Deserialize)]
struct ServiceBootstrap {
    version: u32,
    secret: String,
    session: String,
    parent_pid: u32,
    lease_name: String,
    stop_name: String,
    config: AppConfig,
    image_root: PathBuf,
}
#[derive(Serialize, Deserialize)]
struct ServiceRequest {
    version: u32,
    secret: String,
    session: String,
    sequence: u64,
    generation: u64,
    admission_revision: u64,
    command: SafetyCommand,
}
#[derive(Serialize, Deserialize)]
struct ServiceResponse {
    version: u32,
    secret: String,
    session: String,
    sequence: u64,
    generation: u64,
    admission_revision: u64,
    result: Result<Value, AppError>,
}
const VERSION: u32 = 2;

/// Runs on the lease guardian, never on the ordinary RPC dispatcher. Revocation
/// must precede cleanup; an unsafe result is not authority to terminate.
fn controller_loss_cleanup(revoke: impl FnOnce(), cleanup: impl FnOnce() -> bool) -> bool {
    revoke();
    cleanup()
}

/// Preserve the service's ledger and ownership claim if cleanup could not be
/// confirmed. No retry loop, replay or new input admission is performed. The
/// client observes the retained process as unconfirmed exit, not safe shutdown.
/// This quarantine requires explicit operator handling; process termination
/// itself cannot be represented as proof that Windows released held input.
fn retain_authority_if_unsafe(safe: bool) {
    if !safe {
        loop {
            std::thread::park();
        }
    }
}

fn complete_failed_startup(failure: AppError, emit: impl FnOnce(AppError)) {
    let rollback_confirmed = failure.code != "input_service_start_rollback_unconfirmed";
    emit(failure);
    // A startup error is not proof of cleanup or process exit. Keep ownership
    // quarantined if construction could not positively confirm rollback.
    retain_authority_if_unsafe(rollback_confirmed);
}

fn error(message: impl Into<String>) -> AppError {
    AppError::invalid("safety_service_unavailable", message)
}
fn value<T: Serialize>(result: Result<T, AppError>) -> Result<Value, AppError> {
    result.and_then(|value| serde_json::to_value(value).map_err(|_| error("安全服务结果编码失败")))
}

struct Call {
    command: SafetyCommand,
    generation: u64,
    admission_revision: u64,
    reply: SyncSender<Result<Value, AppError>>,
}
struct StartupLeaseGuard {
    closed: Arc<AtomicBool>,
    accepted: bool,
}
impl Drop for StartupLeaseGuard {
    fn drop(&mut self) {
        if !self.accepted {
            self.closed.store(true, Ordering::Release);
        }
    }
}
pub struct SafetyClient {
    calls: SyncSender<Call>,
    closed: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    admission_revision: Arc<AtomicU64>,
    lease: Arc<ControllerLease>,
    pub pid: u32,
    process: std::os::windows::io::OwnedHandle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceExitStatus {
    Alive,
    Exited,
    Unknown,
}
impl SafetyClient {
    /// `mock` selects only the input-free fixture entry point, not a bypass of
    /// the production readiness gate. The production main has no mock flag.
    pub fn spawn(
        executable: &Path,
        config: AppConfig,
        image_root: PathBuf,
        mock: bool,
    ) -> Result<Self, AppError> {
        Self::spawn_inner(executable, config, image_root, mock, false)
    }

    /// Input-free fixture entry; the production executable dispatches no such
    /// flag. Never use this as a bypass of live cleanup or readiness checks.
    pub fn spawn_mock_unsafe_cleanup(executable: &Path) -> Result<Self, AppError> {
        Self::spawn_inner(
            executable,
            AppConfig::default(),
            std::env::temp_dir(),
            true,
            true,
        )
    }

    fn spawn_inner(
        executable: &Path,
        config: AppConfig,
        image_root: PathBuf,
        mock: bool,
        unsafe_cleanup: bool,
    ) -> Result<Self, AppError> {
        use std::os::windows::process::CommandExt;
        let secret = crate::runtime_executor::session_secret().map_err(error)?;
        let closed = Arc::new(AtomicBool::new(false));
        // Every startup error, including pipe/thread creation, revokes pulses.
        let mut startup = StartupLeaseGuard {
            closed: closed.clone(),
            accepted: false,
        };
        let lease = Arc::new(ControllerLease::new(&secret, closed.clone()).map_err(error)?);
        lease.start_pulsing().map_err(error)?;
        let mut command = std::process::Command::new(executable);
        command
            .arg(if mock && unsafe_cleanup {
                "--runtime-safety-mock-unsafe-cleanup"
            } else if mock {
                "--runtime-safety-mock"
            } else {
                "--runtime-safety"
            })
            .creation_flags(0x0800_0000)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                closed.store(true, Ordering::Release);
                return Err(error(e.to_string()));
            }
        };
        let pid = child.id();
        // Keep the exact process object, not only a reusable PID. Observation
        // remains possible even when the ordinary RPC actor is blocked.
        use std::os::windows::io::AsHandle;
        let process = child
            .as_handle()
            .try_clone_to_owned()
            .map_err(|e| error(e.to_string()))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| error("安全服务输入管道不可用"))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| error("安全服务输出管道不可用"))?;
        let session = secret[..32].to_string();
        let bootstrap = ServiceBootstrap {
            version: VERSION,
            secret: secret.clone(),
            session: session.clone(),
            parent_pid: std::process::id(),
            lease_name: lease.name.clone(),
            stop_name: lease.stop_name.clone(),
            config,
            image_root,
        };
        let generation = Arc::new(AtomicU64::new(0));
        let admission_revision = Arc::new(AtomicU64::new(0));
        let (calls, receiver) = sync_channel::<Call>(1);
        let (ready_tx, ready_rx) = sync_channel(1);
        let actor_closed = closed.clone();
        let actor_generation = generation.clone();
        let actor_revision = admission_revision.clone();
        let actor = std::thread::Builder::new()
            .name("autoflow-safety-client".into())
            .spawn(move || {
                let initialize = (|| {
                    write_document(&mut stdin, &bootstrap)
                        .map_err(|_| error("安全服务初始化写入失败"))?;
                    let ready: ServiceResponse =
                        read_document(&mut stdout).map_err(|_| error("安全服务初始化响应失败"))?;
                    if ready.version != VERSION
                        || ready.secret != secret
                        || ready.session != session
                        || ready.sequence != 0
                    {
                        return Err(error("安全服务初始化身份不匹配"));
                    }
                    actor_generation.store(ready.generation, Ordering::Release);
                    actor_revision.store(ready.admission_revision, Ordering::Release);
                    ready.result.map(|_| ())
                })();
                let initialized = initialize.is_ok();
                let _ = ready_tx.send(initialize);
                if initialized {
                    let mut sequence = 0u64;
                    while !actor_closed.load(Ordering::Acquire) {
                        let call = match receiver.recv_timeout(Duration::from_millis(50)) {
                            Ok(call) => call,
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(_) => break,
                        };
                        sequence = sequence.saturating_add(1);
                        let shutting_down = matches!(call.command, SafetyCommand::Shutdown);
                        let request = ServiceRequest {
                            version: VERSION,
                            secret: secret.clone(),
                            session: session.clone(),
                            sequence,
                            generation: call.generation,
                            admission_revision: call.admission_revision,
                            command: call.command,
                        };
                        let result = (|| {
                            write_document(&mut stdin, &request)
                                .map_err(|_| error("安全服务写入失联"))?;
                            let response: ServiceResponse = read_document(&mut stdout)
                                .map_err(|_| error("安全服务读取失联"))?;
                            if response.version != VERSION
                                || response.secret != secret
                                || response.session != session
                                || response.sequence != sequence
                            {
                                return Err(error("安全服务响应身份或序号不匹配"));
                            }
                            actor_generation.store(response.generation, Ordering::Release);
                            actor_revision.store(response.admission_revision, Ordering::Release);
                            Ok(response.result)
                        })();
                        // A valid server rejection is NOT a pipe failure. In
                        // particular, busy admission cannot disconnect an active run.
                        let communication_failed = result.is_err();
                        let _ = call.reply.send(result.and_then(|result| result));
                        if shutting_down || communication_failed {
                            break;
                        }
                    }
                }
                actor_closed.store(true, Ordering::Release);
                drop(stdin);
                drop(stdout);
                // No forced kill of an authorized safety authority: lease loss is
                // its independent cleanup signal. Its own supervisor owns workers.
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                while std::time::Instant::now() < deadline {
                    if child.try_wait().ok().flatten().is_some() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
        if let Err(e) = actor {
            closed.store(true, Ordering::Release);
            return Err(error(e.to_string()));
        }
        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => {
                startup.accepted = true;
                Ok(Self {
                    calls,
                    closed,
                    generation,
                    admission_revision,
                    lease,
                    pid,
                    process,
                })
            }
            Ok(Err(e)) => {
                closed.store(true, Ordering::Release);
                Err(e)
            }
            Err(_) => {
                closed.store(true, Ordering::Release);
                Err(error("安全服务启动超过3s期限"))
            }
        }
    }
    pub fn call<T: for<'de> Deserialize<'de>>(
        &self,
        command: SafetyCommand,
    ) -> Result<T, AppError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(error("安全服务连接已失效，禁止自动重连或续播"));
        }
        let (reply, response) = sync_channel(1);
        self.calls
            .try_send(Call {
                command,
                generation: self.generation.load(Ordering::Acquire),
                admission_revision: self.admission_revision.load(Ordering::Acquire),
                reply,
            })
            .map_err(|_| {
                AppError::invalid("safety_service_busy", "安全服务请求通道繁忙，请稍后重试")
            })?;
        match response.recv_timeout(Duration::from_secs(5)) {
            Ok(result) => result.and_then(|value| {
                serde_json::from_value(value).map_err(|_| error("安全服务结果格式异常"))
            }),
            Err(_) => {
                self.closed.store(true, Ordering::Release);
                Err(error("安全服务请求超过5s期限，已撤销控制租约"))
            }
        }
    }
    pub fn stop_signal(&self) -> Result<(), AppError> {
        self.lease.request_stop().map_err(|e| {
            self.closed.store(true, Ordering::Release);
            error(e)
        })
    }

    pub fn exit_status(&self) -> ServiceExitStatus {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
        match unsafe {
            windows::Win32::System::Threading::WaitForSingleObject(
                HANDLE(self.process.as_raw_handle()),
                0,
            )
        } {
            WAIT_OBJECT_0 => ServiceExitStatus::Exited,
            WAIT_TIMEOUT => ServiceExitStatus::Alive,
            _ => ServiceExitStatus::Unknown,
        }
    }

    pub fn confirm_exit(&self, timeout: Duration) -> Result<(), AppError> {
        let deadline = std::time::Instant::now() + timeout.min(Duration::from_secs(3));
        loop {
            match self.exit_status() {
                ServiceExitStatus::Exited => return Ok(()),
                ServiceExitStatus::Unknown => {
                    return Err(AppError::invalid(
                        "safety_service_exit_unknown",
                        "安全进程退出状态无法查询，不能报告已退出",
                    ))
                }
                ServiceExitStatus::Alive if std::time::Instant::now() >= deadline => {
                    return Err(AppError::invalid(
                        "safety_service_exit_unconfirmed",
                        "安全进程仍未退出，禁止将关闭请求报告为退出完成",
                    ))
                }
                ServiceExitStatus::Alive => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    }

    pub fn shutdown_confirmed(&self) -> Result<(), AppError> {
        let _ = self.stop_signal();
        let cleanup = self.call::<bool>(SafetyCommand::Shutdown);
        // Also revoke the independent lease if shutdown RPC was rejected,
        // disconnected or busy. No automatic retry/replay of that RPC.
        self.closed.store(true, Ordering::Release);
        self.confirm_exit(Duration::from_secs(3))?;
        if cleanup? {
            Ok(())
        } else {
            Err(AppError::invalid(
                "safety_shutdown_cleanup_unconfirmed",
                "安全进程已退出，但输入清理未确认安全",
            ))
        }
    }
}
impl Drop for SafetyClient {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
    }
}

enum ServiceRuntime {
    Live(Arc<HookService>),
    Mock(Arc<RuntimeController>, bool),
}
impl ServiceRuntime {
    fn admission_revision(&self) -> u64 {
        match self {
            Self::Live(hook) => hook.service_admission_revision(),
            Self::Mock(control, _) => control.background_generation(),
        }
    }
    fn generation(&self) -> u64 {
        match self {
            Self::Live(hook) => hook.service_generation(),
            Self::Mock(control, _) => control.generation(),
        }
    }
    fn validate_request_identity(
        &self,
        command: &SafetyCommand,
        generation: u64,
        admission_revision: u64,
    ) -> Result<(), AppError> {
        if matches!(command, SafetyCommand::Stop | SafetyCommand::Shutdown) {
            return Ok(());
        }
        let current_generation = self.generation();
        let current_revision = self.admission_revision();
        if generation != current_generation || admission_revision != current_revision {
            return Err(AppError::invalid(
                "safety_service_stale_request",
                format!(
                    "stale safety-service request identity: request generation/revision {generation}/{admission_revision}, current {current_generation}/{current_revision}"
                ),
            ));
        }
        Ok(())
    }
    fn stop(&self) {
        match self {
            Self::Live(hook) => hook.emergency_stop(),
            Self::Mock(control, cleanup_safe) => {
                control.request_stop();
                if let Some(token) = control.active_token() {
                    control.begin_cleaning(token);
                    control.finish(token, *cleanup_safe);
                }
            }
        }
    }
    fn fast_shutdown(&self) {
        match self {
            Self::Live(hook) => hook.fast_shutdown_request(),
            Self::Mock(control, _) => {
                control.request_shutdown();
            }
        }
    }
    fn shutdown(&self) -> bool {
        match self {
            Self::Live(hook) => hook.shutdown(),
            Self::Mock(control, cleanup_safe) => {
                control.request_shutdown();
                if let Some(token) = control.active_token() {
                    control.begin_cleaning(token);
                    control.finish(token, *cleanup_safe);
                }
                *cleanup_safe
            }
        }
    }
    fn execute(
        &self,
        command: SafetyCommand,
        generation: u64,
        admission_revision: u64,
        dispatch_hook: Option<fn(&SafetyCommand)>,
    ) -> Result<Value, AppError> {
        self.execute_inner(command, generation, admission_revision, dispatch_hook, None)
    }

    fn execute_inner(
        &self,
        command: SafetyCommand,
        generation: u64,
        admission_revision: u64,
        dispatch_hook: Option<fn(&SafetyCommand)>,
        recovery_commit_hook: Option<fn(&RuntimeController)>,
    ) -> Result<Value, AppError> {
        self.validate_request_identity(&command, generation, admission_revision)?;
        if let Some(hook) = dispatch_hook {
            hook(&command);
            // A fixture hook may deliberately hold dispatch while the independent
            // stop path advances identity. Do not execute that now-stale request.
            self.validate_request_identity(&command, generation, admission_revision)?;
        }
        if let Self::Mock(control, cleanup_safe) = self {
            return match command {
                SafetyCommand::Play { .. } => {
                    let mut lease = control
                        .begin_start_at_revision(Some(generation), admission_revision)
                        .map_err(|e| AppError::invalid("playback_start_rejected", e.to_string()))?;
                    if !control.activate(lease.token()) {
                        return Err(error("mock activation cancelled"));
                    }
                    lease.commit();
                    Ok(Value::Null)
                }
                SafetyCommand::Stop => {
                    self.stop();
                    Ok(Value::Null)
                }
                SafetyCommand::Shutdown => Ok(Value::Bool(self.shutdown())),
                SafetyCommand::PlaybackStatus => {
                    let (phase, observation, provenance) = match control.observe_phase() {
                        RuntimePhaseObservation::Confirmed { phase, provenance } => (
                            format!("{phase:?}"),
                            "confirmed",
                            match provenance {
                                RuntimePhaseProvenance::State => "controller_state",
                                RuntimePhaseProvenance::FaultLatch => "fault_latch",
                                RuntimePhaseProvenance::ShutdownLatch => "shutdown_latch",
                            },
                        ),
                        RuntimePhaseObservation::Unavailable => {
                            ("Unknown".to_string(), "unavailable", "controller_busy")
                        }
                    };
                    Ok(serde_json::json!({
                        "running": control.active_token().is_some(),
                        "phase": phase,
                        "phaseObservation": observation,
                        "phaseProvenance": provenance,
                        "generation": control.generation()
                    }))
                }
                SafetyCommand::Recover => {
                    if let Some(hook) = recovery_commit_hook {
                        hook(control);
                    }
                    let recovered = *cleanup_safe
                        && control.recover_after_cleanup_at(generation, admission_revision);
                    if !recovered {
                        self.validate_request_identity(
                            &SafetyCommand::Recover,
                            generation,
                            admission_revision,
                        )?;
                    }
                    Ok(Value::Bool(recovered))
                }
                _ => Ok(Value::Null),
            };
        }
        let Self::Live(hook) = self else {
            return Err(error("安全服务后端不可用"));
        };
        match command {
            SafetyCommand::UpdateConfig { config } => {
                config.validate()?;
                value(hook.update_config(*config))
            }
            SafetyCommand::ConfigureVision { root } => {
                value(hook.set_vision(crate::automation::VisionService::new(root)))
            }
            SafetyCommand::StartRecording { moves, clicks } => {
                value(hook.start_recording(moves, clicks))
            }
            SafetyCommand::RecordingOptions { moves, clicks } => {
                value(hook.set_recording_options(moves, clicks))
            }
            SafetyCommand::StopRecording { discard_tail } => {
                value(hook.stop_recording(discard_tail))
            }
            SafetyCommand::RecordingStatus => value(Ok(hook.recording_status())),
            SafetyCommand::StartBehavior { name } => value(hook.start_behavior_recording(name)),
            SafetyCommand::StopBehavior => value(hook.stop_behavior_recording()),
            SafetyCommand::DiscardBehavior => value(hook.discard_behavior_recording()),
            SafetyCommand::CompleteBehaviorClaim => value(hook.complete_behavior_recording_claim()),
            SafetyCommand::BehaviorStatus => value(Ok(hook.behavior_recording_status())),
            SafetyCommand::Play { rule } => {
                value(hook.play_with_generation(*rule, generation, admission_revision))
            }
            SafetyCommand::Stop => {
                hook.stop_macro();
                Ok(Value::Null)
            }
            SafetyCommand::PlaybackStatus => value(Ok(hook.playback_status())),
            SafetyCommand::IsPlaying => value(Ok(hook.is_playback_running())),
            SafetyCommand::Notification => value(Ok(hook.take_runtime_notification())),
            SafetyCommand::Acknowledge { id } => {
                value(Ok(hook.acknowledge_runtime_notification(id)))
            }
            SafetyCommand::Shutdown => value(Ok(hook.shutdown())),
            SafetyCommand::Recover => {
                let result = hook.recover_input_safety_at(generation, admission_revision);
                if result.is_err() {
                    self.validate_request_identity(
                        &SafetyCommand::Recover,
                        generation,
                        admission_revision,
                    )?;
                }
                value(result)
            }
        }
    }
}

/// Production `mock=false` is selected by main before any Tauri setup. The
/// input-free mock entry point is only dispatched by the integration fixture.
pub fn worker_main(mock: bool) {
    worker_main_inner(mock, None, true, None);
}

/// Input-free integration fixture only. This always constructs the mock
/// authority; the production main never dispatches this entry point.
pub fn worker_main_mock_with_dispatch_hook(hook: fn(&SafetyCommand)) {
    worker_main_inner(true, Some(hook), true, None);
}

/// Input-free unsafe-cleanup process fixture, not dispatched by production main.
pub fn worker_main_mock_unsafe_cleanup() {
    worker_main_inner(true, None, false, None);
}

/// Input-free startup-error fixture. Production main dispatches no such flag;
/// it tests the same startup response/quarantine policy without creating hooks.
pub fn worker_main_mock_startup_failure(rollback_confirmed: bool) {
    worker_main_inner(true, None, true, Some(rollback_confirmed));
}

fn worker_main_inner(
    mock: bool,
    dispatch_hook: Option<fn(&SafetyCommand)>,
    cleanup_safe: bool,
    startup_failure: Option<bool>,
) {
    let bootstrap: ServiceBootstrap = match read_document(&mut std::io::stdin().lock()) {
        Ok(value) => value,
        Err(_) => return,
    };
    if bootstrap.version != VERSION
        || bootstrap.secret.len() != 64
        || bootstrap.session.len() != 32
        || bootstrap.parent_pid == 0
    {
        return;
    }
    let response = |sequence, result, generation, admission_revision| ServiceResponse {
        version: VERSION,
        secret: bootstrap.secret.clone(),
        session: bootstrap.session.clone(),
        sequence,
        generation,
        admission_revision,
        result,
    };
    let _authority = match claim_authority(mock, &bootstrap.session) {
        Ok(handle) => handle,
        Err(e) => {
            let _ = write_document(
                &mut std::io::stdout().lock(),
                &response(0, Err(error(e)), 0, 0),
            );
            return;
        }
    };
    let mut watch = match ParentWatch::new(
        &bootstrap.lease_name,
        &bootstrap.stop_name,
        bootstrap.parent_pid,
    ) {
        Ok(watch) => watch,
        Err(_) => return,
    };
    if let Some(rollback_confirmed) = startup_failure {
        complete_failed_startup(
            AppError::invalid(
                if rollback_confirmed {
                    "mock_startup_failure"
                } else {
                    "input_service_start_rollback_unconfirmed"
                },
                "input-free startup failure fixture",
            ),
            |failure| {
                let _ = write_document(
                    &mut std::io::stdout().lock(),
                    &response(0, Err(failure), 0, 0),
                );
            },
        );
        return;
    }
    let runtime = if mock {
        Arc::new(ServiceRuntime::Mock(RuntimeController::new(), cleanup_safe))
    } else {
        HookService::bind_frontend_process(bootstrap.parent_pid);
        match HookService::start(
            bootstrap.config,
            crate::automation::VisionService::new(bootstrap.image_root),
        ) {
            Ok(hook) => Arc::new(ServiceRuntime::Live(Arc::new(hook))),
            Err(e) => {
                complete_failed_startup(e, |failure| {
                    let _ = write_document(
                        &mut std::io::stdout().lock(),
                        &response(0, Err(failure), 0, 0),
                    );
                });
                return;
            }
        }
    };
    let lost = Arc::new(AtomicBool::new(false));
    let ending = Arc::new(AtomicBool::new(false));
    let guardian_runtime = runtime.clone();
    let guardian_lost = lost.clone();
    let guardian_ending = ending.clone();
    let guardian = std::thread::Builder::new()
        .name("autoflow-safety-controller-watch".into())
        .spawn(move || {
            while !guardian_ending.load(Ordering::Acquire) {
                if watch.stop_requested() {
                    guardian_runtime.stop();
                }
                if guardian_lost.load(Ordering::Acquire) || !watch.healthy() {
                    guardian_lost.store(true, Ordering::Release);
                    let safe = controller_loss_cleanup(
                        || guardian_runtime.fast_shutdown(),
                        || guardian_runtime.shutdown(),
                    );
                    if safe {
                        // The controller is gone. A blocked ordinary RPC or
                        // stdin reader must not retain a safely drained input
                        // authority indefinitely. Hook shutdown confirms run
                        // teardown, ledger emptiness and containment first.
                        std::process::exit(0);
                    }
                    break;
                }
            }
        });
    let Ok(guardian) = guardian else {
        runtime.fast_shutdown();
        retain_authority_if_unsafe(runtime.shutdown());
        return;
    };
    let (incoming_tx, incoming_rx) = sync_channel(1);
    let reader_lost = lost.clone();
    let reader = std::thread::Builder::new()
        .name("autoflow-safety-control-reader".into())
        .spawn(move || loop {
            let request = read_document::<ServiceRequest>(&mut std::io::stdin().lock());
            if request.is_err() {
                reader_lost.store(true, Ordering::Release);
                break;
            }
            if incoming_tx.send(request).is_err() {
                break;
            }
        });
    if reader.is_err() {
        lost.store(true, Ordering::Release);
    }
    let mut replies = match DocumentWriter::spawn(std::io::stdout()) {
        Ok(writer) => writer,
        Err(_) => {
            runtime.fast_shutdown();
            retain_authority_if_unsafe(runtime.shutdown());
            ending.store(true, Ordering::Release);
            let _ = guardian.join();
            return;
        }
    };
    if replies
        .send(
            response(
                0,
                Ok(Value::String("ready".into())),
                runtime.generation(),
                runtime.admission_revision(),
            ),
            &lost,
            Duration::from_millis(500),
        )
        .is_err()
    {
        lost.store(true, Ordering::Release);
    }
    let mut next_sequence = 1u64;
    while !lost.load(Ordering::Acquire) {
        let request = match incoming_rx.recv_timeout(Duration::from_millis(10)) {
            Ok(Ok(request)) => request,
            Ok(Err(_)) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        };
        if request.version != VERSION
            || request.secret != bootstrap.secret
            || request.session != bootstrap.session
            || request.sequence != next_sequence
        {
            lost.store(true, Ordering::Release);
            break;
        }
        next_sequence = next_sequence.saturating_add(1);
        let shutting_down = matches!(request.command, SafetyCommand::Shutdown);
        let result = runtime.execute(
            request.command,
            request.generation,
            request.admission_revision,
            dispatch_hook,
        );
        if replies
            .send(
                response(
                    request.sequence,
                    result,
                    runtime.generation(),
                    runtime.admission_revision(),
                ),
                &lost,
                Duration::from_millis(500),
            )
            .is_err()
        {
            break;
        }
        if shutting_down {
            break;
        }
    }
    runtime.fast_shutdown();
    retain_authority_if_unsafe(runtime.shutdown());
    ending.store(true, Ordering::Release);
    drop(incoming_rx);
    let _ = guardian.join();
    // stdin reader may still be blocked on a live parent's handle. It owns no
    // input authority; process exit closes it rather than unbounded join.
}

#[cfg(test)]
mod guardian_tests {
    use super::{controller_loss_cleanup, RuntimeController, SafetyCommand, ServiceRuntime};
    use crate::MacroRule;
    use serde_json::Value;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc::sync_channel, Arc};
    use std::time::Duration;

    fn mock_rule() -> MacroRule {
        serde_json::from_value(serde_json::json!({
            "id": "identity-test",
            "name": "input-free",
            "program": {"kind": "macro", "steps": []}
        }))
        .expect("mock rule")
    }

    #[test]
    fn stale_non_emergency_request_is_rejected_without_disturbing_active_run() {
        let control = RuntimeController::new();
        let runtime = ServiceRuntime::Mock(control, true);
        runtime
            .execute(
                SafetyCommand::Play {
                    rule: Box::new(mock_rule()),
                },
                0,
                0,
                None,
            )
            .expect("current play identity");

        let rejection = runtime
            .execute(SafetyCommand::DiscardBehavior, 0, 0, None)
            .expect_err("pre-admission revision must be stale");
        assert_eq!(rejection.code, "safety_service_stale_request");
        let status = runtime
            .execute(SafetyCommand::PlaybackStatus, 0, 1, None)
            .expect("current status identity");
        assert_eq!(status["running"], true);
    }

    #[test]
    fn stale_recover_cannot_unlock_a_new_generation() {
        let control = RuntimeController::new();
        let runtime = ServiceRuntime::Mock(control.clone(), true);
        runtime
            .execute(
                SafetyCommand::Play {
                    rule: Box::new(mock_rule()),
                },
                0,
                0,
                None,
            )
            .expect("current play identity");
        let token = control.active_token().expect("active token");
        control.request_stop();
        assert!(control.begin_cleaning(token));
        assert!(control.finish(token, false));

        let rejection = runtime
            .execute(SafetyCommand::Recover, 0, 1, None)
            .expect_err("old generation recovery must be stale");
        assert_eq!(rejection.code, "safety_service_stale_request");
        let status = runtime
            .execute(SafetyCommand::PlaybackStatus, 1, 2, None)
            .expect("current status identity");
        assert_eq!(status["phase"], "FaultLocked");
        assert_eq!(
            runtime
                .execute(SafetyCommand::Recover, 1, 2, None)
                .expect("current recovery identity"),
            Value::Bool(true)
        );
    }

    #[test]
    fn fault_after_final_dispatch_validation_rejects_recover_at_commit() {
        fn observe_dispatch(_: &SafetyCommand) {}
        fn inject_new_fault(control: &RuntimeController) {
            control.lock_fault();
        }

        let control = RuntimeController::new();
        control.lock_fault();
        let generation = control.generation();
        let admission_revision = control.background_generation();
        let runtime = ServiceRuntime::Mock(control.clone(), true);

        let rejection = runtime
            .execute_inner(
                SafetyCommand::Recover,
                generation,
                admission_revision,
                Some(observe_dispatch),
                Some(inject_new_fault),
            )
            .expect_err("fault after final dispatch validation must stale recovery");

        assert_eq!(rejection.code, "safety_service_stale_request");
        assert_eq!(
            control.phase(),
            crate::runtime_control::RuntimePhase::FaultLocked
        );
        assert!(!control.background_input_allowed());
        assert!(
            control.recover_after_cleanup_at(control.generation(), control.background_generation())
        );
    }

    #[test]
    fn emergency_commands_ignore_stale_request_identity() {
        let control = RuntimeController::new();
        let runtime = ServiceRuntime::Mock(control, true);
        runtime
            .execute(
                SafetyCommand::Play {
                    rule: Box::new(mock_rule()),
                },
                0,
                0,
                None,
            )
            .expect("current play identity");
        runtime
            .execute(SafetyCommand::Stop, u64::MAX, u64::MAX, None)
            .expect("stale stop remains emergency-authorized");
        assert_eq!(runtime.generation(), 1);
        assert_eq!(
            runtime
                .execute(SafetyCommand::Shutdown, 0, 0, None)
                .expect("stale shutdown remains emergency-authorized"),
            Value::Bool(true)
        );
    }

    #[test]
    fn guardian_revokes_and_cleans_without_waiting_for_blocked_business_work() {
        let (entered_tx, entered_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(1);
        let business = std::thread::spawn(move || {
            entered_tx.send(()).expect("entered");
            release_rx.recv().expect("release fixture");
        });
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("blocked business");
        let revoked = Arc::new(AtomicBool::new(false));
        let (done_tx, done_rx) = sync_channel(1);
        let guard_flag = revoked.clone();
        let guardian = std::thread::spawn(move || {
            let safe = controller_loss_cleanup(
                || guard_flag.store(true, Ordering::Release),
                || {
                    assert!(guard_flag.load(Ordering::Acquire));
                    true
                },
            );
            done_tx.send(safe).expect("result");
        });
        let result = done_rx.recv_timeout(Duration::from_secs(1));
        release_tx.send(()).expect("release business");
        business.join().expect("business exit");
        guardian.join().expect("guardian exit");
        assert!(result.expect("guardian did not wait for business work"));
        assert!(revoked.load(Ordering::Acquire));
    }

    #[test]
    fn unconfirmed_cleanup_never_authorizes_guardian_exit() {
        let revoked = AtomicBool::new(false);
        assert!(!controller_loss_cleanup(
            || revoked.store(true, Ordering::Release),
            || {
                assert!(revoked.load(Ordering::Acquire));
                false
            },
        ));
    }
}
