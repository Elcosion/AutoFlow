//! One worker, bounded pending tasks, nonblocking producers. No OS input here.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Arc;
use std::time::Duration;

/// A loop-return flag precedes thread-local destructors. Retain the thread
/// object and, on Windows, require its OS handle to be signaled as well.
pub(crate) fn thread_exit_confirmed(thread: &std::thread::JoinHandle<()>) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        unsafe {
            windows::Win32::System::Threading::WaitForSingleObject(
                windows::Win32::Foundation::HANDLE(thread.as_raw_handle()),
                0,
            ) == windows::Win32::Foundation::WAIT_OBJECT_0
        }
    }
    #[cfg(not(windows))]
    {
        thread.is_finished()
    }
}

#[cfg(windows)]
pub(crate) fn thread_running_confirmed(thread: &std::thread::JoinHandle<()>) -> bool {
    use std::os::windows::io::AsRawHandle;
    unsafe {
        windows::Win32::System::Threading::WaitForSingleObject(
            windows::Win32::Foundation::HANDLE(thread.as_raw_handle()),
            0,
        ) == windows::Win32::Foundation::WAIT_TIMEOUT
    }
}

pub(crate) struct BoundedWorker<T> {
    queue: SyncSender<T>,
    closed: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}
impl<T: Send + 'static> BoundedWorker<T> {
    pub(crate) fn spawn(
        name: &str,
        capacity: usize,
        mut handle: impl FnMut(T) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (queue, incoming) = sync_channel(capacity.clamp(1, 64));
        let closed = Arc::new(AtomicBool::new(false));
        let worker_closed = closed.clone();
        let exited = Arc::new(AtomicBool::new(false));
        let worker_exited = exited.clone();
        let thread = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                while !worker_closed.load(Ordering::Acquire) {
                    match incoming.recv_timeout(Duration::from_millis(20)) {
                        Ok(task) if !worker_closed.load(Ordering::Acquire) => handle(task),
                        Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
                // Only normal handler/loop completion confirms quiescence.
                // A panic does not authorize shutdown to report safety.
                worker_exited.store(true, Ordering::Release);
            })?;
        Ok(Self {
            queue,
            closed,
            exited,
            thread,
        })
    }
    pub(crate) fn try_submit(&self, task: T) -> Result<(), T> {
        if self.closed.load(Ordering::Acquire) {
            return Err(task);
        }
        self.queue.try_send(task).map_err(|error| match error {
            std::sync::mpsc::TrySendError::Full(task)
            | std::sync::mpsc::TrySendError::Disconnected(task) => task,
        })
    }
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    pub(crate) fn is_quiescent(&self) -> bool {
        self.closed.load(Ordering::Acquire)
            && self.exited.load(Ordering::Acquire)
            && thread_exit_confirmed(&self.thread)
    }
}
impl<T> Drop for BoundedWorker<T> {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
    }
}

