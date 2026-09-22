//! Ordinary notices are bounded and nonblocking. Runtime faults are retained
//! until acknowledged, and may only be published by a worker after cleanup.
use crate::runtime_protocol::ScriptStopMode;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const CAPACITY: usize = 16;

#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeNotification {
    pub id: u64,
    pub title: String,
    pub message: String,
    pub mode: ScriptStopMode,
    #[serde(skip)]
    fault: bool,
}

#[derive(Default)]
struct Mailbox {
    next_id: u64,
    pending: VecDeque<RuntimeNotification>,
}

#[derive(Default)]
pub(crate) struct RuntimeNotifications {
    mailbox: Mutex<Mailbox>,
    pending_faults: AtomicUsize,
}

impl RuntimeNotifications {
    pub(crate) fn publish(&self, title: &str, message: &str) -> bool {
        self.publish_with_mode(title, message, ScriptStopMode::Background)
    }

    pub(crate) fn publish_with_mode(
        &self,
        title: &str,
        message: &str,
        mode: ScriptStopMode,
    ) -> bool {
        let Ok(mut mailbox) = self.mailbox.try_lock() else {
            return false;
        };
        let title: String = title.chars().take(128).collect();
        let message: String = message.chars().take(2000).collect();
        if let Some(item) = mailbox
            .pending
            .iter_mut()
            .find(|item| item.title == title && item.message == message)
        {
            if mode == ScriptStopMode::Foreground {
                item.mode = ScriptStopMode::Foreground;
            }
            return true;
        }
        if mailbox.pending.len() >= CAPACITY {
            // Never evict a notice the UI may already be displaying.
            return false;
        }
        mailbox.next_id = mailbox.next_id.saturating_add(1);
        let id = mailbox.next_id;
        mailbox.pending.push_back(RuntimeNotification {
            id,
            title,
            message,
            mode,
            fault: false,
        });
        true
    }

    /// Called only from a playback/trigger worker, after input has been
    /// revoked. Never call from the hook, emergency stop, or an input lock.
    pub(crate) fn publish_fault(&self, title: &str, message: &str) {
        let mut mailbox = self
            .mailbox
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        mailbox.next_id = mailbox.next_id.saturating_add(1);
        let id = mailbox.next_id;
        // Faults take precedence over ordinary completion notices; retain
        // every older fault in order and never discard displaced notices.
        let fault_position = mailbox.pending.iter().take_while(|item| item.fault).count();
        mailbox.pending.insert(
            fault_position,
            RuntimeNotification {
                id,
                title: title.chars().take(128).collect(),
                message: message.chars().take(2000).collect(),
                mode: ScriptStopMode::Foreground,
                fault: true,
            },
        );
        // Publish while holding the same lock that owns this notification.
        self.pending_faults.fetch_add(1, Ordering::Release);
    }

    pub(crate) fn fault_pending(&self) -> bool {
        self.pending_faults.load(Ordering::Acquire) != 0
    }

    pub(crate) fn take(&self) -> Option<RuntimeNotification> {
        // Reads do not consume: React remounts or delayed IPC responses must
        // not lose a notification. Only explicit acknowledgement removes it.
        self.mailbox.try_lock().ok()?.pending.front().cloned()
    }

    /// A notice remains visible in the open UI while foreground presentation
    /// is unsafe; later polls promote the same id once inputs quiesce.
    pub(crate) fn take_when_safe(&self, safe_to_focus: bool) -> Option<RuntimeNotification> {
        let mut notice = self.take()?;
        if !safe_to_focus {
            notice.mode = ScriptStopMode::Background;
        }
        Some(notice)
    }

