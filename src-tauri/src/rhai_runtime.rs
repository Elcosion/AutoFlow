#![allow(dead_code)]

use crate::automation::{
    MatcherMode, MatcherOptions, Point, RgbColor, ScreenRect, VisionApi, VisionError,
    VisionPollBudget, VisionPollOptions, VisionSearchResult, WindowRectValue, MAX_WAIT_MS,
    MIN_POLL_MS,
};
use crate::runtime_protocol::{ScriptStopMessage, ScriptStopMode};
use rhai::{
    ASTNode, Array, Dynamic, Engine, EvalAltResult, Expr, FnCallExpr, ImmutableString, Map,
    Position, Stmt,
};
use serde::Serialize;
use std::any::TypeId;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const RHAI_API_VERSION: u32 = 1;
pub const MAX_RHAI_OPERATIONS: usize = 100_000;
pub const MAX_RHAI_CALL_LEVELS: usize = 32;
pub const MAX_RHAI_EXPR_DEPTH: usize = 64;
pub const MAX_RHAI_STRING_SIZE: usize = 1_000_000;

pub(crate) const CANCELLED: &str = "脚本已被 F12 停止";
const SCRIPT_STOP_REQUESTED: &str = "__autoflow_stop_with_message__";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptOutcome {
    Completed,
    StoppedWithMessage(ScriptStopMessage),
}

pub trait AutomationInput: Send + Sync {
    fn wait_ms(&self, milliseconds: u64, speed: f32, cancel: &AtomicBool) -> Result<(), String>;
    fn wait_random_ms(
        &self,
        minimum: u64,
        maximum: u64,
        speed: f32,
        cancel: &AtomicBool,
    ) -> Result<(), String>;
    fn key_down(&self, key: &str) -> Result<(), String>;
    fn key_up(&self, key: &str) -> Result<(), String>;
    fn move_to(&self, x: i32, y: i32) -> Result<(), String>;
    fn mouse_down(&self, button: &str, x: i32, y: i32) -> Result<(), String>;
    fn mouse_up(&self, button: &str, x: i32, y: i32) -> Result<(), String>;
    /// Release paths must bypass the playback cancellation gate.  F12 may set
    /// that gate before a script has released a button or key.
    fn force_key_up(&self, key: &str) -> Result<(), String> {
        self.key_up(key)
    }
    fn force_mouse_up(&self, button: &str) -> Result<(), String> {
        self.mouse_up(button, 0, 0)
    }
    fn cleanup_injected_input(&self) -> Result<(), String> {
        Ok(())
    }
    fn click(&self, button: &str, x: i32, y: i32) -> Result<(), String> {
        self.move_to(x, y)?;
        self.mouse_down(button, 0, 0)?;
        self.mouse_up(button, 0, 0)
    }
    fn bio_move_to(
        &self,
        x: i32,
        y: i32,
        target_width: Option<f32>,
        followed_by_click: bool,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        let _ = (target_width, followed_by_click, cancel);
        self.move_to(x, y)
    }
    fn bio_click(
        &self,
        button: &str,
        x: i32,
        y: i32,
        target_width: Option<f32>,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        self.bio_move_to(x, y, target_width, true, cancel)?;
        self.click(button, 0, 0)
    }
    fn scroll(&self, delta_x: i32, delta_y: i32) -> Result<(), String>;
    fn type_text(&self, text: &str) -> Result<(), String>;
}

pub struct ExecutionContext {
    pub cancel: Arc<AtomicBool>,
    pub speed: f32,
    pub current_action: usize,
    pub pressed_keys: HashSet<String>,
    pub pressed_mouse_buttons: HashSet<String>,
    pub error_state: Option<String>,
    stop_message: Option<ScriptStopMessage>,
    input: Arc<dyn AutomationInput>,
    vision: Arc<dyn VisionApi>,
    budget: Arc<VisionPollBudget>,
    action_progress: Option<Arc<dyn Fn(usize, String) + Send + Sync>>,
}

impl ExecutionContext {
    pub fn new(
        input: Arc<dyn AutomationInput>,
        cancel: Arc<AtomicBool>,
        speed: f32,
        progress: Option<Arc<dyn Fn(usize) + Send + Sync>>,
    ) -> Self {
        let vision = crate::automation::VisionService::new(
            std::env::temp_dir()
                .join("AutoFlow")
                .join("data")
                .join("images"),
        );
        Self::new_with_vision(input, cancel, speed, progress, vision)
    }

    pub fn new_with_vision(
        input: Arc<dyn AutomationInput>,
        cancel: Arc<AtomicBool>,
        speed: f32,
        progress: Option<Arc<dyn Fn(usize) + Send + Sync>>,
        vision: Arc<dyn VisionApi>,
    ) -> Self {
        Self::new_with_vision_and_action_progress(input, cancel, speed, progress, None, vision)
    }

    pub fn new_with_vision_and_action_progress(
        input: Arc<dyn AutomationInput>,
        cancel: Arc<AtomicBool>,
        speed: f32,
        progress: Option<Arc<dyn Fn(usize) + Send + Sync>>,
        action_progress: Option<Arc<dyn Fn(usize, String) + Send + Sync>>,
        vision: Arc<dyn VisionApi>,
    ) -> Self {
        Self {
            cancel,
            speed: speed.max(0.05),
            current_action: 0,
            pressed_keys: HashSet::new(),
            pressed_mouse_buttons: HashSet::new(),
            error_state: None,
            stop_message: None,
            input,
            vision,
            budget: Arc::new(VisionPollBudget::new(progress)),
            action_progress,
        }
    }

    fn begin_action(&mut self, name: &str) -> Result<(), String> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.to_string());
        }
        self.current_action = self.budget.consume().map_err(|error| error.message)?;
        if let Some(progress) = &self.action_progress {
            progress(self.current_action, name.to_string());
        }
        Ok(())
    }

    fn remember_error(&mut self, error: &str) {
        self.error_state = Some(error.to_string());
    }

    fn release_all(&mut self) -> Result<(), String> {
        let keys = self.pressed_keys.iter().cloned().collect::<Vec<_>>();
        let buttons = self
            .pressed_mouse_buttons
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let mut failures = Vec::new();
        for key in keys {
            match self.input.force_key_up(&key) {
                Ok(()) => {
                    self.pressed_keys.remove(&key);
                }
                Err(error) => failures.push(format!("key {key}: {error}")),
            }
        }
        for button in buttons {
            match self.input.force_mouse_up(&button) {
                Ok(()) => {
                    self.pressed_mouse_buttons.remove(&button);
                }
                Err(error) => failures.push(format!("button {button}: {error}")),
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!("输入释放失败: {}", failures.join("; ")))
        }
    }
}

fn runtime_error(message: impl Into<String>) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(
        Dynamic::from(message.into()),
        Position::NONE,
    ))
}

fn with_context<T, F>(
    state: &Arc<Mutex<ExecutionContext>>,
    action: F,
) -> Result<T, Box<EvalAltResult>>
where
    F: FnOnce(&mut ExecutionContext) -> Result<T, String>,
{
    let mut context = state
        .lock()
        .map_err(|_| runtime_error("执行上下文状态异常"))?;
    match action(&mut context) {
        Ok(value) => Ok(value),
        Err(error) => {
            context.remember_error(&error);
            Err(runtime_error(error))
        }
    }
}

