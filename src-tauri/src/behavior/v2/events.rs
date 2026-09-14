use crate::behavior::BehaviorEvent;
use crate::{AppError, MouseButton};
use serde::{Deserialize, Serialize};

use super::validation::{validate_coordinate, validate_finite, V2_MAX_COORDINATE_ABS};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PointerSample {
    pub timestamp_ms: u64,
    pub x: i32,
    pub y: i32,
}

impl PointerSample {
    pub fn validate(&self) -> Result<(), AppError> {
        validate_coordinate(self.x, self.y)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PointerMoveEpisode {
    pub start_x: i32,
    pub start_y: i32,
    pub end_x: i32,
    pub end_y: i32,
    pub started_at: u64,
    pub duration_ms: u64,
    pub distance_px: f32,
    pub angle: f32,
    pub path_length_px: f32,
    pub sample_points: Vec<PointerSample>,
    pub followed_by_click: bool,
    // Training currently receives no real target geometry from the low-level hook;
    // keep this optional so a future sanitized geometry source can opt in without
    // pretending that a runtime hint was observed during training.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_width_px: Option<f32>,
    #[serde(default)]
    pub endpoint_dwell_ms: u64,
}

impl PointerMoveEpisode {
    pub fn from_samples(
        samples: Vec<PointerSample>,
        followed_by_click: bool,
        target_width_px: Option<f32>,
        endpoint_dwell_ms: u64,
    ) -> Result<Self, AppError> {
        let first = samples.first().copied().ok_or_else(|| {
            AppError::invalid(
                "behavior_v2_empty_episode",
                "PointerMoveEpisode 至少需要一个采样点",
            )
        })?;
        let last = samples.last().copied().unwrap_or(first);
        for pair in samples.windows(2) {
            if pair[1].timestamp_ms <= pair[0].timestamp_ms {
                return Err(AppError::invalid(
                    "behavior_v2_episode_timestamp_order",
                    "PointerMoveEpisode 的采样时间戳必须严格递增",
                ));
            }
        }
        for sample in &samples {
            sample.validate()?;
        }

        let dx = (last.x - first.x) as f32;
        let dy = (last.y - first.y) as f32;
        let distance_px = dx.hypot(dy);
        let path_length_px = samples
            .windows(2)
            .map(|pair| {
                let dx = (pair[1].x - pair[0].x) as f32;
                let dy = (pair[1].y - pair[0].y) as f32;
                dx.hypot(dy)
            })
            .sum::<f32>();
        let angle = dy.atan2(dx);
        validate_finite(distance_px, "distancePx")?;
        validate_finite(path_length_px, "pathLengthPx")?;
        validate_finite(angle, "angle")?;
        if let Some(width) = target_width_px {
            if !width.is_finite() || !(0.0..=10_000.0).contains(&width) {
                return Err(AppError::invalid(
                    "behavior_v2_target_width_invalid",
                    "targetWidthPx 必须是 0 到 10000 之间的有限数值",
                ));
            }
        }

        Ok(Self {
            start_x: first.x,
            start_y: first.y,
            end_x: last.x,
            end_y: last.y,
            started_at: first.timestamp_ms,
            duration_ms: last.timestamp_ms.saturating_sub(first.timestamp_ms),
            distance_px,
            angle,
            path_length_px,
            sample_points: samples,
            followed_by_click,
            target_width_px,
            endpoint_dwell_ms,
        })
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.sample_points.is_empty() {
            return Err(AppError::invalid(
                "behavior_v2_empty_episode",
                "PointerMoveEpisode 不能为空",
            ));
        }
        let rebuilt = Self::from_samples(
            self.sample_points.clone(),
            self.followed_by_click,
            self.target_width_px,
            self.endpoint_dwell_ms,
        )?;
        if rebuilt.start_x != self.start_x
            || rebuilt.start_y != self.start_y
            || rebuilt.end_x != self.end_x
            || rebuilt.end_y != self.end_y
            || rebuilt.started_at != self.started_at
            || rebuilt.duration_ms != self.duration_ms
            || (rebuilt.distance_px - self.distance_px).abs() > 0.01
            || (rebuilt.path_length_px - self.path_length_px).abs() > 0.01
        {
            return Err(AppError::invalid(
                "behavior_v2_episode_summary_mismatch",
                "PointerMoveEpisode 的摘要字段与采样点不一致",
            ));
        }
        if self.endpoint_dwell_ms > 120_000 {
            return Err(AppError::invalid(
                "behavior_v2_dwell_too_long",
                "动作停顿超过安全上限",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClickEpisode {
    pub button: MouseButton,
    pub x: i32,
    pub y: i32,
    pub clicked_at: u64,
    pub pre_click_dwell_ms: u64,
    pub hold_ms: u64,
    pub post_click_dwell_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer_move_episode_index: Option<usize>,
    #[serde(default)]
    pub is_double_click: bool,
}

impl ClickEpisode {
    pub fn validate(&self) -> Result<(), AppError> {
        validate_coordinate(self.x, self.y)?;
        if self.pre_click_dwell_ms > 120_000
            || self.hold_ms > 120_000
            || self.post_click_dwell_ms > 120_000
        {
            return Err(AppError::invalid(
                "behavior_v2_click_timing_invalid",
                "点击时序超过安全上限",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiscardedEvent {
    pub event_index: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SegmentationConfig {
    pub move_end_dwell_ms: u64,
    pub max_event_gap_ms: u64,
    pub click_association_window_ms: u64,
    pub min_episode_distance_px: f32,
    pub min_episode_samples: usize,
    pub max_coordinate_abs: i32,
    pub max_event_count: usize,
}

impl Default for SegmentationConfig {
    fn default() -> Self {
        Self {
            move_end_dwell_ms: 120,
            max_event_gap_ms: 5_000,
            click_association_window_ms: 2_000,
            min_episode_distance_px: 2.0,
            min_episode_samples: 2,
            max_coordinate_abs: V2_MAX_COORDINATE_ABS,
            max_event_count: 250_000,
        }
    }
}

impl SegmentationConfig {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.move_end_dwell_ms == 0
            || self.max_event_gap_ms < self.move_end_dwell_ms
            || self.click_association_window_ms == 0
            || !self.min_episode_distance_px.is_finite()
            || self.min_episode_distance_px < 0.0
            || self.min_episode_samples < 2
            || self.max_coordinate_abs <= 0
            || self.max_event_count == 0
        {
            return Err(AppError::invalid(
                "behavior_v2_segmentation_config_invalid",
                "动作切分配置不合法",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SegmentationResult {
    pub pointer_moves: Vec<PointerMoveEpisode>,
    pub clicks: Vec<ClickEpisode>,
    pub discarded_events: Vec<DiscardedEvent>,
    pub accepted_event_count: usize,
}

impl SegmentationResult {
    pub fn discarded_count(&self) -> usize {
        self.discarded_events.len()
    }
}

pub(crate) fn behavior_event_parts(event: &BehaviorEvent) -> (u64, Option<(i32, i32)>) {
    match event {
        BehaviorEvent::Key { timestamp_ms, .. } => (*timestamp_ms, None),
        BehaviorEvent::MouseMove { timestamp_ms, x, y }
        | BehaviorEvent::MouseButton {
            timestamp_ms, x, y, ..
        }
        | BehaviorEvent::Wheel {
            timestamp_ms, x, y, ..
        } => (*timestamp_ms, Some((*x, *y))),
    }
}

pub(crate) fn mouse_button_from_id(button: u8) -> Option<MouseButton> {
    match button {
        1 => Some(MouseButton::Left),
        2 => Some(MouseButton::Right),
        3 => Some(MouseButton::Middle),
        4 => Some(MouseButton::X1),
        5 => Some(MouseButton::X2),
        _ => None,
    }
}
