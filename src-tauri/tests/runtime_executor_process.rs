//! Interpreter runs in a real subprocess; parent input is exclusively fake.
use autoflow_lib::runtime_executor::{run_parent, ExecutorBootstrap};
use autoflow_lib::runtime_protocol::{RunIdentity, PROTOCOL_VERSION};
use autoflow_lib::{AutomationInput, AutomationProgram, KeyAction, MacroStep, MouseButton};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct FakeInput {
    log: Mutex<Vec<String>>,
    keys: Mutex<Vec<String>>,
    cancel_on_wait: Option<Arc<AtomicBool>>,
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
        self.keys.lock().expect("keys").clear();
        self.record("cleanup".into());
        Ok(())
    }
}
fn bootstrap(program: AutomationProgram, bio: bool) -> ExecutorBootstrap {
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
        initial_held_buttons: vec![],
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
) -> Result<Option<(String, String)>, String> {
    run_parent(
        std::path::Path::new(env!("CARGO_BIN_EXE_runtime_protocol_probe")),
        bootstrap(program, bio),
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
fn rhai_stop_message_returns_only_after_release_without_executing_next_statement() {
    let input = Arc::new(FakeInput::default());
    let program = AutomationProgram::Rhai {
        api_version: 1,
        source: "key_down(\"A\"); stop_with_message(\"done\", \"complete\"); move_to(999,999);"
            .into(),
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
    assert_eq!(result, Some(("done".into(), "complete".into())));
    assert!(input.keys.lock().expect("keys").is_empty());
    assert_eq!(
        *input.log.lock().expect("log"),
        ["down:A", "up:A", "cleanup"]
    );
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
