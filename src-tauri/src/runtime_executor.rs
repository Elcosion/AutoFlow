//! Isolated interpreter and private-pipe input proxy. The worker has no
//! Windows input implementation; only its parent can authorize/send input.
use crate::rhai_runtime::{run_rhai_script, AutomationInput, ExecutionContext, ScriptOutcome};
use crate::runtime_protocol::{read_frame, write_frame, RunIdentity, PROTOCOL_VERSION};
use crate::{AutomationProgram, KeyAction, MacroStep, MouseButton};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Serialize, Deserialize)]
pub struct ExecutorBootstrap {
    pub version: u32,
    pub identity: RunIdentity,
    pub secret: String,
    pub program: AutomationProgram,
    pub speed: f32,
    pub behavior_enabled: bool,
    pub initial_held_buttons: Vec<MouseButton>,
    pub image_root: PathBuf,
    pub assets: Vec<crate::AutomationAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputRequest {
    Wait {
        minimum: u64,
        maximum: u64,
        speed: f32,
    },
    Key {
        key: String,
        down: bool,
        force: bool,
    },
    Move {
        x: i32,
        y: i32,
        bio: bool,
        target_width: Option<f32>,
        click_intent: bool,
    },
    Button {
        button: String,
        down: bool,
        x: i32,
        y: i32,
        force: bool,
    },
    Click {
        button: String,
        x: i32,
        y: i32,
        bio: bool,
        target_width: Option<f32>,
    },
    Scroll {
        x: i32,
        y: i32,
    },
    Text {
        text: String,
    },
    Cleanup,
    Progress {
        step: usize,
        action: String,
    },
}

#[derive(Serialize, Deserialize)]
pub struct ExecutorRequest {
    pub version: u32,
    pub identity: RunIdentity,
    pub secret: String,
    pub sequence: u64,
    pub request: InputRequest,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerMessage {
    Request(ExecutorRequest),
    Finished {
        version: u32,
        secret: String,
        sequence: u64,
        identity: RunIdentity,
        error: Option<String>,
        stop_message: Option<(String, String)>,
    },
}

#[derive(Serialize, Deserialize)]
pub struct InputResponse {
    pub sequence: u64,
    pub error: Option<String>,
}

#[cfg(windows)]
pub(crate) fn session_secret() -> Result<String, String> {
    use windows::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };
    let mut bytes = [0u8; 32];
    let result = unsafe { BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if result.0 < 0 {
        return Err(format!("无法生成执行器会话凭据: {:#x}", result.0));
    }
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn validate_request(request: &InputRequest) -> bool {
    let position = |x: i32, y: i32| x.unsigned_abs() <= 100_000 && y.unsigned_abs() <= 100_000;
    let button = |name: &str| matches!(name, "left" | "right" | "middle" | "x1" | "x2");
    let width =
        |value: &Option<f32>| value.is_none_or(|v| v.is_finite() && v > 0.0 && v <= 100_000.0);
    match request {
        InputRequest::Wait {
            minimum,
            maximum,
            speed,
        } => {
            minimum <= maximum
                && *maximum <= crate::automation::MAX_WAIT_MS
                && speed.is_finite()
                && (0.05..=20.0).contains(speed)
        }
        InputRequest::Key { key, down, force } => {
            !key.is_empty() && key.len() <= 32 && !key.contains('\0') && !(*down && *force)
        }
        InputRequest::Move {
            x, y, target_width, ..
        } => position(*x, *y) && width(target_width),
        InputRequest::Button {
            button: name,
            x,
            y,
            down,
            force,
        } => button(name) && position(*x, *y) && !(*down && *force),
        InputRequest::Click {
            button: name,
            x,
            y,
            target_width,
            ..
        } => button(name) && position(*x, *y) && width(target_width),
        InputRequest::Scroll { x, y } => x.unsigned_abs() <= 12_000 && y.unsigned_abs() <= 12_000,
        InputRequest::Text { text } => text.len() <= 16_384 && !text.contains('\0'),
        InputRequest::Cleanup => true,
        InputRequest::Progress { step, action } => *step <= 100_000 && action.len() <= 128,
    }
}

fn dispatch_request(
    input: &dyn AutomationInput,
    request: InputRequest,
    cancel: &AtomicBool,
    progress: &dyn Fn(usize, String),
) -> Result<(), String> {
    match request {
        InputRequest::Wait {
            minimum,
            maximum,
            speed,
        } => input.wait_random_ms(minimum, maximum, speed, cancel),
        InputRequest::Key { key, down, force } => {
            if down {
                input.key_down(&key)
            } else if force {
                input.force_key_up(&key)
            } else {
                input.key_up(&key)
            }
        }
        InputRequest::Move {
            x,
            y,
            bio,
            target_width,
            click_intent,
        } => {
            if bio {
                input.bio_move_to(x, y, target_width, click_intent, cancel)
            } else {
                input.move_to(x, y)
            }
        }
        InputRequest::Button {
            button,
            down,
            x,
            y,
            force,
        } => {
            if down {
                input.mouse_down(&button, x, y)
            } else if force {
                input.force_mouse_up(&button)
            } else {
                input.mouse_up(&button, x, y)
            }
        }
        InputRequest::Click {
            button,
            x,
            y,
            bio,
            target_width,
        } => {
            if bio {
                input.bio_click(&button, x, y, target_width, cancel)
            } else {
                input.click(&button, x, y)
            }
        }
        InputRequest::Scroll { x, y } => input.scroll(x, y),
        InputRequest::Text { text } => input.type_text(&text),
        InputRequest::Cleanup => input.cleanup_injected_input(),
        InputRequest::Progress { step, action } => {
            progress(step, action);
            Ok(())
        }
    }
}

enum ParentFrame {
    Bootstrap(ExecutorBootstrap),
    Response(InputResponse),
}

/// Run one interpreter iteration. Private pipes have one outstanding request;
/// pipe writes happen on a bounded worker, never on the cancellation/control
/// thread. The supplied input implementation remains the safety authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutorStopCause {
    ExecutionFailed,
    ContainmentUnconfirmed,
}

pub fn run_parent(
    executable: &std::path::Path,
    bootstrap: ExecutorBootstrap,
    input: Arc<dyn AutomationInput>,
    cancel: &AtomicBool,
    progress: &dyn Fn(usize, String),
    revoke: &dyn Fn(),
) -> Result<Option<(String, String)>, String> {
    run_parent_supervised(executable, bootstrap, input, cancel, progress, &|_| {
        revoke()
    })
}

pub fn run_parent_supervised(
    executable: &std::path::Path,
    bootstrap: ExecutorBootstrap,
    input: Arc<dyn AutomationInput>,
    cancel: &AtomicBool,
    progress: &dyn Fn(usize, String),
    revoke: &dyn Fn(ExecutorStopCause),
) -> Result<Option<(String, String)>, String> {
    use std::sync::mpsc::{sync_channel, RecvTimeoutError};
    use std::time::{Duration, Instant};
    let mut command = std::process::Command::new(executable);
    command.arg("--runtime-executor");
    let mut worker = crate::runtime_process::SupervisedWorker::spawn(&mut command)
        .map_err(|error| format!("执行器启动失败: {error}"))?;
    let mut stdout = worker.take_stdout().ok_or("执行器输出管道不可用")?;
    let mut stdin = worker.take_stdin().ok_or("执行器输入管道不可用")?;
    let (incoming_tx, incoming_rx) = sync_channel(1);
    let reader = std::thread::Builder::new()
        .name("autoflow-executor-pipe-reader".into())
        .spawn(move || loop {
            let message = read_frame::<WorkerMessage>(&mut stdout);
            let failed = message.is_err();
            if incoming_tx.send(message).is_err() || failed {
                break;
            }
        })
        .map_err(|error| format!("执行器读取线程启动失败: {error}"))?;
    let (outgoing_tx, outgoing_rx) = sync_channel::<ParentFrame>(1);
    let (written_tx, written_rx) = sync_channel(1);
    let writer = std::thread::Builder::new()
        .name("autoflow-executor-pipe-writer".into())
        .spawn(move || {
            while let Ok(frame) = outgoing_rx.recv() {
                let result = match frame {
                    ParentFrame::Bootstrap(value) => write_frame(&mut stdin, &value),
                    ParentFrame::Response(value) => write_frame(&mut stdin, &value),
                };
                let failed = result.is_err();
                if written_tx.send(result).is_err() || failed {
                    break;
                }
            }
        })
        .map_err(|error| format!("执行器写入线程启动失败: {error}"))?;
    let mut authority = crate::runtime_protocol::ActionReceiver::new(
        bootstrap.identity.clone(),
        bootstrap.secret.clone(),
    )
    .map_err(|_| "执行器会话身份无效".to_string())?;
    let await_write = || -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(crate::rhai_runtime::CANCELLED.into());
            }
            match written_rx.recv_timeout(Duration::from_millis(2)) {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => {
                    return Err("执行器写入失联".into())
                }
                Err(RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(RecvTimeoutError::Timeout) => return Err("执行器写入超过500ms期限".into()),
            }
        }
    };
    let result = (|| {
        outgoing_tx
            .try_send(ParentFrame::Bootstrap(bootstrap.clone()))
            .map_err(|_| "执行器初始化队列不可用")?;
        await_write()?;
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(crate::rhai_runtime::CANCELLED.into());
            }
            let message = match incoming_rx.recv_timeout(Duration::from_millis(2)) {
                Ok(Ok(message)) => message,
                Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => {
                    return Err("执行器意外失联，禁止续播".into())
                }
                Err(RecvTimeoutError::Timeout) => continue,
            };
            match message {
                WorkerMessage::Request(envelope) => {
                    if !validate_request(&envelope.request) {
                        return Err("执行器提交了非法动作".into());
                    }
                    let sequence = envelope.sequence;
                    authority
                        .accept(crate::runtime_protocol::ActionEnvelope {
                            version: envelope.version,
                            secret: envelope.secret,
                            identity: envelope.identity,
                            sequence,
                            action: crate::runtime_protocol::ExecutorAction::Move { x: 0, y: 0 },
                        })
                        .map_err(|error| format!("执行器身份或动作顺序无效: {error:?}"))?;
                    let error =
                        dispatch_request(input.as_ref(), envelope.request, cancel, progress).err();
                    outgoing_tx
                        .try_send(ParentFrame::Response(InputResponse { sequence, error }))
                        .map_err(|_| "执行器响应队列不可用")?;
                    await_write()?;
                }
                WorkerMessage::Finished {
                    version,
                    secret,
                    sequence,
                    identity,
                    error,
                    stop_message,
                } => {
                    authority
                        .accept(crate::runtime_protocol::ActionEnvelope {
                            version,
                            secret,
                            identity,
                            sequence,
                            action: crate::runtime_protocol::ExecutorAction::Move { x: 0, y: 0 },
                        })
                        .map_err(|error| format!("执行器结束身份或顺序无效: {error:?}"))?;
                    if let Some(error) = error {
                        return Err(error);
                    }
                    return Ok(stop_message);
                }
            }
        }
    })();
    authority.revoke();
    if result.is_err() {
        revoke(ExecutorStopCause::ExecutionFailed);
    }
    drop(incoming_rx);
    drop(outgoing_tx);
    drop(written_rx);
    let exit = worker.stop(|| {}, Duration::from_millis(20));
    drop(worker); // also terminates inherited pipe holders in the worker job
    let thread_deadline = Instant::now() + Duration::from_millis(200);
    while (!reader.is_finished() || !writer.is_finished()) && Instant::now() < thread_deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    if !reader.is_finished() || !writer.is_finished() {
        revoke(ExecutorStopCause::ContainmentUnconfirmed);
        return Err("执行器通信线程停止未确认，禁止重新启动".into());
    }
    let _ = reader.join();
    let _ = writer.join();
    if let Some(error) = exit.error {
        revoke(ExecutorStopCause::ContainmentUnconfirmed);
        return Err(format!("执行器退出未确认: {error}"));
    }
    result
}

