use crate::AppError;
use serde::{Deserialize, Serialize};

use super::events::PointerMoveEpisode;
use super::validation::validate_finite;

// Training-quality floors are deliberately broad: normal curves, small
// corrections, and reasonable overshoot remain valid, while a trajectory
// that spends most of its path circling instead of approaching its endpoint
// cannot become a reusable move-to exemplar.
pub const MIN_TRAINING_PATH_EFFICIENCY: f32 = 0.20;
pub const MAX_TRAINING_PATH_RATIO: f32 = 8.0;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PointerFeatures {
    pub movement_time_ms: f32,
    pub distance_px: f32,
    pub path_length_px: f32,
    pub path_efficiency: f32,
    pub mean_speed: f32,
    pub peak_speed: f32,
    pub time_to_peak_ratio: f32,
    pub acceleration_phase_ratio: f32,
    pub deceleration_phase_ratio: f32,
    pub maximum_lateral_deviation_px: f32,
    pub signed_curvature: f32,
    pub endpoint_dwell_ms: f32,
    pub overshoot_count: u32,
    pub overshoot_distance_px: f32,
    pub correction_count: u32,
    pub coverage: f32,
}

impl PointerFeatures {
    pub fn finite(&self) -> bool {
        [
            self.movement_time_ms,
            self.distance_px,
            self.path_length_px,
            self.path_efficiency,
            self.mean_speed,
            self.peak_speed,
            self.time_to_peak_ratio,
            self.acceleration_phase_ratio,
            self.deceleration_phase_ratio,
            self.maximum_lateral_deviation_px,
            self.signed_curvature,
            self.endpoint_dwell_ms,
            self.overshoot_distance_px,
            self.coverage,
        ]
        .iter()
        .all(|value| value.is_finite())
    }
}

