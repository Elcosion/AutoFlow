use crate::automation::{
    AutomationAsset, MAX_ASSET_NAME_LENGTH, MAX_TEMPLATE_HEIGHT, MAX_TEMPLATE_PIXELS,
    MAX_TEMPLATE_WIDTH,
};
use crate::behavior::v2::{
    BehaviorPolicy, BehaviorProfileV2, BehaviorProfileV2File, BehaviorSessionFile,
    BehaviorSessionV2,
};
use crate::behavior::{
    BehaviorProfile, BehaviorProfileFile, BiomimeticInput, BiomimeticInputFile,
    MAX_BEHAVIOR_PROFILE_NAME,
};
use crate::AppError;
use serde::{de::Deserializer, Deserialize, Serialize};
use std::collections::HashSet;

pub const SCHEMA_VERSION: u32 = 6;

pub(crate) fn normalized_virtual_key(key: &str) -> Option<u32> {
    let normalized = key.trim().to_ascii_uppercase();
    let named = [
        ("CTRL", 0x11),
        ("CONTROL", 0x11),
        ("ALT", 0x12),
        ("SHIFT", 0x10),
        ("WIN", 0x5B),
        ("ESC", 0x1B),
        ("ENTER", 0x0D),
        ("SPACE", 0x20),
        ("TAB", 0x09),
        ("BACKSPACE", 0x08),
        ("CAPSLOCK", 0x14),
        ("LEFT", 0x25),
        ("RIGHT", 0x27),
        ("UP", 0x26),
        ("DOWN", 0x28),
    ];
    if let Some((_, value)) = named.iter().find(|(name, _)| *name == normalized) {
        return Some(*value);
    }
    if normalized.len() == 1 {
        let byte = normalized.as_bytes()[0];
        if byte.is_ascii_uppercase() || byte.is_ascii_digit() {
            return Some(u32::from(byte));
        }
    }
    normalized
        .strip_prefix('F')
        .and_then(|number| number.parse::<u32>().ok())
        .filter(|number| (1..=24).contains(number))
        .map(|number| 0x70 + number - 1)
}

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
    /// Whether the playback progress overlay is shown while a macro runs.
    /// This is intentionally global; individual macros do not override it.
    #[serde(default)]
    pub show_playback_overlay: bool,
    #[serde(default)]
    pub hotkeys: Vec<HotkeyRule>,
    #[serde(default)]
    pub text_expansions: Vec<TextExpansionRule>,
    #[serde(default)]
    pub macros: Vec<MacroRule>,
    #[serde(default)]
    pub macro_files: Vec<MacroRuleFile>,
    #[serde(default)]
    pub assets: Vec<AutomationAsset>,
    #[serde(default)]
    pub behavior_profiles: Vec<BehaviorProfile>,
    #[serde(default)]
    pub behavior_profile_files: Vec<BehaviorProfileFile>,
    #[serde(default)]
    pub biomimetic_inputs: Vec<BiomimeticInput>,
    #[serde(default)]
    pub biomimetic_input_files: Vec<BiomimeticInputFile>,
    #[serde(default)]
    pub active_behavior_profile_id: Option<String>,
    #[serde(default)]
    pub selected_behavior_profile_ids: Vec<String>,
    #[serde(default)]
    pub selected_biomimetic_input_ids: Vec<String>,
    #[serde(default)]
    pub biomimetic_enabled: bool,
    #[serde(default = "default_biomimetic_intensity")]
    pub biomimetic_intensity: f32,
    #[serde(default = "default_true")]
    pub retain_behavior_records: bool,
    #[serde(default)]
    pub behavior_sessions_v2: Vec<BehaviorSessionV2>,
    #[serde(default)]
    pub behavior_session_files_v2: Vec<BehaviorSessionFile>,
    #[serde(default)]
    pub behavior_profiles_v2: Vec<BehaviorProfileV2>,
    #[serde(default)]
    pub behavior_profile_v2_files: Vec<BehaviorProfileV2File>,
    #[serde(default)]
    pub active_behavior_profile_v2_id: Option<String>,
    #[serde(default)]
    pub behavior_policy: BehaviorPolicy,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_error: Option<String>,
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
    #[serde(default)]
    pub behavior_policy: Option<BehaviorPolicy>,
    pub program: AutomationProgram,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MacroRuleFile {
    pub id: String,
    pub name: String,
    pub file_name: String,
}

