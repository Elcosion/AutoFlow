//! Interpreter runs in a real subprocess; parent input is exclusively fake.
use autoflow_lib::runtime_executor::{run_parent, ExecutorBootstrap};
use autoflow_lib::runtime_protocol::{
    RunIdentity, ScriptStopMessage, ScriptStopMode, PROTOCOL_VERSION,
};
use autoflow_lib::{AutomationInput, AutomationProgram, KeyAction, MacroStep, MouseButton};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct FakeInput {
    log: Mutex<Vec<String>>,
    keys: Mutex<Vec<String>>,
    cancel_on_wait: Option<Arc<AtomicBool>>,
    fail_cleanup: bool,
}
impl FakeInput {
    fn record(&self, event: String) {
        self.log.lock().expect("log").push(event);
    }
}
impl AutomationInput for FakeInput {
    fn wait_ms(&self, ms: u64, _: f32, _: &AtomicBool) -> Result<(), String> {
        self.record(format!("wait:{ms}"));
        if let Some(cancel) = &self.cancel_on_wait {
            cancel.store(true, Ordering::Release);
            return Err("脚本已被 F12 停止".into());
        }
        Ok(())
    }
    fn wait_random_ms(
        &self,
        min: u64,
        _: u64,
        speed: f32,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        self.wait_ms(min, speed, cancel)
    }
    fn key_down(&self, key: &str) -> Result<(), String> {
        self.keys.lock().expect("keys").push(key.into());
        self.record(format!("down:{key}"));
        Ok(())
    }
    fn key_up(&self, key: &str) -> Result<(), String> {
        self.keys.lock().expect("keys").retain(|value| value != key);
        self.record(format!("up:{key}"));
        Ok(())
    }
    fn move_to(&self, x: i32, y: i32) -> Result<(), String> {
        self.record(format!("move:{x},{y}"));
        Ok(())
    }
    fn mouse_down(&self, button: &str, _: i32, _: i32) -> Result<(), String> {
        self.record(format!("button-down:{button}"));
        Ok(())
    }
    fn mouse_up(&self, button: &str, _: i32, _: i32) -> Result<(), String> {
        self.record(format!("button-up:{button}"));
        Ok(())
    }
    fn scroll(&self, x: i32, y: i32) -> Result<(), String> {
        self.record(format!("scroll:{x},{y}"));
        Ok(())
    }
    fn type_text(&self, text: &str) -> Result<(), String> {
        self.record(format!("text:{text}"));
        Ok(())
    }
    fn bio_click(
        &self,
        button: &str,
        x: i32,
        y: i32,
        _: Option<f32>,
        _: &AtomicBool,
    ) -> Result<(), String> {
        self.record(format!("bio-click:{button}:{x},{y}"));
        Ok(())
    }
    fn cleanup_injected_input(&self) -> Result<(), String> {
        if self.fail_cleanup {
            self.record("cleanup-failed".into());
            return Err("simulated cleanup failure".into());
        }
        self.keys.lock().expect("keys").clear();
        self.record("cleanup".into());
        Ok(())
    }
}
fn bootstrap(program: AutomationProgram, bio: bool) -> ExecutorBootstrap {
    bootstrap_with_initial_held_buttons(program, bio, vec![])
}
fn bootstrap_with_initial_held_buttons(
    program: AutomationProgram,
    bio: bool,
    initial_held_buttons: Vec<MouseButton>,
) -> ExecutorBootstrap {
    ExecutorBootstrap {
        version: PROTOCOL_VERSION,
        identity: RunIdentity {
            session: "input-free-executor-test".into(),
            run: 1,
            generation: 0,
        },
        secret: "f".repeat(64),
        program,
        speed: 1.0,
        behavior_enabled: bio,
        initial_held_buttons,
        image_root: std::env::temp_dir().join("autoflow-executor-tests-images"),
        assets: vec![],
    }
}
fn run(
    program: AutomationProgram,
    input: Arc<FakeInput>,
    bio: bool,
    cancel: &AtomicBool,
    revoked: &AtomicBool,
    progress: &dyn Fn(usize, String),
) -> Result<Option<ScriptStopMessage>, String> {
    run_parent(
        std::path::Path::new(env!("CARGO_BIN_EXE_runtime_protocol_probe")),
        bootstrap(program, bio),
        input,
        cancel,
        progress,
        &|| revoked.store(true, Ordering::Release),
    )
}
fn run_with_initial_held_buttons(
    program: AutomationProgram,
    input: Arc<FakeInput>,
    bio: bool,
    initial_held_buttons: Vec<MouseButton>,
    cancel: &AtomicBool,
    revoked: &AtomicBool,
    progress: &dyn Fn(usize, String),
) -> Result<Option<ScriptStopMessage>, String> {
    run_parent(
        std::path::Path::new(env!("CARGO_BIN_EXE_runtime_protocol_probe")),
        bootstrap_with_initial_held_buttons(program, bio, initial_held_buttons),
        input,
        cancel,
        progress,
        &|| revoked.store(true, Ordering::Release),
    )
}
#[test]
fn graph_interpreter_preserves_order_and_combines_bio_click_with_original_progress() {
    let input = Arc::new(FakeInput::default());
    let progress = Mutex::new(Vec::new());
    let program = AutomationProgram::Macro {
        steps: vec![
            MacroStep::MouseMove { x: -100, y: 30 },
            MacroStep::MouseButton {
                button: MouseButton::Left,
                action: KeyAction::Down,
                x: -100,
                y: 30,
            },
            MacroStep::MouseButton {
                button: MouseButton::Left,
                action: KeyAction::Up,
                x: -100,
                y: 30,
            },
            MacroStep::Delay {
                duration_ms: 17,
                duration_max_ms: None,
            },
            MacroStep::Text {
                text: "example".into(),
            },
        ],
    };
    let revoked = AtomicBool::new(false);
    assert_eq!(
        run(
            program,
            input.clone(),
            true,
            &AtomicBool::new(false),
            &revoked,
            &|step, _| progress.lock().expect("progress").push(step)
        )
        .expect("graph"),
        None
    );
    assert_eq!(
        *input.log.lock().expect("log"),
        ["bio-click:left:-100,30", "wait:17", "text:example"]
    );
    assert_eq!(*progress.lock().expect("progress"), [1, 3, 4, 5]);
    assert!(!revoked.load(Ordering::Acquire));
}
#[test]
fn graph_interpreter_does_not_combine_click_separated_from_move_by_delay() {
    let input = Arc::new(FakeInput::default());
    let progress = Mutex::new(Vec::new());
    let program = AutomationProgram::Macro {
        steps: vec![
            MacroStep::MouseMove { x: 10, y: 20 },
            MacroStep::Delay {
                duration_ms: 17,
                duration_max_ms: None,
            },
            MacroStep::MouseButton {
                button: MouseButton::Left,
                action: KeyAction::Down,
                x: 10,
                y: 20,
            },
            MacroStep::MouseButton {
                button: MouseButton::Left,
                action: KeyAction::Up,
                x: 10,
                y: 20,
            },
        ],
    };

    assert_eq!(
        run(
            program,
            input.clone(),
            true,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            &|step, action| progress.lock().expect("progress").push((step, action)),
        )
        .expect("graph"),
        None
    );
    let log = input.log.lock().expect("log");
    assert_eq!(
        *log,
        [
            "move:10,20",
            "wait:17",
            "button-down:left",
            "button-up:left",
        ]
    );
    assert!(!log.iter().any(|event| event.starts_with("bio-click:")));
    let progress = progress.lock().expect("progress");
    assert_eq!(
        *progress,
        [
            (1, "graph_step".into()),
            (2, "graph_step".into()),
            (3, "graph_step".into()),
            (4, "graph_step".into()),
        ]
    );
    assert!(progress
        .iter()
        .all(|(_, action)| action != "combined_click"));
}
#[test]
fn graph_interpreter_does_not_combine_drag_as_click() {
    let input = Arc::new(FakeInput::default());
    let progress = Mutex::new(Vec::new());
    let program = AutomationProgram::Macro {
        steps: vec![
            MacroStep::MouseMove { x: 10, y: 20 },
            MacroStep::MouseButton {
                button: MouseButton::Left,
                action: KeyAction::Down,
                x: 10,
                y: 20,
            },
            MacroStep::MouseMove { x: 30, y: 40 },
            MacroStep::MouseButton {
                button: MouseButton::Left,
                action: KeyAction::Up,
                x: 30,
                y: 40,
            },
        ],
    };

    assert_eq!(
        run(
            program,
            input.clone(),
            true,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            &|step, action| progress.lock().expect("progress").push((step, action)),
        )
        .expect("graph"),
        None
    );
    let log = input.log.lock().expect("log");
    assert_eq!(
        *log,
        [
            "move:10,20",
            "button-down:left",
            "move:30,40",
            "button-up:left",
        ]
    );
    assert!(!log.iter().any(|event| event.starts_with("bio-click:")));
    let progress = progress.lock().expect("progress");
    assert_eq!(
        *progress,
        [
            (1, "graph_step".into()),
            (2, "graph_step".into()),
            (3, "graph_step".into()),
            (4, "graph_step".into()),
        ]
    );
    assert!(progress
        .iter()
        .all(|(_, action)| action != "combined_click"));
}
#[test]
fn graph_interpreter_does_not_combine_click_when_button_is_initially_held() {
    let input = Arc::new(FakeInput::default());
    let progress = Mutex::new(Vec::new());
    let program = AutomationProgram::Macro {
        steps: vec![
            MacroStep::MouseMove { x: 10, y: 20 },
            MacroStep::MouseButton {
                button: MouseButton::Left,
                action: KeyAction::Down,
                x: 10,
                y: 20,
            },
            MacroStep::MouseButton {
                button: MouseButton::Left,
                action: KeyAction::Up,
                x: 10,
                y: 20,
            },
        ],
    };

    assert_eq!(
        run_with_initial_held_buttons(
            program,
            input.clone(),
            true,
            vec![MouseButton::Left],
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            &|step, action| progress.lock().expect("progress").push((step, action)),
        )
        .expect("graph"),
        None
    );
    let log = input.log.lock().expect("log");
    assert_eq!(*log, ["move:10,20", "button-down:left", "button-up:left"]);
    assert!(!log.iter().any(|event| event.starts_with("bio-click:")));
    let progress = progress.lock().expect("progress");
    assert_eq!(
        *progress,
        [
            (1, "graph_step".into()),
            (2, "graph_step".into()),
            (3, "graph_step".into()),
        ]
    );
    assert!(progress
        .iter()
        .all(|(_, action)| action != "combined_click"));
}
#[test]
fn rhai_stop_message_returns_only_after_release_without_executing_next_statement() {
    let input = Arc::new(FakeInput::default());
    let program = AutomationProgram::Rhai {
        api_version: 1,
        source: "key_down(\"A\"); stop_with_message(\"complete\"); move_to(999,999);".into(),
    };
    let result = run(
        program,
        input.clone(),
        false,
        &AtomicBool::new(false),
        &AtomicBool::new(false),
        &|_, _| {},
    )
    .expect("user stop");
    assert_eq!(
        result,
        Some(ScriptStopMessage {
            title: "AutoFlow".into(),
            message: "complete".into(),
            mode: ScriptStopMode::Background,
        })
    );
    assert!(input.keys.lock().expect("keys").is_empty());
    assert_eq!(
        *input.log.lock().expect("log"),
        ["down:A", "up:A", "cleanup"]
    );
}

