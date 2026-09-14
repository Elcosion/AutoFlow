use crate::AppError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub mod v2;

pub const BEHAVIOR_API_VERSION: u32 = 1;
pub const MAX_BEHAVIOR_PROFILE_NAME: usize = 64;
pub const MAX_BEHAVIOR_EVENTS: u64 = 250_000;
pub const MIN_BEHAVIOR_EVENTS: u64 = 8;
pub const BEHAVIOR_INPUT_API_VERSION: u32 = 1;

const MAX_DISTRIBUTION_VALUE: f32 = 120_000.0;
const MAX_RUNTIME_DELAY_MS: u64 = 120_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorDistribution {
    pub samples: u32,
    pub mean: f32,
    pub std_dev: f32,
    pub min: f32,
    pub max: f32,
    pub p95: f32,
}

impl BehaviorDistribution {
    pub fn validate(&self, label: &str) -> Result<(), AppError> {
        let values = [self.mean, self.std_dev, self.min, self.max, self.p95];
        if values
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
        {
            return Err(AppError::invalid(
                "behavior_distribution_invalid",
                format!("行为档案的 {label} 分布包含无效数字"),
            ));
        }
        if self.max > MAX_DISTRIBUTION_VALUE
            || self.min > self.mean
            || self.mean > self.max
            || self.p95 < self.min
            || self.p95 > self.max
        {
            return Err(AppError::invalid(
                "behavior_distribution_invalid",
                format!("行为档案的 {label} 分布范围无效"),
            ));
        }
        Ok(())
    }

    fn fallback(mean: f32, std_dev: f32, min: f32, max: f32, p95: f32) -> Self {
        Self {
            samples: 0,
            mean,
            std_dev,
            min,
            max,
            p95,
        }
    }