struct PipeProxy {
    bootstrap: ExecutorBootstrap,
    sequence: Mutex<u64>,
    cancelled: Arc<AtomicBool>,
}

impl PipeProxy {
    fn call(&self, request: InputRequest) -> Result<(), String> {
        let mut sequence = self
            .sequence
            .lock()
            .map_err(|_| "执行器通信状态异常".to_string())?;
        *sequence = sequence.checked_add(1).ok_or("执行器动作序号超限")?;
        let envelope = ExecutorRequest {
            version: PROTOCOL_VERSION,
            identity: self.bootstrap.identity.clone(),
            secret: self.bootstrap.secret.clone(),
            sequence: *sequence,
            request,
        };
        write_frame(
            &mut std::io::stdout().lock(),
            &WorkerMessage::Request(envelope),
        )
        .map_err(|_| "执行器与安全端失联".to_string())?;
        let response: InputResponse = read_frame(&mut std::io::stdin().lock())
            .map_err(|_| "执行器与安全端失联".to_string())?;
        if response.sequence != *sequence {
            self.cancelled.store(true, Ordering::Release);
            return Err("执行器响应序号不匹配".into());
        }
        if let Some(error) = response.error {
            if error == crate::rhai_runtime::CANCELLED || error.contains("输入许可已撤销") {
                self.cancelled.store(true, Ordering::Release);
            }
            Err(error)
        } else {
            Ok(())
        }
    }
}

