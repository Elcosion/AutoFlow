//! Prestarted cleanup lane: atomics/unpark producers, one coalesced request.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

const CLOSED: u64 = 1 << 63;
const SEQUENCE: u64 = CLOSED - 1;

struct State {
    requests: AtomicU64,
    completed: AtomicU64,
    active: AtomicBool,
    failed: AtomicBool,
    exited: AtomicBool,
}

pub(crate) struct PriorityCleanup {
    state: Arc<State>,
    handle: JoinHandle<()>,
}

impl PriorityCleanup {
    pub(crate) fn spawn(
        mut cleanup: impl FnMut(u64) -> bool + Send + 'static,
    ) -> std::io::Result<Self> {
        let state = Arc::new(State {
            requests: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            active: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            exited: AtomicBool::new(false),
        });
        let worker = Arc::clone(&state);
        let handle = thread::Builder::new()
            .name("autoflow-priority-cleanup".into())
            .spawn(move || {
                loop {
                    let request = worker.requests.load(Ordering::Acquire);
                    let sequence = request & SEQUENCE;
                    if sequence != worker.completed.load(Ordering::Acquire) {
                        worker.active.store(true, Ordering::Release);
                        let successful =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                cleanup(sequence)
                            }))
                            .is_ok_and(|result| result);
                        if !successful {
                            worker.failed.store(true, Ordering::Release);
                            return; // no completion/healthy exit, no automatic retry
                        }
                        worker.completed.store(sequence, Ordering::Release);
                        worker.active.store(false, Ordering::Release);
                        continue;
                    }
                    if request & CLOSED != 0 {
                        worker.exited.store(true, Ordering::Release);
                        return;
                    }
                    thread::park_timeout(Duration::from_millis(5));
                }
            })?;
        Ok(Self { state, handle })
    }

    /// Allocation and close share one atomic word: no accepted publication can
    /// land behind a closed/exited worker. Never wraps or reuses an identity.
    pub(crate) fn request(&self) -> Option<u64> {
        let previous = self
            .state
            .requests
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                if value & CLOSED != 0
                    || value == SEQUENCE
                    || self.state.failed.load(Ordering::Acquire)
                {
                    None
                } else {
                    Some(value + 1)
                }
            })
            .ok()?;
        self.wake();
        Some(previous + 1)
    }

    pub(crate) fn wake(&self) {
        self.handle.thread().unpark();
    }
    pub(crate) fn close(&self) {
        self.state.requests.fetch_or(CLOSED, Ordering::AcqRel);
        self.wake();
    }
    pub(crate) fn is_closed(&self) -> bool {
        self.state.requests.load(Ordering::Acquire) & CLOSED != 0
    }
    pub(crate) fn is_ready(&self) -> bool {
        !self.is_closed()
            && self.state.requests.load(Ordering::Acquire) & SEQUENCE != SEQUENCE
            && !self.state.failed.load(Ordering::Acquire)
            && crate::bounded_worker::thread_running_confirmed(&self.handle)
    }
    fn work_complete(&self) -> bool {
        !self.state.failed.load(Ordering::Acquire)
            && !self.state.active.load(Ordering::Acquire)
            && self.state.requests.load(Ordering::Acquire) & SEQUENCE
                == self.state.completed.load(Ordering::Acquire)
    }
    pub(crate) fn is_idle(&self) -> bool {
        self.is_ready() && self.work_complete()
    }
    pub(crate) fn is_quiescent(&self) -> bool {
        self.is_closed()
            && self.work_complete()
            && self.state.exited.load(Ordering::Acquire)
            && crate::bounded_worker::thread_exit_confirmed(&self.handle)
    }
}

impl Drop for PriorityCleanup {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;
    use std::time::Instant;

