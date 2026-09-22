//! Bounded presentation mailbox. Producers never wait for a UI or a mutex.
use crate::runtime_protocol::ScriptStopMode;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Mutex;

const CAPACITY: usize = 16;

#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeNotification {
    pub id: u64,
    pub title: String,
    pub message: String,
    pub mode: ScriptStopMode,
}

#[derive(Default)]
struct Mailbox {
    next_id: u64,
    pending: VecDeque<RuntimeNotification>,
}

#[derive(Default)]
pub(crate) struct RuntimeNotifications(Mutex<Mailbox>);

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
        let Ok(mut mailbox) = self.0.try_lock() else {
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
        if mailbox.pending.len() == CAPACITY {
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
        });
        true
    }

    pub(crate) fn take(&self) -> Option<RuntimeNotification> {
        // Reads do not consume: React remounts or delayed IPC responses must
        // not lose a notification. Only explicit acknowledgement removes it.
        self.0.try_lock().ok()?.pending.front().cloned()
    }

    pub(crate) fn acknowledge(&self, id: u64) -> bool {
        let Ok(mut mailbox) = self.0.try_lock() else {
            return false;
        };
        if mailbox.pending.front().is_some_and(|item| item.id == id) {
            mailbox.pending.pop_front();
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
        let guard = queue.0.lock().expect("mailbox");
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
}