    pub(crate) fn acknowledge(&self, id: u64) -> bool {
        let Ok(mut mailbox) = self.mailbox.try_lock() else {
            return false;
        };
        if mailbox.pending.front().is_some_and(|item| item.id == id) {
            if mailbox.pending.pop_front().is_some_and(|item| item.fault) {
                self.pending_faults.fetch_sub(1, Ordering::Release);
            }
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notifications_are_bounded_coalesced_and_consumed_once() {
        let queue = RuntimeNotifications::default();
        for index in 0..100 {
            assert_eq!(
                queue.publish("failure", &index.to_string()),
                index < CAPACITY
            );
        }
        assert!(queue.publish("failure", "0"));
        let mut count = 0;
        let mut previous = 0;
        while let Some(item) = queue.take() {
            assert!(item.id > previous);
            previous = item.id;
            count += 1;
            assert!(queue.acknowledge(item.id));
            assert!(!queue.acknowledge(item.id));
        }
        assert_eq!(count, CAPACITY);
        assert!(queue.take().is_none());
    }
    #[test]
    fn presentation_contention_does_not_wait_or_mutate_execution() {
        let queue = RuntimeNotifications::default();
        let guard = queue.mailbox.lock().expect("mailbox");
        assert!(!queue.publish("title", "message"));
        assert!(queue.take().is_none());
        drop(guard);
        assert!(queue.publish("title", "message"));
    }

    #[test]
    fn duplicate_notification_upgrades_mode_without_changing_identity() {
        let queue = RuntimeNotifications::default();
        assert!(queue.publish("title", "message"));
        let background = queue.take().expect("background notification");
        let id = background.id;
        assert_eq!(background.mode, ScriptStopMode::Background);
        assert!(queue.publish_with_mode("title", "message", ScriptStopMode::Foreground));
        let upgraded = queue.take().expect("upgraded notification");
        assert_eq!(upgraded.id, id);
        assert_eq!(upgraded.mode, ScriptStopMode::Foreground);

        assert!(queue.publish("title", "message"));
        let not_downgraded = queue.take().expect("foreground remains pending");
        assert_eq!(not_downgraded.id, id);
        assert_eq!(not_downgraded.mode, ScriptStopMode::Foreground);
    }

    #[test]
    fn acknowledge_and_publish_are_linearized_at_the_front() {
        let queue = RuntimeNotifications::default();
        assert!(queue.publish("first", "message"));
        let first = queue.take().expect("first notification");
        assert!(queue.publish("second", "message"));
        assert!(!queue.acknowledge(first.id + 1));
        assert_eq!(queue.take().expect("first remains").id, first.id);
        assert!(queue.acknowledge(first.id));
        assert_eq!(queue.take().expect("second follows").title, "second");
    }

    #[test]
    fn faults_survive_full_mailbox_and_contention_until_exact_ack() {
        let queue = std::sync::Arc::new(RuntimeNotifications::default());
        for index in 0..CAPACITY {
            assert!(queue.publish("ordinary", &index.to_string()));
        }
        let guard = queue.mailbox.lock().expect("mailbox");
        let worker = std::sync::Arc::clone(&queue);
        let spawned = std::thread::spawn(move || worker.publish_fault("failed", "runtime error"));
        drop(guard);
        spawned.join().expect("fault producer");
        assert!(queue.fault_pending());
        let deferred = queue.take_when_safe(false).expect("deferred fault");
        assert_eq!(deferred.mode, ScriptStopMode::Background);
        let visible = queue.take_when_safe(true).expect("promoted fault");
        assert_eq!(visible.id, deferred.id);
        assert_eq!(visible.mode, ScriptStopMode::Foreground);
        assert_eq!(visible.message, "runtime error");
        assert!(!queue.acknowledge(visible.id + 1));
        assert!(queue.acknowledge(visible.id));
        assert!(!queue.fault_pending());
        for _ in 0..CAPACITY {
            let current = queue.take().expect("ordinary front retained");
            assert!(!queue.acknowledge(current.id + 1));
            assert!(queue.acknowledge(current.id));
        }
        assert!(queue.take().is_none());
    }
}