fn register_api(engine: &mut Engine, state: Arc<Mutex<ExecutionContext>>) {
    let current = Arc::clone(&state);
    engine.register_fn(
        "stop_with_message",
        move |message: String| -> Result<(), Box<EvalAltResult>> {
            request_script_stop(
                &current,
                "AutoFlow".to_string(),
                message,
                ScriptStopMode::Background,
            )
        },
    );

    let current = Arc::clone(&state);
    engine.register_fn(
        "stop_with_message",
        move |title: String, message: String| -> Result<(), Box<EvalAltResult>> {
            request_script_stop(&current, title, message, ScriptStopMode::Background)
        },
    );

    let current = Arc::clone(&state);
    engine.register_fn(
        "stop_with_message",
        move |message: String, options: Map| -> Result<(), Box<EvalAltResult>> {
            let mode = parse_stop_message_options(&options).map_err(runtime_error)?;
            request_script_stop(&current, "AutoFlow".to_string(), message, mode)
        },
    );

    let current = Arc::clone(&state);
    engine.register_fn("wait_ms", move |milliseconds: i64| {
        with_context(&current, |context| {
            if milliseconds < 0 {
                return Err("wait_ms 的参数必须是非负整数".to_string());
            }
            context.begin_action("wait_ms")?;
            context
                .input
                .wait_ms(milliseconds as u64, context.speed, context.cancel.as_ref())
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("wait_random_ms", move |minimum: i64, maximum: i64| {
        with_context(&current, |context| {
            if minimum < 0 || maximum < 0 {
                return Err("wait_random_ms 的参数必须是非负整数".to_string());
            }
            if maximum < minimum {
                return Err("wait_random_ms 的最大值不能小于最小值".to_string());
            }
            context.begin_action("wait_random_ms")?;
            context.input.wait_random_ms(
                minimum as u64,
                maximum as u64,
                context.speed,
                context.cancel.as_ref(),
            )
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("key_down", move |key: String| {
        with_context(&current, |context| {
            if key.is_empty() {
                return Err("key_down 的按键名不能为空".to_string());
            }
            context.begin_action("key_down")?;
            context.input.key_down(&key)?;
            context.pressed_keys.insert(key);
            Ok(())
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("key_up", move |key: String| {
        with_context(&current, |context| {
            if key.is_empty() {
                return Err("key_up 的按键名不能为空".to_string());
            }
            context.begin_action("key_up")?;
            context.input.key_up(&key)?;
            context.pressed_keys.remove(&key);
            Ok(())
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("press", move |key: String| {
        with_context(&current, |context| {
            if key.is_empty() {
                return Err("press 的按键名不能为空".to_string());
            }
            context.begin_action("press")?;
            context.input.key_down(&key)?;
            context.pressed_keys.insert(key.clone());
            let result = context.input.key_up(&key);
            if result.is_ok() {
                context.pressed_keys.remove(&key);
            }
            result
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("move_to", move |x: i64, y: i64| {
        with_context(&current, |context| {
            let (x, y) = checked_coordinates(x, y)?;
            context.begin_action("move_to")?;
            context.input.move_to(x, y)
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("bio_move_to", move |x: i64, y: i64| {
        with_context(&current, |context| {
            let (x, y) = checked_coordinates(x, y)?;
            context.begin_action("bio_move_to")?;
            context
                .input
                .bio_move_to(x, y, None, false, context.cancel.as_ref())
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("bio_move_to", move |x: i64, y: i64, options: Map| {
        with_context(&current, |context| {
            let (x, y) = checked_coordinates(x, y)?;
            let (target_width, followed_by_click) = parse_behavior_options(&options)?;
            context.begin_action("bio_move_to")?;
            context.input.bio_move_to(
                x,
                y,
                target_width,
                followed_by_click,
                context.cancel.as_ref(),
            )
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("mouse_down", move |button: String, x: i64, y: i64| {
        with_context(&current, |context| {
            let (x, y) = checked_coordinates(x, y)?;
            validate_button(&button)?;
            context.begin_action("mouse_down")?;
            context.input.mouse_down(&button, x, y)?;
            context.pressed_mouse_buttons.insert(button);
            Ok(())
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("mouse_up", move |button: String, x: i64, y: i64| {
        with_context(&current, |context| {
            let (x, y) = checked_coordinates(x, y)?;
            validate_button(&button)?;
            context.begin_action("mouse_up")?;
            context.input.mouse_up(&button, x, y)?;
            context.pressed_mouse_buttons.remove(&button);
            Ok(())
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("click", move |button: String| {
        with_context(&current, |context| {
            validate_button(&button)?;
            context.begin_action("click")?;
            context.input.click(&button, 0, 0)
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("click", move |button: String, x: i64, y: i64| {
        with_context(&current, |context| {
            let (x, y) = checked_coordinates(x, y)?;
            validate_button(&button)?;
            context.begin_action("click")?;
            context.input.click(&button, x, y)
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("click", move |x: i64, y: i64| {
        with_context(&current, |context| {
            let (x, y) = checked_coordinates(x, y)?;
            context.begin_action("click")?;
            let button = "left".to_string();
            context.input.click(&button, x, y)
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("bio_click", move |button: String, x: i64, y: i64| {
        with_context(&current, |context| {
            let (x, y) = checked_coordinates(x, y)?;
            validate_button(&button)?;
            context.begin_action("bio_click")?;
            context
                .input
                .bio_click(&button, x, y, None, context.cancel.as_ref())
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn(
        "bio_click",
        move |button: String, x: i64, y: i64, options: Map| {
            with_context(&current, |context| {
                let (x, y) = checked_coordinates(x, y)?;
                validate_button(&button)?;
                let (target_width, _) = parse_behavior_options(&options)?;
                context.begin_action("bio_click")?;
                context
                    .input
                    .bio_click(&button, x, y, target_width, context.cancel.as_ref())
            })
        },
    );

    let current = Arc::clone(&state);
    engine.register_fn("scroll", move |delta_x: i64, delta_y: i64| {
        with_context(&current, |context| {
            let (delta_x, delta_y) = (checked_integer(delta_x)?, checked_integer(delta_y)?);
            context.begin_action("scroll")?;
            context.input.scroll(delta_x, delta_y)
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("type_text", move |text: String| {
        with_context(&current, |context| {
            if text.is_empty() {
                return Err("type_text 的文本不能为空".to_string());
            }
            context.begin_action("type_text")?;
            context.input.type_text(&text)
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("bio_type_text", move |text: String| {
        with_context(&current, |context| {
            if text.is_empty() {
                return Err("bio_type_text 的文本不能为空".to_string());
            }
            context.begin_action("bio_type_text")?;
            context.input.type_text(&text)
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("bio_type_text", move |text: String, options: Map| {
        with_context(&current, |context| {
            if text.is_empty() {
                return Err("bio_type_text 的文本不能为空".to_string());
            }
            if let Some(mode) = options.get("mode") {
                let mode = mode
                    .clone()
                    .try_cast::<String>()
                    .ok_or_else(|| "bio_type_text.options.mode 必须是字符串".to_string())?;
                if mode != "normal" {
                    return Err("bio_type_text 当前只支持 mode=normal".to_string());
                }
            }
            context.begin_action("bio_type_text")?;
            context.input.type_text(&text)
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("is_cancelled", move || {
        current
            .lock()
            .map(|context| context.cancel.load(Ordering::SeqCst))
            .unwrap_or(true)
    });

    let current = Arc::clone(&state);
    engine.register_fn("active_window_title", move || {
        with_context(&current, |context| {
            context.begin_action("active_window_title")?;
            context
                .vision
                .active_window_title()
                .map_err(vision_error_message)
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("window_exists", move |title_query: String| {
        with_context(&current, |context| {
            context.begin_action("window_exists")?;
            context
                .vision
                .window_exists(&title_query)
                .map_err(vision_error_message)
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn("window_rect", move |title_query: String| {
        with_context(&current, |context| {
            context.begin_action("window_rect")?;
            context
                .vision
                .window_rect(&title_query)
                .map(window_rect_map)
                .map_err(vision_error_message)
        })
    });

    let current = Arc::clone(&state);
    engine.register_fn(
        "wait_window",
        move |title_query: String, timeout_ms: i64, poll_ms: i64| {
            with_context(&current, |context| {
                let timeout = checked_timeout(timeout_ms)?;
                let poll = checked_poll(poll_ms)?;
                context.begin_action("wait_window")?;
                let vision = Arc::clone(&context.vision);
                let cancel = Arc::clone(&context.cancel);
                let budget = Arc::clone(&context.budget);
                vision
                    .wait_window(
                        &title_query,
                        VisionPollOptions::new(timeout, poll, &cancel, &budget),
                    )
                    .map_err(vision_error_message)
            })
        },
    );

    let current = Arc::clone(&state);
    engine.register_fn(
        "pixel_matches",
        move |x: i64, y: i64, red: i64, green: i64, blue: i64, tolerance: i64| {
            with_context(&current, |context| {
                let point = checked_point(x, y)?;
                let expected = checked_rgb(red, green, blue)?;
                let tolerance = checked_byte(tolerance, "tolerance")?;
                context.begin_action("pixel_matches")?;
                let vision = Arc::clone(&context.vision);
                let cancel = Arc::clone(&context.cancel);
                vision
                    .pixel_matches(point, expected, tolerance, &cancel)
                    .map_err(vision_error_message)
            })
        },
    );

    let current = Arc::clone(&state);
    engine.register_fn(
        "wait_pixel",
        move |x: i64,
              y: i64,
              red: i64,
              green: i64,
              blue: i64,
              tolerance: i64,
              timeout_ms: i64,
              poll_ms: i64| {
            with_context(&current, |context| {
                let point = checked_point(x, y)?;
                let expected = checked_rgb(red, green, blue)?;
                let tolerance = checked_byte(tolerance, "tolerance")?;
                let timeout = checked_timeout(timeout_ms)?;
                let poll = checked_poll(poll_ms)?;
                context.begin_action("wait_pixel")?;
                let vision = Arc::clone(&context.vision);
                let cancel = Arc::clone(&context.cancel);
                let budget = Arc::clone(&context.budget);
                vision
                    .wait_pixel(
                        point,
                        expected,
                        tolerance,
                        VisionPollOptions::new(timeout, poll, &cancel, &budget),
                    )
                    .map_err(vision_error_message)
            })
        },
    );

    let current = Arc::clone(&state);
    engine.register_fn(
        "find_image",
        move |file_name: String,
              region_x: i64,
              region_y: i64,
              region_width: i64,
              region_height: i64,
              threshold: f64| {
            with_context(&current, |context| {
                let region = checked_region(region_x, region_y, region_width, region_height)?;
                let threshold = checked_threshold(threshold)?;
                context.begin_action("find_image")?;
                let vision = Arc::clone(&context.vision);
                let cancel = Arc::clone(&context.cancel);
                vision
                    .find_image_diagnostic(
                        &file_name,
                        region,
                        threshold,
                        &cancel,
                        &MatcherOptions::default(),
                    )
                    .map(image_search_map)
                    .map_err(vision_error_message)
            })
        },
    );

    let current = Arc::clone(&state);
    engine.register_fn(
        "wait_image",
        move |file_name: String,
              region_x: i64,
              region_y: i64,
              region_width: i64,
              region_height: i64,
              threshold: f64,
              timeout_ms: i64,
              poll_ms: i64| {
            with_context(&current, |context| {
                let region = checked_region(region_x, region_y, region_width, region_height)?;
                let threshold = checked_threshold(threshold)?;
                let timeout = checked_timeout(timeout_ms)?;
                let poll = checked_poll(poll_ms)?;
                context.begin_action("wait_image")?;
                let vision = Arc::clone(&context.vision);
                let cancel = Arc::clone(&context.cancel);
                let budget = Arc::clone(&context.budget);
                vision
                    .wait_image_diagnostic(
                        &file_name,
                        region,
                        threshold,
                        VisionPollOptions::new(timeout, poll, &cancel, &budget),
                        &MatcherOptions::default(),
                    )
                    .map(image_search_map)
                    .map_err(vision_error_message)
            })
        },
    );

    let current = Arc::clone(&state);
    engine.register_fn(
        "find_image",
        move |file_name: String,
              region_x: i64,
              region_y: i64,
              region_width: i64,
              region_height: i64,
              threshold: f64,
              options: Map| {
            with_context(&current, |context| {
                let region = checked_region(region_x, region_y, region_width, region_height)?;
                let threshold = checked_threshold(threshold)?;
                let matcher_options = checked_match_options(&options)?;
                context.begin_action("find_image")?;
                let vision = Arc::clone(&context.vision);
                let cancel = Arc::clone(&context.cancel);
                vision
                    .find_image_diagnostic(&file_name, region, threshold, &cancel, &matcher_options)
                    .map(image_search_map)
                    .map_err(vision_error_message)
            })
        },
    );

    let current = Arc::clone(&state);
    engine.register_fn(
        "wait_image",
        move |file_name: String,
              region_x: i64,
              region_y: i64,
              region_width: i64,
              region_height: i64,
              threshold: f64,
              timeout_ms: i64,
              poll_ms: i64,
              options: Map| {
            with_context(&current, |context| {
                let region = checked_region(region_x, region_y, region_width, region_height)?;
                let threshold = checked_threshold(threshold)?;
                let timeout = checked_timeout(timeout_ms)?;
                let poll = checked_poll(poll_ms)?;
                let matcher_options = checked_match_options(&options)?;
                context.begin_action("wait_image")?;
                let vision = Arc::clone(&context.vision);
                let cancel = Arc::clone(&context.cancel);
                let budget = Arc::clone(&context.budget);
                vision
                    .wait_image_diagnostic(
                        &file_name,
                        region,
                        threshold,
                        VisionPollOptions::new(timeout, poll, &cancel, &budget),
                        &matcher_options,
                    )
                    .map(image_search_map)
                    .map_err(vision_error_message)
            })
        },
    );
}

fn vision_error_message(error: VisionError) -> String {
    format!("{}：{}", error.code, error.message)
}

fn window_rect_map(value: WindowRectValue) -> Map {
    let mut map = Map::new();
    map.insert("found".into(), Dynamic::from(value.found));
    map.insert("x".into(), Dynamic::from(i64::from(value.x)));
    map.insert("y".into(), Dynamic::from(i64::from(value.y)));
    map.insert("width".into(), Dynamic::from(i64::from(value.width)));
    map.insert("height".into(), Dynamic::from(i64::from(value.height)));
    map
}

fn image_search_map(value: VisionSearchResult) -> Map {
    let mut map = Map::new();
    if let Some(value) = value.image {
        map.insert("found".into(), Dynamic::from(true));
        map.insert("x".into(), Dynamic::from(i64::from(value.x)));
        map.insert("y".into(), Dynamic::from(i64::from(value.y)));
        map.insert("width".into(), Dynamic::from(i64::from(value.width)));
        map.insert("height".into(), Dynamic::from(i64::from(value.height)));
        map.insert("center_x".into(), Dynamic::from(i64::from(value.center_x)));
        map.insert("center_y".into(), Dynamic::from(i64::from(value.center_y)));
        map.insert("score".into(), Dynamic::from(f64::from(value.score)));
    } else {
        map.insert("found".into(), Dynamic::from(false));
        for key in ["x", "y", "width", "height", "center_x", "center_y"] {
            map.insert(key.into(), Dynamic::from(0_i64));
        }
        map.insert("score".into(), Dynamic::from(0.0_f64));
    }
    map.insert(
        "total_ms".into(),
        Dynamic::from(value.diagnostics.total_ms as i64),
    );
    map.insert(
        "capture_ms".into(),
        Dynamic::from(value.diagnostics.capture_ms as i64),
    );
    map.insert(
        "prepare_ms".into(),
        Dynamic::from(value.diagnostics.prepare_ms as i64),
    );
    map.insert(
        "coarse_ms".into(),
        Dynamic::from(value.diagnostics.coarse_ms as i64),
    );
    map.insert(
        "refine_ms".into(),
        Dynamic::from(value.diagnostics.refine_ms as i64),
    );
    map.insert(
        "fallback_ms".into(),
        Dynamic::from(value.diagnostics.fallback_ms as i64),
    );
    map.insert(
        "candidate_count".into(),
        Dynamic::from(value.diagnostics.candidate_count as i64),
    );
    map.insert(
        "previous_hit_used".into(),
        Dynamic::from(value.diagnostics.previous_hit_used),
    );
    map.insert(
        "fallback_used".into(),
        Dynamic::from(value.diagnostics.fallback_used),
    );
    map.insert(
        "matcher_mode".into(),
        Dynamic::from(value.diagnostics.matcher_mode),
    );
    map.insert(
        "matched_scale".into(),
        value
            .diagnostics
            .matched_scale
            .map(|scale| Dynamic::from(f64::from(scale)))
            .unwrap_or_else(|| Dynamic::from(())),
    );
    map.insert(
        "scale_candidates".into(),
        Dynamic::from(
            value
                .diagnostics
                .scale_candidates
                .iter()
                .map(|scale| Dynamic::from(f64::from(*scale)))
                .collect::<Array>(),
        ),
    );
    map.insert(
        "scale_search_ms".into(),
        Dynamic::from(value.diagnostics.scale_search_ms as i64),
    );
    map.insert(
        "matched_width".into(),
        Dynamic::from(i64::from(value.diagnostics.matched_width)),
    );
    map.insert(
        "matched_height".into(),
        Dynamic::from(i64::from(value.diagnostics.matched_height)),
    );
    map.insert(
        "robust_verify_used".into(),
        Dynamic::from(value.diagnostics.robust_verify_used),
    );
    map.insert(
        "robust_verify_ms".into(),
        Dynamic::from(value.diagnostics.robust_verify_ms as i64),
    );
    map.insert(
        "robust_score".into(),
        value
            .diagnostics
            .robust_score
            .map(|score| Dynamic::from(f64::from(score)))
            .unwrap_or_else(|| Dynamic::from(())),
    );
    map.insert(
        "valid_tile_count".into(),
        Dynamic::from(value.diagnostics.valid_tile_count as i64),
    );
    map.insert(
        "discarded_tile_count".into(),
        Dynamic::from(value.diagnostics.discarded_tile_count as i64),
    );
    map.insert(
        "alpha_mask_used".into(),
        Dynamic::from(value.diagnostics.alpha_mask_used),
    );
    map.insert(
        "anchor_recovery_used".into(),
        Dynamic::from(value.diagnostics.anchor_recovery_used),
    );
    map.insert(
        "anchor_candidate_count".into(),
        Dynamic::from(value.diagnostics.anchor_candidate_count as i64),
    );
    map.insert(
        "preferred_scale_hit".into(),
        Dynamic::from(value.diagnostics.preferred_scale_hit),
    );
    map.insert(
        "single_match_ms".into(),
        Dynamic::from(value.diagnostics.single_match_ms as i64),
    );
    map.insert(
        "wait_total_ms".into(),
        Dynamic::from(value.diagnostics.wait_total_ms as i64),
    );
    map
}

fn checked_match_options(options: &Map) -> Result<MatcherOptions, String> {
    let mut parsed = MatcherOptions::default();
    if let Some(value) = options.get("mode") {
        let Some(mode) = value.clone().try_cast::<String>() else {
            return Err(
                "vision_match_mode_invalid：mode 必须是字符串 auto、exact 或 fast".to_string(),
            );
        };
        parsed.mode = match mode.trim().to_ascii_lowercase().as_str() {
            "auto" => MatcherMode::Auto,
            "exact" => MatcherMode::Exact,
            "fast" => MatcherMode::Fast,
            _ => {
                return Err("vision_match_mode_invalid：mode 必须是 auto、exact 或 fast".to_string())
            }
        };
    }
    if let Some(value) = options.get("prefer_last") {
        parsed.prefer_last = value
            .clone()
            .try_cast::<bool>()
            .ok_or_else(|| "vision_match_options_invalid：prefer_last 必须是布尔值".to_string())?;
    }
    if let Some(value) = options.get("max_candidates") {
        let count = value
            .clone()
            .try_cast::<i64>()
            .ok_or_else(|| "vision_match_options_invalid：max_candidates 必须是整数".to_string())?;
        parsed.max_candidates = usize::try_from(count).map_err(|_| {
            "vision_match_options_invalid：max_candidates 必须在 1 到 32 之间".to_string()
        })?;
    }
    if let Some(value) = options.get("scale_min") {
        parsed.scale_min = checked_scale_option(value, "scale_min")?;
    }
    if let Some(value) = options.get("scale_max") {
        parsed.scale_max = checked_scale_option(value, "scale_max")?;
    }
    if let Some(value) = options.get("scale_step") {
        parsed.scale_step = Some(checked_scale_option(value, "scale_step")?);
    }
    parsed
        .validate()
        .map_err(|error| format!("{}：{}", error.code, error.message))?;
    Ok(parsed)
}

fn checked_scale_option(value: &Dynamic, name: &str) -> Result<f32, String> {
    let number = if value.is_int() {
        value
            .as_int()
            .map(|value| value as f64)
            .map_err(|_| format!("vision_scale_invalid：{name} 必须是数字"))?
    } else if value.is_float() {
        value
            .as_float()
            .map_err(|_| format!("vision_scale_invalid：{name} 必须是数字"))?
    } else {
        return Err(format!("vision_scale_invalid：{name} 必须是数字"));
    };
    if !number.is_finite() {
        return Err(format!("vision_scale_invalid：{name} 必须是有限数字"));
    }
    Ok(number as f32)
}

fn checked_point(x: i64, y: i64) -> Result<Point, String> {
    Ok(Point {
        x: checked_integer(x).map_err(|error| format!("capture_region_invalid：{error}"))?,
        y: checked_integer(y).map_err(|error| format!("capture_region_invalid：{error}"))?,
    })
}

fn request_script_stop(
    state: &Arc<Mutex<ExecutionContext>>,
    title: String,
    message: String,
    mode: ScriptStopMode,
) -> Result<(), Box<EvalAltResult>> {
    let title = title.trim();
    let message = message.trim();
    if message.is_empty() {
        return Err(runtime_error("stop_with_message 的提示内容不能为空"));
    }
    if title.chars().count() > 128 || message.chars().count() > 2_000 {
        return Err(runtime_error(
            "stop_with_message 的标题不能超过 128 字，内容不能超过 2000 字",
        ));
    }
    let mut context = state
        .lock()
        .map_err(|_| runtime_error("执行上下文状态异常"))?;
    context
        .begin_action("stop_with_message")
        .map_err(runtime_error)?;
    context.stop_message = Some(ScriptStopMessage {
        title: if title.is_empty() {
            "AutoFlow".to_string()
        } else {
            title.to_string()
        },
        message: message.to_string(),
        mode,
    });
    Err(runtime_error(SCRIPT_STOP_REQUESTED))
}

fn parse_stop_message_options(options: &Map) -> Result<ScriptStopMode, String> {
    if let Some(key) = options.keys().find(|key| key.as_str() != "mode") {
        return Err(format!("stop_with_message 的选项包含未知字段：{key}"));
    }
    let Some(value) = options.get("mode") else {
        return Ok(ScriptStopMode::Background);
    };
    let Some(mode) = value.clone().try_cast::<String>() else {
        return Err("stop_with_message 的 mode 必须是字符串 background 或 foreground".to_string());
    };
    match mode.as_str() {
        "background" => Ok(ScriptStopMode::Background),
        "foreground" => Ok(ScriptStopMode::Foreground),
        _ => Err("stop_with_message 的 mode 必须是 background 或 foreground".to_string()),
    }
}

fn parse_behavior_options(options: &Map) -> Result<(Option<f32>, bool), String> {
    let target_width = options
        .get("target_width")
        .map(|value| {
            if value.is_int() {
                value
                    .as_int()
                    .map(|value| value as f32)
                    .map_err(|_| "bio_move_to.options.target_width 必须是数字".to_string())
            } else if value.is_float() {
                value
                    .as_float()
                    .map(|value| value as f32)
                    .map_err(|_| "bio_move_to.options.target_width 必须是数字".to_string())
            } else {
                Err("bio_move_to.options.target_width 必须是数字".to_string())
            }
        })
        .transpose()?;
    if let Some(width) = target_width {
        if !width.is_finite() || !(0.0..=10_000.0).contains(&width) {
            return Err("bio_move_to.options.target_width 必须在 0 到 10000 之间".to_string());
        }
    }
    let followed_by_click = options
        .get("intent")
        .map(|value| {
            let intent = value
                .clone()
                .try_cast::<String>()
                .ok_or_else(|| "bio_move_to.options.intent 必须是字符串".to_string())?;
            match intent.as_str() {
                "click" => Ok(true),
                "move" => Ok(false),
                _ => Err("bio_move_to.options.intent 只能是 move 或 click".to_string()),
            }
        })
        .transpose()?
        .unwrap_or(false);
    Ok((target_width, followed_by_click))
}

fn checked_region(x: i64, y: i64, width: i64, height: i64) -> Result<ScreenRect, String> {
    if width <= 0 || height <= 0 {
        return Err("capture_region_invalid：捕获区域的宽度和高度必须大于 0".to_string());
    }
    ScreenRect::new(
        checked_integer(x).map_err(|error| format!("capture_region_invalid：{error}"))?,
        checked_integer(y).map_err(|error| format!("capture_region_invalid：{error}"))?,
        u32::try_from(width).map_err(|_| "捕获区域宽度超出范围".to_string())?,
        u32::try_from(height).map_err(|_| "捕获区域高度超出范围".to_string())?,
    )
    .map_err(|error| format!("{}：{}", error.code, error.message))
}

fn checked_byte(value: i64, name: &str) -> Result<u8, String> {
    u8::try_from(value).map_err(|_| format!("{name} 必须在 0 到 255 之间"))
}

fn checked_rgb(red: i64, green: i64, blue: i64) -> Result<RgbColor, String> {
    Ok(RgbColor {
        red: checked_byte(red, "red")?,
        green: checked_byte(green, "green")?,
        blue: checked_byte(blue, "blue")?,
    })
}

fn checked_threshold(value: f64) -> Result<f32, String> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err("vision_threshold_invalid：threshold 必须在 0.0 到 1.0 之间".to_string());
    }
    Ok(value as f32)
}

fn checked_timeout(value: i64) -> Result<std::time::Duration, String> {
    if value < 0 || u64::try_from(value).unwrap_or(u64::MAX) > MAX_WAIT_MS {
        return Err(format!(
            "vision_timeout_invalid：timeout_ms 必须在 0 到 {MAX_WAIT_MS} 之间"
        ));
    }
    Ok(std::time::Duration::from_millis(value as u64))
}

fn checked_poll(value: i64) -> Result<std::time::Duration, String> {
    if value < i64::try_from(MIN_POLL_MS).unwrap_or(i64::MAX) {
        return Err(format!(
            "vision_poll_invalid：poll_ms 不能小于 {MIN_POLL_MS}ms"
        ));
    }
    Ok(std::time::Duration::from_millis(value as u64))
}

fn checked_integer(value: i64) -> Result<i32, String> {
    i32::try_from(value).map_err(|_| "坐标或滚动值超出 i32 范围".to_string())
}

fn checked_coordinates(x: i64, y: i64) -> Result<(i32, i32), String> {
    Ok((checked_integer(x)?, checked_integer(y)?))
}

fn validate_button(button: &str) -> Result<(), String> {
    if matches!(button, "left" | "right" | "middle" | "x1" | "x2") {
        Ok(())
    } else {
        Err("鼠标按钮必须是 left、right、middle、x1 或 x2".to_string())
    }
}

fn configure_engine(engine: &mut Engine, cancel: Arc<AtomicBool>) {
    engine.set_max_operations(MAX_RHAI_OPERATIONS as u64);
    engine.set_max_call_levels(MAX_RHAI_CALL_LEVELS);
    engine.set_max_expr_depths(MAX_RHAI_EXPR_DEPTH, MAX_RHAI_EXPR_DEPTH);
    engine.set_max_string_size(MAX_RHAI_STRING_SIZE);
    engine.on_progress(move |_| {
        if cancel.load(Ordering::SeqCst) {
            Some(Dynamic::UNIT)
        } else {
            None
        }
    });
}

pub fn run_rhai_script(source: &str, context: ExecutionContext) -> Result<ScriptOutcome, String> {
    let cancel = Arc::clone(&context.cancel);
    let state = Arc::new(Mutex::new(context));
    let mut engine = Engine::new();
    configure_engine(&mut engine, cancel);
    register_api(&mut engine, Arc::clone(&state));
    let ast = compile_source(&engine, source)?;
    let result = engine.eval_ast::<Dynamic>(&ast);
    let mut context = state.lock().map_err(|_| "执行上下文状态异常".to_string())?;
    let release_error = context.release_all().err();
    let input_cleanup_error = context.input.cleanup_injected_input().err();
    let cleanup_error = release_error.or(input_cleanup_error);
    if let Some(error) = cleanup_error.as_deref() {
        context.remember_error(error);
    }
    if let Some(stop_message) = context.stop_message.take() {
        if let Some(error) = cleanup_error {
            return Err(format!("脚本停止后输入清理失败，当前状态不安全: {error}"));
        }
        return Ok(ScriptOutcome::StoppedWithMessage(stop_message));
    }
    if let Some(error) = cleanup_error {
        return Err(format!(
            "输入清理失败，当前状态可能仍有按键或鼠标按钮按下: {error}"
        ));
    }
    match result {
        Ok(_) => {
            if context.cancel.load(Ordering::SeqCst) {
                Err(CANCELLED.to_string())
            } else {
                Ok(ScriptOutcome::Completed)
            }
        }
        Err(error) => {
            if context.cancel.load(Ordering::SeqCst) {
                Err(CANCELLED.to_string())
            } else if let Some(message) = context.error_state.take() {
                Err(format_rhai_error(error, &message))
            } else {
                Err(format_rhai_error(error, "脚本执行失败"))
            }
        }
    }
}

pub fn validate_rhai_source(source: &str) -> Result<(), String> {
    inspect_rhai_source(source).map(|_| ())
}

/// A successful compile is not proof that the script is executable: arguments
/// can be dynamic, user functions can shadow native APIs, and automation can
/// fail at runtime. The UI always calls this a *static* inspection.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RhaiValidationReport {
    pub unverified_calls: Vec<UnverifiedRhaiCall>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnverifiedRhaiCall {
    pub name: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub reason: &'static str,
}

#[derive(Clone)]
struct NativeSignature {
    argument_types: Vec<TypeId>,
}

// Never run or even instantiate the real input/vision adapters during static
// validation. register_api merely captures this inert state in native closures.
struct ValidationInput;

impl AutomationInput for ValidationInput {
    fn wait_ms(&self, _: u64, _: f32, _: &AtomicBool) -> Result<(), String> {
        unreachable!("static validation must not execute input")
    }
    fn wait_random_ms(&self, _: u64, _: u64, _: f32, _: &AtomicBool) -> Result<(), String> {
        unreachable!("static validation must not execute input")
    }
    fn key_down(&self, _: &str) -> Result<(), String> {
        unreachable!("static validation must not execute input")
    }
    fn key_up(&self, _: &str) -> Result<(), String> {
        unreachable!("static validation must not execute input")
    }
    fn move_to(&self, _: i32, _: i32) -> Result<(), String> {
        unreachable!("static validation must not execute input")
    }
    fn mouse_down(&self, _: &str, _: i32, _: i32) -> Result<(), String> {
        unreachable!("static validation must not execute input")
    }
    fn mouse_up(&self, _: &str, _: i32, _: i32) -> Result<(), String> {
        unreachable!("static validation must not execute input")
    }
    fn scroll(&self, _: i32, _: i32) -> Result<(), String> {
        unreachable!("static validation must not execute input")
    }
    fn type_text(&self, _: &str) -> Result<(), String> {
        unreachable!("static validation must not execute input")
    }
}

struct ValidationVision;

impl VisionApi for ValidationVision {
    fn active_window_title(&self) -> Result<String, VisionError> {
        unreachable!("static validation must not access vision")
    }
    fn window_exists(&self, _: &str) -> Result<bool, VisionError> {
        unreachable!("static validation must not access vision")
    }
    fn window_rect(&self, _: &str) -> Result<WindowRectValue, VisionError> {
        unreachable!("static validation must not access vision")
    }
    fn wait_window(&self, _: &str, _: VisionPollOptions<'_>) -> Result<bool, VisionError> {
        unreachable!("static validation must not access vision")
    }
    fn pixel_matches(
        &self,
        _: Point,
        _: RgbColor,
        _: u8,
        _: &AtomicBool,
    ) -> Result<bool, VisionError> {
        unreachable!("static validation must not access vision")
    }
    fn wait_pixel(
        &self,
        _: Point,
        _: RgbColor,
        _: u8,
        _: VisionPollOptions<'_>,
    ) -> Result<bool, VisionError> {
        unreachable!("static validation must not access vision")
    }
    fn find_image(
        &self,
        _: &str,
        _: ScreenRect,
        _: f32,
        _: &AtomicBool,
    ) -> Result<Option<crate::automation::ImageMatch>, VisionError> {
        unreachable!("static validation must not access vision")
    }
    fn wait_image(
        &self,
        _: &str,
        _: ScreenRect,
        _: f32,
        _: VisionPollOptions<'_>,
    ) -> Result<Option<crate::automation::ImageMatch>, VisionError> {
        unreachable!("static validation must not access vision")
    }
}

fn validation_context(cancel: Arc<AtomicBool>) -> ExecutionContext {
    ExecutionContext::new_with_vision(
        Arc::new(ValidationInput),
        cancel,
        1.0,
        None,
        Arc::new(ValidationVision),
    )
}

fn registered_api_signatures(engine: &Engine) -> HashMap<String, Vec<NativeSignature>> {
    let mut signatures: HashMap<String, Vec<NativeSignature>> = HashMap::new();
    // `false` excludes Rhai's standard packages, leaving precisely the
    // functions installed by register_api on this particular engine.
    for (name, argument_types) in engine.collect_fn_metadata(
        None,
        |info| {
            Some((
                info.metadata.name.to_string(),
                info.metadata.param_types.to_vec(),
            ))
        },
        false,
    ) {
        signatures
            .entry(name)
            .or_default()
            .push(NativeSignature { argument_types });
    }
    signatures
}

// Rhai 1.26's syntactic builtins in engine.rs/func/call.rs bypass the native
// function registry and are consequently absent from collect_fn_metadata.
// These are Rhai language intrinsics, never AutoFlow API signatures.
const RHAI_SYNTACTIC_FUNCTIONS: &[&str] = &[
    "Fn",
    "print",
    "debug",
    "type_of",
    "call",
    "curry",
    "is_shared",
    "is_def_fn",
    "is_def_var",
];

fn known_literal_type(expr: &Expr) -> Option<TypeId> {
    match expr {
        Expr::IntegerConstant(..) => Some(TypeId::of::<i64>()),
        Expr::FloatConstant(..) => Some(TypeId::of::<f64>()),
        Expr::StringConstant(..) => Some(TypeId::of::<ImmutableString>()),
        Expr::BoolConstant(..) => Some(TypeId::of::<bool>()),
        Expr::CharConstant(..) => Some(TypeId::of::<char>()),
        Expr::Map(..) => Some(TypeId::of::<Map>()),
        Expr::Array(..) => Some(TypeId::of::<Array>()),
        Expr::Unit(..) => Some(TypeId::of::<()>()),
        // Dynamic constants, expressions and variables are never assumed to
        // have a fixed type, even if an optimizer folds them in another mode.
        _ => None,
    }
}

fn call_position(position: Position) -> (Option<usize>, Option<usize>) {
    (position.line(), position.position())
}

fn unchecked_call(
    report: &mut RhaiValidationReport,
    name: &str,
    position: Position,
    reason: &'static str,
) {
    let (line, column) = call_position(position);
    report.unverified_calls.push(UnverifiedRhaiCall {
        name: name.to_owned(),
        line,
        column,
        reason,
    });
}

struct StaticApiEnvironment<'a> {
    native: &'a HashMap<String, Vec<NativeSignature>>,
    known_engine_names: &'a HashSet<String>,
    scripted: &'a HashMap<String, Vec<usize>>,
    shadowed: &'a HashSet<String>,
}

impl StaticApiEnvironment<'_> {
    fn check_call(
        &self,
        report: &mut RhaiValidationReport,
        call: &FnCallExpr,
        receiver: Option<&Expr>,
        position: Position,
    ) -> Result<(), String> {
        let name = call.name.as_str();
        let native = self.native;
        let known_engine_names = self.known_engine_names;
        let scripted = self.scripted;
        let shadowed = self.shadowed;
        let qualified = call.is_qualified();
        if qualified {
            unchecked_call(report, name, position, "带命名空间的调用需运行时确认");
            return Ok(());
        }
        if shadowed.contains(name) {
            unchecked_call(report, name, position, "同名变量或函数指针可能覆盖 API");
            return Ok(());
        }
        // Rhai script methods receive `this` implicitly. Native method calls
        // instead dispatch with the receiver as argument zero.
        let scripted_arity = call.args.len();
        let arity = scripted_arity + usize::from(receiver.is_some());
        if scripted
            .get(name)
            .is_some_and(|overloads| overloads.contains(&scripted_arity))
        {
            unchecked_call(report, name, position, "用户定义的同名函数需运行时确认");
            return Ok(());
        }
        if receiver.is_some_and(|expr| {
            // Object maps can expose FnPtr fields as methods. A variable or
            // computed value may be such a map even if its name matches a
            // native API; neither its target nor its arity is proven here.
            matches!(expr, Expr::Map(..)) || known_literal_type(expr).is_none()
        }) {
            unchecked_call(
                report,
                name,
                position,
                "对象方法可能是函数指针，接收者类型需运行时确认",
            );
            return Ok(());
        }
        let Some(overloads) = native.get(name) else {
            if known_engine_names.contains(name) || RHAI_SYNTACTIC_FUNCTIONS.contains(&name) {
                unchecked_call(
                    report,
                    name,
                    position,
                    "Rhai 内建函数不属于 AutoFlow API，需运行时确认",
                );
                return Ok(());
            }
            if let Some(accepted) = scripted.get(name) {
                return Err(format_static_api_error(
                    name,
                    position,
                    &format!(
                        "用户函数参数个数为 {scripted_arity}，允许 {} 个",
                        accepted
                            .iter()
                            .map(usize::to_string)
                            .collect::<Vec<_>>()
                            .join(" / ")
                    ),
                ));
            }
            return Err(format_static_api_error(
                name,
                position,
                "未识别为 AutoFlow API、Rhai 内建函数或用户函数",
            ));
        };
        let matching_arity: Vec<_> = overloads
            .iter()
            .filter(|sig| sig.argument_types.len() == arity)
            .collect();
        if matching_arity.is_empty() {
            let mut accepted = overloads
                .iter()
                .map(|sig| sig.argument_types.len())
                .collect::<Vec<_>>();
            accepted.extend(scripted.get(name).into_iter().flatten().copied());
            accepted.sort_unstable();
            accepted.dedup();
            return Err(format_static_api_error(
                name,
                position,
                &format!(
                    "参数个数为 {arity}，允许 {} 个",
                    accepted
                        .iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(" / ")
                ),
            ));
        }
        let literal_types = receiver
            .into_iter()
            .chain(call.args.iter())
            .map(known_literal_type)
            .collect::<Vec<_>>();
        if !matching_arity.iter().any(|sig| {
            sig.argument_types
                .iter()
                .zip(&literal_types)
                .all(|(expected, actual)| actual.is_none_or(|actual| *expected == actual))
        }) {
            return Err(format_static_api_error(
                name,
                position,
                "字面量参数类型与任何已注册重载均不匹配",
            ));
        }
        if literal_types.iter().any(Option::is_none) {
            unchecked_call(report, name, position, "变量或表达式的参数类型无法静态确认");
        }
        Ok(())
    }
}

fn format_static_api_error(name: &str, position: Position, problem: &str) -> String {
    match call_position(position) {
        (Some(line), Some(column)) => {
            format!("Rhai API 调用错误（第 {line} 行第 {column} 列）：{name}: {problem}")
        }
        _ => format!("Rhai API 调用错误：{name}: {problem}"),
    }
}

pub fn inspect_rhai_source(source: &str) -> Result<RhaiValidationReport, String> {
    let code = strip_strings_and_comments(source);
    for forbidden in [
        "import",
        "export",
        "eval",
        "load_file",
        "write_file",
        "read_file",
        "spawn",
        "process",
        "system",
        "command",
        "exec",
        "http",
        "network",
        "module",
    ] {
        if contains_identifier(&code, forbidden) {
            return Err(format!(
                "Rhai 脚本包含禁止的能力：{forbidden}（文件、网络、进程和系统命令不可用）"
            ));
        }
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let mut engine = Engine::new();
    configure_engine(&mut engine, Arc::clone(&cancel));
    engine.set_optimization_level(rhai::OptimizationLevel::None);
    register_api(
        &mut engine,
        Arc::new(Mutex::new(validation_context(cancel))),
    );
    let native = registered_api_signatures(&engine);
    let known_engine_names: HashSet<String> = engine
        .collect_fn_metadata(None, |info| Some(info.metadata.name.to_string()), true)
        .into_iter()
        .collect();
    let ast = compile_source(&engine, source)?;
    let mut scripted: HashMap<String, Vec<usize>> = HashMap::new();
    let mut shadowed = HashSet::new();
    for function in ast.iter_functions() {
        let arities = scripted.entry(function.name.to_owned()).or_default();
        arities.push(function.params.len());
        shadowed.extend(function.params.iter().map(|name| (*name).to_owned()));
    }
    ast.walk(&mut |path| {
        if let Some(ASTNode::Stmt(statement)) = path.last() {
            match statement {
                Stmt::Var(data, ..) => {
                    shadowed.insert(data.0.name.to_string());
                }
                Stmt::For(data, ..) => {
                    shadowed.insert(data.0.name.to_string());
                    if let Some(index) = &data.1 {
                        shadowed.insert(index.name.to_string());
                    }
                }
                Stmt::TryCatch(data, ..) => {
                    if let Expr::Variable(variable, ..) = &data.expr {
                        shadowed.insert(variable.1.to_string());
                    }
                }
                _ => (),
            }
        }
        true
    });
    let mut result = RhaiValidationReport::default();
    let mut error = None;
    let environment = StaticApiEnvironment {
        native: &native,
        known_engine_names: &known_engine_names,
        scripted: &scripted,
        shadowed: &shadowed,
    };
    ast.walk(&mut |path| {
        let current = path.last().copied();
        let (call, position, receiver): (&FnCallExpr, Position, Option<&Expr>) = match current {
            Some(ASTNode::Stmt(Stmt::FnCall(call, position))) => (call, *position, None),
            Some(ASTNode::Expr(Expr::FnCall(call, position))) => (call, *position, None),
            Some(ASTNode::Expr(Expr::MethodCall(call, position))) => {
                let receiver = path.iter().rev().nth(1).and_then(|parent| match parent {
                    ASTNode::Expr(Expr::Dot(binary, ..)) => Some(&binary.lhs),
                    _ => None,
                });
                // A receiver not present in this AST shape cannot be safely
                // counted/typed; preserve the call for runtime confirmation.
                if receiver.is_none() {
                    unchecked_call(&mut result, &call.name, *position, "方法接收者无法静态确认");
                    return true;
                }
                (call, *position, receiver)
            }
            _ => return true,
        };
        if call.is_operator_call() {
            return true;
        }
        if let Err(problem) = environment.check_call(&mut result, call, receiver, position) {
            error = Some(problem);
            return false;
        }
        true
    });
    error.map_or(Ok(result), Err)
}

fn compile_source(engine: &Engine, source: &str) -> Result<rhai::AST, String> {
    engine
        .compile(source)
        .map_err(|error| format_rhai_parse_error(error.to_string(), error.position()))
}

fn format_rhai_parse_error(message: String, position: Position) -> String {
    if position.is_none() {
        format!("Rhai 语法错误：{message}")
    } else {
        format!(
            "Rhai 语法错误（第 {} 行第 {} 列）：{message}",
            position.line().unwrap_or(0),
            position.position().unwrap_or(0)
        )
    }
}

fn format_rhai_error(error: Box<EvalAltResult>, fallback: &str) -> String {
    let position = error.position();
    if position.is_none() {
        format!("Rhai {fallback}：{error}")
    } else {
        format!(
            "Rhai {fallback}（第 {} 行第 {} 列）：{error}",
            position.line().unwrap_or(0),
            position.position().unwrap_or(0)
        )
    }
}

fn contains_identifier(source: &str, identifier: &str) -> bool {
    let mut offset = 0;
    while let Some(found) = source[offset..].find(identifier) {
        let start = offset + found;
        let end = start + identifier.len();
        let before = source[..start].chars().next_back();
        let after = source[end..].chars().next();
        if !before.is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
            && !after.is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            return true;
        }
        offset = end;
    }
    false
}

fn strip_strings_and_comments(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let mut quote = None;
    let mut escaped = false;
    let mut line_comment = false;
    let mut characters = source.chars().peekable();
    while let Some(character) = characters.next() {
        if line_comment {
            result.push(if character == '\n' { '\n' } else { ' ' });
            if character == '\n' {
                line_comment = false;
            }
            continue;
        }
        if let Some(current_quote) = quote {
            result.push(if character == '\n' { '\n' } else { ' ' });
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == current_quote {
                quote = None;
            }
            continue;
        }
        if character == '/' && characters.peek() == Some(&'/') {
            characters.next();
            line_comment = true;
            result.push_str("  ");
            continue;
        }
        if character == '"' || character == '\'' {
            quote = Some(character);
            result.push(' ');
        } else {
            result.push(character);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::ImageMatch;
    use std::sync::atomic::AtomicUsize;

    struct TestInput;

    impl AutomationInput for TestInput {
        fn wait_ms(&self, _: u64, _: f32, _: &AtomicBool) -> Result<(), String> {
            Ok(())
        }
        fn wait_random_ms(&self, _: u64, _: u64, _: f32, _: &AtomicBool) -> Result<(), String> {
            Ok(())
        }
        fn key_down(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn key_up(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn move_to(&self, _: i32, _: i32) -> Result<(), String> {
            Ok(())
        }
        fn mouse_down(&self, _: &str, _: i32, _: i32) -> Result<(), String> {
            Ok(())
        }
        fn mouse_up(&self, _: &str, _: i32, _: i32) -> Result<(), String> {
            Ok(())
        }
        fn scroll(&self, _: i32, _: i32) -> Result<(), String> {
            Ok(())
        }
        fn type_text(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    struct CountingInput {
        calls: Arc<AtomicUsize>,
    }

    impl AutomationInput for CountingInput {
        fn wait_ms(&self, _: u64, _: f32, _: &AtomicBool) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn wait_random_ms(&self, _: u64, _: u64, _: f32, _: &AtomicBool) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn key_down(&self, _: &str) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn key_up(&self, _: &str) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn move_to(&self, _: i32, _: i32) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn mouse_down(&self, _: &str, _: i32, _: i32) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn mouse_up(&self, _: &str, _: i32, _: i32) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn scroll(&self, _: i32, _: i32) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn type_text(&self, _: &str) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct CancellingVision;

    impl VisionApi for CancellingVision {
        fn active_window_title(&self) -> Result<String, VisionError> {
            unreachable!("unused mock vision method")
        }

        fn window_exists(&self, _: &str) -> Result<bool, VisionError> {
            unreachable!("unused mock vision method")
        }

        fn window_rect(&self, _: &str) -> Result<WindowRectValue, VisionError> {
            unreachable!("unused mock vision method")
        }

        fn wait_window(&self, _: &str, _: VisionPollOptions<'_>) -> Result<bool, VisionError> {
            unreachable!("unused mock vision method")
        }

        fn pixel_matches(
            &self,
            _: Point,
            _: RgbColor,
            _: u8,
            _: &AtomicBool,
        ) -> Result<bool, VisionError> {
            unreachable!("unused mock vision method")
        }

        fn wait_pixel(
            &self,
            _: Point,
            _: RgbColor,
            _: u8,
            _: VisionPollOptions<'_>,
        ) -> Result<bool, VisionError> {
            unreachable!("unused mock vision method")
        }

        fn find_image(
            &self,
            _: &str,
            _: ScreenRect,
            _: f32,
            cancel: &AtomicBool,
        ) -> Result<Option<ImageMatch>, VisionError> {
            // Model a vision result that finishes after F12 has invalidated
            // the run, but still returns a normal value to its caller.
            cancel.store(true, Ordering::SeqCst);
            Ok(None)
        }

        fn wait_image(
            &self,
            _: &str,
            _: ScreenRect,
            _: f32,
            _: VisionPollOptions<'_>,
        ) -> Result<Option<ImageMatch>, VisionError> {
            unreachable!("unused mock vision method")
        }
    }

    #[test]
    fn rhai_api_validates_arguments_and_operation_limit() {
        let cancel = Arc::new(AtomicBool::new(false));
        let context = ExecutionContext::new(Arc::new(TestInput), Arc::clone(&cancel), 1.0, None);
        let error = run_rhai_script("wait_ms(-1);", context).expect_err("negative wait");
        assert!(error.contains("非负整数"));

        let context = ExecutionContext::new(Arc::new(TestInput), cancel, 1.0, None);
        let error = run_rhai_script(
            "let count = 0; while count < 100001 { count += 1; press(\"A\"); }",
            context,
        )
        .expect_err("operation limit");
        assert!(error.contains("最大操作数") || error.contains("Too many operations"));
    }

    #[test]
    fn cancellation_releases_context_and_reports_f12() {
        let cancel = Arc::new(AtomicBool::new(true));
        let context = ExecutionContext::new(Arc::new(TestInput), cancel, 1.0, None);
        let error = run_rhai_script("key_down(\"A\");", context).expect_err("cancelled script");
        assert!(error.contains("F12"));
    }

    #[test]
    fn late_vision_return_cannot_reach_the_next_input_action_after_cancellation() {
        let cancel = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let context = ExecutionContext::new_with_vision(
            Arc::new(CountingInput {
                calls: Arc::clone(&calls),
            }),
            Arc::clone(&cancel),
            1.0,
            None,
            Arc::new(CancellingVision),
        );

        let error = run_rhai_script(
            r#"find_image("late.png", 0, 0, 100, 100, 0.9); press("A");"#,
            context,
        )
        .expect_err("the next action must observe cancellation");

        assert!(error.contains("F12"));
        assert!(cancel.load(Ordering::SeqCst));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn stop_with_message_ends_script_as_a_successful_user_stop() {
        let cancel = Arc::new(AtomicBool::new(false));
        let context = ExecutionContext::new(Arc::new(TestInput), cancel, 1.0, None);
        let outcome = run_rhai_script(
            r#"key_down("A"); stop_with_message("完成", "任务已经结束"); press("B");"#,
            context,
        )
        .expect("custom stop should not be reported as a runtime failure");
        assert_eq!(
            outcome,
            ScriptOutcome::StoppedWithMessage(ScriptStopMessage {
                title: "完成".to_string(),
                message: "任务已经结束".to_string(),
                mode: ScriptStopMode::Background,
            })
        );
    }

    #[test]
    fn legacy_one_argument_stop_uses_default_title_and_blocks_later_input() {
        let calls = Arc::new(AtomicUsize::new(0));
        let context = ExecutionContext::new(
            Arc::new(CountingInput {
                calls: Arc::clone(&calls),
            }),
            Arc::new(AtomicBool::new(false)),
            1.0,
            None,
        );
        assert_eq!(
            run_rhai_script(r#"stop_with_message("任务已经结束"); press("B");"#, context,)
                .expect("legacy one-argument stop"),
            ScriptOutcome::StoppedWithMessage(ScriptStopMessage {
                title: "AutoFlow".into(),
                message: "任务已经结束".into(),
                mode: ScriptStopMode::Background,
            })
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn stop_with_message_supports_background_and_foreground_options() {
        for (source, expected_mode) in [
            (
                r#"stop_with_message("完成", #{});"#,
                ScriptStopMode::Background,
            ),
            (
                r#"stop_with_message("完成", #{ mode: "background" });"#,
                ScriptStopMode::Background,
            ),
            (
                r#"stop_with_message("完成", #{ mode: "foreground" });"#,
                ScriptStopMode::Foreground,
            ),
        ] {
            let context = ExecutionContext::new(
                Arc::new(TestInput),
                Arc::new(AtomicBool::new(false)),
                1.0,
                None,
            );
            assert_eq!(
                run_rhai_script(source, context).expect("valid stop options"),
                ScriptOutcome::StoppedWithMessage(ScriptStopMessage {
                    title: "AutoFlow".into(),
                    message: "完成".into(),
                    mode: expected_mode,
                })
            );
        }
    }

    #[test]
    fn stop_with_message_two_strings_remain_title_and_message() {
        let context = ExecutionContext::new(
            Arc::new(TestInput),
            Arc::new(AtomicBool::new(false)),
            1.0,
            None,
        );
        assert_eq!(
            run_rhai_script(r#"stop_with_message("完成", "foreground");"#, context)
                .expect("two-string overload"),
            ScriptOutcome::StoppedWithMessage(ScriptStopMessage {
                title: "完成".into(),
                message: "foreground".into(),
                mode: ScriptStopMode::Background,
            })
        );
    }

    #[test]
    fn stop_with_message_rejects_invalid_options_explicitly() {
        for (source, expected) in [
            (
                r#"stop_with_message("完成", #{ surprise: true });"#,
                "未知字段",
            ),
            (
                r#"stop_with_message("完成", #{ mode: 1 });"#,
                "mode 必须是字符串",
            ),
            (
                r#"stop_with_message("完成", #{ mode: "urgent" });"#,
                "mode 必须是 background 或 foreground",
            ),
        ] {
            let context = ExecutionContext::new(
                Arc::new(TestInput),
                Arc::new(AtomicBool::new(false)),
                1.0,
                None,
            );
            let error = run_rhai_script(source, context).expect_err("invalid stop options");
            assert!(error.contains(expected), "unexpected error: {error}");
        }
    }

    #[test]
    fn stop_with_message_rejects_empty_content() {
        let cancel = Arc::new(AtomicBool::new(false));
        let context = ExecutionContext::new(Arc::new(TestInput), cancel, 1.0, None);
        let error = run_rhai_script(r#"stop_with_message(" ");"#, context)
            .expect_err("empty popup content should be rejected");
        assert!(error.contains("提示内容不能为空"));
    }

    #[test]
    fn native_biomimetic_api_accepts_action_context_maps() {
        let cancel = Arc::new(AtomicBool::new(false));
        let context = ExecutionContext::new(Arc::new(TestInput), cancel, 1.0, None);
        run_rhai_script(
            r#"
                bio_move_to(820, 430);
                bio_move_to(820, 430, #{ target_width: 80, intent: "click" });
                bio_click("left", 820, 430);
                bio_type_text("demo", #{ mode: "normal" });
            "#,
            context,
        )
        .expect("native bio API should execute");
    }

    #[test]
    fn forbidden_capabilities_are_rejected() {
        assert!(validate_rhai_source("import \"fs\";").is_err());
        assert!(validate_rhai_source("let value = eval(\"press('A')\");").is_err());
    }

    #[test]
    fn direct_move_to_missing_coordinate_is_rejected_before_execution() {
        let missing = "let x = 1; move_to(x,  );";
        assert!(validate_rhai_source(missing)
            .expect_err("trailing comma is not a second argument")
            .contains("参数个数"));
        assert!(validate_rhai_source("move_to(10)").is_err());
        assert!(validate_rhai_source("move_to(10, 20)").is_ok());
        let calls = Arc::new(AtomicUsize::new(0));
        let context = ExecutionContext::new(
            Arc::new(CountingInput {
                calls: Arc::clone(&calls),
            }),
            Arc::new(AtomicBool::new(false)),
            1.0,
            None,
        );
        assert!(run_rhai_script(missing, context).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn conservative_move_to_checker_ignores_ambiguous_source() {
        assert!(validate_rhai_source("// move_to(x, );\nmove_to(10, 20);").is_ok());
        assert!(validate_rhai_source("let s = \"move_to(x, );\"; move_to(10, 20);").is_ok());
        assert!(validate_rhai_source("fn move_to(x) { x } move_to(10);").is_ok());
        assert!(validate_rhai_source("move_to((10 + 1), 20);").is_ok());
        assert!(validate_rhai_source("let x = 10; x. move_to(20);").is_ok());
        assert!(validate_rhai_source("let x = 1; /* move_to(x, ); */ move_to(x, 20);").is_ok());
        let shadowed = "let move_to = Fn(\"print\"); move_to(10);";
        assert!(
            validate_rhai_source(shadowed).is_ok(),
            "{:?}",
            validate_rhai_source(shadowed)
        );
    }

    #[test]
    fn all_registered_autoflow_api_overloads_are_validated_from_real_signatures() {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut engine = Engine::new();
        register_api(
            &mut engine,
            Arc::new(Mutex::new(validation_context(cancel))),
        );
        let signatures = registered_api_signatures(&engine);
        let count = signatures.values().map(Vec::len).sum::<usize>();
        assert_eq!(
            (signatures.len(), count),
            (24, 33),
            "update this contract when API registration changes"
        );

        for (name, overloads) in &signatures {
            for overload in overloads {
                let args = overload
                    .argument_types
                    .iter()
                    .map(|type_id| {
                        if *type_id == TypeId::of::<i64>() {
                            "1"
                        } else if *type_id == TypeId::of::<f64>() {
                            "0.5"
                        } else if *type_id == TypeId::of::<ImmutableString>() {
                            "\"A\""
                        } else if *type_id == TypeId::of::<Map>() {
                            "#{}"
                        } else {
                            panic!("unknown API parameter TypeId in {name}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let valid = format!("{name}({args});");
                assert!(validate_rhai_source(&valid).is_ok(), "valid {valid}");
            }
            let highest = overloads
                .iter()
                .map(|sig| sig.argument_types.len())
                .max()
                .unwrap();
            for count in 0..=highest + 1 {
                if overloads
                    .iter()
                    .any(|sig| sig.argument_types.len() == count)
                {
                    continue;
                }
                let args = vec!["1"; count].join(", ");
                let invalid = format!("{name}({args});");
                assert!(
                    validate_rhai_source(&invalid)
                        .unwrap_err()
                        .contains("参数个数"),
                    "invalid {invalid}"
                );
            }
        }
    }

    #[test]
    fn inspection_distinguishes_static_errors_from_runtime_unknowns() {
        let wrong_arity = "// leading\nmove_to(1,);";
        let error =
            inspect_rhai_source(wrong_arity).expect_err("trailing comma is not an argument");
        assert!(error.contains("第 2 行第 1 列"), "{error}");
        assert!(inspect_rhai_source("move_to(1, true)")
            .unwrap_err()
            .contains("类型"));
        assert!(inspect_rhai_source("click(1, 2)").is_ok());
        assert!(inspect_rhai_source("click(\"left\", 1)")
            .unwrap_err()
            .contains("类型"));
        assert!(
            inspect_rhai_source("stop_with_message(\"ok\", #{ mode: \"foreground\" })").is_ok()
        );
        assert!(inspect_rhai_source("stop_with_message(\"ok\", 1)")
            .unwrap_err()
            .contains("类型"));
        assert!(inspect_rhai_source("foo(1)")
            .unwrap_err()
            .contains("未识别"));
        assert!(inspect_rhai_source("fn foo(a, b) { a + b } foo(1)")
            .unwrap_err()
            .contains("用户函数"));
        let dynamic_method = inspect_rhai_source("let x = 1; x.move_to(2, 3)")
            .expect("dynamic receiver might be a map method");
        assert!(dynamic_method
            .unverified_calls
            .iter()
            .any(|call| call.name == "move_to"));
        assert!(inspect_rhai_source("1.move_to(20, 30)")
            .unwrap_err()
            .contains("参数个数"));
        assert!(inspect_rhai_source("1.move_to(20)").is_ok());
        assert!(inspect_rhai_source("\"A\".press()").is_ok());
        assert!(inspect_rhai_source("\"A\".press(1)")
            .unwrap_err()
            .contains("参数个数"));
        assert!(inspect_rhai_source("fn move_to(a) { a } move_to(10)").is_ok());
        assert!(
            inspect_rhai_source("let f = Fn(\"print\"); f(1)").is_ok(),
            "{:?}",
            inspect_rhai_source("let f = Fn(\"print\"); f(1)")
        );
        let unresolved = inspect_rhai_source("let x = 1; move_to(x, 20)")
            .expect("variable type remains dynamic");
        assert!(unresolved
            .unverified_calls
            .iter()
            .any(|call| call.name == "move_to" && call.line == Some(1)));
        assert!(inspect_rhai_source(
            "let s = \"move_to(1,)\"; /* move_to(1,) */ move_to((1 + 1), 20)"
        )
        .is_ok());
        assert!(inspect_rhai_source("let x = 1; x.move_to(20)").is_ok());
    }

    #[test]
    fn scripted_method_uses_implicit_this_without_native_receiver_arity() {
        let source = "fn foo(a) { this + a } let x = 1; x.foo(2)";
        let report = inspect_rhai_source(source).expect("script method is legal");
        assert!(report
            .unverified_calls
            .iter()
            .any(|call| call.name == "foo"));
        assert_eq!(
            Engine::new()
                .eval::<i64>(source)
                .expect("pure Rhai evaluation"),
            3
        );

        let shadowing = "fn move_to(a) { this + a } 1.move_to(2)";
        let report =
            inspect_rhai_source(shadowing).expect("script method takes precedence over API");
        assert!(report
            .unverified_calls
            .iter()
            .any(|call| call.name == "move_to"));
        assert_eq!(
            Engine::new()
                .eval::<i64>(shadowing)
                .expect("pure Rhai evaluation"),
            3
        );

        let wrong = inspect_rhai_source("fn foo(a, b) { a + b } 1.foo()")
            .expect_err("script method has zero explicit arguments");
        assert!(wrong.contains("用户函数参数个数为 0，允许 2 个"), "{wrong}");
    }

    #[test]
    fn object_fnptr_methods_remain_unverified_instead_of_falsely_rejected() {
        let source = "fn foo(a) { a + 1 } let o = #{ f: Fn(\"foo\") }; o.f(1)";
        let report = inspect_rhai_source(source).expect("object FnPtr method is legal");
        assert!(report.unverified_calls.iter().any(|call| call.name == "f"));
        assert_eq!(
            Engine::new()
                .eval::<i64>(source)
                .expect("pure Rhai evaluation"),
            2
        );

        let matching_api_name =
            "fn foo(a) { a + 1 } let o = #{ move_to: Fn(\"foo\") }; o.move_to(1)";
        let report = inspect_rhai_source(matching_api_name).expect("map FnPtr method shadows API");
        assert!(report
            .unverified_calls
            .iter()
            .any(|call| call.name == "move_to"));
        assert_eq!(
            Engine::new()
                .eval::<i64>(matching_api_name)
                .expect("pure Rhai evaluation"),
            2
        );
    }

    #[test]
    fn catch_binding_of_function_pointer_remains_unverified() {
        let source = "fn target(a) { a + 1 } try { throw Fn(\"target\") } catch(f) { f(1) }";
        let report = inspect_rhai_source(source).expect("catch binding is a dynamic callable");
        assert!(report.unverified_calls.iter().any(|call| call.name == "f"));
        // Rhai 1.26 does not resolve this catch-bound variable as a direct
        // function call; static inspection must still not claim certainty.
        assert!(Engine::new().eval::<i64>(source).is_err());

        let explicit = "fn target(a) { a + 1 } try { throw Fn(\"target\") } catch(f) { f.call(1) }";
        let report =
            inspect_rhai_source(explicit).expect("explicit FnPtr dispatch remains unverified");
        assert!(report
            .unverified_calls
            .iter()
            .any(|call| call.name == "call"));
        let _ = Engine::new()
            .eval::<Dynamic>(explicit)
            .expect("pure Rhai evaluation succeeds");
    }

    #[test]
    fn method_form_move_to_is_a_real_two_argument_call() {
        let calls = Arc::new(AtomicUsize::new(0));
        let context = ExecutionContext::new(
            Arc::new(CountingInput {
                calls: Arc::clone(&calls),
            }),
            Arc::new(AtomicBool::new(false)),
            1.0,
            None,
        );
        run_rhai_script("let x = 10; x. move_to(20);", context)
            .expect("Rhai accepts method syntax for this native API");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn image_match_options_validate_modes_and_candidate_count() {
        let legacy = checked_match_options(&Map::new()).expect("legacy image options");
        assert_eq!(legacy, MatcherOptions::default());

        let mut options = Map::new();
        options.insert("mode".into(), Dynamic::from("fast"));
        options.insert("prefer_last".into(), Dynamic::from(false));
        options.insert("max_candidates".into(), Dynamic::from(4_i64));
        options.insert("scale_min".into(), Dynamic::from(0.65_f64));
        options.insert("scale_max".into(), Dynamic::from(1.6_f64));
        options.insert("scale_step".into(), Dynamic::from(0.1_f64));
        let parsed = checked_match_options(&options).expect("valid image options");
        assert_eq!(parsed.mode, MatcherMode::Fast);
        assert!(!parsed.prefer_last);
        assert_eq!(parsed.max_candidates, 4);
        assert!((parsed.scale_min - 0.65).abs() < 0.001);
        assert!((parsed.scale_max - 1.6).abs() < 0.001);
        assert_eq!(parsed.scale_step, Some(0.1));

        let mut invalid_mode = Map::new();
        invalid_mode.insert("mode".into(), Dynamic::from("turbo"));
        let error = checked_match_options(&invalid_mode).expect_err("invalid mode");
        assert!(error.contains("vision_match_mode_invalid"));

        let mut invalid_count = Map::new();
        invalid_count.insert("max_candidates".into(), Dynamic::from(0_i64));
        let error = checked_match_options(&invalid_count).expect_err("invalid candidate count");
        assert!(error.contains("vision_match_options_invalid"));

        let mut invalid_scale = Map::new();
        invalid_scale.insert("scale_min".into(), Dynamic::from(2.1_f64));
        let error = checked_match_options(&invalid_scale).expect_err("invalid scale");
        assert!(error.contains("vision_scale_invalid"));

        let context = ExecutionContext::new(
            Arc::new(TestInput),
            Arc::new(AtomicBool::new(false)),
            1.0,
            None,
        );
        let error = run_rhai_script(
            r#"find_image("missing.png", 0, 0, 100, 100, 0.9, #{ mode: "turbo" });"#,
            context,
        )
        .expect_err("Rhai image options overload");
        assert!(error.contains("vision_match_mode_invalid"));
    }

    #[test]
    fn image_result_map_contains_compatible_diagnostics() {
        let result = VisionSearchResult {
            image: None,
            diagnostics: crate::automation::VisionDiagnostics::for_mode(MatcherMode::Auto),
        };
        let map = image_search_map(result);
        for key in [
            "found",
            "score",
            "total_ms",
            "capture_ms",
            "prepare_ms",
            "coarse_ms",
            "refine_ms",
            "fallback_ms",
            "candidate_count",
            "previous_hit_used",
            "fallback_used",
            "matcher_mode",
            "matched_scale",
            "scale_candidates",
            "scale_search_ms",
            "matched_width",
            "matched_height",
            "robust_verify_used",
            "robust_verify_ms",
            "robust_score",
            "valid_tile_count",
            "discarded_tile_count",
            "alpha_mask_used",
            "anchor_recovery_used",
            "anchor_candidate_count",
            "preferred_scale_hit",
            "single_match_ms",
            "wait_total_ms",
        ] {
            assert!(map.contains_key(key), "missing diagnostic key: {key}");
        }
        assert!(map["matched_scale"].is_unit());
        assert!(map["robust_score"].is_unit());

        let mut hit_diagnostics = crate::automation::VisionDiagnostics::for_mode(MatcherMode::Auto);
        hit_diagnostics.matched_scale = Some(1.25);
        hit_diagnostics.scale_candidates = vec![0.8, 1.25];
        hit_diagnostics.matched_width = 56;
        hit_diagnostics.matched_height = 51;
        hit_diagnostics.robust_verify_used = true;
        hit_diagnostics.robust_score = Some(0.98);
        hit_diagnostics.valid_tile_count = 8;
        hit_diagnostics.discarded_tile_count = 1;
        hit_diagnostics.alpha_mask_used = true;
        hit_diagnostics.anchor_recovery_used = true;
        hit_diagnostics.anchor_candidate_count = 4;
        hit_diagnostics.preferred_scale_hit = true;
        hit_diagnostics.single_match_ms = 12;
        hit_diagnostics.wait_total_ms = 64;
        let hit_map = image_search_map(VisionSearchResult {
            image: Some(crate::automation::ImageMatch {
                x: 10,
                y: 20,
                width: 56,
                height: 51,
                center_x: 38,
                center_y: 45,
                score: 1.0,
            }),
            diagnostics: hit_diagnostics,
        });
        assert_eq!(
            hit_map["matched_scale"].clone().try_cast::<f64>(),
            Some(1.25)
        );
        assert_eq!(hit_map["matched_width"].clone().try_cast::<i64>(), Some(56));
        assert_eq!(
            hit_map["matched_height"].clone().try_cast::<i64>(),
            Some(51)
        );
        assert!(
            (hit_map["robust_score"]
                .clone()
                .try_cast::<f64>()
                .expect("robust score")
                - 0.98)
                .abs()
                < 0.001
        );
        assert_eq!(
            hit_map["valid_tile_count"].clone().try_cast::<i64>(),
            Some(8)
        );
        assert_eq!(hit_map["wait_total_ms"].clone().try_cast::<i64>(), Some(64));
    }
}