impl AutomationInput for PipeProxy {
    fn wait_ms(&self, milliseconds: u64, speed: f32, _: &AtomicBool) -> Result<(), String> {
        self.call(InputRequest::Wait {
            minimum: milliseconds,
            maximum: milliseconds,
            speed,
        })
    }
    fn wait_random_ms(
        &self,
        minimum: u64,
        maximum: u64,
        speed: f32,
        _: &AtomicBool,
    ) -> Result<(), String> {
        self.call(InputRequest::Wait {
            minimum,
            maximum,
            speed,
        })
    }
    fn key_down(&self, key: &str) -> Result<(), String> {
        self.call(InputRequest::Key {
            key: key.into(),
            down: true,
            force: false,
        })
    }
    fn key_up(&self, key: &str) -> Result<(), String> {
        self.call(InputRequest::Key {
            key: key.into(),
            down: false,
            force: false,
        })
    }
    fn force_key_up(&self, key: &str) -> Result<(), String> {
        self.call(InputRequest::Key {
            key: key.into(),
            down: false,
            force: true,
        })
    }
    fn move_to(&self, x: i32, y: i32) -> Result<(), String> {
        self.call(InputRequest::Move {
            x,
            y,
            bio: false,
            target_width: None,
            click_intent: false,
        })
    }
    fn mouse_down(&self, button: &str, x: i32, y: i32) -> Result<(), String> {
        self.call(InputRequest::Button {
            button: button.into(),
            down: true,
            x,
            y,
            force: false,
        })
    }
    fn mouse_up(&self, button: &str, x: i32, y: i32) -> Result<(), String> {
        self.call(InputRequest::Button {
            button: button.into(),
            down: false,
            x,
            y,
            force: false,
        })
    }
    fn force_mouse_up(&self, button: &str) -> Result<(), String> {
        self.call(InputRequest::Button {
            button: button.into(),
            down: false,
            x: 0,
            y: 0,
            force: true,
        })
    }
    fn cleanup_injected_input(&self) -> Result<(), String> {
        self.call(InputRequest::Cleanup)
    }
    fn click(&self, button: &str, x: i32, y: i32) -> Result<(), String> {
        self.call(InputRequest::Click {
            button: button.into(),
            x,
            y,
            bio: false,
            target_width: None,
        })
    }
    fn bio_move_to(
        &self,
        x: i32,
        y: i32,
        target_width: Option<f32>,
        followed_by_click: bool,
        _: &AtomicBool,
    ) -> Result<(), String> {
        self.call(InputRequest::Move {
            x,
            y,
            bio: true,
            target_width,
            click_intent: followed_by_click,
        })
    }
    fn bio_click(
        &self,
        button: &str,
        x: i32,
        y: i32,
        target_width: Option<f32>,
        _: &AtomicBool,
    ) -> Result<(), String> {
        self.call(InputRequest::Click {
            button: button.into(),
            x,
            y,
            bio: true,
            target_width,
        })
    }
    fn scroll(&self, x: i32, y: i32) -> Result<(), String> {
        self.call(InputRequest::Scroll { x, y })
    }
    fn type_text(&self, text: &str) -> Result<(), String> {
        self.call(InputRequest::Text { text: text.into() })
    }
}