#[test]
fn rhai_stop_message_foreground_mode_round_trips_through_subprocess() {
    let input = Arc::new(FakeInput::default());
    let result = run(
        AutomationProgram::Rhai {
            api_version: 1,
            source: r#"stop_with_message("complete", #{ mode: "foreground" }); move_to(9,9);"#
                .into(),
        },
        input.clone(),
        false,
        &AtomicBool::new(false),
        &AtomicBool::new(false),
        &|_, _| {},
    )
    .expect("foreground stop");
    assert_eq!(
        result,
        Some(ScriptStopMessage {
            title: "AutoFlow".into(),
            message: "complete".into(),
            mode: ScriptStopMode::Foreground,
        })
    );
    assert_eq!(*input.log.lock().expect("log"), ["cleanup"]);
}

#[test]
fn rhai_stop_message_is_not_returned_when_cleanup_fails() {
    let input = Arc::new(FakeInput {
        fail_cleanup: true,
        ..Default::default()
    });
    let error = run(
        AutomationProgram::Rhai {
            api_version: 1,
            source: r#"stop_with_message("complete", #{ mode: "foreground" });"#.into(),
        },
        input.clone(),
        false,
        &AtomicBool::new(false),
        &AtomicBool::new(false),
        &|_, _| {},
    )
    .expect_err("unsafe cleanup must suppress the successful stop result");
    assert!(error.contains("清理失败"), "{error}");
    assert_eq!(*input.log.lock().expect("log"), ["cleanup-failed"]);
}

