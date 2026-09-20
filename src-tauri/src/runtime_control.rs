//! Concurrency-safe lifecycle control for macro playback.
//!
//! The UI, a global hotkey callback, and the Rhai runner can all request a
//! playback at roughly the same time.  This module is deliberately independent
//! from Windows and from the input implementation so that the safety rules can
//! be tested with deterministic unit tests.

use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimePhase {
    Idle,
    Starting,
    Running,
    Stopping,
    Cleaning,
    FaultLocked,
    ShuttingDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunToken {
    pub(crate) id: u64,
    pub(crate) generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartError {
    ShuttingDown,
    SafetyLocked,
    Cancelled,
    Busy(RuntimePhase),
    StatePoisoned,
}

impl Display for StartError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ShuttingDown => formatter.write_str("AutoFlow 正在关闭，已拒绝启动宏"),
            Self::SafetyLocked => formatter
                .write_str("输入安全状态尚未恢复，已禁止启动宏；请先完成输入清理并确认安全"),
            Self::Cancelled => formatter.write_str("宏启动已被停止请求取消"),
            Self::Busy(phase) => write!(formatter, "已有宏处于 {phase:?} 状态"),
            Self::StatePoisoned => formatter.write_str("运行控制状态异常，请重启 AutoFlow"),
        }
    }
}

#[derive(Debug)]
struct ControllerState {
    phase: RuntimePhase,
    active: Option<RunToken>,
}

/// The single admission and cancellation authority for playback.
pub(crate) struct RuntimeController {
    state: Mutex<ControllerState>,
    generation: AtomicU64,
    background_generation: AtomicU64,
    next_run_id: AtomicU64,
    revoked_through_run_id: AtomicU64,
    run_id_exhausted: AtomicBool,
    shutting_down: AtomicBool,
    input_enabled: AtomicBool,
    fault_latched: AtomicBool,
}

