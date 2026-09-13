use crate::automation::{
    AutomationAsset, MAX_ASSET_NAME_LENGTH, MAX_TEMPLATE_HEIGHT, MAX_TEMPLATE_PIXELS,
    MAX_TEMPLATE_WIDTH,
};
use crate::AppError;
use serde::{de::Deserializer, Deserialize, Serialize};
use std::collections::HashSet;

pub const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default = "default_true")]
    pub global_enabled: bool,
    #[serde(default = "default_emergency_stop")]
    pub emergency_stop: String,
    #[serde(default)]
    pub navigation_auto_collapse: bool,
    #[serde(default)]
    pub launch_at_startup: bool,
    #[serde(default)]
    pub hotkeys: Vec<HotkeyRule>,
    #[serde(default)]
    pub text_expansions: Vec<TextExpansionRule>,
    #[serde(default)]
    pub macros: Vec<MacroRule>,
    #[serde(default)]
    pub assets: Vec<AutomationAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyRule {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub trigger_keys: Vec<String>,
    pub action: HotkeyAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyAction {
    #[serde(rename = "type")]
    pub action_type: String,
    #[serde(default)]
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextExpansionRule {
    pub id: String,
    pub name: String,
    pub abbreviation: String,
    pub replacement: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default)]
    pub sensitive: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MacroRule {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub trigger_keys: Vec<String>,
    #[serde(default)]
    pub mode: MacroMode,
    #[serde(default = "default_repeat_count")]
    pub repeat_count: u32,
    #[serde(default = "default_macro_speed")]
    pub speed: f32,
    #[serde(default = "default_true")]
    pub record_mouse_move: bool,
    #[serde(default = "default_true")]
    pub record_mouse_clicks: bool,
    #[serde(default)]
    pub target: Option<MacroTarget>,
    pub program: AutomationProgram,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AutomationProgram {
    Macro {
        #[serde(default)]
        steps: Vec<MacroStep>,
    },
    Rhai {
        source: String,
        #[serde(rename = "apiVersion", alias = "api_version")]
        api_version: u32,
    },
}

impl Default for AutomationProgram {
    fn default() -> Self {
        Self::Macro { steps: Vec::new() }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MacroRuleWire {
    id: String,
    name: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    trigger_keys: Vec<String>,
    #[serde(default)]
    mode: MacroMode,
    #[serde(default = "default_repeat_count")]
    repeat_count: u32,
    #[serde(default = "default_macro_speed")]
    speed: f32,
    #[serde(default = "default_true")]
    record_mouse_move: bool,
    #[serde(default = "default_true")]
    record_mouse_clicks: bool,
    #[serde(default)]
    target: Option<MacroTarget>,
    #[serde(default)]
    program: Option<AutomationProgram>,
    #[serde(default)]
    steps: Option<Vec<MacroStep>>,
}

impl<'de> Deserialize<'de> for MacroRule {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = MacroRuleWire::deserialize(deserializer)?;
        let program = wire.program.unwrap_or_else(|| AutomationProgram::Macro {
            steps: wire.steps.unwrap_or_default(),
        });
        Ok(Self {
            id: wire.id,
            name: wire.name,
            enabled: wire.enabled,
            trigger_keys: wire.trigger_keys,
            mode: wire.mode,
            repeat_count: wire.repeat_count,
            speed: wire.speed,
            record_mouse_move: wire.record_mouse_move,
            record_mouse_clicks: wire.record_mouse_clicks,
            target: wire.target,
            program,
        })
    }
}

impl MacroRule {
    pub fn macro_steps(&self) -> Option<&[MacroStep]> {
        match &self.program {
            AutomationProgram::Macro { steps } => Some(steps),
            AutomationProgram::Rhai { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MacroTarget {
    pub process_name: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MacroMode {
    #[default]
    Once,
    Repeat,
    Hold,
    Toggle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MacroStep {
    Delay {
        #[serde(rename = "durationMs", alias = "duration_ms")]
        duration_ms: u64,
        #[serde(
            default,
            rename = "durationMaxMs",
            alias = "duration_max_ms",
            skip_serializing_if = "Option::is_none"
        )]
        duration_max_ms: Option<u64>,
    },
    Key {
        key: String,
        action: KeyAction,
    },
    MouseButton {
        button: MouseButton,
        action: KeyAction,
        x: i32,
        y: i32,
    },
    MouseMove {
        x: i32,
        y: i32,
    },
    Wheel {
        #[serde(rename = "deltaX", alias = "delta_x")]
        delta_x: i32,
        #[serde(rename = "deltaY", alias = "delta_y")]
        delta_y: i32,
    },
    Text {
        text: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum KeyAction {
    Down,
    Up,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            global_enabled: true,
            emergency_stop: "F12".to_string(),
            navigation_auto_collapse: false,
            launch_at_startup: false,
            hotkeys: vec![
                HotkeyRule {
                    id: "capslock-to-escape".to_string(),
                    name: "CapsLock 改为 Esc".to_string(),
                    enabled: false,
                    trigger_keys: vec!["CapsLock".to_string()],
                    action: HotkeyAction {
                        action_type: "remap".to_string(),
                        target: "Esc".to_string(),
                    },
                },
                HotkeyRule {
                    id: "open-terminal".to_string(),
                    name: "打开 Windows Terminal".to_string(),
                    enabled: false,
                    trigger_keys: vec!["Ctrl".to_string(), "Alt".to_string(), "T".to_string()],
                    action: HotkeyAction {
                        action_type: "launch".to_string(),
                        target: "wt.exe".to_string(),
                    },
                },
            ],
            text_expansions: vec![TextExpansionRule {
                id: "common-email".to_string(),
                name: "常用邮箱".to_string(),
                abbreviation: "@@".to_string(),
                replacement: "example@example.com".to_string(),
                enabled: false,
                case_sensitive: false,
                sensitive: false,
            }],
            macros: Vec::new(),
            assets: Vec::new(),
        }
    }
}

impl AppConfig {
    pub fn migrate(mut self) -> Result<(Self, bool), AppError> {
        if self.schema_version > SCHEMA_VERSION {
            return Err(AppError::invalid(
                "config_schema_version",
                "配置版本高于当前版本，请升级 AutoFlow 后再打开",
            ));
        }
        let migrated = self.schema_version != SCHEMA_VERSION;
        self.schema_version = SCHEMA_VERSION;
        Ok((self, migrated))
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(AppError::invalid(
                "config_schema_version",
                "配置版本暂不兼容，请导出后重新建立配置",
            ));
        }

        if self.emergency_stop.trim().is_empty() {
            return Err(AppError::invalid(
                "emergency_stop_empty",
                "紧急停止键不能为空，请设置为 F12 或其他按键",
            ));
        }

        let mut hotkey_signatures = HashSet::new();
        for rule in &self.hotkeys {
            if rule.id.trim().is_empty() || rule.name.trim().is_empty() {
                return Err(AppError::invalid(
                    "hotkey_missing_name",
                    "快捷键规则需要填写名称",
                ));
            }
            if rule.trigger_keys.is_empty() {
                return Err(AppError::invalid(
                    "hotkey_missing_trigger",
                    "快捷键规则至少需要一个触发键",
                ));
            }
            let signature = rule
                .trigger_keys
                .iter()
                .map(|key| key.trim().to_ascii_uppercase())
                .collect::<Vec<_>>()
                .join("+");
            if !hotkey_signatures.insert(signature) {
                return Err(AppError::invalid(
                    "hotkey_conflict",
                    "检测到重复的快捷键触发组合，请修改其中一条规则",
                ));
            }
            if rule.action.action_type != "remap" && rule.action.action_type != "launch" {
                return Err(AppError::invalid(
                    "hotkey_action_unsupported",
                    "暂时只支持按键映射和启动程序两种动作",
                ));
            }
            if rule.action.target.trim().is_empty() {
                return Err(AppError::invalid(
                    "hotkey_action_missing_target",
                    "快捷键动作还没有填写目标",
                ));
            }
        }

        let mut text_signatures = HashSet::new();
        for rule in &self.text_expansions {
            let length = rule.abbreviation.chars().count();
            if rule.id.trim().is_empty() || rule.name.trim().is_empty() {
                return Err(AppError::invalid(
                    "text_expansion_missing_name",
                    "文本扩展需要填写名称",
                ));
            }
            if !(1..=32).contains(&length) {
                return Err(AppError::invalid(
                    "text_expansion_abbreviation_length",
                    "缩写长度需要在 1 到 32 个字符之间",
                ));
            }
            if rule.enabled && rule.replacement.is_empty() {
                return Err(AppError::invalid(
                    "text_expansion_missing_replacement",
                    "启用文本扩展前请填写替换内容",
                ));
            }
            let signature = rule.abbreviation.trim().to_lowercase();
            if !text_signatures.insert(signature) {
                return Err(AppError::invalid(
                    "text_expansion_conflict",
                    "检测到重复的文本扩展缩写，请修改其中一条规则",
                ));
            }
        }

        let mut macro_signatures = HashSet::new();
        for rule in &self.macros {
            if rule.id.trim().is_empty() || rule.name.trim().is_empty() {
                return Err(AppError::invalid("macro_missing_name", "宏需要填写名称"));
            }
            if rule.speed <= 0.0 || !rule.speed.is_finite() {
                return Err(AppError::invalid(
                    "macro_invalid_speed",
                    "宏速度必须是大于 0 的数字",
                ));
            }
            if rule.enabled && rule.trigger_keys.is_empty() {
                return Err(AppError::invalid(
                    "macro_missing_trigger",
                    "启用宏前至少需要设置一个触发键",
                ));
            }
            let trigger_keys = rule
                .trigger_keys
                .iter()
                .map(|key| key.trim().to_ascii_uppercase())
                .collect::<HashSet<_>>();
            if trigger_keys.len() == 3
                && trigger_keys
                    == HashSet::from(["CTRL".to_string(), "SHIFT".to_string(), "F9".to_string()])
            {
                return Err(AppError::invalid(
                    "macro_recording_shortcut_reserved",
                    "Ctrl + Shift + F9 保留为录制快捷键，请换一个宏触发组合",
                ));
            }
            if rule.enabled {
                let signature = rule
                    .trigger_keys
                    .iter()
                    .map(|key| key.trim().to_ascii_uppercase())
                    .collect::<Vec<_>>()
                    .join("+");
                if !macro_signatures.insert(signature) {
                    return Err(AppError::invalid(
                        "macro_conflict",
                        "检测到重复的宏触发组合，请修改其中一个宏",
                    ));
                }
            }
            if matches!(rule.mode, MacroMode::Repeat) && rule.repeat_count == 0 {
                return Err(AppError::invalid(
                    "macro_invalid_repeat_count",
                    "固定次数循环至少需要执行 1 次",
                ));
            }
            if let AutomationProgram::Rhai {
                source,
                api_version,
            } = &rule.program
            {
                if *api_version != 1 {
                    return Err(AppError::invalid(
                        "rhai_api_version",
                        "Rhai API 版本不受支持，请使用 apiVersion: 1",
                    ));
                }
                if source.trim().is_empty() {
                    return Err(AppError::invalid(
                        "rhai_source_empty",
                        "高级 Rhai 脚本不能为空",
                    ));
                }
            }
            for step in rule.macro_steps().unwrap_or(&[]) {
                if let MacroStep::Text { text } = step {
                    if text.is_empty() {
                        return Err(AppError::invalid(
                            "macro_empty_text",
                            "宏的输入文本不能为空",
                        ));
                    }
                }
            }
        }

        let mut asset_ids = HashSet::new();
        let mut asset_file_names = HashSet::new();
        for asset in &self.assets {
            if asset.id.trim().is_empty()
                || asset.name.trim().is_empty()
                || asset.name.chars().count() > MAX_ASSET_NAME_LENGTH
            {
                return Err(AppError::invalid(
                    "asset_missing_name",
                    "图像资源需要填写名称和 ID，名称不能超过 128 个字符",
                ));
            }
            if !asset_ids.insert(asset.id.clone()) {
                return Err(AppError::invalid(
                    "asset_duplicate_id",
                    "图像资源 ID 不能重复",
                ));
            }
            if !asset_file_names.insert(asset.file_name.clone()) {
                return Err(AppError::invalid(
                    "asset_duplicate_file",
                    "图像资源文件名不能重复",
                ));
            }
            if asset.file_name.is_empty()
                || asset.file_name.contains('/')
                || asset.file_name.contains('\\')
                || asset.file_name.split(['/', '\\']).any(|part| part == "..")
                || asset.id.contains('/')
                || asset.id.contains('\\')
                || asset.id.split(['/', '\\']).any(|part| part == "..")
            {
                return Err(AppError::invalid(
                    "asset_path_unsafe",
                    "图像资源文件名必须是托管目录中的安全单级文件名",
                ));
            }
            if asset.width == 0
                || asset.height == 0
                || asset.width > MAX_TEMPLATE_WIDTH
                || asset.height > MAX_TEMPLATE_HEIGHT
                || u64::from(asset.width) * u64::from(asset.height) > MAX_TEMPLATE_PIXELS
            {
                return Err(AppError::invalid(
                    "asset_dimensions_invalid",
                    "图像资源尺寸超过允许范围",
                ));
            }
        }

        Ok(())
    }
}

fn default_schema_version() -> u32 {
    // A missing version belongs to the pre-program format and must pass
    // through the migration path instead of being mistaken for v2.
    1
}

fn default_true() -> bool {
    true
}

fn default_emergency_stop() -> String {
    "F12".to_string()
}

fn default_repeat_count() -> u32 {
    1
}

fn default_macro_speed() -> f32 {
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        assert!(AppConfig::default().validate().is_ok());
    }

    #[test]
    fn duplicate_hotkey_triggers_are_rejected() {
        let mut config = AppConfig::default();
        config.hotkeys[1].trigger_keys = config.hotkeys[0].trigger_keys.clone();

        let error = config
            .validate()
            .expect_err("duplicate trigger should fail");
        assert_eq!(error.code, "hotkey_conflict");
    }

    #[test]
    fn empty_text_expansion_replacement_is_rejected() {
        let mut config = AppConfig::default();
        config.text_expansions[0].replacement.clear();
        config.text_expansions[0].enabled = true;

        let error = config
            .validate()
            .expect_err("empty replacement should fail");
        assert_eq!(error.code, "text_expansion_missing_replacement");
    }

    #[test]
    fn duplicate_text_expansions_are_rejected() {
        let mut config = AppConfig::default();
        config.text_expansions.push(TextExpansionRule {
            id: "common-email-copy".to_string(),
            name: "邮箱副本".to_string(),
            abbreviation: "@@".to_string(),
            replacement: "copy@example.com".to_string(),
            enabled: false,
            case_sensitive: false,
            sensitive: false,
        });

        let error = config
            .validate()
            .expect_err("duplicate text expansion should fail");
        assert_eq!(error.code, "text_expansion_conflict");
    }

    #[test]
    fn enabled_macros_cannot_share_a_trigger() {
        let mut config = AppConfig::default();
        let macro_rule = MacroRule {
            id: "macro-a".to_string(),
            name: "宏 A".to_string(),
            enabled: true,
            trigger_keys: vec!["Ctrl".to_string(), "F8".to_string()],
            mode: MacroMode::Once,
            repeat_count: 1,
            speed: 1.0,
            record_mouse_move: true,
            record_mouse_clicks: true,
            target: None,
            program: AutomationProgram::Macro {
                steps: vec![MacroStep::Delay {
                    duration_ms: 10,
                    duration_max_ms: None,
                }],
            },
        };
        config.macros.push(macro_rule.clone());
        config.macros.push(MacroRule {
            id: "macro-b".to_string(),
            name: "宏 B".to_string(),
            ..macro_rule
        });

        let error = config
            .validate()
            .expect_err("duplicate macro trigger should fail");
        assert_eq!(error.code, "macro_conflict");
    }

    #[test]
    fn macro_steps_accept_frontend_camel_case_fields() {
        let delay: MacroStep = serde_json::from_value(serde_json::json!({
            "type": "delay",
            "durationMs": 300
        }))
        .expect("frontend delay step should deserialize");
        assert!(matches!(
            delay,
            MacroStep::Delay {
                duration_ms: 300,
                duration_max_ms: None
            }
        ));

        let wheel: MacroStep = serde_json::from_value(serde_json::json!({
            "type": "wheel",
            "deltaX": 12,
            "deltaY": -120
        }))
        .expect("frontend wheel step should deserialize");
        assert!(matches!(
            wheel,
            MacroStep::Wheel {
                delta_x: 12,
                delta_y: -120
            }
        ));
    }

    #[test]
    fn old_macro_config_defaults_mouse_recording_options_to_enabled() {
        let macro_rule: MacroRule = serde_json::from_value(serde_json::json!({
            "id": "legacy-macro",
            "name": "Legacy",
            "steps": []
        }))
        .expect("legacy macro should remain readable");
        assert!(macro_rule.record_mouse_move);
        assert!(macro_rule.record_mouse_clicks);
    }

    #[test]
    fn legacy_macro_steps_are_migrated_into_the_program_field() {
        let config: AppConfig = serde_json::from_value(serde_json::json!({
            "schemaVersion": 1,
            "macros": [{
                "id": "legacy-macro",
                "name": "Legacy",
                "steps": [{ "type": "text", "text": "中文 \\\"quoted\\\"" }]
            }]
        }))
        .expect("legacy config should deserialize");
        let (config, migrated) = config.migrate().expect("legacy config should migrate");
        assert!(migrated);
        assert_eq!(config.schema_version, SCHEMA_VERSION);
        assert!(matches!(
            &config.macros[0].program,
            AutomationProgram::Macro { steps } if matches!(steps.first(), Some(MacroStep::Text { text }) if text.contains("中文"))
        ));
        let saved = serde_json::to_value(config).expect("migrated config should serialize");
        assert!(saved["macros"][0].get("program").is_some());
        assert!(saved["macros"][0].get("steps").is_none());
    }

    #[test]
    fn rhai_program_requires_api_version_one_and_non_empty_source() {
        let mut config = AppConfig::default();
        config.macros.push(MacroRule {
            id: "rhai-macro".to_string(),
            name: "Rhai".to_string(),
            enabled: false,
            trigger_keys: Vec::new(),
            mode: MacroMode::Once,
            repeat_count: 1,
            speed: 1.0,
            record_mouse_move: true,
            record_mouse_clicks: true,
            target: None,
            program: AutomationProgram::Rhai {
                source: "press(\"A\");".to_string(),
                api_version: 1,
            },
        });
        assert!(config.validate().is_ok());
        if let AutomationProgram::Rhai { api_version, .. } = &mut config.macros[0].program {
            *api_version = 2;
        }
        assert_eq!(
            config.validate().expect_err("wrong API version").code,
            "rhai_api_version"
        );
    }
}