/// Ordered recording transport. Admission and stop-barrier insertion share a
/// short mutex; producers never wait for it. Any loss latches incompleteness.
pub(crate) struct CaptureQueue<T> {
    queue: SyncSender<CaptureMessage<T>>,
    state: std::sync::Mutex<Option<u64>>,
    active: std::sync::atomic::AtomicU64,
    next: std::sync::atomic::AtomicU64,
    failed: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
    accepted: std::sync::atomic::AtomicU64,
    dropped: std::sync::atomic::AtomicU64,
    processing_failed: Arc<std::sync::atomic::AtomicU64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CaptureStatistics {
    pub accepted: u64,
    pub dropped: u64,
    pub processing_failed: u64,
}
enum CaptureMessage<T> {
    Event(u64, std::time::Instant, T),
    Barrier(SyncSender<()>),
}
impl<T: Send + 'static> CaptureQueue<T> {
    pub(crate) fn spawn(
        mut handle: impl FnMut(u64, std::time::Instant, T) -> bool + Send + 'static,
    ) -> std::io::Result<Self> {
        let (queue, incoming) = sync_channel(1024);
        let failed = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let exited = Arc::new(AtomicBool::new(false));
        let worker_failed = failed.clone();
        let worker_closed = closed.clone();
        let worker_exited = exited.clone();
        let processing_failed = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let worker_processing_failed = processing_failed.clone();
        let thread = std::thread::Builder::new()
            .name("autoflow-capture-worker".into())
            .spawn(move || {
                while !worker_closed.load(Ordering::Acquire) {
                    match incoming.recv_timeout(Duration::from_millis(20)) {
                        Ok(_) if worker_closed.load(Ordering::Acquire) => break,
                        Ok(CaptureMessage::Event(session, at, payload)) => {
                            if !handle(session, at, payload) {
                                worker_processing_failed.fetch_add(1, Ordering::AcqRel);
                                worker_failed.store(true, Ordering::Release);
                            }
                        }
                        Ok(CaptureMessage::Barrier(reply)) => {
                            let _ = reply.try_send(());
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => break,
                    }
                }
                worker_exited.store(true, Ordering::Release);
            })?;
        Ok(Self {
            queue,
            state: std::sync::Mutex::new(None),
            active: 0.into(),
            next: 0.into(),
            failed,
            closed,
            exited,
            thread,
            accepted: 0.into(),
            dropped: 0.into(),
            processing_failed,
        })
    }
    pub(crate) fn begin(&self) -> Result<u64, &'static str> {
        let mut state = self.state.try_lock().map_err(|_| "capture_busy")?;
        if state.is_some()
            || self.failed.load(Ordering::Acquire)
            || self.closed.load(Ordering::Acquire)
        {
            return Err("capture_not_ready");
        }
        let previous = self
            .next
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |id| id.checked_add(1))
            .map_err(|_| "capture_identity_exhausted")?;
        let session = previous + 1;
        self.accepted.store(0, Ordering::Release);
        self.dropped.store(0, Ordering::Release);
        self.processing_failed.store(0, Ordering::Release);
        *state = Some(session);
        self.active.store(session, Ordering::Release);
        Ok(session)
    }
    pub(crate) fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire) != 0
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.state.try_lock().map_or(true, |state| state.is_some())
    }

    pub(crate) fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    /// A callback may fail before constructing its payload (for example,
    /// unavailable modifier/configuration state). Such loss must be counted
    /// just like a full queue, never silently reported as complete capture.
    pub(crate) fn note_admission_loss(&self) -> bool {
        if self.active.load(Ordering::Acquire) == 0 {
            return false;
        }
        self.dropped.fetch_add(1, Ordering::AcqRel);
        self.failed.store(true, Ordering::Release);
        true
    }

    /// Final counts are stable only after a successful drain barrier; while
    /// callbacks/processing are active this is a best-effort atomic snapshot.
    pub(crate) fn statistics(&self) -> CaptureStatistics {
        CaptureStatistics {
            accepted: self.accepted.load(Ordering::Acquire),
            dropped: self.dropped.load(Ordering::Acquire),
            processing_failed: self.processing_failed.load(Ordering::Acquire),
        }
    }
    pub(crate) fn submit(&self, at: std::time::Instant, payload: T) -> bool {
        self.submit_inner(at, payload, false)
    }
    /// Admit the final ordered message and close this session's producer gate
    /// in the same short critical section. Failure still closes admission and
    /// latches incompleteness; it must not silently resume capture.
    pub(crate) fn submit_terminal(&self, at: std::time::Instant, payload: T) -> bool {
        self.submit_inner(at, payload, true)
    }
    fn submit_inner(&self, at: std::time::Instant, payload: T, terminal: bool) -> bool {
        let expected = self.active.load(Ordering::Acquire);
        if expected == 0 {
            return false;
        }
        let Ok(state) = self.state.try_lock() else {
            if terminal {
                let _ =
                    self.active
                        .compare_exchange(expected, 0, Ordering::AcqRel, Ordering::Acquire);
            }
            self.dropped.fetch_add(1, Ordering::AcqRel);
            self.failed.store(true, Ordering::Release);
            return false;
        };
        if *state != Some(expected) || self.active.load(Ordering::Acquire) != expected {
            return false;
        }
        if terminal {
            self.active.store(0, Ordering::Release);
        }
        if self
            .queue
            .try_send(CaptureMessage::Event(expected, at, payload))
            .is_err()
        {
            self.dropped.fetch_add(1, Ordering::AcqRel);
            self.failed.store(true, Ordering::Release);
            return false;
        }
        self.accepted.fetch_add(1, Ordering::AcqRel);
        true
    }
    pub(crate) fn drain(&self, timeout: Duration) -> Result<(), &'static str> {
        self.active.store(0, Ordering::Release);
        let deadline = std::time::Instant::now() + timeout.min(Duration::from_millis(500));
        let mut state = loop {
            match self.state.try_lock() {
                Ok(state) => break state,
                Err(std::sync::TryLockError::WouldBlock)
                    if std::time::Instant::now() < deadline =>
                {
                    std::thread::yield_now()
                }
                Err(_) => return Err("capture_busy"),
            }
        };
        if state.is_none() {
            return Err("capture_inactive");
        }
        let (reply, response) = sync_channel(1);
        let mut message = CaptureMessage::Barrier(reply);
        loop {
            match self.queue.try_send(message) {
                Ok(()) => break,
                Err(std::sync::mpsc::TrySendError::Full(returned))
                    if std::time::Instant::now() < deadline =>
                {
                    message = returned;
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => {
                    self.failed.store(true, Ordering::Release);
                    return Err("capture_drain_timeout");
                }
            }
        }
        if response
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .is_err()
        {
            self.failed.store(true, Ordering::Release);
            return Err("capture_drain_timeout");
        }
        *state = None;
        if self.failed.load(Ordering::Acquire) {
            Err("capture_incomplete")
        } else {
            Ok(())
        }
    }
    pub(crate) fn close(&self) {
        self.active.store(0, Ordering::Release);
        self.closed.store(true, Ordering::Release);
    }
    pub(crate) fn is_quiescent(&self) -> bool {
        self.closed.load(Ordering::Acquire)
            && self.exited.load(Ordering::Acquire)
            && thread_exit_confirmed(&self.thread)
    }
}
impl<T> Drop for CaptureQueue<T> {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn normal_loop_return_does_not_confirm_exit_during_thread_local_cleanup() {
        struct ExitBlock {
            entered: SyncSender<()>,
            release: std::sync::mpsc::Receiver<()>,
        }
        impl Drop for ExitBlock {
            fn drop(&mut self) {
                let _ = self.entered.try_send(());
                let _ = self.release.recv_timeout(Duration::from_secs(2));
            }
        }
        std::thread_local! {
            static EXIT_BLOCK: std::cell::RefCell<Option<ExitBlock>> = const { std::cell::RefCell::new(None) };
        }
        let (installed_tx, installed_rx) = sync_channel(1);
        let (entered_tx, entered_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(1);
        let mut block = Some(ExitBlock {
            entered: entered_tx,
            release: release_rx,
        });
        let worker = BoundedWorker::spawn("input-free-tls-exit", 1, move |_: ()| {
            EXIT_BLOCK.with(|slot| *slot.borrow_mut() = block.take());
            let _ = installed_tx.try_send(());
        })
        .expect("worker");
        worker.try_submit(()).expect("install fixture");
        installed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("TLS installed");
        worker.close();
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("TLS cleanup blocked");
        assert!(
            worker.exited.load(Ordering::Acquire),
            "normal loop has returned"
        );
        assert!(
            !worker.is_quiescent(),
            "OS thread still owns unfinished TLS cleanup"
        );
        release_tx.send(()).expect("release input-free fixture");
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !worker.is_quiescent() {
            assert!(
                std::time::Instant::now() < deadline,
                "OS thread did not exit"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn signaled_panicked_worker_is_not_confirmed_as_normal_quiescence() {
        let worker = BoundedWorker::spawn("input-free-panic", 1, |_: ()| {
            panic!("controlled input-free worker failure");
        })
        .expect("worker");
        worker.try_submit(()).expect("fault fixture");
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !thread_exit_confirmed(&worker.thread) {
            assert!(
                std::time::Instant::now() < deadline,
                "fault fixture did not exit"
            );
            std::thread::yield_now();
        }
        worker.close();
        assert!(!worker.exited.load(Ordering::Acquire));
        assert!(
            !worker.is_quiescent(),
            "OS exit alone does not prove successful worker cleanup"
        );
    }

    #[test]
    fn capture_queue_preserves_time_order_and_fences_new_sessions_until_drain() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let worker_seen = seen.clone();
        let queue = CaptureQueue::spawn(move |session, at, value: u32| {
            worker_seen.lock().expect("seen").push((session, at, value));
            true
        })
        .expect("capture");
        let first = queue.begin().expect("first session");
        let at = std::time::Instant::now();
        assert!(queue.submit(at, 1));
        assert!(queue.submit(at + Duration::from_millis(37), 2));
        assert!(queue.begin().is_err());
        queue
            .drain(Duration::from_millis(500))
            .expect("all admitted events");
        assert!(!queue.is_active());
        assert!(!queue.submit(at, 3));
        let next = queue.begin().expect("next session");
        assert_ne!(first, next);
        assert!(queue.submit(at + Duration::from_millis(60), 4));
        queue.drain(Duration::from_millis(500)).expect("next drain");
        assert_eq!(
            queue.statistics(),
            CaptureStatistics {
                accepted: 1,
                dropped: 0,
                processing_failed: 0
            }
        );
        assert_eq!(
            *seen.lock().expect("seen"),
            [
                (first, at, 1),
                (first, at + Duration::from_millis(37), 2),
                (next, at + Duration::from_millis(60), 4)
            ]
        );
        queue.close();
    }

    #[test]
    fn capture_queue_overflow_and_blocked_drain_cannot_report_complete_or_restart() {
        let (entered_tx, entered_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(1);
        let mut release = Some(release_rx);
        let queue = Arc::new(
            CaptureQueue::spawn(move |_, _, _: u32| {
                if let Some(release) = release.take() {
                    entered_tx.send(()).expect("entered");
                    let _ = release.recv_timeout(Duration::from_secs(2));
                }
                true
            })
            .expect("capture"),
        );
        queue.begin().expect("session");
        assert!(queue.submit(std::time::Instant::now(), 0));
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("blocked handler");
        for value in 1..=1024 {
            assert!(queue.submit(std::time::Instant::now(), value));
        }
        assert!(!queue.submit(std::time::Instant::now(), 1025));
        assert_eq!(
            queue.statistics(),
            CaptureStatistics {
                accepted: 1025,
                dropped: 1,
                processing_failed: 0
            }
        );
        let draining = queue.clone();
        let (done_tx, done_rx) = sync_channel(1);
        let caller = std::thread::spawn(move || {
            let _ = done_tx.send(draining.drain(Duration::from_millis(20)));
        });
        let result = done_rx.recv_timeout(Duration::from_secs(1));
        release_tx.send(()).expect("release fixture");
        caller.join().expect("drain caller");
        assert_eq!(result.expect("bounded drain"), Err("capture_drain_timeout"));
        assert!(!queue.is_active());
        assert!(queue.begin().is_err());
        queue.close();
    }

    #[test]
    fn capture_processing_failure_is_explicit_and_disallows_another_session() {
        let queue = CaptureQueue::spawn(|_, _, _: ()| false).expect("capture");
        queue.begin().expect("session");
        assert!(queue.submit(std::time::Instant::now(), ()));
        assert_eq!(
            queue.drain(Duration::from_millis(500)),
            Err("capture_incomplete")
        );
        assert_eq!(
            queue.statistics(),
            CaptureStatistics {
                accepted: 1,
                dropped: 0,
                processing_failed: 1
            }
        );
        assert!(queue.begin().is_err());
        queue.close();
    }

    #[test]
    fn prequeue_loss_is_counted_without_waiting_for_admission_lock() {
        let queue = CaptureQueue::spawn(|_, _, _: ()| true).expect("capture");
        assert!(!queue.note_admission_loss());
        queue.begin().expect("session");
        let guard = queue.state.lock().expect("hold admission");
        assert!(queue.note_admission_loss());
        assert_eq!(queue.statistics().dropped, 1);
        assert!(queue.is_failed());
        drop(guard);
        assert_eq!(
            queue.drain(Duration::from_millis(500)),
            Err("capture_incomplete")
        );
        assert!(!queue.note_admission_loss());
        assert_eq!(queue.statistics().dropped, 1);
        queue.close();
    }

    #[test]
    fn terminal_capture_message_fences_late_events_before_processing() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let worker_seen = seen.clone();
        let queue = CaptureQueue::spawn(move |_, _, value: u32| {
            worker_seen.lock().expect("seen").push(value);
            true
        })
        .expect("input-free capture");
        queue.begin().expect("session");
        let at = std::time::Instant::now();
        assert!(queue.submit(at, 1));
        assert!(queue.submit_terminal(at, 2));
        assert!(!queue.is_active());
        assert!(queue.is_pending());
        assert!(!queue.submit(at, 3));
        assert!(!queue.submit_terminal(at, 4));
        assert!(queue.begin().is_err());
        queue
            .drain(Duration::from_millis(500))
            .expect("ordered terminal drain");
        assert_eq!(*seen.lock().expect("seen"), [1, 2]);
        assert_eq!(queue.statistics().accepted, 2);
        assert_eq!(queue.statistics().dropped, 0);
        queue.begin().expect("new session after confirmed drain");
        queue
            .drain(Duration::from_millis(500))
            .expect("empty drain");
        queue.close();
    }

    #[test]
    fn terminal_admission_contention_closes_capture_and_latches_failure() {
        let queue = CaptureQueue::spawn(|_, _, _: ()| true).expect("capture");
        queue.begin().expect("session");
        let guard = queue.state.lock().expect("hold admission");
        assert!(!queue.submit_terminal(std::time::Instant::now(), ()));
        assert!(!queue.is_active());
        assert!(queue.is_failed());
        drop(guard);
        assert_eq!(queue.statistics().dropped, 1);
        assert!(!queue.submit(std::time::Instant::now(), ()));
        assert_eq!(
            queue.drain(Duration::from_millis(500)),
            Err("capture_incomplete")
        );
        assert!(queue.begin().is_err());
        queue.close();
    }

    #[test]
    fn capture_shutdown_waits_for_inflight_handler_and_discards_pending_events() {
        let (entered_tx, entered_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(1);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let worker_calls = calls.clone();
        let queue = CaptureQueue::spawn(move |_, _, _: u32| {
            worker_calls.fetch_add(1, Ordering::AcqRel);
            entered_tx.send(()).expect("handler entered");
            release_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("release input-free fixture");
            true
        })
        .expect("capture worker");
        queue.begin().expect("session");
        assert!(queue.submit(std::time::Instant::now(), 1));
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("inflight handler");
        assert!(queue.submit(std::time::Instant::now(), 2));
        queue.close();
        assert!(!queue.is_active());
        assert!(!queue.is_quiescent());
        assert!(!queue.submit(std::time::Instant::now(), 3));
        assert!(queue.begin().is_err());
        release_tx.send(()).expect("release fixture");
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !queue.is_quiescent() {
            assert!(std::time::Instant::now() < deadline, "capture did not exit");
            std::thread::yield_now();
        }
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert!(
            queue.is_pending(),
            "shutdown must not pretend it drained data"
        );
        assert_eq!(queue.statistics().accepted, 2);
    }
    #[test]
    fn capture_admission_contention_counts_loss_but_outside_session_is_not_loss() {
        let queue = CaptureQueue::spawn(|_, _, _: ()| true).expect("capture");
        assert!(!queue.submit(std::time::Instant::now(), ()));
        assert_eq!(queue.statistics().dropped, 0);
        queue.begin().expect("session");
        let guard = queue.state.lock().expect("hold admission");
        assert!(!queue.submit(std::time::Instant::now(), ()));
        assert_eq!(queue.statistics().dropped, 1);
        drop(guard);
        assert_eq!(
            queue.drain(Duration::from_millis(500)),
            Err("capture_incomplete")
        );
        assert!(!queue.submit(std::time::Instant::now(), ()));
        assert_eq!(
            queue.statistics(),
            CaptureStatistics {
                accepted: 0,
                dropped: 1,
                processing_failed: 0
            }
        );
        queue.close();
    }

    #[test]
    fn blocked_worker_has_bounded_queue_and_shutdown_discards_pending_work() {
        let (entered_tx, entered_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(1);
        let (exit_tx, exit_rx) = sync_channel(1);
        struct Exit(std::sync::mpsc::SyncSender<()>);
        impl Drop for Exit {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        let exit = Exit(exit_tx);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let worker_calls = calls.clone();
        let worker = BoundedWorker::spawn("input-free-blocked-worker", 2, move |_: u32| {
            let _ = &exit;
            worker_calls.fetch_add(1, Ordering::AcqRel);
            entered_tx.send(()).expect("entry");
            release_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("bounded fixture release");
        })
        .expect("worker");
        assert!(worker.try_submit(1).is_ok());
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker entered");
        assert!(worker.try_submit(2).is_ok());
        assert!(worker.try_submit(3).is_ok());
        assert_eq!(worker.try_submit(4), Err(4));
        worker.close();
        assert!(
            !worker.is_quiescent(),
            "closing admission is not handler exit"
        );
        assert_eq!(worker.try_submit(5), Err(5));
        drop(worker);
        release_tx.send(()).expect("release");
        exit_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker exited without consuming queued tasks");
        assert_eq!(calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn worker_quiescence_requires_inflight_handler_completion() {
        let (entered_tx, entered_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(1);
        let worker = BoundedWorker::spawn("mock-quiescence", 1, move |_: ()| {
            entered_tx.send(()).expect("entered");
            release_rx.recv().expect("release");
        })
        .expect("worker");
        assert!(!worker.is_quiescent());
        worker.try_submit(()).expect("admit");
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("handler");
        worker.close();
        assert!(!worker.is_quiescent());
        release_tx.send(()).expect("release fixture");
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !worker.is_quiescent() {
            assert!(
                std::time::Instant::now() < deadline,
                "normal handler did not finish"
            );
            std::thread::yield_now();
        }
        assert_eq!(worker.try_submit(()), Err(()));
    }
}