impl RuntimeController {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ControllerState {
                phase: RuntimePhase::Idle,
                active: None,
            }),
            generation: AtomicU64::new(0),
            background_generation: AtomicU64::new(0),
            next_run_id: AtomicU64::new(0),
            revoked_through_run_id: AtomicU64::new(0),
            run_id_exhausted: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            input_enabled: AtomicBool::new(true),
            fault_latched: AtomicBool::new(false),
        })
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Latch a channel/desktop fault without waiting for business locks. Only
    /// explicit recovery after validated cleanup can clear this latch.
    pub(crate) fn lock_fault(&self) -> u64 {
        self.fault_latched.store(true, Ordering::Release);
        self.input_enabled.store(false, Ordering::Release);
        let generation = self.request_stop();
        // Recovery may have overlapped the stop-generation increment. Reassert
        // the latch after it so that this fault cannot be lost by that recovery.
        self.fault_latched.store(true, Ordering::Release);
        self.input_enabled.store(false, Ordering::Release);
        if let Ok(mut state) = self.state.try_lock() {
            if state.active.is_none() {
                state.phase = RuntimePhase::FaultLocked;
            }
        }
        // A lifecycle transition may have held the state mutex while this
        // non-blocking fault path ran. Reassert the atomic safety boundary
        // after the best-effort phase update so that such a transition cannot
        // leave input enabled from an earlier view of the latch.
        self.fault_latched.store(true, Ordering::Release);
        self.input_enabled.store(false, Ordering::Release);
        generation
    }

    /// Publish an inactive state without allowing a concurrent fault or
    /// shutdown to be overwritten by a stale transition decision.
    fn settle_inactive(&self, state: &mut ControllerState) {
        state.phase = RuntimePhase::Idle;
        self.input_enabled.store(true, Ordering::Release);

        // Input is enabled before the final safety reads. If a fault/shutdown
        // happened earlier, this branch revokes it again; if it happens later,
        // that path's atomic revocation is ordered after this enable.
        if self.shutting_down.load(Ordering::Acquire) {
            state.phase = RuntimePhase::ShuttingDown;
            self.input_enabled.store(false, Ordering::Release);
        } else if self.fault_latched.load(Ordering::Acquire) {
            state.phase = RuntimePhase::FaultLocked;
            self.input_enabled.store(false, Ordering::Release);
        }
    }

    #[cfg(test)]
    pub(crate) fn begin_start(
        self: &Arc<Self>,
        expected_generation: Option<u64>,
    ) -> Result<StartLease, StartError> {
        self.begin_start_at_revision(expected_generation, self.background_generation())
    }

    /// A request submitted before another admission cannot become a fresh
    /// playback after that run finishes, even without an emergency stop.
    pub(crate) fn begin_start_at_revision(
        self: &Arc<Self>,
        expected_generation: Option<u64>,
        expected_revision: u64,
    ) -> Result<StartLease, StartError> {
        let generation = self.generation();
        if expected_generation.is_some_and(|expected| expected != generation) {
            return Err(StartError::Cancelled);
        }
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(StartError::ShuttingDown);
        }
        let mut state = self.state.lock().map_err(|_| StartError::StatePoisoned)?;
        // A stop may arrive while admission is waiting for this mutex.
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(StartError::ShuttingDown);
        }
        if self.generation() != generation {
            return Err(StartError::Cancelled);
        }
        if self.background_generation() != expected_revision {
            return Err(StartError::Cancelled);
        }
        if state.phase != RuntimePhase::Idle {
            return Err(if state.phase == RuntimePhase::FaultLocked {
                StartError::SafetyLocked
            } else {
                StartError::Busy(state.phase)
            });
        }
        if self.fault_latched.load(Ordering::Acquire) || !self.input_enabled.load(Ordering::Acquire)
        {
            return Err(StartError::SafetyLocked);
        }
        let Some(id) = self.allocate_run_id() else {
            self.fault_latched.store(true, Ordering::Release);
            self.input_enabled.store(false, Ordering::Release);
            state.phase = RuntimePhase::FaultLocked;
            return Err(StartError::SafetyLocked);
        };
        let token = RunToken { id, generation };
        state.phase = RuntimePhase::Starting;
        state.active = Some(token);
        // Invalidate conveniences admitted before this macro, even if it later
        // completes normally without a stop-generation increment.
        self.background_generation.fetch_add(1, Ordering::AcqRel);
        // Background remapping/text expansion must stop being admitted as soon
        // as a playback owns the controller.  The playback permit is enabled
        // again only after all validation has completed and `activate` moves
        // the run to `Running`.
        self.input_enabled.store(false, Ordering::Release);
        Ok(StartLease {
            controller: Arc::clone(self),
            token,
            committed: false,
        })
    }

    pub(crate) fn request_stop(&self) -> u64 {
        self.background_generation.fetch_add(1, Ordering::AcqRel);
        let generation = self
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        // The atomic gate is the safety boundary.  Do not wait for the state
        // mutex here: this method is also called from a low-level keyboard
        // hook and must still revoke future input when a worker owns the
        // lifecycle mutex.
        if let Ok(mut state) = self.state.try_lock() {
            match state.phase {
                RuntimePhase::Idle => {
                    // An emergency stop while idle must not permanently lock
                    // the application.  The generation still invalidates any
                    // queued start request.
                    self.settle_inactive(&mut state);
                }
                RuntimePhase::FaultLocked | RuntimePhase::ShuttingDown => {
                    self.input_enabled.store(false, Ordering::Release);
                }
                RuntimePhase::Starting
                | RuntimePhase::Running
                | RuntimePhase::Stopping
                | RuntimePhase::Cleaning => {
                    self.input_enabled.store(false, Ordering::Release);
                    state.phase = RuntimePhase::Stopping;
                }
            }
        } else {
            self.input_enabled.store(false, Ordering::Release);
        }
        generation
    }

    pub(crate) fn request_shutdown(&self) -> u64 {
        self.shutting_down.store(true, Ordering::Release);
        let generation = self.request_stop();
        if let Ok(mut state) = self.state.try_lock() {
            state.phase = RuntimePhase::ShuttingDown;
        }
        generation
    }

    /// Revoke only the run identified by `token`. The monotonic watermark is
    /// the permission boundary, so this remains immediate even when the
    /// lifecycle mutex is contended. A stale token can only revoke itself and
    /// older identities; it can never affect a newer run id.
    pub(crate) fn revoke_token(&self, token: RunToken) {
        if token.id == 0 {
            return;
        }
        self.revoked_through_run_id
            .fetch_max(token.id, Ordering::AcqRel);
        if let Ok(mut state) = self.state.try_lock() {
            if state.active == Some(token)
                && matches!(state.phase, RuntimePhase::Starting | RuntimePhase::Running)
            {
                state.phase = RuntimePhase::Stopping;
            }
        }
    }

    /// Lock-free token-specific cancellation check used by playback watches.
    /// Run ids are never reused, so a high-watermark cancellation cannot
    /// accidentally revoke a later run.
    pub(crate) fn token_revoked(&self, token: RunToken) -> bool {
        token.id == 0 || token.id <= self.revoked_through_run_id.load(Ordering::Acquire)
    }

    pub(crate) fn activate(&self, token: RunToken) -> bool {
        if self.shutting_down.load(Ordering::Acquire)
            || self.generation() != token.generation
            || self.token_revoked(token)
        {
            return false;
        }
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.active == Some(token)
            && state.phase == RuntimePhase::Starting
            && !self.shutting_down.load(Ordering::Acquire)
            && !self.fault_latched.load(Ordering::Acquire)
            && self.generation() == token.generation
            && !self.token_revoked(token)
        {
            state.phase = RuntimePhase::Running;
            self.input_enabled.store(true, Ordering::Release);
            // Do not let a fault arriving between the admission checks and
            // the permit publication leave this run enabled. A fault arriving
            // after these reads performs its own later atomic revocation.
            if self.shutting_down.load(Ordering::Acquire)
                || self.fault_latched.load(Ordering::Acquire)
                || self.generation() != token.generation
                || self.token_revoked(token)
            {
                self.input_enabled.store(false, Ordering::Release);
                state.phase = if self.shutting_down.load(Ordering::Acquire) {
                    RuntimePhase::ShuttingDown
                } else {
                    RuntimePhase::Stopping
                };
                false
            } else {
                true
            }
        } else {
            false
        }
    }

    pub(crate) fn begin_cleaning(&self, token: RunToken) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.active != Some(token) {
            return false;
        }
        state.phase = RuntimePhase::Cleaning;
        // No new input may enter once the worker starts releasing its ledger.
        self.input_enabled.store(false, Ordering::Release);
        true
    }

    /// Complete the active run.  A stale worker can never complete a newer
    /// run because the token includes both the monotonically increasing id and
    /// the stop generation.
    pub(crate) fn finish(&self, token: RunToken, safe: bool) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.active != Some(token) {
            return false;
        }
        state.active = None;
        if !safe {
            self.fault_latched.store(true, Ordering::Release);
        }
        if safe {
            self.settle_inactive(&mut state);
        } else {
            state.phase = if self.shutting_down.load(Ordering::Acquire) {
                RuntimePhase::ShuttingDown
            } else {
                RuntimePhase::FaultLocked
            };
            self.input_enabled.store(false, Ordering::Release);
        }
        true
    }

    /// Commit recovery only for the exact request identity that proved the
    /// cleanup preconditions. Both identities are checked while the lifecycle
    /// mutex is held and again after publishing the recovered state.
    pub(crate) fn recover_after_cleanup_at(
        &self,
        expected_generation: u64,
        expected_revision: u64,
    ) -> bool {
        if self.shutting_down.load(Ordering::Acquire) {
            return false;
        }
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.active.is_none() && self.fault_latched.load(Ordering::Acquire) {
            if self.generation() != expected_generation
                || self.background_generation() != expected_revision
            {
                return false;
            }
            self.fault_latched.store(false, Ordering::Release);
            state.phase = RuntimePhase::Idle;
            self.input_enabled.store(true, Ordering::Release);
            if self.generation() != expected_generation
                || self.background_generation() != expected_revision
                || self.shutting_down.load(Ordering::Acquire)
            {
                self.fault_latched.store(true, Ordering::Release);
                self.input_enabled.store(false, Ordering::Release);
                state.phase = RuntimePhase::FaultLocked;
                return false;
            }
            true
        } else {
            false
        }
    }

    pub(crate) fn is_quiescent(&self) -> bool {
        self.state
            .try_lock()
            .map(|state| state.active.is_none())
            .unwrap_or(false)
    }

    pub(crate) fn abort_start(&self, token: RunToken) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.active != Some(token)
            || !matches!(
                state.phase,
                RuntimePhase::Starting | RuntimePhase::Stopping | RuntimePhase::ShuttingDown
            )
        {
            return false;
        }
        state.active = None;
        self.settle_inactive(&mut state);
        true
    }

    pub(crate) fn input_allowed(&self, token: RunToken) -> bool {
        !self.token_revoked(token)
            && self.input_enabled.load(Ordering::Acquire)
            && !self.fault_latched.load(Ordering::Acquire)
            && !self.shutting_down.load(Ordering::Acquire)
            && self.generation() == token.generation
            && self
                .state
                .try_lock()
                .map(|state| state.active == Some(token) && state.phase == RuntimePhase::Running)
                .unwrap_or(false)
    }

    /// Background conveniences such as remapping and text expansion may only
    /// inject while no macro owns the broker.  A failed lock check is treated
    /// as unsafe and therefore denies the injection.
    pub(crate) fn background_input_allowed(&self) -> bool {
        if !self.input_enabled.load(Ordering::Acquire)
            || self.shutting_down.load(Ordering::Acquire)
            || self.fault_latched.load(Ordering::Acquire)
        {
            return false;
        }
        self.state
            .try_lock()
            .map(|state| state.active.is_none() && state.phase == RuntimePhase::Idle)
            .unwrap_or(false)
    }

    pub(crate) fn background_generation(&self) -> u64 {
        self.background_generation.load(Ordering::Acquire)
    }

    pub(crate) fn invalidate_background_admission(&self) {
        self.background_generation.fetch_add(1, Ordering::AcqRel);
    }
    pub(crate) fn background_input_allowed_at(&self, generation: u64) -> bool {
        self.background_generation() == generation
            && self.background_input_allowed()
            && self.background_generation() == generation
    }

    pub(crate) fn phase(&self) -> RuntimePhase {
        if self.shutting_down.load(Ordering::Acquire) {
            return RuntimePhase::ShuttingDown;
        }
        if self.fault_latched.load(Ordering::Acquire) {
            return RuntimePhase::FaultLocked;
        }
        self.state
            .try_lock()
            .map(|state| state.phase)
            .unwrap_or(RuntimePhase::FaultLocked)
    }

    pub(crate) fn active_token(&self) -> Option<RunToken> {
        self.state.try_lock().ok().and_then(|state| state.active)
    }

    pub(crate) fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Acquire)
    }

    fn allocate_run_id(&self) -> Option<u64> {
        loop {
            let current = self.next_run_id.load(Ordering::Acquire);
            if current == u64::MAX || self.run_id_exhausted.load(Ordering::Acquire) {
                self.run_id_exhausted.store(true, Ordering::Release);
                return None;
            }
            let next = current + 1;
            if self
                .next_run_id
                .compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(next);
            }
        }
    }
}