fn button_name(button: MouseButton) -> &'static str {
    match button {
        MouseButton::Left => "left",
        MouseButton::Right => "right",
        MouseButton::Middle => "middle",
        MouseButton::X1 => "x1",
        MouseButton::X2 => "x2",
    }
}

fn execute_graph(proxy: &PipeProxy, steps: &[MacroStep]) -> Result<(), String> {
    let mut held_buttons: std::collections::HashSet<_> = proxy
        .bootstrap
        .initial_held_buttons
        .iter()
        .copied()
        .collect();
    let mut index = 0;
    while index < steps.len() {
        proxy.call(InputRequest::Progress {
            step: index + 1,
            action: "graph_step".into(),
        })?;
        if proxy.bootstrap.behavior_enabled {
            if let (
                Some(MacroStep::MouseMove { x, y }),
                Some(MacroStep::MouseButton {
                    button,
                    action: KeyAction::Down,
                    x: dx,
                    y: dy,
                }),
                Some(MacroStep::MouseButton {
                    button: up_button,
                    action: KeyAction::Up,
                    x: ux,
                    y: uy,
                }),
            ) = (steps.get(index), steps.get(index + 1), steps.get(index + 2))
            {
                if button == up_button
                    && (x, y) == (dx, dy)
                    && (x, y) == (ux, uy)
                    && !held_buttons.contains(button)
                {
                    proxy.bio_click(button_name(*button), *x, *y, None, &proxy.cancelled)?;
                    proxy.call(InputRequest::Progress {
                        step: index + 3,
                        action: "combined_click".into(),
                    })?;
                    index += 3;
                    continue;
                }
            }
        }
        match &steps[index] {
            MacroStep::Delay {
                duration_ms,
                duration_max_ms,
            } => proxy.wait_random_ms(
                *duration_ms,
                duration_max_ms.unwrap_or(*duration_ms),
                proxy.bootstrap.speed,
                &proxy.cancelled,
            )?,
            MacroStep::Key { key, action } => {
                if *action == KeyAction::Down {
                    proxy.key_down(key)?
                } else {
                    proxy.key_up(key)?
                }
            }
            MacroStep::MouseMove { x, y } => {
                let intent = matches!(
                    steps.get(index + 1),
                    Some(MacroStep::MouseButton {
                        action: KeyAction::Down,
                        ..
                    })
                );
                if proxy.bootstrap.behavior_enabled {
                    proxy.bio_move_to(*x, *y, None, intent, &proxy.cancelled)?;
                } else {
                    proxy.move_to(*x, *y)?;
                }
            }
            MacroStep::MouseButton {
                button,
                action,
                x,
                y,
            } => {
                let already_at = index > 0
                    && matches!(&steps[index - 1], MacroStep::MouseMove { x: px, y: py } if (x,y) == (px,py));
                let (x, y) = if already_at { (0, 0) } else { (*x, *y) };
                if *action == KeyAction::Down {
                    proxy.mouse_down(button_name(*button), x, y)?;
                    held_buttons.insert(*button);
                } else {
                    proxy.mouse_up(button_name(*button), x, y)?;
                    held_buttons.remove(button);
                }
            }
            MacroStep::Wheel { delta_x, delta_y } => proxy.scroll(*delta_x, *delta_y)?,
            MacroStep::Text { text } => proxy.type_text(text)?,
        }
        index += 1;
    }
    Ok(())
}

