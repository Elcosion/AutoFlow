use crate::{AppError, MouseButton};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

use super::features::PointerFeatures;
use super::model::{
    BehaviorProfileV2, DirectionBucket, PointerBucket, PointerModel, TargetWidthBucket,
};
use super::policy::BehaviorPolicy;
use super::sampler::{mix_seed, random_seed, SeededRng};
use super::validation::{
    clamp_coordinate, validate_coordinate, V2_MAX_GENERATED_DELAY_MS, V2_MAX_GENERATED_POINTS,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PointerActionContext {
    pub start: (i32, i32),
    pub target: (i32, i32),
    pub distance: f32,
    pub angle: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_width: Option<f32>,
    pub followed_by_click: bool,
    pub intensity: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
}

impl PointerActionContext {
    pub fn new(
        start: (i32, i32),
        target: (i32, i32),
        target_width: Option<f32>,
        followed_by_click: bool,
        intensity: f32,
        seed: Option<u64>,
    ) -> Result<Self, AppError> {
        validate_coordinate(start.0, start.1)?;
        validate_coordinate(target.0, target.1)?;
        if let Some(width) = target_width {
            if !width.is_finite() || !(0.0..=10_000.0).contains(&width) {
                return Err(AppError::invalid(
                    "behavior_v2_target_width_invalid",
                    "targetWidthPx 必须是 0 到 10000 之间的有限数值",
                ));
            }
        }
        let dx = (target.0 - start.0) as f32;
        let dy = (target.1 - start.1) as f32;
        let distance = dx.hypot(dy);
        Ok(Self {
            start,
            target,
            distance,
            angle: dy.atan2(dx),
            target_width,
            followed_by_click,
            intensity: if intensity.is_finite() {
                intensity.clamp(0.0, 1.0)
            } else {
                0.0
            },
            seed,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SampledPointerFeatures {
    #[serde(flatten)]
    pub features: PointerFeatures,
    pub trained: bool,
    pub coverage: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorActionDiagnostic {
    pub action_index: u64,
    pub runtime_seed: u64,
    pub action_seed: u64,
    pub action_kind: String,
    pub bucket: String,
    pub trained: bool,
    pub coverage: f32,
    pub fallback_level: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_width: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub movement_time_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_to_peak_ratio: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceleration_phase_ratio: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deceleration_phase_ratio: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_efficiency: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overshoot_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correction_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overshoot_distance_px: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correction_strength: Option<f32>,
    #[serde(default)]
    pub overshoot_enabled: bool,
    #[serde(default)]
    pub correction_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_click_dwell_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_click_dwell_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PointerTrajectoryPoint {
    pub x: i32,
    pub y: i32,
    pub delay_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PointerTrajectory {
    pub points: Vec<PointerTrajectoryPoint>,
    pub sampled_features: SampledPointerFeatures,
    pub bucket: String,
    #[serde(default)]
    pub fallback_level: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<BehaviorActionDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClickPlan {
    pub pre_click_dwell_ms: u64,
    pub hold_ms: u64,
    pub post_click_dwell_ms: u64,
    pub button: MouseButton,
    pub followed_by_move: bool,
    pub bucket: String,
    #[serde(default)]
    pub coverage: f32,
    #[serde(default)]
    pub fallback_level: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<BehaviorActionDiagnostic>,
}

#[derive(Debug, Clone, Copy)]
enum RuntimeActionKind {
    Pointer = 1,
    Click = 2,
}

#[derive(Debug, Clone)]
pub struct BehaviorRuntimeV2 {
    profile: BehaviorProfileV2,
    policy: BehaviorPolicy,
    runtime_seed: u64,
    action_index: u64,
    last_diagnostic: Option<BehaviorActionDiagnostic>,
}

impl BehaviorRuntimeV2 {
    pub fn new(profile: BehaviorProfileV2, policy: BehaviorPolicy) -> Result<Self, AppError> {
        let runtime_seed = policy.seed.unwrap_or_else(random_seed);
        Self::with_runtime_seed(profile, policy, runtime_seed)
    }

    pub fn with_runtime_seed(
        profile: BehaviorProfileV2,
        policy: BehaviorPolicy,
        runtime_seed: u64,
    ) -> Result<Self, AppError> {
        profile.validate()?;
        let policy = policy.normalized()?;
        Ok(Self {
            profile,
            policy,
            runtime_seed,
            action_index: 0,
            last_diagnostic: None,
        })
    }

    pub fn profile(&self) -> &BehaviorProfileV2 {
        &self.profile
    }

    pub fn policy(&self) -> &BehaviorPolicy {
        &self.policy
    }

    pub fn runtime_seed(&self) -> u64 {
        self.runtime_seed
    }

    pub fn action_index(&self) -> u64 {
        self.action_index
    }

    pub fn last_diagnostic(&self) -> Option<&BehaviorActionDiagnostic> {
        self.last_diagnostic.as_ref()
    }

    pub fn applies_pointer_behavior(&self) -> bool {
        self.policy.enabled
            && (self.policy.timing_strength > f32::EPSILON
                || self.policy.pointer_path_strength > f32::EPSILON
                || self.policy.correction_strength > f32::EPSILON)
    }

    pub fn applies_behavior(&self) -> bool {
        self.policy.enabled
            && (self.policy.timing_strength > f32::EPSILON
                || self.policy.pointer_path_strength > f32::EPSILON
                || self.policy.pause_strength > f32::EPSILON
                || self.policy.correction_strength > f32::EPSILON)
    }

    pub fn pointer_trajectory(
        &mut self,
        start: (i32, i32),
        target: (i32, i32),
        target_width: Option<f32>,
        followed_by_click: bool,
        cancel: Option<&AtomicBool>,
    ) -> Result<PointerTrajectory, AppError> {
        let action_index = self.action_index;
        let action_seed = self.next_action_seed(
            RuntimeActionKind::Pointer,
            start,
            target,
            target_width,
            followed_by_click,
            0,
        );
        let context = PointerActionContext::new(
            start,
            target,
            target_width,
            followed_by_click,
            1.0,
            Some(action_seed),
        )?;
        let mut trajectory =
            generate_pointer_trajectory_with_policy(&self.profile, &context, &self.policy, cancel)?;
        let diagnostic = pointer_diagnostic(
            action_index,
            self.runtime_seed,
            action_seed,
            &context,
            &trajectory,
            (self.policy.correction_strength * context.intensity).clamp(0.0, 1.0),
        );
        trajectory.diagnostic = Some(diagnostic.clone());
        self.last_diagnostic = Some(diagnostic);
        Ok(trajectory)
    }

    pub fn click_plan(
        &mut self,
        button: MouseButton,
        followed_by_move: bool,
        cancel: Option<&AtomicBool>,
    ) -> Result<ClickPlan, AppError> {
        let action_index = self.action_index;
        let action_seed = self.next_action_seed(
            RuntimeActionKind::Click,
            (0, 0),
            (0, 0),
            None,
            followed_by_move,
            mouse_button_code(button),
        );
        let mut plan = sample_click_plan_with_policy(
            &self.profile,
            button,
            followed_by_move,
            1.0,
            Some(action_seed),
            &self.policy,
            cancel,
        )?;
        let diagnostic = click_diagnostic(action_index, self.runtime_seed, action_seed, &plan);
        plan.diagnostic = Some(diagnostic.clone());
        self.last_diagnostic = Some(diagnostic);
        Ok(plan)
    }

    fn next_action_seed(
        &mut self,
        kind: RuntimeActionKind,
        start: (i32, i32),
        target: (i32, i32),
        target_width: Option<f32>,
        followed_by_click: bool,
        button: i64,
    ) -> u64 {
        let action_index = self.action_index;
        self.action_index = self.action_index.saturating_add(1);
        mix_seed(
            self.runtime_seed,
            &[
                action_index as i64,
                kind as i64,
                start.0 as i64,
                start.1 as i64,
                target.0 as i64,
                target.1 as i64,
                target_width.map(f32::to_bits).unwrap_or(u32::MAX) as i64,
                i64::from(followed_by_click),
                button,
            ],
        )
    }

    /// Compatibility bridge for the existing Windows input adapter. New code
    /// should use `pointer_trajectory`, which retains the action context and
    /// fallback metadata.
    pub fn plan_mouse_move(
        &mut self,
        start: (i32, i32),
        target: (i32, i32),
    ) -> Vec<crate::behavior::BiomimeticPoint> {
        self.pointer_trajectory(start, target, None, false, None)
            .map(|trajectory| {
                trajectory
                    .points
                    .into_iter()
                    .map(|point| crate::behavior::BiomimeticPoint {
                        x: point.x,
                        y: point.y,
                        delay_ms: point.delay_ms,
                    })
                    .collect()
            })
            .unwrap_or_else(|_| {
                vec![crate::behavior::BiomimeticPoint {
                    x: target.0,
                    y: target.1,
                    delay_ms: 0,
                }]
            })
    }

    pub fn adjust_delay_ms(&mut self, base: u64, _kind: crate::behavior::DelayKind) -> u64 {
        base.min(super::validation::V2_MAX_GENERATED_DELAY_MS)
    }
}

pub fn generate_pointer_trajectory(
    profile: &BehaviorProfileV2,
    context: &PointerActionContext,
    cancel: Option<&AtomicBool>,
) -> Result<PointerTrajectory, AppError> {
    let policy = BehaviorPolicy::from_legacy(
        context.intensity > f32::EPSILON,
        context.intensity,
        Some(profile.id.clone()),
    );
    generate_pointer_trajectory_with_policy(profile, context, &policy, cancel)
}

pub fn generate_pointer_trajectory_with_policy(
    profile: &BehaviorProfileV2,
    context: &PointerActionContext,
    policy: &BehaviorPolicy,
    cancel: Option<&AtomicBool>,
) -> Result<PointerTrajectory, AppError> {
    validate_coordinate(context.start.0, context.start.1)?;
    validate_coordinate(context.target.0, context.target.1)?;
    let intensity = context.intensity.clamp(0.0, 1.0);
    let fallback_features = fallback_features(context.distance);
    if intensity <= f32::EPSILON {
        return Ok(PointerTrajectory {
            points: vec![
                PointerTrajectoryPoint {
                    x: context.start.0,
                    y: context.start.1,
                    delay_ms: 0,
                },
                PointerTrajectoryPoint {
                    x: context.target.0,
                    y: context.target.1,
                    delay_ms: 0,
                },
            ],
            sampled_features: SampledPointerFeatures {
                features: fallback_features,
                trained: false,
                coverage: 0.0,
            },
            bucket: "disabled".to_string(),
            fallback_level: 0,
            fallback_reason: Some("behavior_disabled".to_string()),
            diagnostic: None,
        });
    }

    if context.start == context.target {
        return Ok(PointerTrajectory {
            points: vec![PointerTrajectoryPoint {
                x: context.target.0,
                y: context.target.1,
                delay_ms: 0,
            }],
            sampled_features: SampledPointerFeatures {
                features: fallback_features,
                trained: false,
                coverage: 0.0,
            },
            bucket: "zero_distance".to_string(),
            fallback_level: 0,
            fallback_reason: Some("zero_distance".to_string()),
            diagnostic: None,
        });
    }

    let key = PointerBucket {
        distance: super::model::DistanceBucket::from_distance(context.distance),
        direction: DirectionBucket::from_angle(context.angle),
        followed_by_click: context.followed_by_click,
        target_width: TargetWidthBucket::from_width(context.target_width),
    };
    let seed = context.seed.or(policy.seed).unwrap_or_else(random_seed);
    let mut rng = SeededRng::from_seed(seed);
    let sparse_exact_bucket = profile
        .pointer_model
        .has_sparse_exact_bucket(&key, &profile.model_config);
    let selection = profile
        .pointer_model
        .select_bucket(&key, &profile.model_config);
    let (mut sampled, bucket, fallback_reason, fallback_level, trained, coverage) = match selection
    {
        Some((bucket, label, mut reason, fallback_level)) => {
            let mut fallback_level = fallback_level;
            if sparse_exact_bucket {
                reason = Some("insufficient_samples_in_exact_bucket".to_string());
                fallback_level = fallback_level.max(1);
            }
            if context.target_width.is_some() && bucket.key.target_width != key.target_width {
                reason = Some(
                    if bucket.key.target_width == TargetWidthBucket::Unknown {
                        "target_width_wildcard_fallback"
                    } else {
                        "target_width_bucket_fallback"
                    }
                    .to_string(),
                );
                fallback_level = fallback_level.max(1);
            }
            let trained = reason.is_none()
                && bucket.valid_sample_count >= profile.model_config.min_bucket_samples;
            (
                PointerModel::sample_features(bucket, &mut rng),
                label,
                reason,
                fallback_level,
                trained,
                bucket.coverage,
            )
        }
        None => (
            fallback_features,
            key.label(),
            Some(
                if sparse_exact_bucket {
                    "insufficient_samples_in_exact_bucket"
                } else {
                    "no_trained_bucket"
                }
                .to_string(),
            ),
            if sparse_exact_bucket { 1 } else { u8::MAX },
            false,
            0.0,
        ),
    };
    let sampled_training_distance = sampled.distance_px.max(1.0);
    sampled.distance_px = context.distance;
    sampled.coverage = coverage;
    let geometric_duration = (context.distance / 900.0 * 1000.0).clamp(20.0, 2_500.0);
    let timing_blend = (intensity * policy.timing_strength).clamp(0.0, 1.0);
    let trained_duration = sampled.movement_time_ms.clamp(20.0, 2_500.0);
    let target_width_adjustment = context
        .target_width
        .filter(|width| *width > 0.0)
        .map(|width| (1.0 + (context.distance / width.max(1.0)).ln_1p() * 0.08).clamp(1.0, 1.6))
        .unwrap_or(1.0);
    let duration_ms = (geometric_duration + (trained_duration - geometric_duration) * timing_blend)
        * target_width_adjustment
        / policy.speed_scale.max(0.1);
    let duration_ms = duration_ms.clamp(20.0, 2_500.0).round() as u64;
    let path_blend = (intensity * policy.pointer_path_strength).clamp(0.0, 1.0);
    let lateral_scale = context.distance / sampled_training_distance;
    let efficiency_penalty =
        (1.0 / sampled.path_efficiency.clamp(0.5, 1.0) - 1.0) * context.distance * 0.35;
    let lateral_deviation = (sampled.maximum_lateral_deviation_px * lateral_scale)
        .max(efficiency_penalty)
        .mul_add(path_blend, 0.0)
        .clamp(0.0, (context.distance * 0.25).max(1.0));
    let curvature_sign = if sampled.signed_curvature.abs() > 0.0001 {
        sampled.signed_curvature.signum()
    } else if rng.next_unit() >= 0.5 {
        1.0
    } else {
        -1.0
    };
    let direction_x = (context.target.0 - context.start.0) as f32 / context.distance;
    let direction_y = (context.target.1 - context.start.1) as f32 / context.distance;
    let normal_x = -direction_y;
    let normal_y = direction_x;
    let correction_blend = (intensity * policy.correction_strength).clamp(0.0, 1.0);
    let correction_enabled = correction_blend > f32::EPSILON
        && (sampled.overshoot_count > 0 || sampled.correction_count > 0);
    let target_correction_limit = context
        .target_width
        .filter(|width| *width > 0.0)
        .map(|width| width * 0.5)
        .unwrap_or(context.distance * 0.12)
        .min(context.distance * 0.2)
        .max(0.0);
    let overshoot_distance = if correction_enabled && sampled.overshoot_count > 0 {
        (sampled.overshoot_distance_px * lateral_scale * correction_blend)
            .clamp(0.0, target_correction_limit)
    } else {
        0.0
    };
    let correction_deviation = if correction_enabled && sampled.correction_count > 0 {
        (sampled.maximum_lateral_deviation_px * lateral_scale * 0.5 * correction_blend)
            .clamp(0.0, target_correction_limit)
    } else {
        0.0
    };
    let segment_limit = ((duration_ms / 2).max(2) as usize).min(V2_MAX_GENERATED_POINTS - 1);
    let segments = ((context.distance / 28.0).ceil() as usize)
        .clamp(2, 64)
        .min(segment_limit.max(2));
    let mut points = Vec::with_capacity(segments + 1);
    points.push(PointerTrajectoryPoint {
        x: context.start.0,
        y: context.start.1,
        delay_ms: 0,
    });
    let mut previous_elapsed = 0u64;
    for index in 1..=segments {
        if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            return Err(AppError::invalid(
                "behavior_cancelled",
                "仿生鼠标轨迹已被 F12 或取消操作中止",
            ));
        }
        let linear_t = index as f32 / segments as f32;
        let progress = asymmetric_progress(linear_t, &sampled);
        let correction_phase = if correction_enabled {
            ((linear_t - 0.78) / 0.22).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let overshoot_progress = if overshoot_distance > 0.0 {
            (overshoot_distance / context.distance.max(1.0))
                * (std::f32::consts::PI * correction_phase).sin()
        } else {
            0.0
        };
        let along_progress = progress + overshoot_progress;
        let lateral = curvature_sign
            * lateral_deviation
            * (std::f32::consts::PI * progress.clamp(0.0, 1.0)).sin()
            * (0.85 + rng.next_unit() * 0.15);
        let correction_lateral =
            correction_deviation * (std::f32::consts::TAU * correction_phase).sin();
        let x = context.start.0 as f32
            + (context.target.0 - context.start.0) as f32 * along_progress
            + normal_x * (lateral + correction_lateral);
        let y = context.start.1 as f32
            + (context.target.1 - context.start.1) as f32 * along_progress
            + normal_y * (lateral + correction_lateral);
        let elapsed = (duration_ms as f32 * linear_t).round() as u64;
        let delay_ms = elapsed
            .saturating_sub(previous_elapsed)
            .clamp(1, V2_MAX_GENERATED_DELAY_MS);
        previous_elapsed = previous_elapsed.saturating_add(delay_ms);
        points.push(PointerTrajectoryPoint {
            x: if index == segments {
                context.target.0
            } else {
                clamp_coordinate(x)
            },
            y: if index == segments {
                context.target.1
            } else {
                clamp_coordinate(y)
            },
            delay_ms,
        });
    }
    Ok(PointerTrajectory {
        points,
        sampled_features: SampledPointerFeatures {
            features: sampled,
            trained,
            coverage,
        },
        bucket,
        fallback_level,
        fallback_reason,
        diagnostic: None,
    })
}

pub fn sample_click_plan(
    profile: &BehaviorProfileV2,
    button: MouseButton,
    followed_by_move: bool,
    intensity: f32,
    seed: Option<u64>,
    cancel: Option<&AtomicBool>,
) -> Result<ClickPlan, AppError> {
    let policy = BehaviorPolicy::from_legacy(
        intensity > f32::EPSILON,
        intensity,
        Some(profile.id.clone()),
    );
    sample_click_plan_with_policy(
        profile,
        button,
        followed_by_move,
        intensity,
        seed,
        &policy,
        cancel,
    )
}

pub fn sample_click_plan_with_policy(
    profile: &BehaviorProfileV2,
    button: MouseButton,
    followed_by_move: bool,
    intensity: f32,
    seed: Option<u64>,
    policy: &BehaviorPolicy,
    cancel: Option<&AtomicBool>,
) -> Result<ClickPlan, AppError> {
    if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
        return Err(AppError::invalid(
            "behavior_cancelled",
            "仿生点击已被 F12 或取消操作中止",
        ));
    }
    let intensity = if intensity.is_finite() {
        intensity.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if intensity <= f32::EPSILON {
        return Ok(ClickPlan {
            pre_click_dwell_ms: 0,
            hold_ms: 0,
            post_click_dwell_ms: 0,
            button,
            followed_by_move,
            bucket: "disabled".to_string(),
            coverage: 0.0,
            fallback_level: 0,
            fallback_reason: Some("behavior_disabled".to_string()),
            diagnostic: None,
        });
    }
    let mut rng = SeededRng::from_seed(seed.unwrap_or_else(random_seed));
    let (pre, hold, post, bucket, fallback_reason, fallback_level, coverage) = match profile
        .click_model
        .select_bucket(button, followed_by_move, &profile.model_config)
    {
        Some((bucket, reason, fallback_level)) => (
            bucket.pre_click_dwell_ms.sample(&mut rng),
            bucket.hold_ms.sample_robust(&mut rng),
            bucket.post_click_dwell_ms.sample(&mut rng),
            format!("button={:?}/afterMove={}", button, followed_by_move),
            reason,
            fallback_level,
            bucket.coverage,
        ),
        None => (
            0.0,
            0.0,
            0.0,
            format!("button={:?}/afterMove={}", button, followed_by_move),
            Some("no_trained_click_bucket".to_string()),
            u8::MAX,
            0.0,
        ),
    };
    let pause_blend = (intensity * policy.pause_strength).clamp(0.0, 1.0);
    let hold_blend = (intensity * policy.timing_strength).clamp(0.0, 1.0);
    let pre = (pre * pause_blend).round() as u64;
    let hold = if hold <= 0.0 || hold_blend <= f32::EPSILON {
        0
    } else {
        (hold * hold_blend).round().max(1.0) as u64
    };
    let post = (post * pause_blend).round() as u64;
    Ok(ClickPlan {
        pre_click_dwell_ms: pre.min(V2_MAX_GENERATED_DELAY_MS),
        hold_ms: hold.min(V2_MAX_GENERATED_DELAY_MS),
        post_click_dwell_ms: post.min(V2_MAX_GENERATED_DELAY_MS),
        button,
        followed_by_move,
        bucket,
        coverage,
        fallback_level,
        fallback_reason,
        diagnostic: None,
    })
}

fn pointer_diagnostic(
    action_index: u64,
    runtime_seed: u64,
    action_seed: u64,
    context: &PointerActionContext,
    trajectory: &PointerTrajectory,
    correction_strength: f32,
) -> BehaviorActionDiagnostic {
    let features = &trajectory.sampled_features.features;
    let correction_enabled = correction_strength > f32::EPSILON
        && (features.overshoot_count > 0 || features.correction_count > 0);
    let overshoot_enabled = correction_enabled
        && features.overshoot_count > 0
        && features.overshoot_distance_px > f32::EPSILON;
    BehaviorActionDiagnostic {
        action_index,
        runtime_seed,
        action_seed,
        action_kind: "pointer".to_string(),
        bucket: trajectory.bucket.clone(),
        trained: trajectory.sampled_features.trained,
        coverage: trajectory.sampled_features.coverage,
        fallback_level: trajectory.fallback_level,
        fallback_reason: trajectory.fallback_reason.clone(),
        target_width: context.target_width,
        movement_time_ms: Some(trajectory.points.iter().map(|point| point.delay_ms).sum()),
        time_to_peak_ratio: Some(features.time_to_peak_ratio),
        acceleration_phase_ratio: Some(features.acceleration_phase_ratio),
        deceleration_phase_ratio: Some(features.deceleration_phase_ratio),
        path_efficiency: Some(features.path_efficiency),
        overshoot_count: Some(features.overshoot_count),
        correction_count: Some(features.correction_count),
        overshoot_distance_px: Some(features.overshoot_distance_px),
        correction_strength: Some(correction_strength),
        overshoot_enabled,
        correction_enabled,
        pre_click_dwell_ms: None,
        hold_ms: None,
        post_click_dwell_ms: None,
    }
}

fn click_diagnostic(
    action_index: u64,
    runtime_seed: u64,
    action_seed: u64,
    plan: &ClickPlan,
) -> BehaviorActionDiagnostic {
    BehaviorActionDiagnostic {
        action_index,
        runtime_seed,
        action_seed,
        action_kind: "click".to_string(),
        bucket: plan.bucket.clone(),
        trained: plan.fallback_reason.is_none(),
        coverage: plan.coverage,
        fallback_level: plan.fallback_level,
        fallback_reason: plan.fallback_reason.clone(),
        target_width: None,
        movement_time_ms: None,
        time_to_peak_ratio: None,
        acceleration_phase_ratio: None,
        deceleration_phase_ratio: None,
        path_efficiency: None,
        overshoot_count: None,
        correction_count: None,
        overshoot_distance_px: None,
        correction_strength: None,
        overshoot_enabled: false,
        correction_enabled: false,
        pre_click_dwell_ms: Some(plan.pre_click_dwell_ms),
        hold_ms: Some(plan.hold_ms),
        post_click_dwell_ms: Some(plan.post_click_dwell_ms),
    }
}

fn fallback_features(distance: f32) -> PointerFeatures {
    let distance = distance.max(0.0);
    let movement_time_ms = (distance / 900.0 * 1000.0).clamp(20.0, 2_500.0);
    PointerFeatures {
        movement_time_ms,
        distance_px: distance,
        path_length_px: distance,
        path_efficiency: 1.0,
        mean_speed: distance / (movement_time_ms / 1000.0).max(0.001),
        peak_speed: distance / (movement_time_ms / 1000.0).max(0.001),
        time_to_peak_ratio: 0.5,
        acceleration_phase_ratio: 0.5,
        deceleration_phase_ratio: 0.5,
        maximum_lateral_deviation_px: 0.0,
        signed_curvature: 0.0,
        endpoint_dwell_ms: 0.0,
        overshoot_count: 0,
        overshoot_distance_px: 0.0,
        correction_count: 0,
        coverage: 0.0,
    }
}

fn mouse_button_code(button: MouseButton) -> i64 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 3,
        MouseButton::X1 => 4,
        MouseButton::X2 => 5,
    }
}

fn asymmetric_progress(t: f32, features: &PointerFeatures) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let peak = features.time_to_peak_ratio.clamp(0.15, 0.85);
    let acceleration_ratio = features.acceleration_phase_ratio.clamp(0.1, 0.9);
    let deceleration_ratio = features.deceleration_phase_ratio.clamp(0.1, 0.9);
    let acceleration_exponent = (1.15 + (1.0 - acceleration_ratio) * 2.25).clamp(1.15, 3.4);
    let deceleration_exponent = (1.15 + deceleration_ratio * 2.25).clamp(1.15, 3.4);
    let peak_distance = (deceleration_exponent * peak
        / (acceleration_exponent * (1.0 - peak) + deceleration_exponent * peak))
        .clamp(0.1, 0.9);
    if t <= peak {
        peak_distance * (t / peak).powf(acceleration_exponent)
    } else {
        1.0 - (1.0 - peak_distance) * ((1.0 - t) / (1.0 - peak)).powf(deceleration_exponent)
    }
}
