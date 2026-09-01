use crate::{
    AppConfig, AppError, KeyAction, MacroMode, MacroRule, MacroStep, MacroTarget, MouseButton,
};
use serde::Serialize;
use std::sync::{Arc, Mutex};

#[cfg(windows)]
use std::collections::{HashMap, HashSet};
#[cfg(windows)]
use std::process::Command;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
#[cfg(windows)]
use std::thread;
#[cfg(windows)]
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct HookService {
    shared: Arc<HookShared>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MacroRecordingStatus {
    pub active: bool,
    pub step_count: usize,
    pub target_locked: bool,
    pub target_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MacroRecordingResult {
    pub steps: Vec<MacroStep>,
    pub target: Option<MacroTarget>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MacroPlaybackStatus {
    pub running: bool,
    pub current_step: usize,
    pub total_steps: usize,
    pub last_error: Option<String>,
}

impl HookService {
    pub fn start(config: AppConfig) -> Result<Self, AppError> {
        let shared = Arc::new(HookShared::new(config));

        #[cfg(windows)]
        {
            let _ = HOOK_SHARED.set(Arc::clone(&shared));
            let thread_shared = Arc::clone(&shared);
            thread::Builder::new()
                .name("autoflow-keyboard-hook".to_string())
                .spawn(move || hook_thread(thread_shared))
                .map_err(|error| {
                    AppError::with_detail(
                        "hook_start_failed",
                        "全局键盘监听启动失败",
                        error.to_string(),
                    )
                })?;
        }

        Ok(Self { shared })
    }

    pub fn update_config(&self, config: AppConfig) -> Result<(), AppError> {
        let mut current = self
            .shared
            .config
            .lock()
            .map_err(|_| AppError::internal("输入服务状态异常，请重启 AutoFlow"))?;
        *current = config;
        #[cfg(windows)]
        self.shared.clear_transient_state();
        Ok(())
    }

    pub fn emergency_stop(&self) {
        #[cfg(windows)]
        self.shared.clear_transient_state();
    }

    pub fn start_recording(&self) -> Result<(), AppError> {
        #[cfg(windows)]
        {
            self.shared.start_recording()
        }
        #[cfg(not(windows))]
        Err(AppError::invalid(
            "recording_unsupported",
            "宏录制目前只支持 Windows 桌面端",
        ))
    }

    pub fn stop_recording(&self) -> Result<MacroRecordingResult, AppError> {
        #[cfg(windows)]
        {
            self.shared.stop_recording()
        }
        #[cfg(not(windows))]
        Err(AppError::invalid(
            "recording_unsupported",
            "宏录制目前只支持 Windows 桌面端",
        ))
    }

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

    pub fn is_playback_running(&self) -> bool {
        #[cfg(windows)]
        {
            return self.shared.is_playback_running();
        }
        #[cfg(not(windows))]
        false
    }

    pub fn recording_status(&self) -> MacroRecordingStatus {
        #[cfg(windows)]
        {
            return self.shared.recording_status();
        }
        #[cfg(not(windows))]
        MacroRecordingStatus {
            active: false,
            step_count: 0,
            target_locked: false,
            target_name: None,
        }
    }

    pub fn playback_status(&self) -> MacroPlaybackStatus {
        #[cfg(windows)]
        {
            return self.shared.playback_status();
        }
        #[cfg(not(windows))]
        MacroPlaybackStatus {
            running: false,
            current_step: 0,
            total_steps: 0,
            last_error: None,
        }
    }
}

impl Drop for HookService {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            self.shared.shutdown.store(true, Ordering::SeqCst);
            let thread_id = self.shared.thread_id.load(Ordering::SeqCst);
            if thread_id != 0 {
                unsafe {
                    use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};
                    let _ = PostThreadMessageW(
                        thread_id,
                        WM_QUIT,
                        windows::Win32::Foundation::WPARAM(0),
                        windows::Win32::Foundation::LPARAM(0),
                    );
                }
            }
        }
    }
}