/// Dispatch before the Tauri UI starts. There is no hook/input service here.
pub fn worker_main() {
    let bootstrap: ExecutorBootstrap = match read_frame(&mut std::io::stdin().lock()) {
        Ok(value) => value,
        Err(_) => return,
    };
    if bootstrap.version != PROTOCOL_VERSION
        || bootstrap.secret.len() != 64
        || bootstrap.identity.run == 0
        || !bootstrap.speed.is_finite()
        || !(0.05..=20.0).contains(&bootstrap.speed)
    {
        return;
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let proxy = Arc::new(PipeProxy {
        bootstrap: bootstrap.clone(),
        sequence: Mutex::new(0),
        cancelled: cancelled.clone(),
    });
    let result = match &bootstrap.program {
        AutomationProgram::Macro { steps } => {
            execute_graph(&proxy, steps).map(|()| ScriptOutcome::Completed)
        }
        AutomationProgram::Rhai { source, .. } => {
            let vision = crate::automation::VisionService::new(bootstrap.image_root.clone());
            vision.set_assets(&bootstrap.assets);
            let progress_proxy = proxy.clone();
            let progress = Arc::new(move |step, action| {
                let _ = progress_proxy.call(InputRequest::Progress { step, action });
            });
            let context = ExecutionContext::new_with_vision_and_action_progress(
                proxy.clone(),
                cancelled,
                bootstrap.speed,
                None,
                Some(progress),
                vision,
            );
            run_rhai_script(source, context)
        }
    };
    let (error, stop_message) = match result {
        Ok(ScriptOutcome::Completed) => (None, None),
        Ok(ScriptOutcome::StoppedWithMessage { title, message }) => (None, Some((title, message))),
        Err(error) => (Some(error), None),
    };
    let _ = write_frame(
        &mut std::io::stdout().lock(),
        &WorkerMessage::Finished {
            version: PROTOCOL_VERSION,
            secret: bootstrap.secret,
            sequence: proxy
                .sequence
                .lock()
                .map(|sequence| sequence.saturating_add(1))
                .unwrap_or(0),
            identity: bootstrap.identity,
            error,
            stop_message,
        },
    );
}
