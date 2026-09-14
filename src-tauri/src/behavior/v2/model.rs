use crate::behavior::BehaviorApi;
use crate::{AppError, MouseButton};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};

use super::events::{ClickEpisode, PointerMoveEpisode, SegmentationConfig};
use super::features::{extract_pointer_features, PointerFeatures};
use super::sampler::SeededRng;
use super::session::BehaviorSessionV2;
use super::BEHAVIOR_V2_API_VERSION;

const DEFAULT_MIN_BUCKET_SAMPLES: u32 = 3;
const DEFAULT_MAX_EXEMPLARS_PER_BUCKET: usize = 32;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorModelConfig {
    #[serde(default = "default_min_bucket_samples")]
    pub min_bucket_samples: u32,
    #[serde(default = "default_max_exemplars_per_bucket")]
    pub max_exemplars_per_bucket: usize,
    #[serde(default = "default_min_quality_episodes")]
    pub min_quality_episodes: u32,
    #[serde(default = "default_usable_quality_episodes")]
    pub usable_quality_episodes: u32,
    #[serde(default = "default_good_quality_episodes")]
    pub good_quality_episodes: u32,
    #[serde(default = "default_min_bucket_coverage")]
    pub min_bucket_coverage: f32,
}

impl Default for BehaviorModelConfig {
    fn default() -> Self {
        Self {
            min_bucket_samples: default_min_bucket_samples(),
            max_exemplars_per_bucket: default_max_exemplars_per_bucket(),
            min_quality_episodes: default_min_quality_episodes(),
            usable_quality_episodes: default_usable_quality_episodes(),
            good_quality_episodes: default_good_quality_episodes(),
            min_bucket_coverage: default_min_bucket_coverage(),
        }
    }
}

impl BehaviorModelConfig {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.min_bucket_samples == 0
            || self.max_exemplars_per_bucket == 0
            || self.max_exemplars_per_bucket > 256
            || self.min_quality_episodes == 0
            || self.min_quality_episodes > self.usable_quality_episodes
            || self.usable_quality_episodes > self.good_quality_episodes
            || !self.min_bucket_coverage.is_finite()
            || !(0.0..=1.0).contains(&self.min_bucket_coverage)
        {
            return Err(AppError::invalid(
                "behavior_v2_model_config_invalid",
                "行为模型质量阈值配置无效",
            ));
        }
        Ok(())
    }
}

fn default_min_bucket_samples() -> u32 {
    DEFAULT_MIN_BUCKET_SAMPLES
}

fn default_max_exemplars_per_bucket() -> usize {
    DEFAULT_MAX_EXEMPLARS_PER_BUCKET
}

fn default_min_quality_episodes() -> u32 {
    3
}

fn default_usable_quality_episodes() -> u32 {
    8
}

fn default_good_quality_episodes() -> u32 {
    16
}