pub fn extract_pointer_features(episode: &PointerMoveEpisode) -> Result<PointerFeatures, AppError> {
    episode.validate()?;
    if episode.sample_points.len() < 2 || episode.duration_ms == 0 {
        return Err(AppError::invalid(
            "behavior_v2_feature_samples_insufficient",
            "轨迹采样点不足，无法计算运动学特征",
        ));
    }

    let start = episode.sample_points.first().copied().ok_or_else(|| {
        AppError::invalid("behavior_v2_feature_samples_insufficient", "轨迹没有起点")
    })?;
    let end = episode.sample_points.last().copied().unwrap_or(start);
    let direct_dx = (end.x - start.x) as f32;
    let direct_dy = (end.y - start.y) as f32;
    let direct_distance = direct_dx.hypot(direct_dy);
    let movement_time_ms = episode.duration_ms as f32;
    let mut path_length_px = 0.0f32;
    let mut speeds = Vec::<(f32, u64)>::new();
    let mut vectors = Vec::<(f32, f32)>::new();
    for pair in episode.sample_points.windows(2) {
        let dx = (pair[1].x - pair[0].x) as f32;
        let dy = (pair[1].y - pair[0].y) as f32;
        let distance = dx.hypot(dy);
        let delta_ms = pair[1].timestamp_ms.saturating_sub(pair[0].timestamp_ms);
        if delta_ms == 0 {
            continue;
        }
        path_length_px += distance;
        vectors.push((dx, dy));
        if distance > 0.0 {
            speeds.push((distance / (delta_ms as f32 / 1000.0), pair[1].timestamp_ms));
        }
    }
    if !path_length_px.is_finite() || speeds.is_empty() {
        return Err(AppError::invalid(
            "behavior_v2_feature_samples_insufficient",
            "轨迹没有有效的位移时间间隔",
        ));
    }

    let mean_speed = path_length_px / (movement_time_ms / 1000.0).max(0.001);
    let (peak_index, (peak_speed, peak_at)) = speeds
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| left.0.total_cmp(&right.0))
        .map(|(index, value)| (index, *value))
        .unwrap_or((0, (0.0, start.timestamp_ms)));
    let peak_elapsed_ms = peak_at.saturating_sub(start.timestamp_ms) as f32;
    let time_to_peak_ratio = (peak_elapsed_ms / movement_time_ms).clamp(0.0, 1.0);
    let acceleration_phase_ratio = time_to_peak_ratio;
    let deceleration_phase_ratio = (1.0 - time_to_peak_ratio).clamp(0.0, 1.0);

    let maximum_lateral_deviation_px = episode
        .sample_points
        .iter()
        .map(|sample| point_line_distance(*sample, start, end))
        .fold(0.0, f32::max);

    let signed_curvature = vectors
        .windows(2)
        .map(|pair| {
            let cross = pair[0].0 * pair[1].1 - pair[0].1 * pair[1].0;
            let dot = pair[0].0 * pair[1].0 + pair[0].1 * pair[1].1;
            cross.atan2(dot)
        })
        .sum::<f32>()
        / path_length_px.max(1.0);

    let direction_x = if direct_distance > 0.0 {
        direct_dx / direct_distance
    } else {
        0.0
    };
    let direction_y = if direct_distance > 0.0 {
        direct_dy / direct_distance
    } else {
        0.0
    };
    let mut overshoot_count = 0u32;
    let mut overshoot_distance_px = 0.0f32;
    let mut previously_overshot = false;
    for sample in &episode.sample_points {
        let relative_x = (sample.x - start.x) as f32;
        let relative_y = (sample.y - start.y) as f32;
        let projection = relative_x * direction_x + relative_y * direction_y;
        let overshoot = (projection - direct_distance).max(0.0);
        if overshoot > direct_distance.max(1.0) * 0.01 {
            if !previously_overshot {
                overshoot_count = overshoot_count.saturating_add(1);
            }
            overshoot_distance_px = overshoot_distance_px.max(overshoot);
            previously_overshot = true;
        } else {
            previously_overshot = false;
        }
    }

    let correction_count = vectors
        .windows(2)
        .filter(|pair| {
            let first_length = pair[0].0.hypot(pair[0].1);
            let second_length = pair[1].0.hypot(pair[1].1);
            first_length > 0.0
                && second_length > 0.0
                && (pair[0].0 * pair[1].0 + pair[0].1 * pair[1].1) / (first_length * second_length)
                    < -0.25
        })
        .count() as u32;

    let features = PointerFeatures {
        movement_time_ms,
        distance_px: direct_distance,
        path_length_px,
        path_efficiency: if path_length_px > 0.0 {
            (direct_distance / path_length_px).clamp(0.0, 1.0)
        } else {
            0.0
        },
        mean_speed,
        peak_speed,
        time_to_peak_ratio,
        acceleration_phase_ratio,
        deceleration_phase_ratio,
        maximum_lateral_deviation_px,
        signed_curvature,
        endpoint_dwell_ms: episode.endpoint_dwell_ms as f32,
        overshoot_count,
        overshoot_distance_px,
        correction_count,
        coverage: (episode.sample_points.len() as f32 / 8.0).clamp(0.0, 1.0),
    };
    if !features.finite() {
        return Err(AppError::invalid(
            "behavior_v2_feature_non_finite",
            "轨迹特征计算产生了非有限数值",
        ));
    }
    for value in [
        features.movement_time_ms,
        features.distance_px,
        features.path_length_px,
        features.mean_speed,
        features.peak_speed,
        features.maximum_lateral_deviation_px,
        features.endpoint_dwell_ms,
        features.overshoot_distance_px,
    ] {
        validate_finite(value, "pointerFeature")?;
    }
    let _ = peak_index;
    Ok(features)
}

/// Return a stable, explainable reason when a finite episode is unsafe as a
/// reusable training exemplar. This is intentionally separate from feature
/// extraction so structural validity and model quality remain distinguishable.
pub fn training_quality_rejection_reason(features: &PointerFeatures) -> Option<&'static str> {
    if !features.finite() {
        return Some("feature_non_finite");
    }
    if features.distance_px <= f32::EPSILON {
        return Some("direct_distance_zero");
    }
    let path_ratio = features.path_length_px / features.distance_px;
    if !path_ratio.is_finite() || path_ratio > MAX_TRAINING_PATH_RATIO {
        return Some("path_ratio_exceeded");
    }
    if features.path_efficiency < MIN_TRAINING_PATH_EFFICIENCY {
        return Some("path_efficiency_below_floor");
    }
    None
}

fn point_line_distance(
    point: super::events::PointerSample,
    start: super::events::PointerSample,
    end: super::events::PointerSample,
) -> f32 {
    let line_x = (end.x - start.x) as f32;
    let line_y = (end.y - start.y) as f32;
    let line_length = line_x.hypot(line_y);
    let point_x = (point.x - start.x) as f32;
    let point_y = (point.y - start.y) as f32;
    if line_length <= f32::EPSILON {
        point_x.hypot(point_y)
    } else {
        (line_x * point_y - line_y * point_x).abs() / line_length
    }
}