impl MacroRuleFile {
    pub fn from_macro(rule: &MacroRule, file_name: String) -> Self {
        Self {
            id: rule.id.clone(),
            name: rule.name.clone(),
            file_name,
        }
    }
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
    import_error: Option<String>,
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
    behavior_policy: Option<BehaviorPolicy>,
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
            import_error: wire.import_error,
            enabled: wire.enabled,
            trigger_keys: wire.trigger_keys,
            mode: wire.mode,
            repeat_count: wire.repeat_count,
            speed: wire.speed,
            record_mouse_move: wire.record_mouse_move,
            record_mouse_clicks: wire.record_mouse_clicks,
            target: wire.target,
            behavior_policy: wire.behavior_policy,
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
            show_playback_overlay: false,
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
            macro_files: Vec::new(),
            assets: Vec::new(),
            behavior_profiles: Vec::new(),
            behavior_profile_files: Vec::new(),
            biomimetic_inputs: Vec::new(),
            biomimetic_input_files: Vec::new(),
            active_behavior_profile_id: None,
            selected_behavior_profile_ids: Vec::new(),
            selected_biomimetic_input_ids: Vec::new(),
            biomimetic_enabled: false,
            biomimetic_intensity: default_biomimetic_intensity(),
            retain_behavior_records: true,
            behavior_sessions_v2: Vec::new(),
            behavior_session_files_v2: Vec::new(),
            behavior_profiles_v2: Vec::new(),
            behavior_profile_v2_files: Vec::new(),
            active_behavior_profile_v2_id: None,
            behavior_policy: BehaviorPolicy::default(),
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
        if self.selected_behavior_profile_ids.is_empty() {
            if let Some(active_id) = &self.active_behavior_profile_id {
                self.selected_behavior_profile_ids.push(active_id.clone());
            }
        }
        if !self.behavior_policy.enabled && self.biomimetic_enabled {
            self.behavior_policy = BehaviorPolicy::from_legacy(
                true,
                self.biomimetic_intensity,
                self.active_behavior_profile_v2_id.clone(),
            );
        }
        if self.active_behavior_profile_v2_id.is_none() {
            self.active_behavior_profile_v2_id = self
                .behavior_profiles_v2
                .first()
                .map(|profile| profile.id.clone());
        }
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
        let emergency_vk = normalized_virtual_key(&self.emergency_stop).ok_or_else(|| {
            AppError::invalid(
                "emergency_stop_unsupported",
                "紧急停止键无法识别；请使用 F12 或其他受支持的单键",
            )
        })?;

        if !self.biomimetic_intensity.is_finite()
            || !(0.0..=1.0).contains(&self.biomimetic_intensity)
        {
            return Err(AppError::invalid(
                "behavior_intensity_invalid",
                "仿生操作强度必须在 0 到 1 之间",
            ));
        }
        self.behavior_policy.validate()?;

        let mut v2_session_ids = HashSet::new();
        for session in &self.behavior_sessions_v2 {
            session.validate()?;
            if !v2_session_ids.insert(session.id.clone()) {
                return Err(AppError::invalid(
                    "behavior_v2_session_duplicate_id",
                    "V2 训练会话 ID 不能重复",
                ));
            }
        }
        let mut v2_profile_ids = HashSet::new();
        for profile in &self.behavior_profiles_v2 {
            profile.validate()?;
            if !v2_profile_ids.insert(profile.id.clone()) {
                return Err(AppError::invalid(
                    "behavior_v2_profile_duplicate_id",
                    "V2 行为档案 ID 不能重复",
                ));
            }
        }
        if let Some(profile_id) = &self.active_behavior_profile_v2_id {
            if !v2_profile_ids.contains(profile_id) {
                return Err(AppError::invalid(
                    "behavior_v2_profile_not_found",
                    "当前 V2 行为档案不存在",
                ));
            }
        }
        if let Some(profile_id) = &self.behavior_policy.profile_id {
            if !v2_profile_ids.contains(profile_id) {
                return Err(AppError::invalid(
                    "behavior_v2_profile_not_found",
                    "行为策略绑定的 V2 行为档案不存在",
                ));
            }
        }