fn default_min_bucket_coverage() -> f32 {
    0.5
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum DistanceBucket {
    Near,
    Short,
    Medium,
    Long,
}

impl DistanceBucket {
    pub fn from_distance(distance: f32) -> Self {
        if distance < 80.0 {
            Self::Near
        } else if distance < 220.0 {
            Self::Short
        } else if distance < 600.0 {
            Self::Medium
        } else {
            Self::Long
        }
    }

    pub fn rank(self) -> u8 {
        match self {
            Self::Near => 0,
            Self::Short => 1,
            Self::Medium => 2,
            Self::Long => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "UPPERCASE")]
pub enum DirectionBucket {
    N,
    Ne,
    E,
    Se,
    S,
    Sw,
    W,
    Nw,
}

impl DirectionBucket {
    pub fn from_angle(angle: f32) -> Self {
        let normalized = angle.rem_euclid(std::f32::consts::TAU);
        let index = ((normalized / (std::f32::consts::FRAC_PI_4)) + 0.5).floor() as usize % 8;
        [
            Self::E,
            Self::Ne,
            Self::N,
            Self::Nw,
            Self::W,
            Self::Sw,
            Self::S,
            Self::Se,
        ][index]
    }

    pub fn angle(self) -> f32 {
        match self {
            Self::E => 0.0,
            Self::Ne => -std::f32::consts::FRAC_PI_4,
            Self::N => -std::f32::consts::FRAC_PI_2,
            Self::Nw => -3.0 * std::f32::consts::FRAC_PI_4,
            Self::W => std::f32::consts::PI,
            Self::Sw => 3.0 * std::f32::consts::FRAC_PI_4,
            Self::S => std::f32::consts::FRAC_PI_2,
            Self::Se => std::f32::consts::FRAC_PI_4,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum TargetWidthBucket {
    Unknown,
    Narrow,
    Medium,
    Wide,
}

impl TargetWidthBucket {
    pub fn from_width(width: Option<f32>) -> Self {
        match width.filter(|value| value.is_finite() && *value > 0.0) {
            None => Self::Unknown,
            Some(value) if value < 40.0 => Self::Narrow,
            Some(value) if value < 100.0 => Self::Medium,
            Some(_) => Self::Wide,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct PointerBucket {
    pub distance: DistanceBucket,
    pub direction: DirectionBucket,
    pub followed_by_click: bool,
    pub target_width: TargetWidthBucket,
}

impl PointerBucket {
    pub fn for_episode(episode: &PointerMoveEpisode) -> Self {
        Self {
            distance: DistanceBucket::from_distance(episode.distance_px),
            direction: DirectionBucket::from_angle(episode.angle),
            followed_by_click: episode.followed_by_click,
            target_width: TargetWidthBucket::from_width(episode.target_width_px),
        }
    }

    pub fn label(&self) -> String {
        format!(
            "{:?}/{:?}/click={}/width={:?}",
            self.distance, self.direction, self.followed_by_click, self.target_width
        )
        .to_ascii_lowercase()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FeatureDistribution {
    pub samples: u32,
    pub min: f32,
    pub p10: f32,
    pub p25: f32,
    pub p50: f32,
    pub p75: f32,
    pub p90: f32,
    pub max: f32,
}

impl FeatureDistribution {
    pub fn from_values(values: &[f32]) -> Option<Self> {
        let mut sorted = values
            .iter()
            .copied()
            .filter(|value| value.is_finite())
            .collect::<Vec<_>>();
        if sorted.is_empty() {
            return None;
        }
        sorted.sort_by(f32::total_cmp);
        let percentile = |fraction: f32| {
            let index = (fraction * (sorted.len().saturating_sub(1) as f32)).round() as usize;
            sorted[index]
        };
        Some(Self {
            samples: sorted.len().min(u32::MAX as usize) as u32,
            min: sorted[0],
            p10: percentile(0.10),
            p25: percentile(0.25),
            p50: percentile(0.50),
            p75: percentile(0.75),
            p90: percentile(0.90),
            max: *sorted.last().unwrap_or(&sorted[0]),
        })
    }

    pub fn sample(&self, rng: &mut SeededRng) -> f32 {
        let unit = rng.next_unit();
        let (left, right, local) = if unit < 0.10 {
            (self.min, self.p10, unit / 0.10)
        } else if unit < 0.25 {
            (self.p10, self.p25, (unit - 0.10) / 0.15)
        } else if unit < 0.50 {
            (self.p25, self.p50, (unit - 0.25) / 0.25)
        } else if unit < 0.75 {
            (self.p50, self.p75, (unit - 0.50) / 0.25)
        } else if unit < 0.90 {
            (self.p75, self.p90, (unit - 0.75) / 0.15)
        } else {
            (self.p90, self.max, (unit - 0.90) / 0.10)
        };
        (left + (right - left) * local).clamp(self.min.min(self.max), self.min.max(self.max))
    }

    pub fn validate(&self, field: &str) -> Result<(), AppError> {
        let values = [
            self.min, self.p10, self.p25, self.p50, self.p75, self.p90, self.max,
        ];
        if self.samples == 0
            || values.iter().any(|value| !value.is_finite())
            || values.windows(2).any(|pair| pair[0] > pair[1])
        {
            return Err(AppError::invalid(
                "behavior_v2_distribution_invalid",
                format!("{field} 的经验分布无效"),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PointerFeatureDistributions {
    pub movement_time_ms: FeatureDistribution,
    pub distance_px: FeatureDistribution,
    pub path_length_px: FeatureDistribution,
    pub path_efficiency: FeatureDistribution,
    pub mean_speed: FeatureDistribution,
    pub peak_speed: FeatureDistribution,
    pub time_to_peak_ratio: FeatureDistribution,
    pub acceleration_phase_ratio: FeatureDistribution,
    pub deceleration_phase_ratio: FeatureDistribution,
    pub maximum_lateral_deviation_px: FeatureDistribution,
    pub signed_curvature: FeatureDistribution,
    pub endpoint_dwell_ms: FeatureDistribution,
    pub overshoot_count: FeatureDistribution,
    pub overshoot_distance_px: FeatureDistribution,
    pub correction_count: FeatureDistribution,
}

impl PointerFeatureDistributions {
    pub fn from_features(features: &[PointerFeatures]) -> Option<Self> {
        let distribution = |values: Vec<f32>| FeatureDistribution::from_values(&values);
        Some(Self {
            movement_time_ms: distribution(
                features
                    .iter()
                    .map(|value| value.movement_time_ms)
                    .collect(),
            )?,
            distance_px: distribution(features.iter().map(|value| value.distance_px).collect())?,
            path_length_px: distribution(
                features.iter().map(|value| value.path_length_px).collect(),
            )?,
            path_efficiency: distribution(
                features.iter().map(|value| value.path_efficiency).collect(),
            )?,
            mean_speed: distribution(features.iter().map(|value| value.mean_speed).collect())?,
            peak_speed: distribution(features.iter().map(|value| value.peak_speed).collect())?,
            time_to_peak_ratio: distribution(
                features
                    .iter()
                    .map(|value| value.time_to_peak_ratio)
                    .collect(),
            )?,
            acceleration_phase_ratio: distribution(
                features
                    .iter()
                    .map(|value| value.acceleration_phase_ratio)
                    .collect(),
            )?,
            deceleration_phase_ratio: distribution(
                features
                    .iter()
                    .map(|value| value.deceleration_phase_ratio)
                    .collect(),
            )?,
            maximum_lateral_deviation_px: distribution(
                features
                    .iter()
                    .map(|value| value.maximum_lateral_deviation_px)
                    .collect(),
            )?,
            signed_curvature: distribution(
                features
                    .iter()
                    .map(|value| value.signed_curvature)
                    .collect(),
            )?,
            endpoint_dwell_ms: distribution(
                features
                    .iter()
                    .map(|value| value.endpoint_dwell_ms)
                    .collect(),
            )?,
            overshoot_count: distribution(
                features
                    .iter()
                    .map(|value| value.overshoot_count as f32)
                    .collect(),
            )?,
            overshoot_distance_px: distribution(
                features
                    .iter()
                    .map(|value| value.overshoot_distance_px)
                    .collect(),
            )?,
            correction_count: distribution(
                features
                    .iter()
                    .map(|value| value.correction_count as f32)
                    .collect(),
            )?,
        })
    }

    pub fn sample(&self, rng: &mut SeededRng) -> PointerFeatures {
        PointerFeatures {
            movement_time_ms: self.movement_time_ms.sample(rng),
            distance_px: self.distance_px.sample(rng),
            path_length_px: self.path_length_px.sample(rng),
            path_efficiency: self.path_efficiency.sample(rng).clamp(0.0, 1.0),
            mean_speed: self.mean_speed.sample(rng).max(0.0),
            peak_speed: self.peak_speed.sample(rng).max(0.0),
            time_to_peak_ratio: self.time_to_peak_ratio.sample(rng).clamp(0.0, 1.0),
            acceleration_phase_ratio: self.acceleration_phase_ratio.sample(rng).clamp(0.0, 1.0),
            deceleration_phase_ratio: self.deceleration_phase_ratio.sample(rng).clamp(0.0, 1.0),
            maximum_lateral_deviation_px: self.maximum_lateral_deviation_px.sample(rng).max(0.0),
            signed_curvature: self.signed_curvature.sample(rng),
            endpoint_dwell_ms: self.endpoint_dwell_ms.sample(rng).max(0.0),
            overshoot_count: self.overshoot_count.sample(rng).max(0.0).round() as u32,
            overshoot_distance_px: self.overshoot_distance_px.sample(rng).max(0.0),
            correction_count: self.correction_count.sample(rng).max(0.0).round() as u32,
            coverage: 1.0,
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        let fields = [
            ("movementTimeMs", &self.movement_time_ms),
            ("distancePx", &self.distance_px),
            ("pathLengthPx", &self.path_length_px),
            ("pathEfficiency", &self.path_efficiency),
            ("meanSpeed", &self.mean_speed),
            ("peakSpeed", &self.peak_speed),
            ("timeToPeakRatio", &self.time_to_peak_ratio),
            ("accelerationPhaseRatio", &self.acceleration_phase_ratio),
            ("decelerationPhaseRatio", &self.deceleration_phase_ratio),
            (
                "maximumLateralDeviationPx",
                &self.maximum_lateral_deviation_px,
            ),
            ("signedCurvature", &self.signed_curvature),
            ("endpointDwellMs", &self.endpoint_dwell_ms),
            ("overshootCount", &self.overshoot_count),
            ("overshootDistancePx", &self.overshoot_distance_px),
            ("correctionCount", &self.correction_count),
        ];
        for (name, distribution) in fields {
            distribution.validate(name)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PointerBucketModel {
    pub key: PointerBucket,
    pub valid_sample_count: u32,
    pub coverage: f32,
    pub features: PointerFeatureDistributions,
    #[serde(default)]
    pub exemplars: Vec<PointerFeatures>,
    #[serde(default)]
    pub fallback_level: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PointerModel {
    pub buckets: Vec<PointerBucketModel>,
    pub total_episode_count: u32,
    pub valid_episode_count: u32,
    pub discarded_episode_count: u32,
}

impl PointerModel {
    pub fn empty() -> Self {
        Self {
            buckets: Vec::new(),
            total_episode_count: 0,
            valid_episode_count: 0,
            discarded_episode_count: 0,
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.valid_episode_count > self.total_episode_count
            || self.discarded_episode_count > self.total_episode_count
        {
            return Err(AppError::invalid(
                "behavior_v2_pointer_model_counts",
                "PointerModel 的样本计数不一致",
            ));
        }
        for bucket in &self.buckets {
            if bucket.valid_sample_count == 0
                || !bucket.coverage.is_finite()
                || !(0.0..=1.0).contains(&bucket.coverage)
            {
                return Err(AppError::invalid(
                    "behavior_v2_pointer_bucket_invalid",
                    "PointerModel bucket 的 coverage 或样本数无效",
                ));
            }
            bucket.features.validate()?;
            if bucket.exemplars.len() > DEFAULT_MAX_EXEMPLARS_PER_BUCKET
                || bucket.exemplars.iter().any(|exemplar| !exemplar.finite())
            {
                return Err(AppError::invalid(
                    "behavior_v2_pointer_exemplars_invalid",
                    "PointerModel bucket 的 exemplar 无效",
                ));
            }
        }
        Ok(())
    }

    pub fn select_bucket(
        &self,
        key: &PointerBucket,
        config: &BehaviorModelConfig,
    ) -> Option<(&PointerBucketModel, String, Option<String>, u8)> {
        if let Some(exact) = self.buckets.iter().find(|bucket| {
            bucket.key == *key && bucket.valid_sample_count >= config.min_bucket_samples
        }) {
            return Some((exact, exact.key.label(), None, 0));
        }

        let candidate = self
            .buckets
            .iter()
            .filter(|bucket| {
                bucket.valid_sample_count > 0
                    && bucket.key.followed_by_click == key.followed_by_click
            })
            .min_by_key(|bucket| {
                let distance = (i16::from(bucket.key.distance.rank())
                    - i16::from(key.distance.rank()))
                .unsigned_abs();
                let direction = if bucket.key.direction == key.direction {
                    0
                } else {
                    1
                };
                let width = if bucket.key.target_width == key.target_width
                    || bucket.key.target_width == TargetWidthBucket::Unknown
                    || key.target_width == TargetWidthBucket::Unknown
                {
                    0
                } else {
                    1
                };
                (distance, direction, width)
            });
        candidate.map(|bucket| {
            let distance_level = (i16::from(bucket.key.distance.rank())
                - i16::from(key.distance.rank()))
            .unsigned_abs() as u8;
            let context_level = u8::from(bucket.key.direction != key.direction)
                + u8::from(bucket.key.target_width != key.target_width);
            let level = distance_level.saturating_add(context_level).max(1);
            (
                bucket,
                bucket.key.label(),
                Some(if bucket.valid_sample_count < config.min_bucket_samples {
                    "insufficient_samples_in_exact_bucket".to_string()
                } else {
                    "parent_bucket_fallback".to_string()
                }),
                level,
            )
        })
    }

    pub fn quality(&self, config: &BehaviorModelConfig) -> ModelQuality {
        let below_good_sample_threshold = self.valid_episode_count < config.usable_quality_episodes
            || (self.valid_episode_count >= config.usable_quality_episodes
                && self.valid_episode_count < config.good_quality_episodes);
        if self.valid_episode_count < config.min_quality_episodes || self.buckets.is_empty() {
            ModelQuality::Insufficient
        } else if below_good_sample_threshold
            || self
                .buckets
                .iter()
                .any(|bucket| bucket.coverage < config.min_bucket_coverage)
        {
            ModelQuality::Usable
        } else {
            ModelQuality::Good
        }
    }

    pub fn sample_features(bucket: &PointerBucketModel, rng: &mut SeededRng) -> PointerFeatures {
        if bucket.exemplars.is_empty() {
            bucket.features.sample(rng)
        } else {
            let index = (rng.next_u64() as usize) % bucket.exemplars.len();
            bucket.exemplars[index]
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClickBucketModel {
    pub button: MouseButton,
    pub followed_by_move: bool,
    pub valid_sample_count: u32,
    pub coverage: f32,
    pub pre_click_dwell_ms: FeatureDistribution,
    pub hold_ms: FeatureDistribution,
    pub post_click_dwell_ms: FeatureDistribution,
    #[serde(default)]
    pub fallback_level: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClickModel {
    pub buckets: Vec<ClickBucketModel>,
    pub total_click_count: u32,
    pub valid_click_count: u32,
}

impl ClickModel {
    pub fn empty() -> Self {
        Self {
            buckets: Vec::new(),
            total_click_count: 0,
            valid_click_count: 0,
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.valid_click_count > self.total_click_count {
            return Err(AppError::invalid(
                "behavior_v2_click_model_counts",
                "ClickModel 的样本计数不一致",
            ));
        }
        for bucket in &self.buckets {
            if bucket.valid_sample_count == 0
                || !bucket.coverage.is_finite()
                || !(0.0..=1.0).contains(&bucket.coverage)
            {
                return Err(AppError::invalid(
                    "behavior_v2_click_bucket_invalid",
                    "ClickModel bucket 无效",
                ));
            }
            bucket.pre_click_dwell_ms.validate("preClickDwellMs")?;
            bucket.hold_ms.validate("holdMs")?;
            bucket.post_click_dwell_ms.validate("postClickDwellMs")?;
        }
        Ok(())
    }

    pub fn select_bucket(
        &self,
        button: MouseButton,
        followed_by_move: bool,
        config: &BehaviorModelConfig,
    ) -> Option<(&ClickBucketModel, Option<String>, u8)> {
        if let Some(exact) = self.buckets.iter().find(|bucket| {
            bucket.button == button
                && bucket.followed_by_move == followed_by_move
                && bucket.valid_sample_count > 0
        }) {
            if exact.valid_sample_count >= config.min_bucket_samples {
                return Some((exact, None, 0));
            }
            return Some((
                exact,
                Some("insufficient_samples_in_exact_click_bucket".to_string()),
                1,
            ));
        }

        self.buckets
            .iter()
            .find(|bucket| bucket.button == button && bucket.valid_sample_count > 0)
            .map(|bucket| (bucket, Some("click_context_fallback".to_string()), 2))
            .or_else(|| {
                self.buckets
                    .iter()
                    .find(|bucket| bucket.valid_sample_count > 0)
                    .map(|bucket| (bucket, Some("button_bucket_fallback".to_string()), 3))
            })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModelQuality {
    Insufficient,
    Usable,
    Good,
}

impl Display for ModelQuality {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Insufficient => "insufficient",
            Self::Usable => "usable",
            Self::Good => "good",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BucketCoverage {
    pub bucket: String,
    pub valid_sample_count: u32,
    pub coverage: f32,
    pub fallback_level: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CoverageSummary {
    pub raw_event_count: u64,
    pub pointer_episode_count: u32,
    pub valid_pointer_episode_count: u32,
    #[serde(default)]
    pub click_associated_pointer_episode_count: u32,
    pub click_episode_count: u32,
    pub discarded_event_count: u32,
    pub discarded_reasons: HashMap<String, u32>,
    pub bucket_coverage: Vec<BucketCoverage>,
    pub quality: ModelQuality,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SourceRetention {
    #[default]
    Persisted,
    Ephemeral,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorProfileV2 {
    pub id: String,
    pub name: String,
    pub api_version: u32,
    pub source_session_ids: Vec<String>,
    pub created_at_ms: u64,
    #[serde(default)]
    pub source_retention: SourceRetention,
    #[serde(default)]
    pub model_config: BehaviorModelConfig,
    pub coverage: CoverageSummary,
    pub pointer_model: PointerModel,
    pub click_model: ClickModel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typing_model: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_model: Option<serde_json::Value>,
}

impl BehaviorProfileV2 {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.id.trim().is_empty() || self.name.trim().is_empty() {
            return Err(AppError::invalid(
                "behavior_v2_profile_name_missing",
                "V2 行为档案必须包含名称和 ID",
            ));
        }
        if self.api_version != BEHAVIOR_V2_API_VERSION {
            return Err(AppError::invalid(
                "behavior_v2_profile_version",
                "V2 行为档案 API 版本不受支持",
            ));
        }
        if self.source_session_ids.is_empty() {
            return Err(AppError::invalid(
                "behavior_v2_profile_source_missing",
                "V2 行为档案至少需要一个训练会话来源",
            ));
        }
        self.pointer_model.validate()?;
        self.click_model.validate()?;
        self.model_config.validate()?;
        if self.coverage.click_associated_pointer_episode_count
            > self.coverage.valid_pointer_episode_count
        {
            return Err(AppError::invalid(
                "behavior_v2_click_association_count",
                "与点击关联的轨迹数不能超过有效轨迹数",
            ));
        }
        if self.coverage.quality != self.pointer_model.quality(&self.model_config) {
            return Err(AppError::invalid(
                "behavior_v2_quality_mismatch",
                "档案质量标记与 PointerModel 不一致",
            ));
        }
        Ok(())
    }

    pub fn generated_api(&self) -> BehaviorApi {
        BehaviorApi {
            version: BEHAVIOR_V2_API_VERSION,
            profile_id: self.id.clone(),
            profile_name: self.name.clone(),
            functions: vec![
                super::super::BehaviorApiFunction {
                    name: "bio_move_to".to_string(),
                    signature: "bio_move_to(x, y, options?)".to_string(),
                    description: "按当前宏绑定的 V2 策略生成带加速、峰值和减速的鼠标移动".to_string(),
                },
                super::super::BehaviorApiFunction {
                    name: "bio_click".to_string(),
                    signature: "bio_click(button, x, y)".to_string(),
                    description: "将移动到目标与点击停顿作为一个组合动作执行".to_string(),
                },
                super::super::BehaviorApiFunction {
                    name: "bio_type_text".to_string(),
                    signature: "bio_type_text(text, options?)".to_string(),
                    description: "预留的策略化文本输入接口；当前阶段保持原始文本语义".to_string(),
                },
            ],
            source: format!(
                "// AutoFlow behavior API v2\n// 当前模型：{} ({})\n\n// 策略由宏绑定，不读取 UI 当前选中状态\nbio_move_to(820, 430);\nbio_move_to(820, 430, #{{ target_width: 80, intent: \"click\" }});\nbio_click(\"left\", 820, 430);\nbio_type_text(\"示例文本\", #{{ mode: \"normal\" }});\n",
                self.name.replace(['\r', '\n'], " "),
                self.id.replace(['\r', '\n'], " ")
            ),
        }
    }
}

pub fn train_behavior_profile(session: &BehaviorSessionV2) -> Result<BehaviorProfileV2, AppError> {
    train_behavior_profile_with_retention(session, SourceRetention::Persisted)
}

pub fn train_behavior_profile_with_retention(
    session: &BehaviorSessionV2,
    source_retention: SourceRetention,
) -> Result<BehaviorProfileV2, AppError> {
    session.validate()?;
    let model_config = BehaviorModelConfig::default();
    let segmentation =
        super::segment::segment_mouse_actions(&session.raw_events, &SegmentationConfig::default())?;
    let mut groups = HashMap::<PointerBucket, Vec<PointerFeatures>>::new();
    let mut valid_pointer_episode_count = 0u32;
    let mut click_associated_pointer_episode_count = 0u32;
    let mut discarded_reasons = HashMap::<String, u32>::new();
    for discarded in &segmentation.discarded_events {
        *discarded_reasons
            .entry(discarded.reason.clone())
            .or_default() += 1;
    }
    for episode in &segmentation.pointer_moves {
        match extract_pointer_features(episode) {
            Ok(features) if features.finite() => {
                valid_pointer_episode_count += 1;
                if episode.followed_by_click {
                    click_associated_pointer_episode_count += 1;
                }
                groups
                    .entry(PointerBucket::for_episode(episode))
                    .or_default()
                    .push(features);
            }
            Ok(_) | Err(_) => {
                *discarded_reasons
                    .entry("feature_extraction_failed".to_string())
                    .or_default() += 1;
            }
        }
    }

    let total_pointer_episode_count = segmentation.pointer_moves.len() as u32;
    let pointer_model = build_pointer_model(
        total_pointer_episode_count,
        valid_pointer_episode_count,
        &groups,
    );
    let click_model = build_click_model(&segmentation.clicks);
    let bucket_coverage = pointer_model
        .buckets
        .iter()
        .map(|bucket| BucketCoverage {
            bucket: bucket.key.label(),
            valid_sample_count: bucket.valid_sample_count,
            coverage: bucket.coverage,
            fallback_level: bucket.fallback_level,
        })
        .collect::<Vec<_>>();
    let quality = pointer_model.quality(&model_config);
    let profile = BehaviorProfileV2 {
        id: format!("behavior-v2-{}", session.id),
        name: session.name.clone(),
        api_version: BEHAVIOR_V2_API_VERSION,
        source_session_ids: vec![session.id.clone()],
        created_at_ms: session.created_at_ms,
        source_retention,
        model_config,
        coverage: CoverageSummary {
            raw_event_count: session.raw_events.len() as u64,
            pointer_episode_count: total_pointer_episode_count,
            valid_pointer_episode_count,
            click_associated_pointer_episode_count,
            click_episode_count: segmentation.clicks.len() as u32,
            discarded_event_count: segmentation.discarded_events.len() as u32,
            discarded_reasons,
            bucket_coverage,
            quality,
        },
        pointer_model,
        click_model,
        typing_model: None,
        scroll_model: None,
    };
    profile.validate()?;
    Ok(profile)
}

fn build_pointer_model(
    total_episode_count: u32,
    valid_episode_count: u32,
    groups: &HashMap<PointerBucket, Vec<PointerFeatures>>,
) -> PointerModel {
    let mut entries = groups.iter().collect::<Vec<_>>();
    entries.sort_by_key(|(bucket, _)| bucket.label());
    let buckets = entries
        .into_iter()
        .filter_map(|(key, features)| {
            Some(PointerBucketModel {
                key: key.clone(),
                valid_sample_count: features.len().min(u32::MAX as usize) as u32,
                coverage: if total_episode_count == 0 {
                    0.0
                } else {
                    features.len() as f32 / total_episode_count as f32
                },
                features: PointerFeatureDistributions::from_features(features)?,
                exemplars: features
                    .iter()
                    .take(DEFAULT_MAX_EXEMPLARS_PER_BUCKET)
                    .copied()
                    .collect(),
                fallback_level: 0,
            })
        })
        .collect();
    PointerModel {
        buckets,
        total_episode_count,
        valid_episode_count,
        discarded_episode_count: total_episode_count.saturating_sub(valid_episode_count),
    }
}

fn build_click_model(clicks: &[ClickEpisode]) -> ClickModel {
    let mut groups = HashMap::<(MouseButton, bool), Vec<&ClickEpisode>>::new();
    for click in clicks {
        groups
            .entry((click.button, click.pointer_move_episode_index.is_some()))
            .or_default()
            .push(click);
    }
    let mut entries = groups.into_iter().collect::<Vec<_>>();
    entries.sort_by_key(|((button, followed), _)| (format!("{button:?}"), *followed));
    let buckets = entries
        .into_iter()
        .filter_map(|((button, followed_by_move), values)| {
            Some(ClickBucketModel {
                button,
                followed_by_move,
                valid_sample_count: values.len().min(u32::MAX as usize) as u32,
                coverage: values.len() as f32 / clicks.len().max(1) as f32,
                pre_click_dwell_ms: distribution(
                    values
                        .iter()
                        .map(|value| value.pre_click_dwell_ms as f32)
                        .collect(),
                )?,
                hold_ms: distribution(values.iter().map(|value| value.hold_ms as f32).collect())?,
                post_click_dwell_ms: distribution(
                    values
                        .iter()
                        .map(|value| value.post_click_dwell_ms as f32)
                        .collect(),
                )?,
                fallback_level: 0,
            })
        })
        .collect();
    ClickModel {
        buckets,
        total_click_count: clicks.len().min(u32::MAX as usize) as u32,
        valid_click_count: clicks.len().min(u32::MAX as usize) as u32,
    }
}

fn distribution(values: Vec<f32>) -> Option<FeatureDistribution> {
    FeatureDistribution::from_values(&values)
}