pub(crate) struct StartLease {
    controller: Arc<RuntimeController>,
    token: RunToken,
    committed: bool,
}

impl StartLease {
    pub(crate) fn token(&self) -> RunToken {
        self.token
    }

    pub(crate) fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for StartLease {
    fn drop(&mut self) {
        if !self.committed {
            self.controller.abort_start(self.token);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RuntimeController, RuntimePhase, StartError};
    use std::sync::atomic::Ordering;

    #[test]
    fn queued_request_cannot_start_after_competing_run_completes() {
        let controller = RuntimeController::new();
        let generation = controller.generation();
        let queued_revision = controller.background_generation();
        let mut ui = controller
            .begin_start_at_revision(Some(generation), queued_revision)
            .expect("UI admission");
        let token = ui.token();
        assert!(controller.activate(token));
        ui.commit();
        assert!(controller.begin_cleaning(token));
        assert!(controller.finish(token, true));
        assert_eq!(controller.generation(), generation);
        assert!(matches!(
            controller.begin_start_at_revision(Some(generation), queued_revision),
            Err(StartError::Cancelled)
        ));
        assert_eq!(controller.phase(), RuntimePhase::Idle);
        assert!(controller
            .begin_start_at_revision(Some(generation), controller.background_generation())
            .is_ok());
    }

    #[test]
    fn failed_competing_admission_also_invalidates_prior_queued_request() {
        let controller = RuntimeController::new();
        let revision = controller.background_generation();
        let competitor = controller.begin_start(None).expect("starting");
        drop(competitor); // validation/startup failed, not an executed macro
        assert_eq!(controller.phase(), RuntimePhase::Idle);
        assert!(matches!(
            controller.begin_start_at_revision(None, revision),
            Err(StartError::Cancelled)
        ));
    }

    #[test]
    fn stale_background_work_cannot_resume_after_stop_or_normal_macro_completion() {
        let controller = RuntimeController::new();
        let before_stop = controller.background_generation();
        assert!(controller.background_input_allowed_at(before_stop));
        controller.request_stop();
        assert!(!controller.background_input_allowed_at(before_stop));
        let before_macro = controller.background_generation();
        let mut lease = controller.begin_start(None).expect("admission");
        let token = lease.token();
        assert!(controller.activate(token));
        lease.commit();
        assert!(controller.begin_cleaning(token));
        assert!(controller.finish(token, true));
        assert!(!controller.background_input_allowed_at(before_macro));
        assert!(controller.background_input_allowed_at(controller.background_generation()));
    }

    #[test]
    fn channel_fault_survives_stop_and_successful_cleanup_until_explicit_recovery() {
        let controller = RuntimeController::new();
        let mut lease = controller.begin_start(None).expect("start");
        let token = lease.token();
        assert!(controller.activate(token));
        lease.commit();
        controller.lock_fault();
        assert!(!controller.input_allowed(token));
        controller.request_stop();
        assert!(controller.begin_cleaning(token));
        assert!(controller.finish(token, true));
        assert_eq!(controller.phase(), RuntimePhase::FaultLocked);
        assert!(!controller.background_input_allowed());
        assert!(matches!(
            controller.begin_start(None),
            Err(StartError::SafetyLocked)
        ));
        assert!(controller
            .recover_after_cleanup_at(controller.generation(), controller.background_generation()));
        assert_eq!(controller.phase(), RuntimePhase::Idle);
    }

    #[test]
    fn contended_fault_latch_is_authoritative_until_explicit_recovery() {
        let controller = RuntimeController::new();
        let state = controller.state.lock().expect("state");

        // The emergency path must not wait for this lifecycle lock. Its atomic
        // latch is immediately authoritative even though the stored phase
        // cannot be updated yet.
        controller.lock_fault();
        assert_eq!(state.phase, RuntimePhase::Idle);
        assert_eq!(controller.phase(), RuntimePhase::FaultLocked);
        assert!(!controller.background_input_allowed());
        drop(state);

        assert!(matches!(
            controller.begin_start(None),
            Err(StartError::SafetyLocked)
        ));
        assert!(controller
            .recover_after_cleanup_at(controller.generation(), controller.background_generation()));
        assert_eq!(controller.phase(), RuntimePhase::Idle);
        assert!(controller.background_input_allowed());
    }

    #[test]
    fn clean_finish_after_contended_fault_cannot_restore_idle_or_input() {
        let controller = RuntimeController::new();
        let mut lease = controller.begin_start(None).expect("start");
        let token = lease.token();
        assert!(controller.activate(token));
        lease.commit();

        let state = controller.state.lock().expect("state");
        controller.lock_fault();
        assert_eq!(state.phase, RuntimePhase::Running);
        assert_eq!(controller.phase(), RuntimePhase::FaultLocked);
        drop(state);

        assert!(controller.begin_cleaning(token));
        assert!(controller.finish(token, true));
        assert_eq!(controller.phase(), RuntimePhase::FaultLocked);
        assert!(!controller.background_input_allowed());
        assert!(matches!(
            controller.begin_start(None),
            Err(StartError::SafetyLocked)
        ));
        assert!(controller
            .recover_after_cleanup_at(controller.generation(), controller.background_generation()));
        assert_eq!(controller.phase(), RuntimePhase::Idle);
    }

    #[test]
    fn stale_recovery_identity_cannot_clear_a_new_fault_latch() {
        let controller = RuntimeController::new();
        controller.lock_fault();
        let stale_generation = controller.generation();
        let stale_revision = controller.background_generation();

        // A later fault/stop after an outer service validation advances both
        // identities. The old recovery must fail at the lifecycle commit.
        controller.lock_fault();
        assert!(!controller.recover_after_cleanup_at(stale_generation, stale_revision));
        assert_eq!(controller.phase(), RuntimePhase::FaultLocked);
        assert!(!controller.background_input_allowed());

        assert!(controller
            .recover_after_cleanup_at(controller.generation(), controller.background_generation()));
        assert_eq!(controller.phase(), RuntimePhase::Idle);
    }

    #[test]
    fn only_one_start_can_be_admitted() {
        let controller = RuntimeController::new();
        let mut first = controller.begin_start(None).expect("first start");
        assert_eq!(controller.phase(), RuntimePhase::Starting);
        assert!(matches!(
            controller.begin_start(None),
            Err(StartError::Busy(RuntimePhase::Starting))
        ));
        let token = first.token();
        assert!(controller.activate(token));
        first.commit();
        assert_eq!(controller.phase(), RuntimePhase::Running);
        assert!(matches!(
            controller.begin_start(None),
            Err(StartError::Busy(RuntimePhase::Running))
        ));
    }

    #[test]
    fn stop_revokes_input_before_cleanup_and_stale_token_cannot_send() {
        let controller = RuntimeController::new();
        let mut lease = controller.begin_start(None).expect("start");
        let token = lease.token();
        assert!(controller.activate(token));
        lease.commit();
        controller.request_stop();
        assert!(!controller.input_allowed(token));
        assert!(controller.begin_cleaning(token));
        assert!(controller.finish(token, true));
        assert_eq!(controller.phase(), RuntimePhase::Idle);
    }

    #[test]
    fn token_revoke_is_immediate_without_the_state_mutex_and_is_identity_scoped() {
        let controller = RuntimeController::new();
        let mut lease = controller.begin_start(None).expect("start");
        let token = lease.token();
        assert!(controller.activate(token));
        lease.commit();

        let state = controller.state.lock().expect("state");
        controller.revoke_token(token);
        assert!(controller.token_revoked(token));
        assert!(!controller.input_allowed(token));
        assert!(!controller.token_revoked(super::RunToken {
            id: token.id + 1,
            generation: token.generation,
        }));
        drop(state);

        assert!(!controller.input_allowed(token));
        assert!(controller.begin_cleaning(token));
        assert!(controller.finish(token, true));
    }

    #[test]
    fn stale_token_revoke_cannot_cancel_a_newer_active_run() {
        let controller = RuntimeController::new();
        let mut old_lease = controller.begin_start(None).expect("old start");
        let old_token = old_lease.token();
        assert!(controller.activate(old_token));
        old_lease.commit();
        assert!(controller.begin_cleaning(old_token));
        assert!(controller.finish(old_token, true));

        let mut new_lease = controller.begin_start(None).expect("new start");
        let new_token = new_lease.token();
        assert!(controller.activate(new_token));
        new_lease.commit();

        // Models a paused key-up callback resuming with the captured old
        // identity after an unrelated UI run has become active.
        controller.revoke_token(old_token);
        assert!(controller.token_revoked(old_token));
        assert!(!controller.token_revoked(new_token));
        assert!(controller.input_allowed(new_token));

        assert!(controller.begin_cleaning(new_token));
        assert!(controller.finish(new_token, true));
    }

    #[test]
    fn token_revocation_is_monotonic_and_run_identity_exhaustion_fails_closed() {
        let controller = RuntimeController::new();
        let mut first_lease = controller.begin_start(None).expect("first start");
        let first = first_lease.token();
        assert!(controller.activate(first));
        first_lease.commit();
        controller.revoke_token(first);
        controller.revoke_token(first);
        assert_eq!(
            controller.revoked_through_run_id.load(Ordering::Acquire),
            first.id
        );
        assert!(controller.begin_cleaning(first));
        assert!(controller.finish(first, true));

        let mut second_lease = controller.begin_start(None).expect("second start");
        let second = second_lease.token();
        assert!(controller.activate(second));
        second_lease.commit();
        controller.revoke_token(second);
        controller.revoke_token(first);
        assert_eq!(
            controller.revoked_through_run_id.load(Ordering::Acquire),
            second.id
        );
        assert!(!controller.input_allowed(second));
        assert!(controller.begin_cleaning(second));
        assert!(controller.finish(second, true));

        let exhausted = RuntimeController::new();
        exhausted.next_run_id.store(u64::MAX - 1, Ordering::Release);
        let last = exhausted.begin_start(None).expect("last unique identity");
        assert_eq!(last.token().id, u64::MAX);
        drop(last);
        assert!(matches!(
            exhausted.begin_start(None),
            Err(StartError::SafetyLocked)
        ));
        assert!(exhausted.run_id_exhausted.load(Ordering::Acquire));
        assert_eq!(exhausted.phase(), RuntimePhase::FaultLocked);
    }

    #[test]
    fn stale_worker_cannot_finish_a_newer_run() {
        let controller = RuntimeController::new();
        let first = controller.begin_start(None).expect("first start");
        let first_token = first.token();
        drop(first);
        let mut second = controller.begin_start(None).expect("second start");
        let second_token = second.token();
        assert_ne!(first_token.id, second_token.id);
        assert!(controller.activate(second_token));
        second.commit();
        assert!(!controller.finish(first_token, true));
        assert_eq!(controller.active_token(), Some(second_token));
    }

    #[test]
    fn cleanup_failure_locks_until_explicit_recovery() {
        let controller = RuntimeController::new();
        let mut lease = controller.begin_start(None).expect("start");
        let token = lease.token();
        assert!(controller.activate(token));
        lease.commit();
        controller.request_stop();
        controller.begin_cleaning(token);
        assert!(controller.finish(token, false));
        assert_eq!(controller.phase(), RuntimePhase::FaultLocked);
        assert!(matches!(
            controller.begin_start(None),
            Err(StartError::SafetyLocked)
        ));
        assert!(controller
            .recover_after_cleanup_at(controller.generation(), controller.background_generation()));
        assert!(controller.begin_start(None).is_ok());
    }

    #[test]
    fn shutdown_rejects_future_starts_and_never_reenables_input() {
        let controller = RuntimeController::new();
        controller.request_shutdown();
        assert_eq!(controller.phase(), RuntimePhase::ShuttingDown);
        assert!(matches!(
            controller.begin_start(None),
            Err(StartError::ShuttingDown)
        ));
    }

    #[test]
    fn cancelled_start_is_removed_even_when_stop_changes_phase_first() {
        let controller = RuntimeController::new();
        let lease = controller.begin_start(None).expect("start");
        let token = lease.token();
        controller.request_stop();
        drop(lease);

        assert!(controller.is_quiescent());
        assert_eq!(controller.phase(), RuntimePhase::Idle);
        assert!(controller.begin_start(None).is_ok());
        assert!(!controller.input_allowed(token));
    }

    #[test]
    fn cleaning_phase_revokes_input_before_release() {
        let mut lease = RuntimeController::new().begin_start(None).expect("start");
        let controller = std::sync::Arc::clone(&lease.controller);
        let token = lease.token();
        assert!(controller.activate(token));
        lease.commit();
        assert!(controller.input_allowed(token));
        assert!(controller.begin_cleaning(token));
        assert!(!controller.input_allowed(token));
        assert!(controller.finish(token, true));
    }

    #[test]
    fn stop_does_not_wait_for_the_lifecycle_mutex() {
        let controller = RuntimeController::new();
        let mut lease = controller.begin_start(None).expect("start");
        let token = lease.token();
        assert!(controller.activate(token));
        lease.commit();
        let guard = controller.state.lock().expect("state");
        // This would deadlock if stopping acquired the business mutex.
        controller.request_stop();
        assert_ne!(controller.generation(), token.generation);
        assert!(!controller.input_allowed(token));
        drop(guard);
        assert!(controller.begin_cleaning(token));
        assert!(controller.finish(token, true));
    }

    #[test]
    fn ten_thousand_stop_restart_cycles_never_accept_old_work() {
        let controller = RuntimeController::new();
        let mut previous = None;
        for _ in 0..10_000 {
            let generation = controller.generation();
            let mut lease = controller.begin_start(Some(generation)).expect("start");
            let token = lease.token();
            assert!(controller.activate(token));
            lease.commit();
            if let Some(old) = previous {
                assert!(!controller.input_allowed(old));
                assert!(!controller.finish(old, true));
            }
            controller.request_stop();
            assert!(!controller.input_allowed(token));
            assert!(matches!(
                controller.begin_start(Some(generation)),
                Err(StartError::Cancelled)
            ));
            assert!(controller.begin_cleaning(token));
            assert!(controller.finish(token, true));
            previous = Some(token);
        }
    }
}