        let mut behavior_ids = HashSet::new();
        for profile in &self.behavior_profiles {
            profile.validate()?;
            if profile.name.chars().count() > MAX_BEHAVIOR_PROFILE_NAME {
                return Err(AppError::invalid(
                    "behavior_profile_name_too_long",
                    "行为档案名称不能超过 64 个字符",
                ));
            }
            if !behavior_ids.insert(profile.id.clone()) {
                return Err(AppError::invalid(
                    "behavior_profile_duplicate_id",
                    "行为档案 ID 不能重复",
                ));
            }
        }
        if let Some(active_id) = &self.active_behavior_profile_id {
            if !behavior_ids.contains(active_id) {
                return Err(AppError::invalid(
                    "behavior_profile_not_found",
                    "当前选中的行为档案不存在",
                ));
            }
        }
        for profile_id in &self.selected_behavior_profile_ids {
            if !behavior_ids.contains(profile_id) {
                return Err(AppError::invalid(
                    "behavior_profile_not_found",
                    "选中的行为档案不存在",
                ));
            }
        }

        for macro_rule in &self.macros {
            if macro_rule.import_error.is_some() {
                continue;
            }
            if let Some(policy) = &macro_rule.behavior_policy {
                policy.validate()?;
                if let Some(profile_id) = &policy.profile_id {
                    if !v2_profile_ids.contains(profile_id) {
                        return Err(AppError::invalid(
                            "behavior_v2_profile_not_found",
                            "宏绑定的 V2 行为档案不存在",
                        ));
                    }
                }
            }
        }

