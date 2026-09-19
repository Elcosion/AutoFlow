//! Safety primitives for injected keyboard and mouse input.
//!
//! The Windows hook callback must never own the only copy of input state.  A
//! playback instance records a key/button only after the corresponding down
//! event was accepted by Windows, and it removes that record only after the
//! matching up event succeeds.  Cleanup is serialized with normal injection
//! and is deliberately bounded so a failed Windows input call cannot turn into
//! an infinite retry loop.

#[cfg(windows)]
use crate::runtime_control::{RunToken, RuntimeController};
#[cfg(windows)]
use crate::MouseButton;
#[cfg(windows)]
use std::collections::{HashMap, HashSet};
#[cfg(windows)]
use std::fs::{self, OpenOptions};
#[cfg(windows)]
use std::path::PathBuf;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(windows)]
use std::sync::{Arc, Mutex};
#[cfg(windows)]
use std::thread;
#[cfg(windows)]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(windows)]
pub const MAX_CLEANUP_ATTEMPTS: usize = 3;
#[cfg(windows)]
pub const MAX_DIAGNOSTIC_EVENTS: usize = 256;
#[cfg(windows)]
pub(crate) const UNICODE_INPUT_TAG: u32 = 0x1_0000;
#[cfg(windows)]
fn input_key_label(key: u32) -> String {
    if key & UNICODE_INPUT_TAG != 0 {
        "unicode_unit".into()
    } else {
        format!("{key:#x}")
    }
}
#[cfg(windows)]
const MAX_DIAGNOSTIC_BYTES: usize = 64 * 1024;

/// A small, append-oriented safety log.  It intentionally stores lifecycle
/// facts and error codes only; it never stores scripts or typed text.
#[cfg(windows)]
#[derive(Clone)]
pub struct SafetyDiagnostics {
    path: Arc<PathBuf>,
    queue: Option<std::sync::mpsc::SyncSender<DiagnosticMessage>>,
    sequence: Arc<AtomicU64>,
    lost: Arc<AtomicU64>,
}

#[cfg(windows)]
enum DiagnosticMessage {
    Line(String),
    Flush(std::sync::mpsc::SyncSender<bool>),
}

#[cfg(windows)]
impl Default for SafetyDiagnostics {
    fn default() -> Self {
        #[cfg(test)]
        let root = std::env::temp_dir()
            .join("autoflow-safety-tests")
            .join(std::process::id().to_string());
        #[cfg(not(test))]
        let root = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("com.autoflow.desktop")
            .join("data")
            .join("diagnostics");
        Self::at(root.join("input-safety.jsonl"))
    }
}