    #[test]
    fn stopped_requests_coalesce_on_one_worker_and_close_fences_publication() {
        let (entered_tx, entered_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(1);
        let worker = PriorityCleanup::spawn(move |sequence| {
            if sequence == 1 {
                entered_tx.send(thread::current().id()).expect("entered");
                release_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("release");
            } else {
                entered_tx.send(thread::current().id()).expect("successor");
            }
            true
        })
        .expect("mock worker");
        assert_eq!(worker.request(), Some(1));
        let first = entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first");
        for sequence in 2..=1000 {
            assert_eq!(worker.request(), Some(sequence));
        }
        assert!(!worker.is_idle());
        worker.close();
        assert_eq!(worker.request(), None);
        release_tx.send(()).expect("release");
        assert_eq!(
            entered_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("coalesced"),
            first
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        while !worker.is_quiescent() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert_eq!(worker.state.completed.load(Ordering::Acquire), 1000);
    }

    #[test]
    fn producer_close_races_never_lose_an_accepted_request_or_reopen_the_lane() {
        for _ in 0..100 {
            let executed = Arc::new(AtomicU64::new(0));
            let observed = Arc::clone(&executed);
            let worker = Arc::new(
                PriorityCleanup::spawn(move |sequence| {
                    observed.store(sequence, Ordering::Release);
                    true
                })
                .expect("mock cleanup lane"),
            );
            let barrier = Arc::new(std::sync::Barrier::new(3));
            let producer_lane = Arc::clone(&worker);
            let producer_barrier = Arc::clone(&barrier);
            let producer = thread::spawn(move || {
                producer_barrier.wait();
                producer_lane.request()
            });
            let closer_lane = Arc::clone(&worker);
            let closer_barrier = Arc::clone(&barrier);
            let closer = thread::spawn(move || {
                closer_barrier.wait();
                closer_lane.close();
            });
            barrier.wait();
            let accepted = producer.join().expect("producer exit");
            closer.join().expect("closer exit");
            let deadline = Instant::now() + Duration::from_secs(1);
            while !worker.is_quiescent() {
                assert!(Instant::now() < deadline, "accepted cleanup must drain");
                thread::yield_now();
            }
            assert_eq!(executed.load(Ordering::Acquire), accepted.unwrap_or(0));
            assert_eq!(
                worker.state.completed.load(Ordering::Acquire),
                accepted.unwrap_or(0)
            );
            assert_eq!(worker.request(), None);
        }
    }

    #[test]
    fn completed_loop_is_not_exit_confirmation_until_tls_destructors_finish() {
        struct Finalizer(
            std::sync::mpsc::SyncSender<()>,
            std::sync::mpsc::Receiver<()>,
        );
        impl Drop for Finalizer {
            fn drop(&mut self) {
                self.0.send(()).expect("destructor entered");
                self.1
                    .recv_timeout(Duration::from_secs(2))
                    .expect("destructor release");
            }
        }
        thread_local! {
            static FINALIZER: std::cell::RefCell<Option<Finalizer>> = const { std::cell::RefCell::new(None) };
        }
        let (entered_tx, entered_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(1);
        let mut resources = Some(Finalizer(entered_tx, release_rx));
        let worker = PriorityCleanup::spawn(move |_| {
            FINALIZER.with(|slot| *slot.borrow_mut() = resources.take());
            true
        })
        .expect("mock lane");
        assert_eq!(worker.request(), Some(1));
        worker.close();
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("loop returned");
        assert!(worker.state.exited.load(Ordering::Acquire));
        assert!(
            !worker.is_quiescent(),
            "retained OS thread is still in TLS destructor"
        );
        assert_eq!(worker.request(), None);
        release_tx.send(()).expect("release private destructor");
        let deadline = Instant::now() + Duration::from_secs(1);
        while !worker.is_quiescent() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }

    #[test]
    fn exhausted_request_identity_is_unready_and_cannot_wrap() {
        let worker = PriorityCleanup::spawn(|sequence| {
            assert_eq!(sequence, SEQUENCE);
            true
        })
        .expect("mock lane");
        worker.state.requests.store(SEQUENCE, Ordering::Release);
        assert!(!worker.is_ready());
        assert_eq!(worker.request(), None);
        assert_eq!(worker.state.requests.load(Ordering::Acquire), SEQUENCE);
        worker.close();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !worker.is_quiescent() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }

    #[test]
    fn panic_does_not_publish_completion_or_healthy_exit() {
        let worker = PriorityCleanup::spawn(|_| panic!("mock cleanup panic")).expect("mock lane");
        assert_eq!(worker.request(), Some(1));
        let deadline = Instant::now() + Duration::from_secs(1);
        while !crate::bounded_worker::thread_exit_confirmed(&worker.handle) {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert_eq!(worker.state.completed.load(Ordering::Acquire), 0);
        assert!(!worker.state.exited.load(Ordering::Acquire));
        assert!(!worker.is_ready());
        assert_eq!(worker.request(), None);
        worker.close();
        assert!(!worker.is_quiescent());
    }

    #[test]
    fn failed_cleanup_cannot_be_retried_or_confirmed_as_safe_exit() {
        let worker = PriorityCleanup::spawn(|_| false).expect("mock worker");
        assert_eq!(worker.request(), Some(1));
        let deadline = Instant::now() + Duration::from_secs(1);
        while !crate::bounded_worker::thread_exit_confirmed(&worker.handle) {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert_eq!(worker.request(), None);
        assert!(!worker.is_idle());
        worker.close();
        assert!(!worker.is_quiescent());
    }
}