        let mut input_ids = HashSet::new();
        for input in &self.biomimetic_inputs {
            input.validate()?;
            if !input_ids.insert(input.id.clone()) {
                return Err(AppError::invalid(
                    "biomimetic_input_duplicate_id",
                    "仿生输入文件 ID 不能重复",
                ));
            }
        }
        for input_id in &self.selected_biomimetic_input_ids {
            if !input_ids.contains(input_id) {
                return Err(AppError::invalid(
                    "biomimetic_input_not_found",
                    "选中的仿生输入文件不存在",
                ));
            }
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
            if rule.import_error.is_some() {
                if rule.enabled {
                    return Err(AppError::invalid(
                        "macro_import_invalid_enabled",
                        "导入错误的宏必须保持停用，修复脚本后才能启用",
                    ));
                }
                continue;
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
            if rule
                .trigger_keys
                .iter()
                .filter_map(|key| normalized_virtual_key(key))
                .any(|vk| vk == emergency_vk)
            {
                return Err(AppError::invalid(
                    "macro_emergency_key_conflict",
                    "宏快捷键不能包含紧急停止键；请更换快捷键组合",
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
            if !asset_file_names.insert(asset.file_name.to_lowercase()) {
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
    // through the migration path instead of being mistaken for v4.
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

fn default_biomimetic_intensity() -> f32 {
    0.65
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::v2::{
        train_behavior_profile_with_retention, BehaviorCaptureMetadata, BehaviorSessionV2,
        SourceRetention,
    };

    fn macro_fixture(id: &str, trigger_keys: Vec<&str>, mode: MacroMode) -> MacroRule {
        MacroRule {
            id: id.to_string(),
            name: id.to_string(),
            import_error: None,
            enabled: true,
            trigger_keys: trigger_keys.into_iter().map(str::to_string).collect(),
            mode,
            repeat_count: 3,
            speed: 1.0,
            record_mouse_move: true,
            record_mouse_clicks: true,
            target: None,
            behavior_policy: None,
            program: AutomationProgram::Macro {
                steps: vec![MacroStep::Delay {
                    duration_ms: 10,
                    duration_max_ms: None,
                }],
            },
        }
    }

    #[test]
    fn default_config_is_valid() {
        assert!(AppConfig::default().validate().is_ok());
    }

    #[test]
    fn playback_overlay_defaults_to_disabled_and_legacy_configs_stay_disabled() {
        let default_config = AppConfig::default();
        assert!(!default_config.show_playback_overlay);

        let mut saved = serde_json::to_value(default_config).expect("config should serialize");
        saved
            .as_object_mut()
            .expect("config should be an object")
            .remove("showPlaybackOverlay");
        let restored: AppConfig = serde_json::from_value(saved)
            .expect("legacy config without overlay setting should deserialize");
        assert!(!restored.show_playback_overlay);
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
            import_error: None,
            enabled: true,
            trigger_keys: vec!["Ctrl".to_string(), "F8".to_string()],
            mode: MacroMode::Once,
            repeat_count: 1,
            speed: 1.0,
            record_mouse_move: true,
            record_mouse_clicks: true,
            target: None,
            behavior_policy: None,
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
    fn global_hold_and_ordinary_shortcuts_validate() {
        let mut config = AppConfig::default();
        config.macros.push(macro_fixture(
            "ordinary-shortcut",
            vec!["Ctrl", "F9"],
            MacroMode::Once,
        ));
        config
            .validate()
            .expect("ordinary shortcut should validate");

        config.macros[0].mode = MacroMode::Hold;
        config
            .validate()
            .expect("enabled Hold with a global shortcut should validate");
    }

    #[test]
    fn macro_shortcuts_cannot_contain_default_or_custom_emergency_key() {
        for (keys, mode) in [
            (vec!["F12"], MacroMode::Hold),
            (vec!["Ctrl", "F12"], MacroMode::Once),
        ] {
            let mut config = AppConfig::default();
            config
                .macros
                .push(macro_fixture("emergency-conflict", keys, mode));
            assert_eq!(
                config.validate().expect_err("F12 conflict").code,
                "macro_emergency_key_conflict"
            );
        }

        let mut config = AppConfig {
            emergency_stop: "F11".to_string(),
            ..AppConfig::default()
        };
        config.macros.push(macro_fixture(
            "custom-emergency-conflict",
            vec!["Shift", "F11"],
            MacroMode::Hold,
        ));
        assert_eq!(
            config.validate().expect_err("custom conflict").code,
            "macro_emergency_key_conflict"
        );
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
    fn schema_five_configs_gain_an_empty_macro_file_index() {
        let config: AppConfig = serde_json::from_value(serde_json::json!({
            "schemaVersion": 5,
            "macros": [{
                "id": "inline-macro",
                "name": "Inline macro",
                "steps": []
            }]
        }))
        .expect("schema five config should deserialize");
        assert!(config.macro_files.is_empty());
        assert_eq!(config.macros.len(), 1);

        let (config, migrated) = config.migrate().expect("schema five should migrate");
        assert!(migrated);
        assert_eq!(config.schema_version, SCHEMA_VERSION);
        let saved = serde_json::to_value(config).expect("migrated config should serialize");
        assert_eq!(saved["macroFiles"], serde_json::json!([]));
    }

    #[test]
    fn rhai_program_requires_api_version_one_and_non_empty_source() {
        let mut config = AppConfig::default();
        config.macros.push(MacroRule {
            id: "rhai-macro".to_string(),
            name: "Rhai".to_string(),
            import_error: None,
            enabled: false,
            trigger_keys: Vec::new(),
            mode: MacroMode::Once,
            repeat_count: 1,
            speed: 1.0,
            record_mouse_move: true,
            record_mouse_clicks: true,
            target: None,
            behavior_policy: None,
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

    #[test]
    fn invalid_imported_macro_must_stay_disabled_until_repaired() {
        let mut config = AppConfig::default();
        config.macros.push(MacroRule {
            id: "invalid-import".to_string(),
            name: "待修复".to_string(),
            import_error: Some("JSON 格式不合法".to_string()),
            enabled: false,
            trigger_keys: Vec::new(),
            mode: MacroMode::Once,
            repeat_count: 1,
            speed: 1.0,
            record_mouse_move: true,
            record_mouse_clicks: true,
            target: None,
            behavior_policy: None,
            program: AutomationProgram::Rhai {
                source: "broken".to_string(),
                api_version: 1,
            },
        });
        assert!(config.validate().is_ok());
        config.macros[0].enabled = true;
        assert_eq!(
            config.validate().unwrap_err().code,
            "macro_import_invalid_enabled"
        );
    }

    #[test]
    fn legacy_configs_get_safe_biomimetic_defaults() {
        let config: AppConfig = serde_json::from_value(serde_json::json!({
            "schemaVersion": 2,
            "macros": []
        }))
        .expect("v2 config should deserialize");
        assert!(config.behavior_profiles.is_empty());
        assert!(config.active_behavior_profile_id.is_none());
        assert!(config.selected_behavior_profile_ids.is_empty());
        assert!(config.selected_biomimetic_input_ids.is_empty());
        assert!(!config.biomimetic_enabled);
        assert_eq!(config.biomimetic_intensity, 0.65);
        assert!(config.retain_behavior_records);
        let (config, migrated) = config.migrate().expect("v2 config should migrate");
        assert!(migrated);
        assert_eq!(config.schema_version, SCHEMA_VERSION);
        config.validate().expect("migrated config should validate");
    }

    #[test]
    fn biomimetic_intensity_is_bounded() {
        let config = AppConfig {
            biomimetic_intensity: 1.1,
            ..AppConfig::default()
        };
        let error = config
            .validate()
            .expect_err("out of range intensity should fail");
        assert_eq!(error.code, "behavior_intensity_invalid");
    }

    #[test]
    fn v2_policy_has_safe_defaults_when_missing_from_old_config() {
        let config: AppConfig = serde_json::from_value(serde_json::json!({
            "schemaVersion": 4,
            "macros": []
        }))
        .expect("old config should deserialize with V2 defaults");
        assert!(!config.behavior_policy.enabled);
        assert_eq!(config.behavior_policy.timing_strength, 0.0);
        assert_eq!(config.behavior_policy.correction_strength, 0.15);
        let (config, migrated) = config.migrate().expect("old config should migrate");
        assert!(migrated);
        assert_eq!(config.schema_version, SCHEMA_VERSION);
        config
            .validate()
            .expect("migrated V2 defaults should validate");
    }

    #[test]
    fn v2_policy_strengths_are_bounded() {
        let mut config = AppConfig::default();
        config.behavior_policy.pointer_path_strength = 1.1;
        let error = config
            .validate()
            .expect_err("out of range V2 strength should fail");
        assert_eq!(error.code, "behavior_policy_strength_invalid");
    }

    #[test]
    fn v2_active_profile_and_macro_policy_round_trip() {
        let session = BehaviorSessionV2 {
            id: "config-session".to_string(),
            name: "Config session".to_string(),
            api_version: crate::BEHAVIOR_V2_API_VERSION,
            created_at_ms: 1,
            duration_ms: 0,
            task_tag: None,
            capture_metadata: BehaviorCaptureMetadata::default(),
            raw_events: Vec::new(),
        };
        let profile = train_behavior_profile_with_retention(&session, SourceRetention::Persisted)
            .expect("empty session still produces an explicit insufficient profile");
        let profile_id = profile.id.clone();
        let macro_rule = MacroRule {
            id: "policy-macro".to_string(),
            name: "Policy macro".to_string(),
            import_error: None,
            enabled: false,
            trigger_keys: vec!["F8".to_string()],
            mode: MacroMode::Once,
            repeat_count: 1,
            speed: 1.0,
            record_mouse_move: true,
            record_mouse_clicks: true,
            target: None,
            behavior_policy: Some(BehaviorPolicy {
                enabled: true,
                profile_id: Some(profile_id.clone()),
                timing_strength: 0.5,
                pointer_path_strength: 0.4,
                pause_strength: 0.2,
                correction_strength: 0.3,
                speed_scale: 1.1,
                seed: Some(42),
            }),
            program: AutomationProgram::Macro { steps: Vec::new() },
        };
        let mut config = AppConfig::default();
        config.behavior_sessions_v2.push(session);
        config.behavior_profiles_v2.push(profile);
        config.active_behavior_profile_v2_id = Some(profile_id.clone());
        config.behavior_policy.profile_id = Some(profile_id);
        config.macros.push(macro_rule);

        let serialized = serde_json::to_string(&config).expect("config should serialize");
        let round_trip: AppConfig =
            serde_json::from_str(&serialized).expect("config should deserialize");
        round_trip
            .validate()
            .expect("round-tripped config should validate");
        assert_eq!(
            round_trip.macros[0]
                .behavior_policy
                .as_ref()
                .and_then(|policy| policy.seed),
            Some(42)
        );
        assert_eq!(
            round_trip.active_behavior_profile_v2_id,
            round_trip.behavior_policy.profile_id
        );
    }
}