struct HookShared {
    config: Mutex<AppConfig>,
    #[cfg(windows)]
    pressed: Mutex<HashSet<u32>>,
    #[cfg(windows)]
    latched_hotkeys: Mutex<HashSet<String>>,
    #[cfg(windows)]
    active_remaps: Mutex<HashMap<u32, u32>>,
    #[cfg(windows)]
    text_buffer: Mutex<String>,
    #[cfg(windows)]
    recorder: Mutex<RecorderState>,
    #[cfg(windows)]
    playback: Mutex<PlaybackState>,
    #[cfg(windows)]
    thread_id: AtomicU32,
    #[cfg(windows)]
    shutdown: AtomicBool,
    #[cfg(windows)]
    native_clicker_hotkey_registered: AtomicBool,
}

impl HookShared {
    fn new(config: AppConfig) -> Self {
        Self {
            config: Mutex::new(config),
            #[cfg(windows)]
            pressed: Mutex::new(HashSet::new()),
            #[cfg(windows)]
            latched_hotkeys: Mutex::new(HashSet::new()),
            #[cfg(windows)]
            active_remaps: Mutex::new(HashMap::new()),
            #[cfg(windows)]
            text_buffer: Mutex::new(String::new()),
            #[cfg(windows)]
            recorder: Mutex::new(RecorderState::default()),
            #[cfg(windows)]
            playback: Mutex::new(PlaybackState::default()),
            #[cfg(windows)]
            thread_id: AtomicU32::new(0),
            #[cfg(windows)]
            shutdown: AtomicBool::new(false),
            #[cfg(windows)]
            native_clicker_hotkey_registered: AtomicBool::new(false),
        }
    }

    #[cfg(windows)]
    fn clear_transient_state(&self) {
        self.stop_playback();
        if let Ok(mut recorder) = self.recorder.lock() {
            recorder.active = false;
            recorder.last_event = None;
            recorder.last_mouse_move = None;
            recorder.steps.clear();
            recorder.completed_steps = None;
            recorder.pressed_keys.clear();
            recorder.pressed_buttons.clear();
        }
        if let Ok(mut remaps) = self.active_remaps.lock() {
            for target_vk in remaps.drain().map(|(_, target_vk)| target_vk) {
                let _ = send_key(target_vk, false);
            }
        }
        if let Ok(mut pressed) = self.pressed.lock() {
            pressed.clear();
        }
        if let Ok(mut latched) = self.latched_hotkeys.lock() {
            latched.clear();
        }
        if let Ok(mut buffer) = self.text_buffer.lock() {
            buffer.clear();
        }
    }

    #[cfg(windows)]
    fn start_recording(&self) -> Result<(), AppError> {
        if self.is_playback_running() {
            return Err(AppError::invalid(
                "macro_busy",
                "宏正在运行，请先停止播放后再录制",
            ));
        }
        let mut recorder = self
            .recorder
            .lock()
            .map_err(|_| AppError::internal("录制器状态异常，请重启 AutoFlow"))?;
        if recorder.active {
            return Err(AppError::invalid("recording_active", "宏录制已经在进行中"));
        }
        recorder.active = true;
        recorder.started_at = Some(Instant::now());
        recorder.last_event = None;
        recorder.last_mouse_move = None;
        recorder.steps.clear();
        recorder.completed_steps = None;
        recorder.pressed_keys.clear();
        recorder.pressed_buttons.clear();
        Ok(())
    }