    fn sample(&self, rng: &mut Random) -> f32 {
        let spread = self.std_dev.max((self.max - self.min) / 8.0);
        let gaussianish = (0..12).map(|_| rng.next_unit()).sum::<f32>() - 6.0;
        (self.mean + gaussianish * spread).clamp(self.min, self.max)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorProfile {
    pub id: String,
    pub name: String,
    pub api_version: u32,
    pub sample_count: u64,
    pub duration_ms: u64,
    pub keyboard_events: u64,
    pub mouse_events: u64,
    pub wheel_events: u64,
    pub key_hold_ms: BehaviorDistribution,
    pub key_interval_ms: BehaviorDistribution,
    pub click_hold_ms: BehaviorDistribution,
    pub click_interval_ms: BehaviorDistribution,
    pub mouse_speed_px_per_sec: BehaviorDistribution,
    pub mouse_pause_ms: BehaviorDistribution,
    pub mouse_direction_change_rate: f32,
    pub mouse_jitter_px: f32,
    #[serde(default)]
    pub raw_events: Vec<BehaviorEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BehaviorEvent {
    Key {
        #[serde(rename = "timestampMs")]
        timestamp_ms: u64,
        vk: u32,
        #[serde(rename = "scanCode")]
        scan_code: u32,
        #[serde(rename = "isDown")]
        is_down: bool,
    },
    MouseMove {
        #[serde(rename = "timestampMs")]
        timestamp_ms: u64,
        x: i32,
        y: i32,
    },
    MouseButton {
        #[serde(rename = "timestampMs")]
        timestamp_ms: u64,
        button: u8,
        #[serde(rename = "isDown")]
        is_down: bool,
        x: i32,
        y: i32,
    },
    Wheel {
        #[serde(rename = "timestampMs")]
        timestamp_ms: u64,
        #[serde(rename = "deltaX")]
        delta_x: i32,
        #[serde(rename = "deltaY")]
        delta_y: i32,
        x: i32,
        y: i32,
    },
}

impl BehaviorEvent {
    fn timestamp_ms(&self) -> u64 {
        match self {
            Self::Key { timestamp_ms, .. }
            | Self::MouseMove { timestamp_ms, .. }
            | Self::MouseButton { timestamp_ms, .. }
            | Self::Wheel { timestamp_ms, .. } => *timestamp_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorProfileFile {
    pub id: String,
    pub name: String,
    pub file_name: String,
    pub sample_count: u64,
    pub duration_ms: u64,
    pub keyboard_events: u64,
    pub mouse_events: u64,
    pub wheel_events: u64,
    pub created_at_ms: u64,
}

impl BehaviorProfileFile {
    pub fn from_profile(profile: &BehaviorProfile, file_name: String) -> Self {
        Self {
            id: profile.id.clone(),
            name: profile.name.clone(),
            file_name,
            sample_count: profile.sample_count,
            duration_ms: profile.duration_ms,
            keyboard_events: profile.keyboard_events,
            mouse_events: profile.mouse_events,
            wheel_events: profile.wheel_events,
            created_at_ms: now_millis(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BiomimeticInput {
    pub id: String,
    pub name: String,
    pub api_version: u32,
    pub source_profile_ids: Vec<String>,
    pub created_at_ms: u64,
    pub profile: BehaviorProfile,
}

impl BiomimeticInput {
    pub fn from_profile(profile: &BehaviorProfile) -> Self {
        let mut derived_profile = profile.clone();
        derived_profile.raw_events.clear();
        Self {
            id: format!("biomimetic-input-{}", now_nanos()),
            name: format_name(&format!("{} 仿生输入", profile.name)),
            api_version: BEHAVIOR_INPUT_API_VERSION,
            source_profile_ids: vec![profile.id.clone()],
            created_at_ms: now_millis(),
            profile: derived_profile,
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.id.trim().is_empty() || self.name.trim().is_empty() {
            return Err(AppError::invalid(
                "biomimetic_input_missing_name",
                "仿生输入文件需要填写名称和 ID",
            ));
        }
        if self.api_version != BEHAVIOR_INPUT_API_VERSION {
            return Err(AppError::invalid(
                "biomimetic_input_api_version",
                "仿生输入文件 API 版本不受支持",
            ));
        }
        if self.source_profile_ids.is_empty() {
            return Err(AppError::invalid(
                "biomimetic_input_missing_source",
                "仿生输入文件至少需要一个训练档案来源",
            ));
        }
        self.profile.validate()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BiomimeticInputFile {
    pub id: String,
    pub name: String,
    pub file_name: String,
    pub source_profile_ids: Vec<String>,
    pub created_at_ms: u64,
}

impl BiomimeticInputFile {
    pub fn from_input(input: &BiomimeticInput, file_name: String) -> Self {
        Self {
            id: input.id.clone(),
            name: input.name.clone(),
            file_name,
            source_profile_ids: input.source_profile_ids.clone(),
            created_at_ms: input.created_at_ms,
        }
    }
}

impl BehaviorProfile {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.id.trim().is_empty() || self.name.trim().is_empty() {
            return Err(AppError::invalid(
                "behavior_profile_missing_name",
                "行为档案需要填写名称和 ID",
            ));
        }
        if self.name.chars().count() > MAX_BEHAVIOR_PROFILE_NAME {
            return Err(AppError::invalid(
                "behavior_profile_name_too_long",
                "行为档案名称不能超过 64 个字符",
            ));
        }
        if self.api_version != BEHAVIOR_API_VERSION {
            return Err(AppError::invalid(
                "behavior_api_version",
                "行为档案 API 版本不受支持",
            ));
        }
        if self.sample_count == 0 || self.sample_count > MAX_BEHAVIOR_EVENTS {
            return Err(AppError::invalid(
                "behavior_sample_count_invalid",
                "行为档案的样本数量无效",
            ));
        }
        if self.duration_ms > 86_400_000 {
            return Err(AppError::invalid(
                "behavior_duration_invalid",
                "行为档案录制时长不能超过 24 小时",
            ));
        }
        if !self.mouse_direction_change_rate.is_finite()
            || !(0.0..=1.0).contains(&self.mouse_direction_change_rate)
            || !self.mouse_jitter_px.is_finite()
            || !(0.0..=500.0).contains(&self.mouse_jitter_px)
        {
            return Err(AppError::invalid(
                "behavior_mouse_metrics_invalid",
                "行为档案的鼠标特征超出允许范围",
            ));
        }
        for (label, distribution) in [
            ("按键保持时长", &self.key_hold_ms),
            ("按键间隔", &self.key_interval_ms),
            ("点击保持时长", &self.click_hold_ms),
            ("点击间隔", &self.click_interval_ms),
            ("鼠标速度", &self.mouse_speed_px_per_sec),
            ("鼠标停顿", &self.mouse_pause_ms),
        ] {
            distribution.validate(label)?;
        }
        if self.raw_events.len() as u64 > MAX_BEHAVIOR_EVENTS {
            return Err(AppError::invalid(
                "behavior_raw_event_count_invalid",
                "行为档案原始事件数量超过安全上限",
            ));
        }
        if !self.raw_events.is_empty() && self.raw_events.len() as u64 != self.sample_count {
            return Err(AppError::invalid(
                "behavior_raw_event_count_mismatch",
                "行为档案原始事件数量与样本数量不一致",
            ));
        }
        let mut previous_timestamp = 0;
        for event in &self.raw_events {
            if event.timestamp_ms() < previous_timestamp {
                return Err(AppError::invalid(
                    "behavior_raw_event_order",
                    "行为档案原始事件时间顺序无效",
                ));
            }
            previous_timestamp = event.timestamp_ms();
        }
        Ok(())
    }

    pub fn generated_api(&self) -> BehaviorApi {
        BehaviorApi {
            version: BEHAVIOR_API_VERSION,
            profile_id: self.id.clone(),
            profile_name: self.name.clone(),
            functions: vec![
                BehaviorApiFunction {
                    name: "bio_press".to_string(),
                    signature: "bio_press(key)".to_string(),
                    description: "按下并释放一个按键，使用当前行为档案的节奏".to_string(),
                },
                BehaviorApiFunction {
                    name: "bio_move_to".to_string(),
                    signature: "bio_move_to(x, y)".to_string(),
                    description: "沿带有速度、曲率和微小抖动的轨迹移动鼠标".to_string(),
                },
                BehaviorApiFunction {
                    name: "bio_click".to_string(),
                    signature: "bio_click(button, x, y)".to_string(),
                    description: "移动到坐标并以行为档案的按键节奏点击".to_string(),
                },
                BehaviorApiFunction {
                    name: "bio_type_text".to_string(),
                    signature: "bio_type_text(text)".to_string(),
                    description: "逐字符输入文本并应用个人按键间隔".to_string(),
                },
                BehaviorApiFunction {
                    name: "bio_scroll".to_string(),
                    signature: "bio_scroll(delta_x, delta_y)".to_string(),
                    description: "发送滚轮操作并应用个人操作停顿".to_string(),
                },
            ],
            source: format!(
                "// AutoFlow 仿生操作 API v{}\n// 当前档案：{} ({})\n// 在设置中启用仿生输入后，以下 API 会使用该档案。\n\nfn bio_press(key) {{ press(key); }}\nfn bio_move_to(x, y) {{ move_to(x, y); }}\nfn bio_click(button, x, y) {{ click(button, x, y); }}\nfn bio_type_text(text) {{ type_text(text); }}\nfn bio_scroll(delta_x, delta_y) {{ scroll(delta_x, delta_y); }}\n",
                BEHAVIOR_API_VERSION,
                escape_comment(&self.name),
                escape_comment(&self.id)
            ),
        }
    }

    pub fn combine(profiles: &[BehaviorProfile]) -> Result<Self, AppError> {
        if profiles.is_empty() {
            return Err(AppError::invalid(
                "behavior_profile_missing_source",
                "至少需要选择一个行为档案",
            ));
        }
        for profile in profiles {
            profile.validate()?;
        }

        let sample_count = profiles
            .iter()
            .map(|profile| profile.sample_count)
            .fold(0_u64, u64::saturating_add)
            .clamp(MIN_BEHAVIOR_EVENTS, MAX_BEHAVIOR_EVENTS);
        let duration_ms = profiles
            .iter()
            .map(|profile| profile.duration_ms)
            .fold(0_u64, u64::saturating_add)
            .min(86_400_000);
        let keyboard_events = profiles
            .iter()
            .map(|profile| profile.keyboard_events)
            .fold(0_u64, u64::saturating_add)
            .min(MAX_BEHAVIOR_EVENTS);
        let mouse_events = profiles
            .iter()
            .map(|profile| profile.mouse_events)
            .fold(0_u64, u64::saturating_add)
            .min(MAX_BEHAVIOR_EVENTS);
        let wheel_events = profiles
            .iter()
            .map(|profile| profile.wheel_events)
            .fold(0_u64, u64::saturating_add)
            .min(MAX_BEHAVIOR_EVENTS);
        let profile_weight = |profile: &BehaviorProfile| profile.sample_count.max(1) as f32;
        let weight_sum = profiles.iter().map(profile_weight).sum::<f32>().max(1.0);
        let weighted = |value: fn(&BehaviorProfile) -> f32| {
            profiles
                .iter()
                .map(|profile| value(profile) * profile.sample_count.max(1) as f32)
                .sum::<f32>()
                / weight_sum
        };

        let mouse_direction_change_rate =
            weighted(|profile| profile.mouse_direction_change_rate).clamp(0.0, 1.0);
        let mouse_jitter_px = weighted(|profile| profile.mouse_jitter_px).clamp(0.0, 500.0);
        let raw_events = Vec::new();
        let profile = Self {
            id: format!("behavior-combined-{}", now_nanos()),
            name: "组合仿生训练".to_string(),
            api_version: BEHAVIOR_API_VERSION,
            sample_count,
            duration_ms,
            keyboard_events,
            mouse_events,
            wheel_events,
            key_hold_ms: combine_distributions(
                &profiles
                    .iter()
                    .map(|profile| &profile.key_hold_ms)
                    .collect::<Vec<_>>(),
            ),
            key_interval_ms: combine_distributions(
                &profiles
                    .iter()
                    .map(|profile| &profile.key_interval_ms)
                    .collect::<Vec<_>>(),
            ),
            click_hold_ms: combine_distributions(
                &profiles
                    .iter()
                    .map(|profile| &profile.click_hold_ms)
                    .collect::<Vec<_>>(),
            ),
            click_interval_ms: combine_distributions(
                &profiles
                    .iter()
                    .map(|profile| &profile.click_interval_ms)
                    .collect::<Vec<_>>(),
            ),
            mouse_speed_px_per_sec: combine_distributions(
                &profiles
                    .iter()
                    .map(|profile| &profile.mouse_speed_px_per_sec)
                    .collect::<Vec<_>>(),
            ),
            mouse_pause_ms: combine_distributions(
                &profiles
                    .iter()
                    .map(|profile| &profile.mouse_pause_ms)
                    .collect::<Vec<_>>(),
            ),
            mouse_direction_change_rate,
            mouse_jitter_px,
            raw_events,
        };
        profile.validate()?;
        Ok(profile)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorRecordingStatus {
    pub active: bool,
    pub capture_started: bool,
    pub duration_ms: u64,
    pub event_count: u64,
    pub keyboard_events: u64,
    pub mouse_events: u64,
    pub wheel_events: u64,
    pub capped: bool,
    /// Whether the raw session will be persisted when recording stops.
    /// Raw events are always collected in bounded memory while recording.
    pub persisting_raw_session: bool,
    pub session_name: Option<String>,
}

#[derive(Debug)]
pub struct BehaviorRecordingResult {
    pub profile: BehaviorProfile,
    pub persist_raw_session: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorApi {
    pub version: u32,
    pub profile_id: String,
    pub profile_name: String,
    pub functions: Vec<BehaviorApiFunction>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorApiFunction {
    pub name: String,
    pub signature: String,
    pub description: String,
}

#[derive(Debug, Default)]
struct SampleStats {
    values: Vec<f32>,
    next_replacement: usize,
}

impl SampleStats {
    fn push(&mut self, value: f32) {
        if !value.is_finite() || value < 0.0 {
            return;
        }
        if self.values.len() == 4096 {
            self.values[self.next_replacement] = value;
            self.next_replacement = (self.next_replacement + 1) % self.values.len();
        } else {
            self.values.push(value);
        }
    }

    fn distribution(&self, fallback: BehaviorDistribution) -> BehaviorDistribution {
        if self.values.is_empty() {
            return fallback;
        }
        let mut sorted = self.values.clone();
        sorted.sort_by(f32::total_cmp);
        let mean = sorted.iter().sum::<f32>() / sorted.len() as f32;
        let variance = sorted
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f32>()
            / sorted.len() as f32;
        let percentile = |fraction: f32| {
            let index = ((sorted.len() - 1) as f32 * fraction).round() as usize;
            sorted[index]
        };
        BehaviorDistribution {
            samples: sorted.len().min(u32::MAX as usize) as u32,
            mean,
            std_dev: variance.sqrt(),
            min: sorted[0],
            max: *sorted.last().unwrap_or(&sorted[0]),
            p95: percentile(0.95),
        }
    }
}

#[derive(Debug, Default)]
pub struct BehaviorRecorder {
    active: bool,
    capture_started: bool,
    session_name: Option<String>,
    started_at: Option<Instant>,
    event_count: u64,
    keyboard_events: u64,
    mouse_events: u64,
    wheel_events: u64,
    capped: bool,
    key_down: HashMap<u32, Instant>,
    button_down: HashMap<u8, Instant>,
    last_key_down: Option<Instant>,
    last_click_down: Option<Instant>,
    last_mouse: Option<(Instant, i32, i32)>,
    last_direction: Option<(f32, f32)>,
    direction_samples: u64,
    direction_changes: u64,
    key_hold_ms: SampleStats,
    key_interval_ms: SampleStats,
    click_hold_ms: SampleStats,
    click_interval_ms: SampleStats,
    mouse_speed_px_per_sec: SampleStats,
    mouse_pause_ms: SampleStats,
    persist_raw_session: bool,
    raw_events: Vec<BehaviorEvent>,
}

impl BehaviorRecorder {
    pub fn start(&mut self, name: &str, persist_raw_session: bool) -> Result<(), AppError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(AppError::invalid(
                "behavior_profile_missing_name",
                "行为训练名称不能为空",
            ));
        }
        if name.chars().count() > MAX_BEHAVIOR_PROFILE_NAME {
            return Err(AppError::invalid(
                "behavior_profile_name_too_long",
                "行为训练名称不能超过 64 个字符",
            ));
        }
        if self.active {
            return Err(AppError::invalid(
                "behavior_recording_active",
                "行为训练录制已经在进行中",
            ));
        }
        *self = Self::default();
        self.active = true;
        self.session_name = Some(name.to_string());
        self.started_at = Some(Instant::now());
        self.persist_raw_session = persist_raw_session;
        Ok(())
    }

    pub fn status(&self) -> BehaviorRecordingStatus {
        BehaviorRecordingStatus {
            active: self.active,
            capture_started: self.capture_started,
            duration_ms: self
                .started_at
                .map(|started| started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
                .unwrap_or(0),
            event_count: self.event_count,
            keyboard_events: self.keyboard_events,
            mouse_events: self.mouse_events,
            wheel_events: self.wheel_events,
            capped: self.capped,
            persisting_raw_session: self.persist_raw_session,
            session_name: self.session_name.clone(),
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn record_key(&mut self, key: u32, scan_code: u32, is_down: bool) {
        if !self.accept_event() {
            return;
        }
        let now = Instant::now();
        self.retain_event(BehaviorEvent::Key {
            timestamp_ms: self.timestamp_ms(now),
            vk: key,
            scan_code,
            is_down,
        });
        self.keyboard_events += 1;
        if is_down {
            if let Some(previous) = self.last_key_down {
                self.key_interval_ms
                    .push(duration_ms(now.duration_since(previous)));
            }
            self.last_key_down = Some(now);
            self.key_down.entry(key).or_insert(now);
        } else if let Some(started) = self.key_down.remove(&key) {
            self.key_hold_ms
                .push(duration_ms(now.duration_since(started)));
        }
    }

    pub fn record_mouse_move(&mut self, x: i32, y: i32) {
        if !self.accept_event() {
            return;
        }
        let now = Instant::now();
        self.retain_event(BehaviorEvent::MouseMove {
            timestamp_ms: self.timestamp_ms(now),
            x,
            y,
        });
        self.mouse_events += 1;
        if let Some((previous_at, previous_x, previous_y)) = self.last_mouse {
            let elapsed = previous_at.elapsed();
            let elapsed_ms = duration_ms(elapsed);
            let dx = (x - previous_x) as f32;
            let dy = (y - previous_y) as f32;
            let distance = dx.hypot(dy);
            if elapsed_ms >= 4.0 && distance > 0.5 {
                let speed = distance / elapsed.as_secs_f32().max(0.001);
                self.mouse_speed_px_per_sec
                    .push(speed.clamp(0.0, MAX_DISTRIBUTION_VALUE));
                let direction = (dx / distance, dy / distance);
                if let Some(previous) = self.last_direction {
                    self.direction_samples += 1;
                    if previous.0 * direction.0 + previous.1 * direction.1 < 0.65 {
                        self.direction_changes += 1;
                    }
                }
                self.last_direction = Some(direction);
            } else if elapsed_ms >= 8.0 {
                self.mouse_pause_ms.push(elapsed_ms);
            }
        }
        self.last_mouse = Some((now, x, y));
    }

    pub fn record_mouse_button(&mut self, button: u8, is_down: bool, x: i32, y: i32) {
        if !self.accept_event() {
            return;
        }
        let now = Instant::now();
        self.retain_event(BehaviorEvent::MouseButton {
            timestamp_ms: self.timestamp_ms(now),
            button,
            is_down,
            x,
            y,
        });
        self.mouse_events += 1;
        if is_down {
            if let Some(previous) = self.last_click_down {
                self.click_interval_ms
                    .push(duration_ms(now.duration_since(previous)));
            }
            self.last_click_down = Some(now);
            self.button_down.entry(button).or_insert(now);
        } else if let Some(started) = self.button_down.remove(&button) {
            self.click_hold_ms
                .push(duration_ms(now.duration_since(started)));
        }
    }

    pub fn record_wheel(&mut self, delta_x: i32, delta_y: i32, x: i32, y: i32) {
        if self.accept_event() {
            self.retain_event(BehaviorEvent::Wheel {
                timestamp_ms: self.timestamp_ms(Instant::now()),
                delta_x,
                delta_y,
                x,
                y,
            });
            self.wheel_events += 1;
            self.mouse_events += 1;
        }
    }

    pub fn stop(&mut self) -> Result<BehaviorRecordingResult, AppError> {
        if !self.active {
            return Err(AppError::invalid(
                "behavior_recording_inactive",
                "当前没有正在进行的行为训练录制",
            ));
        }
        let duration_ms = self
            .started_at
            .map(|started| started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
            .unwrap_or(0);
        if self.event_count < MIN_BEHAVIOR_EVENTS {
            self.reset();
            return Err(AppError::invalid(
                "behavior_insufficient_data",
                format!("行为训练样本不足，至少需要 {MIN_BEHAVIOR_EVENTS} 个键鼠事件"),
            ));
        }
        let persist_raw_session = self.persist_raw_session;
        let profile = BehaviorProfile {
            id: new_profile_id(),
            name: self
                .session_name
                .clone()
                .unwrap_or_else(|| "我的行为档案".to_string()),
            api_version: BEHAVIOR_API_VERSION,
            sample_count: self.event_count,
            duration_ms,
            keyboard_events: self.keyboard_events,
            mouse_events: self.mouse_events,
            wheel_events: self.wheel_events,
            key_hold_ms: self.key_hold_ms.distribution(default_key_hold()),
            key_interval_ms: self.key_interval_ms.distribution(default_key_interval()),
            click_hold_ms: self.click_hold_ms.distribution(default_click_hold()),
            click_interval_ms: self
                .click_interval_ms
                .distribution(default_click_interval()),
            mouse_speed_px_per_sec: self
                .mouse_speed_px_per_sec
                .distribution(default_mouse_speed()),
            mouse_pause_ms: self.mouse_pause_ms.distribution(default_mouse_pause()),
            mouse_direction_change_rate: if self.direction_samples == 0 {
                0.18
            } else {
                self.direction_changes as f32 / self.direction_samples as f32
            }
            .clamp(0.0, 1.0),
            mouse_jitter_px: estimate_jitter(&self.mouse_speed_px_per_sec),
            raw_events: std::mem::take(&mut self.raw_events),
        };
        self.reset();
        profile.validate()?;
        Ok(BehaviorRecordingResult {
            profile,
            persist_raw_session,
        })
    }

    fn accept_event(&mut self) -> bool {
        if !self.active {
            return false;
        }
        if self.event_count >= MAX_BEHAVIOR_EVENTS {
            self.capped = true;
            return false;
        }
        self.capture_started = true;
        self.event_count += 1;
        true
    }

    fn timestamp_ms(&self, now: Instant) -> u64 {
        self.started_at
            .map(|started| {
                now.duration_since(started)
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64
            })
            .unwrap_or(0)
    }

    fn retain_event(&mut self, event: BehaviorEvent) {
        // The retention setting controls disk persistence, not whether the
        // current in-memory training session can be segmented into V2 data.
        // accept_event() keeps this vector within MAX_BEHAVIOR_EVENTS.
        self.raw_events.push(event);
    }
}

#[derive(Debug, Clone, Copy)]
pub enum DelayKind {
    General,
    KeyHold,
    KeyInterval,
    ClickHold,
    ClickInterval,
    MousePause,
}

#[derive(Debug, Clone)]
pub struct BiomimeticRuntime {
    profile: BehaviorProfile,
    intensity: f32,
    rng: Random,
}

impl BiomimeticRuntime {
    pub fn new(profile: BehaviorProfile, intensity: f32) -> Result<Self, AppError> {
        profile.validate()?;
        Ok(Self {
            rng: Random::from_profile(&profile),
            profile,
            intensity: intensity.clamp(0.0, 1.0),
        })
    }

    pub fn from_profiles(profiles: &[BehaviorProfile], intensity: f32) -> Result<Self, AppError> {
        Self::new(BehaviorProfile::combine(profiles)?, intensity)
    }

    pub fn from_inputs(inputs: &[BiomimeticInput], intensity: f32) -> Result<Self, AppError> {
        let profiles = inputs
            .iter()
            .map(|input| input.profile.clone())
            .collect::<Vec<_>>();
        Self::from_profiles(&profiles, intensity)
    }

    pub fn adjust_delay_ms(&mut self, base: u64, kind: DelayKind) -> u64 {
        let base = base.min(MAX_RUNTIME_DELAY_MS);
        if self.intensity <= f32::EPSILON {
            return base;
        }
        let distribution = match kind {
            DelayKind::General => &self.profile.key_interval_ms,
            DelayKind::KeyHold => &self.profile.key_hold_ms,
            DelayKind::KeyInterval => &self.profile.key_interval_ms,
            DelayKind::ClickHold => &self.profile.click_hold_ms,
            DelayKind::ClickInterval => &self.profile.click_interval_ms,
            DelayKind::MousePause => &self.profile.mouse_pause_ms,
        };
        let learned = distribution.sample(&mut self.rng).round().max(0.0) as u64;
        let blended = if base == 0 {
            (learned as f32 * self.intensity).round().max(0.0) as u64
        } else {
            // Explicit waits carry macro semantics (for example, waiting for
            // a page to load), so the learned rhythm only contributes a
            // bounded relative variation instead of replacing the wait.
            let mean = distribution.mean.max(1.0);
            let relative_variation = ((learned as f32 / mean) - 1.0).clamp(-0.35, 0.35);
            (base as f32 * (1.0 + relative_variation * self.intensity))
                .round()
                .max(0.0) as u64
        };
        blended.min(MAX_RUNTIME_DELAY_MS)
    }

    pub fn plan_mouse_move(
        &mut self,
        start: (i32, i32),
        target: (i32, i32),
    ) -> Vec<BiomimeticPoint> {
        let dx = (target.0 - start.0) as f32;
        let dy = (target.1 - start.1) as f32;
        let distance = dx.hypot(dy);
        if distance <= 0.5 {
            return vec![BiomimeticPoint {
                x: target.0,
                y: target.1,
                delay_ms: 0,
            }];
        }

        let speed = self
            .profile
            .mouse_speed_px_per_sec
            .sample(&mut self.rng)
            .max(120.0);
        let duration_ms = (distance / speed * 1000.0).clamp(20.0, 2500.0);
        let segments = ((distance / 28.0).ceil() as usize).clamp(2, 64);
        let normal = (-dy / distance, dx / distance);
        let curvature =
            (self.profile.mouse_jitter_px.max(1.0) * (0.5 + self.rng.next_unit()) * self.intensity)
                .min(distance * 0.25);
        let control = (
            start.0 as f32 + dx * 0.5 + normal.0 * curvature,
            start.1 as f32 + dy * 0.5 + normal.1 * curvature,
        );
        let mut points = Vec::with_capacity(segments + 1);
        points.push(BiomimeticPoint {
            x: start.0,
            y: start.1,
            delay_ms: 0,
        });
        for index in 1..=segments {
            let t = index as f32 / segments as f32;
            let inverse = 1.0 - t;
            let x = inverse * inverse * start.0 as f32
                + 2.0 * inverse * t * control.0
                + t * t * target.0 as f32;
            let y = inverse * inverse * start.1 as f32
                + 2.0 * inverse * t * control.1
                + t * t * target.1 as f32;
            let delay_ms = (duration_ms / segments as f32).round().max(1.0) as u64;
            points.push(BiomimeticPoint {
                x: if index == segments {
                    target.0
                } else {
                    x.round() as i32
                },
                y: if index == segments {
                    target.1
                } else {
                    y.round() as i32
                },
                delay_ms,
            });
        }
        points
    }

    pub fn profile(&self) -> &BehaviorProfile {
        &self.profile
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BiomimeticPoint {
    pub x: i32,
    pub y: i32,
    pub delay_ms: u64,
}

#[derive(Debug, Clone)]
struct Random {
    state: u64,
}

impl Random {
    fn from_profile(profile: &BehaviorProfile) -> Self {
        let mut seed = profile
            .id
            .bytes()
            .fold(0x9E37_79B9_7F4A_7C15_u64, |state, byte| {
                state.rotate_left(7) ^ u64::from(byte).wrapping_mul(0x1000_0001)
            });
        seed ^= now_nanos();
        Self {
            state: if seed == 0 { 1 } else { seed },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.state = value;
        value
    }

    fn next_unit(&mut self) -> f32 {
        (self.next_u64() as f64 / u64::MAX as f64) as f32
    }
}

fn duration_ms(duration: Duration) -> f32 {
    duration.as_secs_f32() * 1000.0
}

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn format_name(value: &str) -> String {
    let name = value.trim();
    name.chars().take(MAX_BEHAVIOR_PROFILE_NAME).collect()
}

fn combine_distributions(distributions: &[&BehaviorDistribution]) -> BehaviorDistribution {
    let weights = distributions
        .iter()
        .map(|distribution| distribution.samples.max(1) as f32)
        .collect::<Vec<_>>();
    let weight_sum = weights.iter().sum::<f32>().max(1.0);
    let mean = distributions
        .iter()
        .zip(&weights)
        .map(|(distribution, weight)| distribution.mean * weight)
        .sum::<f32>()
        / weight_sum;
    let variance = distributions
        .iter()
        .zip(&weights)
        .map(|(distribution, weight)| {
            (distribution.std_dev.powi(2) + (distribution.mean - mean).powi(2)) * weight
        })
        .sum::<f32>()
        / weight_sum;
    let min = distributions
        .iter()
        .map(|distribution| distribution.min)
        .fold(f32::INFINITY, f32::min);
    let max = distributions
        .iter()
        .map(|distribution| distribution.max)
        .fold(0.0, f32::max);
    let p95 = (distributions
        .iter()
        .zip(&weights)
        .map(|(distribution, weight)| distribution.p95 * weight)
        .sum::<f32>()
        / weight_sum)
        .clamp(min, max);
    BehaviorDistribution {
        samples: distributions
            .iter()
            .map(|distribution| u64::from(distribution.samples))
            .fold(0_u64, u64::saturating_add)
            .min(u64::from(u32::MAX)) as u32,
        mean: mean.clamp(min, max),
        std_dev: variance.max(0.0).sqrt(),
        min,
        max,
        p95,
    }
}

fn new_profile_id() -> String {
    format!("behavior-{}", now_nanos())
}

fn escape_comment(value: &str) -> String {
    value.replace(['\r', '\n'], " ")
}

fn estimate_jitter(speed: &SampleStats) -> f32 {
    if speed.values.len() < 2 {
        return 1.5;
    }
    let mean = speed.values.iter().sum::<f32>() / speed.values.len() as f32;
    let variance = speed
        .values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f32>()
        / speed.values.len() as f32;
    (variance.sqrt() / mean.max(1.0) * 8.0).clamp(0.5, 12.0)
}

fn default_key_hold() -> BehaviorDistribution {
    BehaviorDistribution::fallback(85.0, 25.0, 35.0, 250.0, 135.0)
}

fn default_key_interval() -> BehaviorDistribution {
    BehaviorDistribution::fallback(95.0, 40.0, 35.0, 500.0, 180.0)
}

fn default_click_hold() -> BehaviorDistribution {
    BehaviorDistribution::fallback(85.0, 25.0, 35.0, 250.0, 135.0)
}

fn default_click_interval() -> BehaviorDistribution {
    BehaviorDistribution::fallback(180.0, 80.0, 60.0, 800.0, 360.0)
}

fn default_mouse_speed() -> BehaviorDistribution {
    BehaviorDistribution::fallback(850.0, 320.0, 150.0, 2200.0, 1400.0)
}

fn default_mouse_pause() -> BehaviorDistribution {
    BehaviorDistribution::fallback(60.0, 40.0, 10.0, 500.0, 140.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn recorder_with_events() -> BehaviorProfile {
        let mut recorder = BehaviorRecorder::default();
        recorder.start("测试档案", true).unwrap();
        for index in 0..4 {
            recorder.record_key(0x41 + index, 0x1e + index, true);
            thread::sleep(Duration::from_millis(2));
            recorder.record_key(0x41 + index, 0x1e + index, false);
            recorder.record_mouse_button(1, true, 100, 200);
            thread::sleep(Duration::from_millis(2));
            recorder.record_mouse_button(1, false, 100, 200);
            recorder.record_mouse_move((index * 20) as i32, (index * 15) as i32);
            recorder.record_wheel(0, -120, 100, 200);
        }
        recorder.stop().unwrap().profile
    }

    #[test]
    fn recorder_builds_aggregate_profile_with_complete_raw_events() {
        let profile = recorder_with_events();
        assert!(profile.sample_count >= MIN_BEHAVIOR_EVENTS);
        assert!(profile.key_hold_ms.samples > 0);
        assert!(profile.click_hold_ms.samples > 0);
        assert_eq!(profile.raw_events.len() as u64, profile.sample_count);
        let serialized = serde_json::to_string(&profile).unwrap();
        assert!(serialized.contains("\"rawEvents\""));
        assert!(serialized.contains("\"scanCode\""));
        profile.validate().unwrap();
    }

    #[test]
    fn recorder_can_disable_session_persistence_without_losing_training_events() {
        let mut recorder = BehaviorRecorder::default();
        recorder.start("统计档案", false).unwrap();
        for _ in 0..MIN_BEHAVIOR_EVENTS {
            recorder.record_key(65, 30, true);
            recorder.record_key(65, 30, false);
        }
        let result = recorder.stop().unwrap();
        assert!(!result.persist_raw_session);
        assert_eq!(
            result.profile.raw_events.len() as u64,
            result.profile.sample_count
        );
        assert!(result.profile.key_hold_ms.samples > 0);
    }

    #[test]
    fn recorder_keeps_bounded_training_events_when_session_persistence_is_off() {
        let mut recorder = BehaviorRecorder::default();
        recorder.start("有界训练", false).unwrap();
        for index in 0..=MAX_BEHAVIOR_EVENTS {
            recorder.record_mouse_move((index % 100) as i32, (index % 80) as i32);
        }
        let status = recorder.status();
        assert!(status.capped);
        assert_eq!(status.event_count, MAX_BEHAVIOR_EVENTS);
        assert!(!status.persisting_raw_session);

        let result = recorder.stop().unwrap();
        assert!(!result.persist_raw_session);
        assert_eq!(result.profile.sample_count, MAX_BEHAVIOR_EVENTS);
        assert_eq!(result.profile.raw_events.len() as u64, MAX_BEHAVIOR_EVENTS);
    }

    #[test]
    fn insufficient_recording_has_stable_error_code() {
        let mut recorder = BehaviorRecorder::default();
        recorder.start("短样本", true).unwrap();
        recorder.record_key(65, 30, true);
        let error = recorder.stop().unwrap_err();
        assert_eq!(error.code, "behavior_insufficient_data");
    }

    #[test]
    fn invalid_profile_is_rejected() {
        let mut profile = recorder_with_events();
        profile.mouse_direction_change_rate = 2.0;
        let error = profile.validate().unwrap_err();
        assert_eq!(error.code, "behavior_mouse_metrics_invalid");
    }

    #[test]
    fn mouse_plan_is_bounded_and_ends_exactly_at_target() {
        let profile = recorder_with_events();
        let mut runtime = BiomimeticRuntime::new(profile, 1.0).unwrap();
        let points = runtime.plan_mouse_move((10, 20), (400, 300));
        assert!((3..=65).contains(&points.len()));
        assert_eq!(
            points.first().map(|point| (point.x, point.y)),
            Some((10, 20))
        );
        assert_eq!(
            points.last().map(|point| (point.x, point.y)),
            Some((400, 300))
        );
        assert!(points.iter().skip(1).all(|point| point.delay_ms > 0));
    }

    #[test]
    fn zero_intensity_keeps_explicit_delay() {
        let profile = recorder_with_events();
        let mut runtime = BiomimeticRuntime::new(profile, 0.0).unwrap();
        assert_eq!(runtime.adjust_delay_ms(123, DelayKind::General), 123);
    }

    #[test]
    fn generated_api_contains_real_rhai_wrappers() {
        let profile = recorder_with_events();
        let api = profile.generated_api();
        assert_eq!(api.version, BEHAVIOR_API_VERSION);
        assert!(api.source.contains("fn bio_click"));
        assert!(api.source.contains("press(key)"));
    }

    #[test]
    fn multiple_profiles_are_combined_for_runtime() {
        let first = recorder_with_events();
        let second = recorder_with_events();
        let combined = BehaviorProfile::combine(&[first.clone(), second.clone()]).unwrap();
        assert_eq!(combined.raw_events.len(), 0);
        assert_eq!(
            combined.sample_count,
            (first.sample_count + second.sample_count).min(MAX_BEHAVIOR_EVENTS)
        );
        let mut runtime = BiomimeticRuntime::from_profiles(&[first, second], 0.8).unwrap();
        assert!(runtime.adjust_delay_ms(100, DelayKind::General) <= MAX_RUNTIME_DELAY_MS);
    }

    #[test]
    fn generated_input_is_derived_without_raw_events() {
        let profile = recorder_with_events();
        let input = BiomimeticInput::from_profile(&profile);
        assert_eq!(input.source_profile_ids, vec![profile.id]);
        assert!(input.profile.raw_events.is_empty());
        input.validate().unwrap();
        let runtime = BiomimeticRuntime::from_inputs(&[input], 1.0);
        assert!(runtime.is_ok());
    }

    #[test]
    fn legacy_profile_without_raw_events_remains_readable() {
        let profile = recorder_with_events();
        let mut value = serde_json::to_value(&profile).unwrap();
        value.as_object_mut().unwrap().remove("rawEvents");
        let restored: BehaviorProfile = serde_json::from_value(value).unwrap();
        assert!(restored.raw_events.is_empty());
        restored.validate().unwrap();
    }
}
