use crate::AppError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorPolicy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default = "default_strength")]
    pub timing_strength: f32,
    #[serde(default = "default_strength")]
    pub pointer_path_strength: f32,
    #[serde(default = "default_strength")]
    pub pause_strength: f32,
    #[serde(default = "default_correction_strength")]
    pub correction_strength: f32,
    #[serde(default = "default_speed_scale")]
    pub speed_scale: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
}

impl Default for BehaviorPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            profile_id: None,
            timing_strength: 0.0,
            pointer_path_strength: 0.0,
            pause_strength: 0.0,
            correction_strength: default_correction_strength(),
            speed_scale: default_speed_scale(),
            seed: None,
        }
    }
}

impl BehaviorPolicy {
    pub fn from_legacy(enabled: bool, intensity: f32, profile_id: Option<String>) -> Self {
        let strength = if intensity.is_finite() {
            intensity.clamp(0.0, 1.0)
        } else {
            0.0
        };
        Self {
            enabled,
            profile_id,
            timing_strength: strength,
            pointer_path_strength: strength,
            pause_strength: strength,
            correction_strength: (strength * 0.5).clamp(0.0, 1.0),
            speed_scale: 1.0,
            seed: None,
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        let strengths = [
            self.timing_strength,
            self.pointer_path_strength,
            self.pause_strength,
            self.correction_strength,
        ];
        if strengths
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
        {
            return Err(AppError::invalid(
                "behavior_policy_strength_invalid",
                "行为策略强度必须在 0 到 1 之间",
            ));
        }
        if !self.speed_scale.is_finite() || !(0.1..=4.0).contains(&self.speed_scale) {
            return Err(AppError::invalid(
                "behavior_policy_speed_invalid",
                "行为策略 speedScale 必须在 0.1 到 4 之间",
            ));
        }
        if self
            .profile_id
            .as_ref()
            .is_some_and(|id| id.trim().is_empty())
        {
            return Err(AppError::invalid(
                "behavior_policy_profile_invalid",
                "行为策略 profileId 不能是空字符串",
            ));
        }
        Ok(())
    }

    pub fn normalized(&self) -> Result<Self, AppError> {
        self.validate()?;
        Ok(Self {
            enabled: self.enabled,
            profile_id: self.profile_id.clone(),
            timing_strength: self.timing_strength.clamp(0.0, 1.0),
            pointer_path_strength: self.pointer_path_strength.clamp(0.0, 1.0),
            pause_strength: self.pause_strength.clamp(0.0, 1.0),
            correction_strength: self.correction_strength.clamp(0.0, 1.0),
            speed_scale: self.speed_scale.clamp(0.1, 4.0),
            seed: self.seed,
        })
    }
}

fn default_strength() -> f32 {
    0.0
}

fn default_correction_strength() -> f32 {
    0.15
}

fn default_speed_scale() -> f32 {
    1.0
}