    #[cfg(windows)]
    fn stop_recording(&self) -> Result<MacroRecordingResult, AppError> {
        let mut recorder = self
            .recorder
            .lock()
            .map_err(|_| AppError::internal("录制器状态异常，请重启 AutoFlow"))?;
        if recorder.active {
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

    #[cfg(windows)]
    fn finish_recording(&self) {
        if let Ok(mut recorder) = self.recorder.lock() {
            if recorder.active {
                finish_recorder(&mut recorder);
            }
        }
    }

    #[cfg(windows)]
    fn is_recording(&self) -> bool {
        self.recorder
            .lock()
            .map(|recorder| recorder.active)
            .unwrap_or(false)
    }

    #[cfg(windows)]
    fn recording_status(&self) -> MacroRecordingStatus {
        self.recorder
            .lock()
            .map(|recorder| MacroRecordingStatus {
                active: recorder.active,
                step_count: if recorder.active {
                    recorder.steps.len()
                } else {
                    recorder.completed_steps.as_ref().map_or(0, Vec::len)
                },
                target_locked: false,
                target_name: None,
            })
            .unwrap_or(MacroRecordingStatus {
                active: false,
                step_count: 0,
                target_locked: false,
                target_name: None,
            })
    }

    #[cfg(windows)]
    fn is_playback_running(&self) -> bool {
        self.playback
            .lock()
            .map(|playback| playback.running)
            .unwrap_or(false)
    }

    #[cfg(windows)]
    fn playback_status(&self) -> MacroPlaybackStatus {
        self.playback
            .lock()
            .map(|playback| MacroPlaybackStatus {
                running: playback.running,
                current_step: playback.current_step,
                total_steps: playback.total_steps,
                last_error: playback.last_error.clone(),
            })
            .unwrap_or(MacroPlaybackStatus {
                running: false,
                current_step: 0,
                total_steps: 0,
                last_error: Some("播放状态读取失败，请重启 AutoFlow".to_string()),
            })
    }

    #[cfg(windows)]
    fn set_playback_error(&self, message: impl Into<String>) {
        if let Ok(mut playback) = self.playback.lock() {
            playback.running = false;
            playback.stop = None;
            playback.last_error = Some(message.into());
        }
    }

    #[cfg(windows)]
    fn set_playback_step(&self, current_step: usize) {
        if let Ok(mut playback) = self.playback.lock() {
            playback.current_step = current_step;
        }
    }

    #[cfg(windows)]
    fn start_playback(self: &Arc<Self>, macro_rule: MacroRule) -> Result<(), AppError> {
        let macro_rule = normalize_playback_rule(macro_rule);
        if macro_rule.steps.is_empty() {
            let error =
                AppError::invalid("macro_empty", "这个宏还没有步骤，录制或添加步骤后才能播放");
            self.set_playback_error(error.message.clone());
            return Err(error);
        }
        // Macros always act on the current foreground program. A saved target
        // from older versions is intentionally ignored so the same macro can
        // be used everywhere.
        let mut playback = self
            .playback
            .lock()
            .map_err(|_| AppError::internal("播放状态异常，请重启 AutoFlow"))?;
        if playback.running {
            return Err(AppError::invalid("macro_busy", "已有一个宏正在运行"));
        }
        let stop = Arc::new(AtomicBool::new(false));
        playback.running = true;
        playback.stop = Some(Arc::clone(&stop));
        playback.current_step = 0;
        playback.total_steps = macro_rule.steps.len();
        playback.last_error = None;
        let shared = Arc::clone(self);
        thread::Builder::new()
            .name("autoflow-macro-playback".to_string())
            .spawn(move || {
                // Give Windows a moment to finish foreground activation before
                // injecting the first recorded event.
                thread::sleep(Duration::from_millis(120));
                let result = play_macro_thread(&shared, &macro_rule, &stop);
                if let Ok(mut playback) = shared.playback.lock() {
                    playback.running = false;
                    playback.stop = None;
                    if let Err(error) = result {
                        playback.last_error = Some(error);
                    }
                }
            })
            .map_err(|error| {
                playback.running = false;
                playback.stop = None;
                AppError::with_detail("playback_start_failed", "宏播放启动失败", error.to_string())
            })?;
        Ok(())
    }

    #[cfg(windows)]
    fn stop_playback(&self) {
        if let Ok(playback) = self.playback.lock() {
            if let Some(stop) = &playback.stop {
                stop.store(true, Ordering::SeqCst);
            }
        }
    }
}

#[cfg(windows)]
fn normalize_playback_rule(mut macro_rule: MacroRule) -> MacroRule {
    // The basic clicker is intentionally not recorded as arbitrary events.
    // It always clicks at the current cursor position, in whichever program is
    // active, so a bad/partial recording can never turn it into a no-op.
    if macro_rule.name.trim() == "连点器" {
        macro_rule.mode = MacroMode::Toggle;
        macro_rule.steps = vec![
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
        ];
    }
    macro_rule
}

#[cfg(windows)]
#[derive(Default)]
struct RecorderState {
    active: bool,
    started_at: Option<Instant>,
    last_event: Option<Instant>,
    last_mouse_move: Option<(Instant, i32, i32)>,
    steps: Vec<MacroStep>,
    completed_steps: Option<Vec<MacroStep>>,
    pressed_keys: HashSet<u32>,
    pressed_buttons: HashSet<MouseButton>,
}

#[cfg(windows)]
#[derive(Default)]
struct PlaybackState {
    running: bool,
    stop: Option<Arc<AtomicBool>>,
    current_step: usize,
    total_steps: usize,
    last_error: Option<String>,
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
            log::error!("全局键盘监听启动失败: {error}");
            return;
        }
    };
    let mouse_hook =
        unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), Some(HINSTANCE::default()), 0) };
    let mouse_hook = match mouse_hook {
        Ok(hook) => hook,
        Err(error) => {
            log::error!("全局鼠标监听启动失败: {error}");
            let _ = unsafe { UnhookWindowsHookEx(hook) };
            return;
        }
    };

    // Use Windows' native hotkey delivery for the clicker. Low-level keyboard
    // hooks are still needed for F12, recording and general macros, but they
    // are not a dependable foundation for a Ctrl+function-key toggle.
    let native_clicker_hotkey = unsafe {
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
    if !native_clicker_hotkey {
        log::warn!("Ctrl+F8 原生热键注册失败，将使用兼容监听方式");
    }

    let mut message = MSG::default();
    loop {
        if shared.shutdown.load(Ordering::SeqCst) {
            break;
        }
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if result.0 <= 0 {
            break;
        }
        if message.message == WM_HOTKEY && message.wParam.0 as i32 == CLICKER_HOTKEY_ID {
            process_native_clicker_hotkey(&shared);
            continue;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    let _ = unsafe { UnhookWindowsHookEx(hook) };
    let _ = unsafe { UnhookWindowsHookEx(mouse_hook) };
    if native_clicker_hotkey {
        let _ = unsafe { UnregisterHotKey(None, CLICKER_HOTKEY_ID) };
    }
    shared
        .native_clicker_hotkey_registered
        .store(false, Ordering::SeqCst);
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
    // Low-level hooks report left/right modifiers as distinct keys. Rules are
    // configured with the user-facing Ctrl / Alt / Shift names, so normalize
    // before tracking or matching a combination.
    let vk = canonical_virtual_key(info.vkCode);

    let config = match shared.config.lock() {
        Ok(config) => config.clone(),
        Err(_) => return CallNextHookEx(None, code, message, data),
    };
    let mut pressed = match shared.pressed.lock() {
        Ok(pressed) => pressed,
        Err(_) => return CallNextHookEx(None, code, message, data),
    };

    if is_down {
        pressed.insert(vk);
    } else {
        pressed.remove(&vk);
        if let Ok(mut latched) = shared.latched_hotkeys.lock() {
            latched.retain(|signature| !signature.split('+').any(|key| key_to_vk(key) == Some(vk)));
        }
    }

    if key_to_vk(&config.emergency_stop) == Some(vk) && is_down {
        // F12 must never discard a macro the user has just recorded. During a
        // recording it ends capture and leaves the result available for the
        // UI's “停止录制” button to save; otherwise it remains the emergency
        // stop for playback and other active rules.
        if shared.is_recording() {
            shared.finish_recording();
        } else {
            shared.clear_transient_state();
        }
        return LRESULT(1);
    }

    if recording_input_is_allowed(shared) {
        record_keyboard_event(shared, vk, is_down);
    }

    // A recording is a transparent capture session: do not let configured
    // hotkeys or text expansions consume the keys that the user is recording.
    if shared.is_recording() {
        return CallNextHookEx(None, code, message, data);
    }

    if is_up {
        stop_hold_macros_on_key_up(shared, &config, vk);
        if let Ok(mut remaps) = shared.active_remaps.lock() {
            if let Some(target_vk) = remaps.remove(&vk) {
                let _ = send_key(target_vk, false);
                return LRESULT(1);
            }
        }
        return CallNextHookEx(None, code, message, data);
    }

    if !config.global_enabled {
        return CallNextHookEx(None, code, message, data);
    }

    if process_macro_key_down(shared, &config, vk, &pressed) {
        return LRESULT(1);
    }

    if let Some(rule) = config.hotkeys.iter().find(|rule| {
        rule.enabled
            && rule.action.action_type == "remap"
            && rule.trigger_keys.len() == 1
            && key_to_vk(&rule.trigger_keys[0]) == Some(vk)
    }) {
        if let Some(target_vk) = key_to_vk(&rule.action.target) {
            if let Ok(mut remaps) = shared.active_remaps.lock() {
                remaps.insert(vk, target_vk);
            }
            let _ = send_key(target_vk, true);
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
            || !trigger_vks.iter().all(|key| pressed.contains(key))
        {
            continue;
        }
        let signature = trigger_vks
            .iter()
            .map(|key| key.to_string())
            .collect::<Vec<_>>()
            .join("+");
        let mut latched = match shared.latched_hotkeys.lock() {
            Ok(latched) => latched,
            Err(_) => continue,
        };
        if latched.insert(signature) {
            let target = rule.action.target.clone();
            thread::spawn(move || launch_target(&target));
            return LRESULT(1);
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
        let replacement = rule.replacement.clone();
        buffer.clear();
        drop(buffer);
        send_backspaces(length);
        let _ = send_unicode_text(&replacement);
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
        WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
    };

    if code < 0 || data.0 == 0 {
        return CallNextHookEx(None, code, message, data);
    }
    let info = *(data.0 as *const MSLLHOOKSTRUCT);
    if info.flags & 0x00000001 != 0 {
        return CallNextHookEx(None, code, message, data);
    }
    let Some(shared) = HOOK_SHARED.get() else {
        return CallNextHookEx(None, code, message, data);
    };
    let message_id = message.0 as u32;
    let x = info.pt.x;
    let y = info.pt.y;
    // Motion is meaningful even before the user has clicked into the target
    // application. Capture it as soon as recording starts, while button and
    // wheel events still exclude AutoFlow itself so the stop control is not
    // replayed as part of the macro.
    if message_id == WM_MOUSEMOVE && shared.is_recording() {
        record_mouse_move(shared, x, y);
        return CallNextHookEx(None, code, message, data);
    }
    if !recording_input_is_allowed(shared) {
        return CallNextHookEx(None, code, message, data);
    }

    match message_id {
        WM_LBUTTONDOWN => record_mouse_button(shared, MouseButton::Left, KeyAction::Down, x, y),
        WM_LBUTTONUP => record_mouse_button(shared, MouseButton::Left, KeyAction::Up, x, y),
        WM_RBUTTONDOWN => record_mouse_button(shared, MouseButton::Right, KeyAction::Down, x, y),
        WM_RBUTTONUP => record_mouse_button(shared, MouseButton::Right, KeyAction::Up, x, y),
        WM_MBUTTONDOWN => record_mouse_button(shared, MouseButton::Middle, KeyAction::Down, x, y),
        WM_MBUTTONUP => record_mouse_button(shared, MouseButton::Middle, KeyAction::Up, x, y),
        WM_XBUTTONDOWN => {
            record_mouse_button(shared, x_button(info.mouseData), KeyAction::Down, x, y)
        }
        WM_XBUTTONUP => record_mouse_button(shared, x_button(info.mouseData), KeyAction::Up, x, y),
        WM_MOUSEWHEEL => record_mouse_wheel(shared, 0, i32::from((info.mouseData >> 16) as i16)),
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
    use windows::Win32::System::Threading::GetCurrentProcessId;

    foreground_process_id() == Some(unsafe { GetCurrentProcessId() })
}

#[cfg(windows)]
fn recording_input_is_allowed(shared: &HookShared) -> bool {
    // Keep the stop-recording click out of the result, but otherwise record
    // every foreground app. A macro belongs to the user's input, not a window.
    shared.is_recording() && !is_own_process_foreground()
}

#[cfg(windows)]
fn record_keyboard_event(shared: &HookShared, vk: u32, is_down: bool) {
    if let Ok(mut recorder) = shared.recorder.lock() {
        if !recorder.active {
            return;
        }
        let already_pressed = recorder.pressed_keys.contains(&vk);
        if is_down == already_pressed {
            return;
        }
        // The click or Alt+Tab used to enter the target program can finish
        // only after that program becomes foreground. Do not store a lone key
        // release; it would make a recorded action impossible to replay.
        if !is_down && !already_pressed {
            return;
        }
        push_record_step(
            &mut recorder,
            MacroStep::Key {
                key: key_name_from_vk(vk),
                action: if is_down {
                    KeyAction::Down
                } else {
                    KeyAction::Up
                },
            },
        );
        if is_down {
            recorder.pressed_keys.insert(vk);
        } else {
            recorder.pressed_keys.remove(&vk);
        }
    }
}

#[cfg(windows)]
fn record_mouse_button(
    shared: &HookShared,
    button: MouseButton,
    action: KeyAction,
    x: i32,
    y: i32,
) {
    if let Ok(mut recorder) = shared.recorder.lock() {
        if !recorder.active {
            return;
        }
        // When the user clicks a target window to bring it forward, Windows
        // may report only the release after we lock onto that target. Ignore
        // this unmatched release so a normal click is always recorded as a
        // down + up pair.
        if matches!(action, KeyAction::Up) && !recorder.pressed_buttons.contains(&button) {
            return;
        }
        push_record_step(
            &mut recorder,
            MacroStep::MouseButton {
                button,
                action,
                x,
                y,
            },
        );
        if matches!(action, KeyAction::Down) {
            recorder.pressed_buttons.insert(button);
        } else {
            recorder.pressed_buttons.remove(&button);
        }
    }
}

#[cfg(windows)]
fn record_mouse_move(shared: &HookShared, x: i32, y: i32) {
    if let Ok(mut recorder) = shared.recorder.lock() {
        if !recorder.active {
            return;
        }
        let now = Instant::now();
        if let Some((previous_at, previous_x, previous_y)) = recorder.last_mouse_move {
            let elapsed = now.duration_since(previous_at).as_millis();
            let distance = (x - previous_x).abs().max((y - previous_y).abs());
            // Preserve the cursor path while avoiding thousands of visually
            // identical points from a high-polling-rate mouse.
            if elapsed < 16 && distance <= 3 {
                return;
            }
        }
        recorder.last_mouse_move = Some((now, x, y));
        push_record_step(&mut recorder, MacroStep::MouseMove { x, y });
    }
}

#[cfg(windows)]
fn record_mouse_wheel(shared: &HookShared, delta_x: i32, delta_y: i32) {
    if let Ok(mut recorder) = shared.recorder.lock() {
        if recorder.active {
            push_record_step(&mut recorder, MacroStep::Wheel { delta_x, delta_y });
        }
    }
}

#[cfg(windows)]
fn push_record_step(recorder: &mut RecorderState, step: MacroStep) {
    let now = Instant::now();
    if let Some(last_event) = recorder.last_event {
        let elapsed = now.duration_since(last_event).as_millis() as u64;
        if elapsed >= 8 {
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
fn process_macro_key_down(
    shared: &Arc<HookShared>,
    config: &AppConfig,
    _vk: u32,
    pressed: &HashSet<u32>,
) -> bool {
    if shared.is_recording() {
        return false;
    }
    for rule in config.macros.iter().filter(|rule| rule.enabled) {
        if shared
            .native_clicker_hotkey_registered
            .load(Ordering::SeqCst)
            && is_native_clicker_rule(rule)
        {
            continue;
        }
        let trigger_vks = rule
            .trigger_keys
            .iter()
            .filter_map(|key| key_to_vk(key))
            .collect::<Vec<_>>();
        if trigger_vks.is_empty()
            || trigger_vks.len() != rule.trigger_keys.len()
            || !trigger_vks.iter().all(|key| pressed.contains(key))
        {
            continue;
        }
        let signature = format!(
            "macro+{}",
            trigger_vks
                .iter()
                .map(|key| key.to_string())
                .collect::<Vec<_>>()
                .join("+")
        );
        let Ok(mut latched) = shared.latched_hotkeys.lock() else {
            continue;
        };
        if !latched.insert(signature) {
            continue;
        }
        drop(latched);
        if matches!(rule.mode, MacroMode::Toggle) && shared.is_playback_running() {
            shared.stop_playback();
        } else if let Err(error) = shared.start_playback(rule.clone()) {
            log::warn!("宏触发失败: {}", error.message);
        }
        return true;
    }
    false
}

#[cfg(windows)]
fn stop_hold_macros_on_key_up(shared: &HookShared, config: &AppConfig, vk: u32) {
    if config.macros.iter().any(|rule| {
        rule.enabled
            && matches!(rule.mode, MacroMode::Hold)
            && rule
                .trigger_keys
                .iter()
                .filter_map(|key| key_to_vk(key))
                .any(|key| key == vk)
    }) {
        shared.stop_playback();
    }
}

#[cfg(windows)]
static HOOK_SHARED: std::sync::OnceLock<Arc<HookShared>> = std::sync::OnceLock::new();

#[cfg(windows)]
const CLICKER_HOTKEY_ID: i32 = 0x4155;

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
fn process_native_clicker_hotkey(shared: &Arc<HookShared>) {
    if shared.is_recording() {
        return;
    }
    let Ok(config) = shared.config.lock().map(|config| config.clone()) else {
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
    if shared.is_playback_running() {
        shared.stop_playback();
    } else if let Err(error) = shared.start_playback(rule.clone()) {
        shared.set_playback_error(error.message.clone());
        log::warn!("连点器触发失败: {}", error.message);
    }
}

#[cfg(windows)]
fn key_to_vk(key: &str) -> Option<u32> {
    let normalized = key.trim().to_ascii_uppercase();
    let named = HashMap::from([
        ("CTRL", 0x11),
        ("CONTROL", 0x11),
        ("ALT", 0x12),
        ("SHIFT", 0x10),
        ("WIN", 0x5B),
        ("ESC", 0x1B),
        ("ENTER", 0x0D),
        ("SPACE", 0x20),
        ("TAB", 0x09),
        ("BACKSPACE", 0x08),
        ("CAPSLOCK", 0x14),
        ("F12", 0x7B),
        ("LEFT", 0x25),
        ("RIGHT", 0x27),
        ("UP", 0x26),
        ("DOWN", 0x28),
    ]);
    if let Some(value) = named.get(normalized.as_str()) {
        return Some(*value);
    }
    if normalized.len() == 1 {
        let byte = normalized.as_bytes()[0];
        if byte.is_ascii_uppercase() || byte.is_ascii_digit() {
            return Some(u32::from(byte));
        }
    }
    if let Some(number) = normalized
        .strip_prefix('F')
        .and_then(|number| number.parse::<u32>().ok())
    {
        if (1..=24).contains(&number) {
            return Some(0x70 + number - 1);
        }
    }
    None
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
fn play_macro_thread(
    shared: &HookShared,
    macro_rule: &MacroRule,
    stop: &Arc<AtomicBool>,
) -> Result<(), String> {
    let max_iterations = match macro_rule.mode {
        MacroMode::Once => Some(1),
        MacroMode::Repeat => Some(macro_rule.repeat_count.max(1)),
        MacroMode::Hold | MacroMode::Toggle => None,
    };
    let mut iterations = 0;
    let mut held_keys = HashSet::new();
    let mut held_buttons = HashSet::new();

    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if !play_macro_steps(
            shared,
            &macro_rule.steps,
            macro_rule.speed,
            stop,
            &mut held_keys,
            &mut held_buttons,
        )? {
            break;
        }
        iterations += 1;
        if max_iterations.is_some_and(|max| iterations >= max) {
            break;
        }
    }

    for vk in held_keys.drain() {
        let _ = send_key(vk, false);
    }
    for button in held_buttons.drain() {
        let _ = send_mouse_button(button, KeyAction::Up);
    }
    Ok(())
}

#[cfg(windows)]
fn play_macro_steps(
    shared: &HookShared,
    steps: &[MacroStep],
    speed: f32,
    stop: &Arc<AtomicBool>,
    held_keys: &mut HashSet<u32>,
    held_buttons: &mut HashSet<MouseButton>,
) -> Result<bool, String> {
    let speed = speed.max(0.05);
    for (index, step) in steps.iter().enumerate() {
        if stop.load(Ordering::SeqCst) {
            return Ok(false);
        }
        shared.set_playback_step(index + 1);
        match step {
            MacroStep::Delay {
                duration_ms,
                duration_max_ms,
            } => {
                let delay = randomized_delay_ms(*duration_ms, *duration_max_ms);
                if !sleep_interruptible(delay as f32 / speed, stop) {
                    return Ok(false);
                }
            }
            MacroStep::Key { key, action } => {
                let Some(vk) = key_to_vk(key) else {
                    return Err(format!("第 {} 步的按键“{}”暂不支持", index + 1, key));
                };
                send_key(vk, matches!(action, KeyAction::Down))?;
                if matches!(action, KeyAction::Down) {
                    held_keys.insert(vk);
                } else {
                    held_keys.remove(&vk);
                }
            }
            MacroStep::MouseButton {
                button,
                action,
                x,
                y,
            } => {
                if *x != 0 || *y != 0 {
                    send_mouse_move(*x, *y)?;
                }
                send_mouse_button(*button, *action)?;
                if matches!(action, KeyAction::Down) {
                    held_buttons.insert(*button);
                } else {
                    held_buttons.remove(button);
                }
            }
            MacroStep::MouseMove { x, y } => send_mouse_move(*x, *y)?,
            MacroStep::Wheel { delta_x, delta_y } => send_mouse_wheel(*delta_x, *delta_y)?,
            MacroStep::Text { text } => send_unicode_text(text)?,
        }
    }
    Ok(true)
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
fn sleep_interruptible(duration_ms: f32, stop: &Arc<AtomicBool>) -> bool {
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
        SendInput, INPUT, INPUT_0, INPUT_TYPE, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    };
    let flags = if key_down {
        Default::default()
    } else {
        KEYEVENTF_KEYUP
    };
    let input = INPUT {
        r#type: INPUT_TYPE(1),
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk as u16),
                wScan: 0,
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
fn send_backspaces(count: usize) {
    for _ in 0..count {
        let _ = send_key(0x08, true);
        let _ = send_key(0x08, false);
    }
}

#[cfg(windows)]
fn send_unicode_text(text: &str) -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_TYPE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
        VIRTUAL_KEY,
    };
    for unit in text.encode_utf16() {
        for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
            let input = INPUT {
                r#type: INPUT_TYPE(1),
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0),
                        wScan: unit,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
            if sent != 1 {
                return Err("Windows 拒绝了文本输入。若目标软件以管理员身份运行，请也以管理员身份启动 AutoFlow。".to_string());
            }
        }
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
fn launch_target(target: &str) {
    for command_line in target
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let parts = split_command_line(command_line);
        let Some((program, args)) = parts.split_first() else {
            continue;
        };
        if let Err(error) = Command::new(program).args(args).spawn() {
            log::error!("启动快捷动作失败: {command_line}: {error}");
        }
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
        canonical_virtual_key, is_keyboard_modifier, is_shift_key, is_text_modifier,
        randomized_delay_ms, shifted_printable_character, split_command_line,
    };
    use std::collections::HashSet;

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
    fn split_command_line_preserves_quoted_paths_and_arguments() {
        assert_eq!(
            split_command_line(r#""C:\Program Files\Demo\demo.exe" --open"#),
            vec![
                r#"C:\Program Files\Demo\demo.exe"#.to_string(),
                "--open".to_string(),
            ]
        );
    }
}