#[cfg(windows)]
impl SafetyDiagnostics {
    pub fn at(path: PathBuf) -> Self {
        let writer_path = path.clone();
        let flush_path = path.clone();
        Self::with_writer_and_flush(
            path,
            move |line| {
                let Some(parent) = writer_path.parent() else {
                    return false;
                };
                if fs::create_dir_all(parent).is_err() {
                    return false;
                }
                use std::io::Write;
                let written = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&writer_path)
                    .and_then(|mut file| file.write_all(line.as_bytes()))
                    .is_ok();
                if written {
                    Self::trim_file(&writer_path);
                }
                written
            },
            move || {
                OpenOptions::new()
                    .write(true)
                    .open(&flush_path)
                    .and_then(|file| file.sync_all())
                    .is_ok()
            },
        )
    }

    #[cfg(test)]
    fn with_writer(path: PathBuf, write: impl FnMut(&str) -> bool + Send + 'static) -> Self {
        Self::with_writer_and_flush(path, write, || true)
    }

    fn with_writer_and_flush(
        path: PathBuf,
        mut write: impl FnMut(&str) -> bool + Send + 'static,
        mut flush: impl FnMut() -> bool + Send + 'static,
    ) -> Self {
        let (queue, incoming) = std::sync::mpsc::sync_channel::<DiagnosticMessage>(64);
        let lost = Arc::new(AtomicU64::new(0));
        let worker_lost = lost.clone();
        // Only this worker owns disk I/O. Producers never wait for it. On final
        // sender drop it drains admitted records; no callback joins the worker.
        let started = thread::Builder::new()
            .name("autoflow-safety-diagnostic".into())
            .spawn(move || {
                let mut healthy = true;
                while let Ok(message) = incoming.recv() {
                    match message {
                        DiagnosticMessage::Line(line) => {
                            if !write(&line) {
                                healthy = false;
                                worker_lost.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        DiagnosticMessage::Flush(reply) => {
                            let synced = flush();
                            let _ = reply.try_send(
                                healthy && synced && worker_lost.load(Ordering::Relaxed) == 0,
                            );
                        }
                    }
                }
            })
            .is_ok();
        Self {
            path: Arc::new(path),
            queue: started.then_some(queue),
            sequence: Arc::new(AtomicU64::new(0)),
            lost,
        }
    }

    pub fn path(&self) -> PathBuf {
        (*self.path).clone()
    }

    pub fn record(&self, event: &str, fields: &[(&str, String)]) {
        let mut object = serde_json::Map::new();
        for (key, value) in fields.iter().take(16) {
            object.insert(
                key.chars().take(64).collect(),
                serde_json::json!(value.chars().take(512).collect::<String>()),
            );
        }
        // Reserved metadata cannot be replaced by caller fields. Payloads and
        // queue capacity are bounded independently of caller input length.
        object.insert("timestamp_ms".to_string(), serde_json::json!(unix_millis()));
        object.insert(
            "event".to_string(),
            serde_json::json!(event.chars().take(128).collect::<String>()),
        );
        object.insert(
            "sequence".to_string(),
            serde_json::json!(self.sequence.fetch_add(1, Ordering::Relaxed)),
        );
        object.insert(
            "lost_events".to_string(),
            serde_json::json!(self.lost.load(Ordering::Relaxed)),
        );
        let Ok(mut line) = serde_json::to_string(&object) else {
            return;
        };
        line.push('\n');
        if self
            .queue
            .as_ref()
            .is_none_or(|queue| queue.try_send(DiagnosticMessage::Line(line)).is_err())
        {
            self.lost.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Non-hook shutdown checkpoint only. A barrier confirms all earlier
    /// admitted records and the writer's sync operation, not concurrently
    /// produced records. Disk stalls cannot hold the caller beyond its budget.
    pub fn flush(&self, timeout: std::time::Duration) -> bool {
        let Some(queue) = &self.queue else {
            return false;
        };
        let deadline =
            std::time::Instant::now() + timeout.min(std::time::Duration::from_millis(500));
        let (reply, response) = std::sync::mpsc::sync_channel(1);
        let mut message = DiagnosticMessage::Flush(reply);
        loop {
            match queue.try_send(message) {
                Ok(()) => break,
                Err(std::sync::mpsc::TrySendError::Full(returned))
                    if std::time::Instant::now() < deadline =>
                {
                    message = returned;
                    thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(_) => return false,
            }
        }
        response
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap_or(false)
    }

    /// Both entry points enqueue only; neither starts per-event threads nor
    /// performs disk I/O on hook, input, or emergency threads.
    pub fn record_async(&self, event: &'static str, fields: Vec<(String, String)>) {
        let fields = fields
            .iter()
            .take(16)
            .map(|(key, value)| (key.as_str(), value.chars().take(512).collect()))
            .collect::<Vec<_>>();
        self.record(event, &fields);
    }

    fn trim_file(path: &std::path::Path) {
        let Ok(contents) = fs::read_to_string(path) else {
            return;
        };
        if contents.len() <= MAX_DIAGNOSTIC_BYTES
            && contents.lines().count() <= MAX_DIAGNOSTIC_EVENTS
        {
            return;
        }
        let mut lines = contents.lines().collect::<Vec<_>>();
        if lines.len() > MAX_DIAGNOSTIC_EVENTS {
            let keep_from = lines.len() - MAX_DIAGNOSTIC_EVENTS;
            lines.drain(..keep_from);
        }
        let mut kept = lines.join("\n");
        if !kept.is_empty() {
            kept.push('\n');
        }
        while kept.len() > MAX_DIAGNOSTIC_BYTES {
            let Some(end) = kept.find('\n') else {
                kept.clear();
                break;
            };
            kept.drain(..=end);
        }
        let _ = fs::write(path, kept);
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupFailure {
    pub kind: &'static str,
    pub value: String,
    pub attempts: usize,
    pub last_error: String,
}

#[cfg(windows)]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CleanupReport {
    pub released_keys: usize,
    pub released_buttons: usize,
    pub failures: Vec<CleanupFailure>,
}

#[cfg(windows)]
impl CleanupReport {
    pub fn is_safe(&self) -> bool {
        self.failures.is_empty()
    }

    pub fn unreleased_count(&self) -> usize {
        self.failures.len()
    }
}

/// Serializes input transactions across all producers in one safety service.
///
/// `operation` serializes the complete send-and-track transaction.  This is
/// important when an emergency cleanup races a playback step: cleanup cannot
/// release a key between the successful OS call and the state registration.
#[cfg(windows)]
#[derive(Default)]
pub(crate) struct InputBroker {
    operation: Arc<Mutex<()>>,
    key_owners: Mutex<HashMap<u32, u64>>,
    button_owners: Mutex<HashMap<MouseButton, u64>>,
    next_owner: AtomicU64,
}

#[cfg(windows)]
impl InputBroker {
    pub(crate) fn counts(&self) -> (usize, usize) {
        let (Ok(_operation), Ok(keys), Ok(buttons)) = (
            self.operation.try_lock(),
            self.key_owners.try_lock(),
            self.button_owners.try_lock(),
        ) else {
            return (usize::MAX, usize::MAX);
        };
        (keys.len(), buttons.len())
    }
}

#[cfg(windows)]
pub struct InjectedInputState {
    operation: Arc<Mutex<()>>,
    broker: Arc<InputBroker>,
    owner: u64,
    keys: Mutex<HashSet<u32>>,
    buttons: Mutex<HashSet<MouseButton>>,
    last_input: Mutex<Option<String>>,
    last_input_at_ms: Mutex<Option<u64>>,
}

#[cfg(windows)]
impl Default for InjectedInputState {
    fn default() -> Self {
        Self::with_broker(Arc::new(InputBroker::default()))
    }
}

#[cfg(windows)]
impl InjectedInputState {
    pub(crate) fn with_broker(broker: Arc<InputBroker>) -> Self {
        let owner = broker
            .next_owner
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .ok()
            .and_then(|previous| previous.checked_add(1))
            .unwrap_or(0);
        Self {
            operation: broker.operation.clone(),
            broker,
            owner,
            keys: Mutex::new(HashSet::new()),
            buttons: Mutex::new(HashSet::new()),
            last_input: Mutex::new(None),
            last_input_at_ms: Mutex::new(None),
        }
    }
}

#[cfg(windows)]
impl InjectedInputState {
    /// Bound contention before an OS transaction. This cannot interrupt an
    /// already-admitted Windows call, but no later sender waits indefinitely.
    fn acquire_operation(
        &self,
        cancel: Option<&AtomicBool>,
    ) -> Result<std::sync::MutexGuard<'_, ()>, String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
        loop {
            Self::check_cancel(cancel)?;
            match self.operation.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(std::sync::TryLockError::WouldBlock)
                    if std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err("输入事务锁异常，未发送输入".into())
                }
                Err(_) => return Err("输入事务锁超过100ms获取期限，未发送输入".into()),
            }
        }
    }

    fn mark_last_input(&self, description: String) {
        if let Ok(mut last_input) = self.last_input.try_lock() {
            *last_input = Some(description);
        }
        if let Ok(mut last_input_at_ms) = self.last_input_at_ms.try_lock() {
            *last_input_at_ms = Some(unix_millis());
        }
    }

    fn check_cancel(cancel: Option<&AtomicBool>) -> Result<(), String> {
        if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            Err("脚本已被 F12 停止".to_string())
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    pub fn send_key_down<F>(
        &self,
        vk: u32,
        cancel: Option<&AtomicBool>,
        send: F,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        self.send_key_down_with_permission(vk, cancel, || Ok(()), send)
    }

    pub fn send_key_up<F>(
        &self,
        vk: u32,
        cancel: Option<&AtomicBool>,
        send: F,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        let _operation = self.acquire_operation(cancel)?;
        // This is the permissionless recovery path, not an ordinary script
        // key_up. Never synthesize releases for keys absent from our ledger.
        let mut keys = self
            .keys
            .try_lock()
            .map_err(|_| "注入键盘状态异常".to_string())?;
        if !keys.contains(&vk) {
            return Ok(());
        }
        let mut owners = self
            .broker
            .key_owners
            .try_lock()
            .map_err(|_| "输入归属账本不可用".to_string())?;
        if owners.get(&vk) != Some(&self.owner) {
            return Err("输入键归属不明，未发送释放".into());
        }
        Self::check_cancel(cancel)?;
        send()?;
        self.mark_last_input(format!("key_up:{}", input_key_label(vk)));
        keys.remove(&vk);
        owners.remove(&vk);
        Ok(())
    }

    #[cfg(test)]
    pub fn send_button_down<F>(
        &self,
        button: MouseButton,
        cancel: Option<&AtomicBool>,
        send: F,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        self.send_button_down_with_permission(button, cancel, || Ok(()), send)
    }

    pub fn send_button_up<F>(
        &self,
        button: MouseButton,
        cancel: Option<&AtomicBool>,
        send: F,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        let _operation = self.acquire_operation(cancel)?;
        let mut buttons = self
            .buttons
            .try_lock()
            .map_err(|_| "注入鼠标状态异常".to_string())?;
        if !buttons.contains(&button) {
            return Ok(());
        }
        let mut owners = self
            .broker
            .button_owners
            .try_lock()
            .map_err(|_| "输入归属账本不可用".to_string())?;
        if owners.get(&button) != Some(&self.owner) {
            return Err("鼠标按钮归属不明，未发送释放".into());
        }
        Self::check_cancel(cancel)?;
        send()?;
        self.mark_last_input(format!("button_up:{button:?}"));
        buttons.remove(&button);
        owners.remove(&button);
        Ok(())
    }

    /// The permission-aware variants are the only operations playback is
    /// allowed to use.  Permission is checked while holding the same
    /// operation lock as ledger mutation. An OS call already admitted when a
    /// stop arrives may finish; its successful press is still registered and
    /// must be cleaned. Revocation denies subsequent transactions.
    pub fn send_key_down_with_permission<P, F>(
        &self,
        vk: u32,
        cancel: Option<&AtomicBool>,
        permission: P,
        send: F,
    ) -> Result<(), String>
    where
        P: FnOnce() -> Result<(), String>,
        F: FnOnce() -> Result<(), String>,
    {
        let _operation = self.acquire_operation(cancel)?;
        let mut keys = self
            .keys
            .try_lock()
            .map_err(|_| "输入键盘账本不可用，未发送输入".to_string())?;
        let mut owners = self
            .broker
            .key_owners
            .try_lock()
            .map_err(|_| "输入归属账本不可用".to_string())?;
        if self.owner == 0
            || owners.get(&vk).is_some_and(|owner| *owner != self.owner)
            || (keys.contains(&vk) != (owners.get(&vk) == Some(&self.owner)))
        {
            return Err("输入键已由其他功能持有或归属不明，未发送按下".into());
        }
        permission()?;
        Self::check_cancel(cancel)?;
        send()?;
        self.mark_last_input(format!("key_down:{}", input_key_label(vk)));
        keys.insert(vk);
        owners.insert(vk, self.owner);
        Ok(())
    }

    pub fn send_key_up_with_permission<P, F>(
        &self,
        vk: u32,
        cancel: Option<&AtomicBool>,
        permission: P,
        send: F,
    ) -> Result<(), String>
    where
        P: FnOnce() -> Result<(), String>,
        F: FnOnce() -> Result<(), String>,
    {
        let _operation = self.acquire_operation(cancel)?;
        let mut keys = self
            .keys
            .try_lock()
            .map_err(|_| "输入键盘账本不可用，未发送输入".to_string())?;
        let mut owners = self
            .broker
            .key_owners
            .try_lock()
            .map_err(|_| "输入归属账本不可用".to_string())?;
        if !keys.contains(&vk) {
            return Ok(());
        }
        if owners.get(&vk) != Some(&self.owner) {
            return Err("输入键归属不明，未发送释放".into());
        }
        permission()?;
        Self::check_cancel(cancel)?;
        send()?;
        self.mark_last_input(format!("key_up:{}", input_key_label(vk)));
        keys.remove(&vk);
        owners.remove(&vk);
        Ok(())
    }

    pub fn send_button_down_with_permission<P, F>(
        &self,
        button: MouseButton,
        cancel: Option<&AtomicBool>,
        permission: P,
        send: F,
    ) -> Result<(), String>
    where
        P: FnOnce() -> Result<(), String>,
        F: FnOnce() -> Result<(), String>,
    {
        let _operation = self.acquire_operation(cancel)?;
        let mut buttons = self
            .buttons
            .try_lock()
            .map_err(|_| "输入鼠标账本不可用，未发送输入".to_string())?;
        let mut owners = self
            .broker
            .button_owners
            .try_lock()
            .map_err(|_| "输入归属账本不可用".to_string())?;
        if self.owner == 0
            || owners
                .get(&button)
                .is_some_and(|owner| *owner != self.owner)
            || (buttons.contains(&button) != (owners.get(&button) == Some(&self.owner)))
        {
            return Err("鼠标按钮已由其他功能持有或归属不明，未发送按下".into());
        }
        permission()?;
        Self::check_cancel(cancel)?;
        send()?;
        self.mark_last_input(format!("button_down:{button:?}"));
        buttons.insert(button);
        owners.insert(button, self.owner);
        Ok(())
    }

    /// Each UTF-16 down is registered before its matching up. A failed up
    /// remains in the same ledger used by emergency cleanup. Text content is
    /// never included in diagnostic descriptions.
    pub(crate) fn send_text_with_permission<P, F>(
        &self,
        text: &str,
        cancel: Option<&AtomicBool>,
        mut permission: P,
        mut send: F,
    ) -> Result<(), String>
    where
        P: FnMut() -> Result<(), String>,
        F: FnMut(u32, bool) -> Result<(), String>,
    {
        for unit in text.encode_utf16() {
            let key = UNICODE_INPUT_TAG | u32::from(unit);
            self.send_key_down_with_permission(key, cancel, &mut permission, || send(key, true))?;
            // Recovery release must run even if cancellation arrived during
            // the successful down; it cannot introduce any new held input.
            self.send_key_up(key, None, || send(key, false))?;
        }
        Ok(())
    }

    pub fn perform_with_permission<P, F>(
        &self,
        description: impl Into<String>,
        cancel: Option<&AtomicBool>,
        permission: P,
        send: F,
    ) -> Result<(), String>
    where
        P: FnOnce() -> Result<(), String>,
        F: FnOnce() -> Result<(), String>,
    {
        let _operation = self.acquire_operation(cancel)?;
        permission()?;
        Self::check_cancel(cancel)?;
        send()?;
        self.mark_last_input(description.into());
        Ok(())
    }

    /// Release all app-owned input.  The cancel flag is intentionally not
    /// consulted here: cleanup must still release a button after F12.
    pub fn cleanup<FKey, FButton>(&self, mut key_up: FKey, mut button_up: FButton) -> CleanupReport
    where
        FKey: FnMut(u32) -> Result<(), String>,
        FButton: FnMut(MouseButton) -> Result<(), String>,
    {
        let lock_deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
        let _operation = loop {
            match self.operation.try_lock() {
                Ok(operation) => break operation,
                Err(std::sync::TryLockError::WouldBlock)
                    if std::time::Instant::now() < lock_deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(_) => {
                    return CleanupReport {
                        failures: vec![CleanupFailure {
                            kind: "state",
                            value: "operation_lock".to_string(),
                            attempts: 0,
                            last_error: "输入清理锁不可用或超过100ms获取期限".to_string(),
                        }],
                        ..CleanupReport::default()
                    };
                }
            }
        };

        let mut tracked_keys = match self.keys.try_lock() {
            Ok(keys) => keys,
            Err(_) => {
                return CleanupReport {
                    failures: vec![CleanupFailure {
                        kind: "state",
                        value: "key_ledger".to_string(),
                        attempts: 0,
                        last_error: "input key ledger is unavailable".to_string(),
                    }],
                    ..CleanupReport::default()
                };
            }
        };
        let mut tracked_buttons = match self.buttons.try_lock() {
            Ok(buttons) => buttons,
            Err(_) => {
                return CleanupReport {
                    failures: vec![CleanupFailure {
                        kind: "state",
                        value: "button_ledger".to_string(),
                        attempts: 0,
                        last_error: "input button ledger is unavailable".to_string(),
                    }],
                    ..CleanupReport::default()
                };
            }
        };
        let mut report = CleanupReport::default();
        let (Ok(mut key_owners), Ok(mut button_owners)) = (
            self.broker.key_owners.try_lock(),
            self.broker.button_owners.try_lock(),
        ) else {
            return CleanupReport {
                failures: vec![CleanupFailure {
                    kind: "state",
                    value: "ownership_ledger".into(),
                    attempts: 0,
                    last_error: "输入归属账本不可用，未执行释放".into(),
                }],
                ..CleanupReport::default()
            };
        };
        let keys = tracked_keys.iter().copied().collect::<Vec<_>>();
        let buttons = tracked_buttons.iter().copied().collect::<Vec<_>>();

        for vk in keys {
            if key_owners.get(&vk) != Some(&self.owner) {
                report.failures.push(CleanupFailure {
                    kind: "key",
                    value: input_key_label(vk),
                    attempts: 0,
                    last_error: "输入归属不一致，未执行释放".into(),
                });
                continue;
            }
            let mut released = false;
            for attempt in 1..=MAX_CLEANUP_ATTEMPTS {
                self.mark_last_input(format!("cleanup_key_up:{}", input_key_label(vk)));
                match key_up(vk) {
                    Ok(()) => {
                        released = true;
                        report.released_keys += 1;
                        break;
                    }
                    Err(error) if attempt == MAX_CLEANUP_ATTEMPTS => {
                        report.failures.push(CleanupFailure {
                            kind: "key",
                            value: input_key_label(vk),
                            attempts: attempt,
                            last_error: error,
                        });
                    }
                    Err(_) => {}
                }
            }
            if released {
                tracked_keys.remove(&vk);
                key_owners.remove(&vk);
            }
        }

        for button in buttons {
            if button_owners.get(&button) != Some(&self.owner) {
                report.failures.push(CleanupFailure {
                    kind: "button",
                    value: format!("{button:?}"),
                    attempts: 0,
                    last_error: "输入归属不一致，未执行释放".into(),
                });
                continue;
            }
            let mut released = false;
            for attempt in 1..=MAX_CLEANUP_ATTEMPTS {
                self.mark_last_input(format!("cleanup_button_up:{button:?}"));
                match button_up(button) {
                    Ok(()) => {
                        released = true;
                        report.released_buttons += 1;
                        break;
                    }
                    Err(error) if attempt == MAX_CLEANUP_ATTEMPTS => {
                        report.failures.push(CleanupFailure {
                            kind: "button",
                            value: format!("{button:?}"),
                            attempts: attempt,
                            last_error: error,
                        });
                    }
                    Err(_) => {}
                }
            }
            if released {
                tracked_buttons.remove(&button);
                button_owners.remove(&button);
            }
        }

        report
    }

    pub fn counts(&self) -> (usize, usize) {
        // Serialize snapshots with sends/cleanup.  A playback admission check
        // must not observe an empty ledger while a background operation has
        // already passed its permission check but has not registered its
        // successful input yet.
        let Some(_operation) = self.operation.try_lock().ok() else {
            return (usize::MAX, usize::MAX);
        };
        let keys = self
            .keys
            .try_lock()
            .map(|keys| keys.len())
            .unwrap_or(usize::MAX);
        let buttons = self
            .buttons
            .try_lock()
            .map(|buttons| buttons.len())
            .unwrap_or(usize::MAX);
        (keys, buttons)
    }

    pub fn has_key(&self, vk: u32) -> bool {
        self.keys
            .try_lock()
            .map(|keys| keys.contains(&vk))
            .unwrap_or(true)
    }

    pub(crate) fn buttons_snapshot(&self) -> Result<Vec<MouseButton>, String> {
        let _operation = self
            .operation
            .try_lock()
            .map_err(|_| "输入事务尚未结束".to_string())?;
        self.buttons
            .try_lock()
            .map(|buttons| buttons.iter().copied().collect())
            .map_err(|_| "鼠标输入账本不可用".to_string())
    }

    pub fn last_input(&self) -> Option<String> {
        self.last_input
            .try_lock()
            .ok()
            .and_then(|last| last.clone())
    }

    pub fn last_input_at_ms(&self) -> Option<u64> {
        self.last_input_at_ms
            .try_lock()
            .ok()
            .and_then(|timestamp| *timestamp)
    }
}

/// Capability held by one admitted playback instance.  Normal input must go
/// through this object; emergency cleanup deliberately uses the underlying
/// ledger without a permit so a revoked run can still release its inputs.
#[cfg(windows)]
#[derive(Clone)]
pub struct InputPermit {
    state: Arc<InjectedInputState>,
    controller: Arc<RuntimeController>,
    token: RunToken,
}

#[cfg(windows)]
impl InputPermit {
    pub(crate) fn send_text<F>(
        &self,
        text: &str,
        cancel: Option<&AtomicBool>,
        send: F,
    ) -> Result<(), String>
    where
        F: FnMut(u32, bool) -> Result<(), String>,
    {
        self.state.send_text_with_permission(
            text,
            cancel,
            || {
                if self.controller.input_allowed(self.token) {
                    Ok(())
                } else {
                    Err("输入许可已撤销，当前动作已停止".into())
                }
            },
            send,
        )
    }

    pub fn new(
        state: Arc<InjectedInputState>,
        controller: Arc<RuntimeController>,
        token: RunToken,
    ) -> Self {
        Self {
            state,
            controller,
            token,
        }
    }

    pub fn send_key_down<F>(
        &self,
        vk: u32,
        cancel: Option<&AtomicBool>,
        send: F,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        let controller = Arc::clone(&self.controller);
        let token = self.token;
        self.state.send_key_down_with_permission(
            vk,
            cancel,
            move || {
                if controller.input_allowed(token) {
                    Ok(())
                } else {
                    Err("输入许可已撤销，当前动作已停止".to_string())
                }
            },
            send,
        )
    }

    pub fn send_key_up<F>(
        &self,
        vk: u32,
        cancel: Option<&AtomicBool>,
        send: F,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        let controller = Arc::clone(&self.controller);
        let token = self.token;
        self.state.send_key_up_with_permission(
            vk,
            cancel,
            move || {
                if controller.input_allowed(token) {
                    Ok(())
                } else {
                    Err("输入许可已撤销，当前动作已停止".to_string())
                }
            },
            send,
        )
    }

    pub fn send_button_down<F>(
        &self,
        button: MouseButton,
        cancel: Option<&AtomicBool>,
        send: F,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        let controller = Arc::clone(&self.controller);
        let token = self.token;
        self.state.send_button_down_with_permission(
            button,
            cancel,
            move || {
                if controller.input_allowed(token) {
                    Ok(())
                } else {
                    Err("输入许可已撤销，当前动作已停止".to_string())
                }
            },
            send,
        )
    }

    pub fn force_key_up<F>(&self, vk: u32, send: F) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        self.state.send_key_up(vk, None, send)
    }

    pub fn force_button_up<F>(&self, button: MouseButton, send: F) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        self.state.send_button_up(button, None, send)
    }

    pub fn perform<F>(
        &self,
        description: impl Into<String>,
        cancel: Option<&AtomicBool>,
        send: F,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        let controller = Arc::clone(&self.controller);
        let token = self.token;
        self.state.perform_with_permission(
            description,
            cancel,
            move || {
                if controller.input_allowed(token) {
                    Ok(())
                } else {
                    Err("输入许可已撤销，当前动作已停止".to_string())
                }
            },
            send,
        )
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::{InjectedInputState, InputPermit, SafetyDiagnostics, MAX_CLEANUP_ATTEMPTS};
    use crate::MouseButton;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn terminal_diagnostic_flush_round_trips_an_isolated_file() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "autoflow-terminal-flush-{}-{nonce}.jsonl",
            std::process::id()
        ));
        let diagnostics = SafetyDiagnostics::at(path.clone());
        diagnostics.record("shutdown_requested", &[("instance_id", "7".into())]);
        diagnostics.record("input_cleanup_completed", &[("released_keys", "1".into())]);
        diagnostics.record("application_shutdown_cleanup_completed", &[]);
        assert!(diagnostics.flush(std::time::Duration::from_millis(500)));
        let contents = std::fs::read_to_string(&path).expect("synchronized file");
        let records: Vec<serde_json::Value> = contents
            .lines()
            .map(|line| serde_json::from_str(line).expect("JSONL"))
            .collect();
        assert_eq!(records.len(), 3);
        assert_eq!(
            records[2]["event"],
            "application_shutdown_cleanup_completed"
        );
        for (sequence, record) in records.iter().enumerate() {
            assert_eq!(record["sequence"], sequence);
            assert_eq!(record["lost_events"], 0);
        }
        drop(diagnostics);
        std::fs::remove_file(path).expect("remove only uniquely created fixture log");
    }

    #[test]
    fn diagnostic_flush_confirms_prior_records_then_sync_in_order() {
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let written = order.clone();
        let synced = order.clone();
        let diagnostics = SafetyDiagnostics::with_writer_and_flush(
            std::env::temp_dir().join("mock-diagnostic-flush"),
            move |line| {
                let value: serde_json::Value = serde_json::from_str(line).expect("record");
                written
                    .lock()
                    .expect("order")
                    .push(value["event"].as_str().expect("event").to_string());
                true
            },
            move || {
                synced.lock().expect("order").push("sync".into());
                true
            },
        );
        diagnostics.record("cleanup_started", &[]);
        diagnostics.record("cleanup_completed", &[]);
        assert!(diagnostics.flush(std::time::Duration::from_millis(500)));
        assert_eq!(
            *order.lock().expect("order"),
            ["cleanup_started", "cleanup_completed", "sync"]
        );
    }

    #[test]
    fn failed_diagnostic_write_or_sync_is_not_confirmed_as_persisted() {
        let failed_write =
            SafetyDiagnostics::with_writer(std::env::temp_dir().join("mock-write-failure"), |_| {
                false
            });
        failed_write.record("terminal", &[]);
        assert!(!failed_write.flush(std::time::Duration::from_millis(500)));
        let failed_sync = SafetyDiagnostics::with_writer_and_flush(
            std::env::temp_dir().join("mock-sync-failure"),
            |_| true,
            || false,
        );
        failed_sync.record("terminal", &[]);
        assert!(!failed_sync.flush(std::time::Duration::from_millis(500)));
    }

    #[test]
    fn blocked_diagnostic_writer_does_not_hold_shutdown_flush_indefinitely() {
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let diagnostics = SafetyDiagnostics::with_writer(
            std::env::temp_dir().join("mock-blocked-flush"),
            move |_| {
                entered_tx.send(()).expect("entered");
                release_rx.recv().expect("release writer");
                true
            },
        );
        diagnostics.record("terminal", &[]);
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("writer blocked");
        let flush = diagnostics.clone();
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        let caller = std::thread::spawn(move || {
            let _ = done_tx.send(flush.flush(std::time::Duration::from_millis(20)));
        });
        let result = done_rx.recv_timeout(std::time::Duration::from_secs(1));
        release_tx.send(()).expect("release fixture");
        caller.join().expect("flush caller");
        assert!(!result.expect("flush returned while writer still blocked"));
        assert!(diagnostics.flush(std::time::Duration::from_millis(500)));
    }

    #[test]
    fn exhausted_owner_identity_never_wraps_or_reuses_an_existing_owner() {
        let broker = Arc::new(super::InputBroker::default());
        broker.next_owner.store(u64::MAX - 1, Ordering::Release);
        let last = InjectedInputState::with_broker(broker.clone());
        assert_eq!(last.owner, u64::MAX);
        last.send_key_down(0x08, None, || Ok(()))
            .expect("last identity");
        for _ in 0..3 {
            let rejected = InjectedInputState::with_broker(broker.clone());
            assert_eq!(rejected.owner, 0);
            assert!(rejected
                .send_key_down(0x09, None, || panic!("exhausted owner must not inject"))
                .is_err());
            assert_eq!(broker.next_owner.load(Ordering::Acquire), u64::MAX);
        }
        assert!(last.cleanup(|_| Ok(()), |_| Ok(())).is_safe());
        assert_eq!(broker.counts(), (0, 0));
    }

    #[test]
    fn shared_broker_rejects_cross_owner_key_press_and_never_releases_another_owner() {
        let broker = Arc::new(super::InputBroker::default());
        let remap = InjectedInputState::with_broker(broker.clone());
        let text = InjectedInputState::with_broker(broker.clone());
        remap
            .send_key_down(0x08, None, || Ok(()))
            .expect("remap down");
        assert_eq!(broker.counts(), (1, 0));
        assert!(text
            .send_key_down(0x08, None, || panic!("conflicting press must not send"))
            .is_err());
        text.send_key_up(0x08, None, || panic!("foreign recovery release"))
            .expect("no-op");
        text.send_key_up_with_permission(
            0x08,
            None,
            || Ok(()),
            || panic!("foreign normal release"),
        )
        .expect("no-op");
        assert!(text
            .cleanup(|_| panic!("foreign cleanup"), |_| panic!("no buttons"))
            .is_safe());
        assert!(!remap
            .cleanup(|_| Err("mock failed release".into()), |_| Ok(()))
            .is_safe());
        assert!(text
            .send_key_down(0x08, None, || panic!("failed cleanup still owns key"))
            .is_err());
        assert!(remap.cleanup(|_| Ok(()), |_| Ok(())).is_safe());
        assert_eq!(broker.counts(), (0, 0));
        text.send_key_down(0x08, None, || Ok(()))
            .expect("new ownership");
        assert_eq!(remap.counts(), (0, 0));
        assert_eq!(text.counts(), (1, 0));
        assert!(text.cleanup(|_| Ok(()), |_| Ok(())).is_safe());
    }

    #[test]
    fn shared_broker_button_collision_and_ownership_corruption_fail_closed() {
        let broker = Arc::new(super::InputBroker::default());
        let first = InjectedInputState::with_broker(broker.clone());
        let second = InjectedInputState::with_broker(broker.clone());
        first
            .send_button_down(MouseButton::Left, None, || Ok(()))
            .expect("down");
        assert!(second
            .send_button_down(MouseButton::Left, None, || panic!("collision"))
            .is_err());
        second
            .send_button_up(MouseButton::Left, None, || panic!("foreign up"))
            .expect("no-op");
        broker
            .button_owners
            .lock()
            .expect("fault injection")
            .insert(MouseButton::Left, second.owner);
        let report = first.cleanup(
            |_| panic!("no keys"),
            |_| panic!("unknown ownership cannot release"),
        );
        assert!(!report.is_safe());
        assert_eq!(report.failures[0].attempts, 0);
        assert_eq!(first.counts(), (0, 1));
        broker
            .button_owners
            .lock()
            .expect("repair fixture")
            .insert(MouseButton::Left, first.owner);
        assert!(first.cleanup(|_| Ok(()), |_| Ok(())).is_safe());
        assert_eq!(broker.counts(), (0, 0));
    }

    #[test]
    fn unavailable_broker_ownership_retains_local_input_and_refuses_os_release() {
        let broker = Arc::new(super::InputBroker::default());
        let state = InjectedInputState::with_broker(broker.clone());
        state.send_key_down(0x41, None, || Ok(())).expect("down");
        let held = broker.key_owners.lock().expect("hold ownership");
        assert_eq!(broker.counts(), (usize::MAX, usize::MAX));
        assert!(state
            .send_key_up(0x41, None, || panic!("unavailable owner"))
            .is_err());
        assert!(!state
            .cleanup(|_| panic!("no ownership guard"), |_| panic!("no buttons"))
            .is_safe());
        assert_eq!(state.counts(), (1, 0));
        drop(held);
        assert!(state.cleanup(|_| Ok(()), |_| Ok(())).is_safe());
    }

    #[test]
    fn ordinary_and_recovery_transactions_return_while_operation_remains_locked() {
        let state = Arc::new(InjectedInputState::default());
        state.send_key_down(0x41, None, || Ok(())).expect("down");
        let held = state.operation.lock().expect("hold transaction");
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let worker_state = state.clone();
        let worker = std::thread::spawn(move || {
            let down = worker_state.send_key_down_with_permission(
                0x42,
                None,
                || Ok(()),
                || panic!("must not inject"),
            );
            let release = worker_state.send_key_up(0x41, None, || {
                panic!("must not release without serialization")
            });
            let movement = worker_state.perform_with_permission(
                "move",
                None,
                || Ok(()),
                || panic!("must not move"),
            );
            tx.send((down, release, movement)).expect("result");
        });
        let (down, release, movement) = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("bounded return without releasing held lock");
        assert!(down.expect_err("down denied").contains("100ms"));
        assert!(release.expect_err("release denied").contains("100ms"));
        assert!(movement.expect_err("move denied").contains("100ms"));
        worker.join().expect("worker");
        drop(held);
        assert_eq!(state.counts(), (1, 0));
        assert!(state.cleanup(|_| Ok(()), |_| Ok(())).is_safe());
    }

    #[test]
    fn canceled_transaction_does_not_wait_for_contended_operation() {
        let state = InjectedInputState::default();
        let held = state.operation.lock().expect("held");
        let cancel = AtomicBool::new(true);
        let result = state.send_key_down_with_permission(
            0x41,
            Some(&cancel),
            || panic!("permission unnecessary"),
            || panic!("no input"),
        );
        assert!(result.expect_err("cancel").contains("F12"));
        drop(held);
        assert_eq!(state.counts(), (0, 0));
    }

    #[test]
    fn unicode_failed_release_remains_tracked_and_cleanup_redacts_content() {
        let state = InjectedInputState::default();
        let mut calls = Vec::new();
        assert!(state
            .send_text_with_permission(
                "密",
                None,
                || Ok(()),
                |key, down| {
                    calls.push((key, down));
                    if down {
                        Ok(())
                    } else {
                        Err("mock release failure".into())
                    }
                }
            )
            .is_err());
        assert_eq!(
            calls,
            vec![
                (super::UNICODE_INPUT_TAG | u32::from('密'), true),
                (super::UNICODE_INPUT_TAG | u32::from('密'), false)
            ]
        );
        assert_eq!(state.counts(), (1, 0));
        assert_eq!(state.last_input(), Some("key_down:unicode_unit".into()));
        let failed = state.cleanup(|_| Err("mock denied".into()), |_| Ok(()));
        assert_eq!(failed.failures[0].value, "unicode_unit");
        assert_eq!(state.counts(), (1, 0));
        assert!(state
            .cleanup(
                |key| {
                    assert_eq!(key, calls[0].0);
                    Ok(())
                },
                |_| Ok(())
            )
            .is_safe());
        assert_eq!(state.counts(), (0, 0));
    }

    #[test]
    fn unicode_cancel_during_down_still_releases_and_prevents_next_unit() {
        let state = InjectedInputState::default();
        let cancel = AtomicBool::new(false);
        let mut calls = Vec::new();
        assert!(state
            .send_text_with_permission(
                "ab",
                Some(&cancel),
                || Ok(()),
                |key, down| {
                    calls.push((key, down));
                    if down {
                        cancel.store(true, Ordering::SeqCst);
                    }
                    Ok(())
                }
            )
            .is_err());
        assert_eq!(calls.len(), 2);
        assert!(calls[0].1);
        assert!(!calls[1].1);
        assert_eq!(calls[0].0, calls[1].0);
        assert_eq!(state.counts(), (0, 0));
    }

    #[test]
    fn unicode_surrogate_units_are_sent_and_tracked_separately() {
        let state = InjectedInputState::default();
        let mut calls = Vec::new();
        state
            .send_text_with_permission(
                "😀",
                None,
                || Ok(()),
                |key, down| {
                    calls.push((key, down));
                    Ok(())
                },
            )
            .expect("text");
        let expected = "😀"
            .encode_utf16()
            .flat_map(|unit| {
                [
                    (super::UNICODE_INPUT_TAG | u32::from(unit), true),
                    (super::UNICODE_INPUT_TAG | u32::from(unit), false),
                ]
            })
            .collect::<Vec<_>>();
        assert_eq!(calls, expected);
        assert_eq!(state.counts(), (0, 0));
    }

    #[test]
    fn blocked_diagnostic_sink_has_bounded_admission_and_reports_loss() {
        use std::sync::mpsc::sync_channel;
        use std::time::Duration;
        let (entered_tx, entered_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(1);
        let (line_tx, line_rx) = sync_channel(128);
        let mut first = true;
        let diagnostics = SafetyDiagnostics::with_writer("unused-mock-path".into(), move |line| {
            if first {
                first = false;
                entered_tx.send(()).expect("entered");
                release_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("fixture release");
            }
            line_tx.send(line.to_string()).expect("capture");
            true
        });
        diagnostics.record("first", &[]);
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("blocked sink");
        for _ in 0..64 {
            diagnostics.record("queued", &[]);
        }
        // Calls complete while the writer is provably blocked, without spawning
        // another worker or allowing the pending queue to grow.
        for _ in 0..100 {
            diagnostics.record_async("overflow", vec![]);
        }
        assert_eq!(diagnostics.lost.load(Ordering::Relaxed), 100);
        release_tx.send(()).expect("release");
        for _ in 0..65 {
            line_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("drained");
        }
        diagnostics.record(
            "after_loss",
            &[
                ("event", "cannot replace metadata".into()),
                ("long", "界".repeat(2000)),
            ],
        );
        let line = line_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("loss record");
        let value: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(value["event"], "after_loss");
        assert_eq!(value["lost_events"], 100);
        assert_eq!(value["long"].as_str().expect("text").chars().count(), 512);
        assert_eq!(value["sequence"], 165);
    }

    #[test]
    fn failed_diagnostic_write_is_visible_in_next_record() {
        use std::sync::mpsc::sync_channel;
        use std::time::Duration;
        let (tx, rx) = sync_channel(1);
        let mut first = true;
        let diagnostics = SafetyDiagnostics::with_writer("unused-mock-path".into(), move |line| {
            tx.send(line.to_string()).expect("capture");
            let result = !first;
            first = false;
            result
        });
        diagnostics.record("failed", &[]);
        rx.recv_timeout(Duration::from_secs(1)).expect("first");
        // Sink completion notification precedes its failure counter update;
        // wait with a deadline, rather than relying on scheduling sleeps.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while diagnostics.lost.load(Ordering::Relaxed) == 0 && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert_eq!(diagnostics.lost.load(Ordering::Relaxed), 1);
        diagnostics.record("next", &[]);
        let value: serde_json::Value =
            serde_json::from_str(&rx.recv_timeout(Duration::from_secs(1)).expect("next"))
                .expect("json");
        assert_eq!(value["lost_events"], 1);
    }

    #[test]
    fn cleanup_lock_contention_retains_ledger_and_reports_unsafe() {
        let state = InjectedInputState::default();
        state.send_key_down(0x41, None, || Ok(())).expect("down");
        let guard = state.operation.lock().expect("operation");
        let report = state.cleanup(
            |_| panic!("must not send without lock"),
            |_| panic!("must not send without lock"),
        );
        assert!(!report.is_safe());
        assert_eq!(report.failures[0].value, "operation_lock");
        assert_eq!(report.failures[0].attempts, 0);
        assert_eq!(state.counts(), (usize::MAX, usize::MAX));
        drop(guard);
        assert_eq!(state.counts(), (1, 0));
        assert!(state.cleanup(|_| Ok(()), |_| Ok(())).is_safe());
    }

    #[test]
    fn cleanup_ledger_contention_fails_without_sending_and_retains_input() {
        let state = InjectedInputState::default();
        state
            .send_key_down(0x41, None, || Ok(()))
            .expect("fake down");
        let held = state.keys.lock().expect("key ledger");
        let report = state.cleanup(
            |_| panic!("ledger unavailable"),
            |_| panic!("ledger unavailable"),
        );
        assert!(!report.is_safe());
        assert_eq!(report.failures[0].value, "key_ledger");
        drop(held);
        assert_eq!(state.counts(), (1, 0));
        let held = state.buttons.lock().expect("button ledger");
        let report = state.cleanup(
            |_| panic!("ledger unavailable"),
            |_| panic!("ledger unavailable"),
        );
        assert!(!report.is_safe());
        assert_eq!(report.failures[0].value, "button_ledger");
        drop(held);
        assert_eq!(state.counts(), (1, 0));
        assert!(state.cleanup(|_| Ok(()), |_| Ok(())).is_safe());
    }

    #[test]
    fn diagnostic_contention_does_not_prevent_tracked_release() {
        let state = InjectedInputState::default();
        state
            .send_key_down(0x41, None, || Ok(()))
            .expect("fake down");
        let description = state.last_input.lock().expect("diagnostic");
        let timestamp = state.last_input_at_ms.lock().expect("diagnostic timestamp");
        let report = state.cleanup(|_| Ok(()), |_| panic!("no buttons"));
        assert!(report.is_safe());
        assert_eq!(report.released_keys, 1);
        drop(description);
        drop(timestamp);
        assert_eq!(state.counts(), (0, 0));
    }

    #[test]
    fn permissioned_send_holds_ledger_before_side_effect_and_tracks_inflight_cancel() {
        let state = InjectedInputState::default();
        let cancelled = AtomicBool::new(false);
        state
            .send_key_down_with_permission(
                0x41,
                Some(&cancelled),
                || Ok(()),
                || {
                    assert!(
                        state.keys.try_lock().is_err(),
                        "ledger must already be acquired"
                    );
                    cancelled.store(true, Ordering::Release);
                    Ok(())
                },
            )
            .expect("in-flight fake down finishes");
        assert_eq!(state.counts(), (1, 0));
        assert!(state.cleanup(|_| Ok(()), |_| Ok(())).is_safe());
        let held = state.buttons.lock().expect("held ledger");
        assert!(state
            .send_button_down_with_permission(
                MouseButton::Left,
                None,
                || Ok(()),
                || panic!("must not inject without ledger")
            )
            .is_err());
        drop(held);
        assert_eq!(state.counts(), (0, 0));
    }

    #[test]
    fn key_is_tracked_only_after_successful_down() {
        let state = InjectedInputState::default();
        assert!(state.send_key_down(0x41, None, || Ok(())).is_ok());
        assert_eq!(state.counts(), (1, 0));
        let failed = state.send_key_down(0x42, None, || Err("down failed".to_string()));
        assert!(failed.is_err());
        assert_eq!(state.counts(), (1, 0));
    }

    #[test]
    fn key_is_removed_only_after_successful_up() {
        let state = InjectedInputState::default();
        state.send_key_down(0x41, None, || Ok(())).unwrap();
        assert!(state
            .send_key_up(0x41, None, || Err("up failed".to_string()))
            .is_err());
        assert_eq!(state.counts(), (1, 0));
        state.send_key_up(0x41, None, || Ok(())).unwrap();
        assert_eq!(state.counts(), (0, 0));
    }

    #[test]
    fn left_mouse_down_is_tracked_after_success() {
        let state = InjectedInputState::default();
        state
            .send_button_down(MouseButton::Left, None, || Ok(()))
            .unwrap();
        assert_eq!(state.counts(), (0, 1));
    }

    #[test]
    fn right_mouse_down_is_tracked_after_success() {
        let state = InjectedInputState::default();
        state
            .send_button_down(MouseButton::Right, None, || Ok(()))
            .unwrap();
        assert_eq!(state.counts(), (0, 1));
    }

    #[test]
    fn mouse_up_failure_keeps_button_registered() {
        let state = InjectedInputState::default();
        state
            .send_button_down(MouseButton::Left, None, || Ok(()))
            .unwrap();
        assert!(state
            .send_button_up(MouseButton::Left, None, || Err("up failed".to_string()))
            .is_err());
        assert_eq!(state.counts(), (0, 1));
    }

    #[test]
    fn cleanup_releases_one_key_exactly_once_when_first_attempt_succeeds() {
        let state = InjectedInputState::default();
        state.send_key_down(0x41, None, || Ok(())).unwrap();
        let calls = AtomicUsize::new(0);
        let report = state.cleanup(
            |_: u32| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            |_: MouseButton| Ok(()),
        );
        assert!(report.is_safe());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.counts(), (0, 0));
    }

    #[test]
    fn cleanup_retries_a_transient_key_release() {
        let state = InjectedInputState::default();
        state.send_key_down(0x41, None, || Ok(())).unwrap();
        let calls = AtomicUsize::new(0);
        let report = state.cleanup(
            |_: u32| {
                let call = calls.fetch_add(1, Ordering::SeqCst);
                if call == 0 {
                    Err("temporary".to_string())
                } else {
                    Ok(())
                }
            },
            |_: MouseButton| Ok(()),
        );
        assert!(report.is_safe());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn cleanup_is_bounded_for_a_persistent_key_failure() {
        let state = InjectedInputState::default();
        state.send_key_down(0x41, None, || Ok(())).unwrap();
        let calls = AtomicUsize::new(0);
        let report = state.cleanup(
            |_: u32| {
                calls.fetch_add(1, Ordering::SeqCst);
                Err("persistent".to_string())
            },
            |_: MouseButton| Ok(()),
        );
        assert!(!report.is_safe());
        assert_eq!(calls.load(Ordering::SeqCst), MAX_CLEANUP_ATTEMPTS);
        assert_eq!(state.counts(), (1, 0));
    }

    #[test]
    fn cleanup_releases_left_button_after_cancel() {
        let state = InjectedInputState::default();
        state
            .send_button_down(MouseButton::Left, None, || Ok(()))
            .unwrap();
        let cancel = AtomicBool::new(true);
        let report = state.cleanup(
            |_| Ok(()),
            |button| {
                assert_eq!(button, MouseButton::Left);
                Ok(())
            },
        );
        assert!(cancel.load(Ordering::SeqCst));
        assert!(report.is_safe());
        assert_eq!(state.counts(), (0, 0));
    }

    #[test]
    fn cancellation_blocks_a_new_key_down() {
        let state = InjectedInputState::default();
        let cancel = AtomicBool::new(true);
        assert!(state.send_key_down(0x41, Some(&cancel), || Ok(())).is_err());
        assert_eq!(state.counts(), (0, 0));
    }

    #[test]
    fn cancellation_blocks_a_new_mouse_down() {
        let state = InjectedInputState::default();
        let cancel = AtomicBool::new(true);
        assert!(state
            .send_button_down(MouseButton::Left, Some(&cancel), || Ok(()))
            .is_err());
        assert_eq!(state.counts(), (0, 0));
    }

    #[test]
    fn cleanup_does_not_consult_cancel_flag() {
        let state = InjectedInputState::default();
        state.send_key_down(0x41, None, || Ok(())).unwrap();
        let report = state.cleanup(|_| Ok(()), |_| Ok(()));
        assert!(report.is_safe());
    }

    #[test]
    fn cleanup_handles_key_and_button_together() {
        let state = InjectedInputState::default();
        state.send_key_down(0x41, None, || Ok(())).unwrap();
        state
            .send_button_down(MouseButton::Right, None, || Ok(()))
            .unwrap();
        let report = state.cleanup(|_| Ok(()), |_| Ok(()));
        assert_eq!(report.released_keys, 1);
        assert_eq!(report.released_buttons, 1);
        assert_eq!(state.counts(), (0, 0));
    }

    #[test]
    fn failed_button_cleanup_remains_visible_for_recovery() {
        let state = InjectedInputState::default();
        state
            .send_button_down(MouseButton::Right, None, || Ok(()))
            .unwrap();
        let report = state.cleanup(|_| Ok(()), |_| Err("blocked".to_string()));
        assert_eq!(report.unreleased_count(), 1);
        assert_eq!(state.counts(), (0, 1));
    }

    #[test]
    fn a_second_cleanup_can_recover_a_previous_failure() {
        let state = InjectedInputState::default();
        state.send_key_down(0x41, None, || Ok(())).unwrap();
        assert!(!state
            .cleanup(|_| Err("blocked".to_string()), |_| Ok(()))
            .is_safe());
        assert!(state.cleanup(|_| Ok(()), |_| Ok(())).is_safe());
        assert_eq!(state.counts(), (0, 0));
    }

    #[test]
    fn cleanup_report_keeps_per_input_failure_context() {
        let state = InjectedInputState::default();
        state.send_key_down(0x41, None, || Ok(())).unwrap();
        let report = state.cleanup(|_| Err("win32=5".to_string()), |_| Ok(()));
        assert_eq!(report.failures[0].kind, "key");
        assert_eq!(report.failures[0].value, "0x41");
        assert_eq!(report.failures[0].attempts, MAX_CLEANUP_ATTEMPTS);
        assert_eq!(report.failures[0].last_error, "win32=5");
    }

    #[test]
    fn duplicate_down_is_one_tracked_input() {
        let state = InjectedInputState::default();
        state.send_key_down(0x41, None, || Ok(())).unwrap();
        state.send_key_down(0x41, None, || Ok(())).unwrap();
        assert_eq!(state.counts(), (1, 0));
    }

    #[test]
    fn duplicate_up_is_idempotent_after_success() {
        let state = InjectedInputState::default();
        state.send_key_down(0x41, None, || Ok(())).unwrap();
        state.send_key_up(0x41, None, || Ok(())).unwrap();
        state.send_key_up(0x41, None, || Ok(())).unwrap();
        assert_eq!(state.counts(), (0, 0));
    }

    #[test]
    fn operation_serializes_send_and_tracking() {
        let state = InjectedInputState::default();
        state.send_key_down(0x41, None, || Ok(())).unwrap();
        state
            .send_button_down(MouseButton::Left, None, || Ok(()))
            .unwrap();
        let report = state.cleanup(|_| Ok(()), |_| Ok(()));
        assert!(report.is_safe());
        assert_eq!(state.counts(), (0, 0));
    }

    #[test]
    fn x_buttons_are_tracked_without_collapsing_into_left_button() {
        let state = InjectedInputState::default();
        state
            .send_button_down(MouseButton::X1, None, || Ok(()))
            .unwrap();
        state
            .send_button_down(MouseButton::X2, None, || Ok(()))
            .unwrap();
        assert_eq!(state.counts(), (0, 2));
        state.cleanup(|_| Ok(()), |_| Ok(()));
        assert_eq!(state.counts(), (0, 0));
    }

    #[test]
    fn cleanup_does_not_release_untracked_buttons() {
        let state = InjectedInputState::default();
        let calls = AtomicUsize::new(0);
        let report = state.cleanup(
            |_| Ok(()),
            |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        );
        assert!(report.is_safe());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn failed_down_does_not_trigger_cleanup_release() {
        let state = InjectedInputState::default();
        assert!(state
            .send_button_down(MouseButton::Left, None, || Err("down".to_string()))
            .is_err());
        let calls = AtomicUsize::new(0);
        state.cleanup(
            |_| Ok(()),
            |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cleanup_failure_does_not_clear_tracking_before_retry() {
        let state = InjectedInputState::default();
        state
            .send_button_down(MouseButton::Left, None, || Ok(()))
            .unwrap();
        state.cleanup(|_| Ok(()), |_| Err("still down".to_string()));
        assert_eq!(state.counts(), (0, 1));
    }

    #[test]
    fn playback_permit_revocation_blocks_new_input_but_allows_forced_release() {
        let controller = crate::runtime_control::RuntimeController::new();
        let mut lease = controller
            .begin_start(None)
            .expect("start should be admitted");
        let token = lease.token();
        assert!(controller.activate(token));
        lease.commit();
        let state = Arc::new(InjectedInputState::default());
        let permit = InputPermit::new(Arc::clone(&state), controller.clone(), token);

        permit
            .send_key_down(0x41, None, || Ok(()))
            .expect("running playback may press a key");
        controller.request_stop();
        let send_called = AtomicBool::new(false);
        assert!(permit
            .send_key_down(0x42, None, || {
                send_called.store(true, Ordering::SeqCst);
                Ok(())
            })
            .is_err());
        assert!(!send_called.load(Ordering::SeqCst));
        assert_eq!(state.counts(), (1, 0));

        permit
            .force_key_up(0x41, || Ok(()))
            .expect("cleanup must be allowed after revocation");
        assert_eq!(state.counts(), (0, 0));
        assert!(controller.begin_cleaning(token));
        assert!(controller.finish(token, true));
    }

    #[test]
    fn playback_permit_checks_motion_and_text_like_operations_atomically() {
        let controller = crate::runtime_control::RuntimeController::new();
        let mut lease = controller
            .begin_start(None)
            .expect("start should be admitted");
        let token = lease.token();
        assert!(controller.activate(token));
        lease.commit();
        let state = Arc::new(InjectedInputState::default());
        let permit = InputPermit::new(state, controller.clone(), token);
        let calls = AtomicUsize::new(0);
        permit
            .perform("mouse_move", None, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .unwrap();
        controller.request_stop();
        assert!(permit
            .perform("unicode_text", None, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
