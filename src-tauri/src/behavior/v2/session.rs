use crate::behavior::{BehaviorEvent, BehaviorProfile};
use crate::AppError;
use serde::{Deserialize, Serialize};

use super::events::behavior_event_parts;
use super::model::{BehaviorProfileV2, SourceRetention};
use super::validation::{validate_coordinate, V2_MAX_SESSION_EVENTS};
use super::BEHAVIOR_V2_API_VERSION;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorCaptureMetadata {
    #[serde(default = "default_platform")]
    pub platform: String,
    #[serde(default = "default_hook_name")]
    pub hook: String,
    #[serde(default)]
    pub screen_width: Option<u32>,
    #[serde(default)]
    pub screen_height: Option<u32>,
    #[serde(default = "default_true")]
    pub retained_raw_events: bool,
    #[serde(default)]
    pub dropped_event_count: u64,
}

impl Default for BehaviorCaptureMetadata {
    fn default() -> Self {
        Self {
            platform: default_platform(),
            hook: default_hook_name(),
            screen_width: None,
            screen_height: None,
            retained_raw_events: true,
            dropped_event_count: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorSessionV2 {
    pub id: String,
    pub name: String,
    pub api_version: u32,
    pub created_at_ms: u64,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_tag: Option<String>,
    pub capture_metadata: BehaviorCaptureMetadata,
    pub raw_events: Vec<BehaviorEvent>,
}

impl BehaviorSessionV2 {
    pub fn from_legacy_profile(profile: &BehaviorProfile) -> Result<Self, AppError> {
        if profile.raw_events.is_empty() {
            return Err(AppError::invalid(
                "behavior_v2_raw_events_missing",
                "当前录制未保留原始事件，无法建立 V2 训练会话；请重新开启原始记录",
            ));
        }
        let session = Self {
            id: format!("session-v2-{}", profile.id),
            name: profile.name.clone(),
            api_version: BEHAVIOR_V2_API_VERSION,
            created_at_ms: now_millis(),
            duration_ms: profile.duration_ms,
            task_tag: None,
            capture_metadata: BehaviorCaptureMetadata::default(),
            raw_events: profile.raw_events.clone(),
        };
        session.validate()?;
        Ok(session)
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.id.trim().is_empty() || self.name.trim().is_empty() {
            return Err(AppError::invalid(
                "behavior_v2_session_name_missing",
                "V2 训练会话必须包含名称和 ID",
            ));
        }
        if self.api_version != BEHAVIOR_V2_API_VERSION {
            return Err(AppError::invalid(
                "behavior_v2_session_version",
                "训练会话 API 版本不受支持",
            ));
        }
        if self.name.chars().count() > 64 {
            return Err(AppError::invalid(
                "behavior_v2_session_name_too_long",
                "训练会话名称不能超过 64 个字符",
            ));
        }
        if self.duration_ms > 86_400_000 {
            return Err(AppError::invalid(
                "behavior_v2_session_duration_invalid",
                "训练会话时长不能超过 24 小时",
            ));
        }
        if self.raw_events.len() > V2_MAX_SESSION_EVENTS {
            return Err(AppError::invalid(
                "behavior_v2_session_event_limit",
                "训练会话原始事件超过安全上限",
            ));
        }
        let mut previous_timestamp = None;
        for event in &self.raw_events {
            let (timestamp, coordinate) = behavior_event_parts(event);
            if previous_timestamp.is_some_and(|previous| timestamp < previous) {
                return Err(AppError::invalid(
                    "behavior_v2_timestamp_order",
                    "训练会话原始事件时间戳不是非递减顺序",
                ));
            }
            if let Some((x, y)) = coordinate {
                validate_coordinate(x, y)?;
            }
            previous_timestamp = Some(timestamp);
        }
        if self.capture_metadata.dropped_event_count > self.raw_events.len() as u64 {
            return Err(AppError::invalid(
                "behavior_v2_capture_metadata_invalid",
                "采集元数据中的丢弃事件数量不合理",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorSessionFile {
    pub id: String,
    pub name: String,
    pub file_name: String,
    pub created_at_ms: u64,
    pub duration_ms: u64,
    pub raw_event_count: u64,
}

impl BehaviorSessionFile {
    pub fn from_session(session: &BehaviorSessionV2, file_name: String) -> Self {
        Self {
            id: session.id.clone(),
            name: session.name.clone(),
            file_name,
            created_at_ms: session.created_at_ms,
            duration_ms: session.duration_ms,
            raw_event_count: session.raw_events.len() as u64,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorProfileV2File {
    pub id: String,
    pub name: String,
    pub file_name: String,
    pub source_session_ids: Vec<String>,
    pub created_at_ms: u64,
    pub quality: String,
    #[serde(default)]
    pub source_retention: SourceRetention,
}

impl BehaviorProfileV2File {
    pub fn from_profile(profile: &BehaviorProfileV2, file_name: String) -> Self {
        Self {
            id: profile.id.clone(),
            name: profile.name.clone(),
            file_name,
            source_session_ids: profile.source_session_ids.clone(),
            created_at_ms: profile.created_at_ms,
            quality: profile.coverage.quality.to_string(),
            source_retention: profile.source_retention,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_platform() -> String {
    std::env::consts::OS.to_string()
}

fn default_hook_name() -> String {
    "WH_KEYBOARD_LL/WH_MOUSE_LL".to_string()
}

fn now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