#[test]
fn executor_rejects_mixed_protocol_versions() {
    let input = Arc::new(FakeInput::default());
    let mut incompatible = bootstrap(
        AutomationProgram::Rhai {
            api_version: 1,
            source: r#"stop_with_message("never");"#.into(),
        },
        false,
    );
    incompatible.version = PROTOCOL_VERSION - 1;
    let revoked = AtomicBool::new(false);
    let error = run_parent(
        std::path::Path::new(env!("CARGO_BIN_EXE_runtime_protocol_probe")),
        incompatible,
        input.clone(),
        &AtomicBool::new(false),
        &|_, _| {},
        &|| revoked.store(true, Ordering::Release),
    )
    .expect_err("mixed protocol versions must fail closed");
    assert!(error.contains("失联") || error.contains("结束"), "{error}");
    assert!(revoked.load(Ordering::Acquire));
    assert!(input.log.lock().expect("log").is_empty());
}
#[test]
fn cancel_in_a_ten_thousand_step_graph_drops_all_late_input_and_requires_parent_cleanup() {
    let cancel = Arc::new(AtomicBool::new(false));
    let input = Arc::new(FakeInput {
        cancel_on_wait: Some(cancel.clone()),
        ..Default::default()
    });
    let mut steps = vec![
        MacroStep::Key {
            key: "A".into(),
            action: KeyAction::Down,
        },
        MacroStep::Delay {
            duration_ms: 1,
            duration_max_ms: None,
        },
    ];
    steps.extend((0..10_000).map(|x| MacroStep::MouseMove { x, y: 100 }));
    let revoked = AtomicBool::new(false);
    let error = run(
        AutomationProgram::Macro { steps },
        input.clone(),
        false,
        &cancel,
        &revoked,
        &|_, _| {},
    )
    .expect_err("cancelled");
    assert!(error.contains("F12"));
    assert!(revoked.load(Ordering::Acquire));
    assert_eq!(*input.log.lock().expect("log"), ["down:A", "wait:1"]);
    assert_eq!(
        input.keys.lock().expect("keys").len(),
        1,
        "killing an executor must not claim input was released"
    );
    input.cleanup_injected_input().expect("parent cleanup");
    assert!(input.keys.lock().expect("keys").is_empty());
}
