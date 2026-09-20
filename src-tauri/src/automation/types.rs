use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const MAX_VISION_OPERATIONS: usize = 100_000;
pub const MIN_POLL_MS: u64 = 50;
pub const MAX_POLL_MS: u64 = 5_000;
pub const MAX_WAIT_MS: u64 = 120_000;
pub const MAX_CAPTURE_WIDTH: u32 = 7_680;
pub const MAX_CAPTURE_HEIGHT: u32 = 4_320;
pub const MAX_CAPTURE_PIXELS: u64 = 8_388_608;
pub const MAX_TEMPLATE_WIDTH: u32 = 2_048;
pub const MAX_TEMPLATE_HEIGHT: u32 = 2_048;
pub const MAX_TEMPLATE_PIXELS: u64 = 4_194_304;
pub const MAX_CACHED_TEMPLATES: usize = 64;
pub const MAX_CACHED_TEMPLATE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_ASSET_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_ASSET_NAME_LENGTH: usize = 128;
pub const MIN_MATCH_SCALE: f32 = 0.5;
pub const MAX_MATCH_SCALE: f32 = 2.0;
pub const MAX_SCALE_CANDIDATES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(pub isize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl ScreenRect {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Result<Self, VisionError> {
        let rect = Self {
            x,
            y,
            width,
            height,
        };
        rect.validate()?;
        Ok(rect)
    }

    pub const fn from_parts(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn validate(&self) -> Result<(), VisionError> {
        if self.width == 0 || self.height == 0 {
            return Err(VisionError::new(
                "capture_region_invalid",
                "捕获区域的宽度和高度必须大于 0",
            ));
        }
        if self.width > MAX_CAPTURE_WIDTH
            || self.height > MAX_CAPTURE_HEIGHT
            || u64::from(self.width) * u64::from(self.height) > MAX_CAPTURE_PIXELS
        {
            return Err(VisionError::new(
                "capture_region_invalid",
                "捕获区域过大，请缩小搜索范围",
            ));
        }
        self.right_i64()?;
        self.bottom_i64()?;
        Ok(())
    }

    pub fn right_i64(&self) -> Result<i64, VisionError> {
        i64::from(self.x)
            .checked_add(i64::from(self.width))
            .ok_or_else(|| VisionError::new("capture_region_invalid", "捕获区域坐标溢出"))
    }

    pub fn bottom_i64(&self) -> Result<i64, VisionError> {
        i64::from(self.y)
            .checked_add(i64::from(self.height))
            .ok_or_else(|| VisionError::new("capture_region_invalid", "捕获区域坐标溢出"))
    }

    pub fn contains(&self, other: &Self) -> Result<bool, VisionError> {
        Ok(i64::from(other.x) >= i64::from(self.x)
            && i64::from(other.y) >= i64::from(self.y)
            && other.right_i64()? <= self.right_i64()?
            && other.bottom_i64()? <= self.bottom_i64()?)
    }

    pub fn intersection(&self, other: &Self) -> Option<Self> {
        let left = i64::from(self.x).max(i64::from(other.x));
        let top = i64::from(self.y).max(i64::from(other.y));
        let right = self.right_i64().ok()?.min(other.right_i64().ok()?);
        let bottom = self.bottom_i64().ok()?.min(other.bottom_i64().ok()?);
        if right <= left || bottom <= top {
            return None;
        }
        Some(Self {
            x: i32::try_from(left).ok()?,
            y: i32::try_from(top).ok()?,
            width: u32::try_from(right - left).ok()?,
            height: u32::try_from(bottom - top).ok()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgbColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageMatch {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub center_x: i32,
    pub center_y: i32,
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowRectValue {
    pub found: bool,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl WindowRectValue {
    pub const fn not_found() -> Self {
        Self {
            found: false,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        }
    }

    pub const fn found(rect: ScreenRect) -> Self {
        Self {
            found: true,
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationAsset {
    pub id: String,
    pub name: String,
    pub file_name: String,
    pub width: u32,
    pub height: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisionError {
    pub code: String,
    pub message: String,
}

impl VisionError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn cancelled() -> Self {
        Self::new("vision_cancelled", "视觉操作已被 F12 或 Toggle 停止")
    }
}

impl Display for VisionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}：{}", self.code, self.message)
    }
}

impl std::error::Error for VisionError {}

#[derive(Debug)]
pub struct CaptureFrame {
    pub origin: Point,
    pub width: u32,
    pub height: u32,
    pixels_bgra: Arc<[u8]>,
    valid_regions: Arc<[ScreenRect]>,
}

impl Clone for CaptureFrame {
    fn clone(&self) -> Self {
        Self {
            origin: self.origin,
            width: self.width,
            height: self.height,
            pixels_bgra: Arc::clone(&self.pixels_bgra),
            valid_regions: Arc::clone(&self.valid_regions),
        }
    }
}

impl CaptureFrame {
    pub fn from_bgra(
        origin: Point,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    ) -> Result<Self, VisionError> {
        let region = ScreenRect::from_parts(origin.x, origin.y, width, height);
        Self::from_bgra_with_valid_regions(origin, width, height, pixels, vec![region])
    }

    pub(crate) fn from_bgra_with_valid_regions(
        origin: Point,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
        valid_regions: Vec<ScreenRect>,
    ) -> Result<Self, VisionError> {
        let expected = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| VisionError::new("capture_region_invalid", "帧尺寸溢出"))?;
        if width == 0 || height == 0 || pixels.len() != expected {
            return Err(VisionError::new(
                "capture_region_invalid",
                "捕获帧尺寸或像素数据无效",
            ));
        }
        let frame_region = ScreenRect::from_parts(origin.x, origin.y, width, height);
        let valid_regions = valid_regions
            .into_iter()
            .filter_map(|region| region.intersection(&frame_region))
            .collect::<Vec<_>>();
        Ok(Self {
            origin,
            width,
            height,
            pixels_bgra: pixels.into(),
            valid_regions: valid_regions.into(),
        })
    }

    pub fn pixel(&self, point: Point) -> Option<RgbColor> {
        let local_x = i64::from(point.x) - i64::from(self.origin.x);
        let local_y = i64::from(point.y) - i64::from(self.origin.y);
        if local_x < 0
            || local_y < 0
            || local_x >= i64::from(self.width)
            || local_y >= i64::from(self.height)
        {
            return None;
        }
        let index = (usize::try_from(local_y).ok()? * usize::try_from(self.width).ok()?
            + usize::try_from(local_x).ok()?)
        .checked_mul(4)?;
        Some(RgbColor {
            blue: *self.pixels_bgra.get(index)?,
            green: *self.pixels_bgra.get(index + 1)?,
            red: *self.pixels_bgra.get(index + 2)?,
        })
    }

    pub fn crop(&self, region: ScreenRect) -> Result<Self, VisionError> {
        region.validate()?;
        let source = ScreenRect::from_parts(self.origin.x, self.origin.y, self.width, self.height);
        if !source.contains(&region)? {
            return Err(VisionError::new(
                "capture_region_invalid",
                "捕获区域超出帧范围",
            ));
        }
        let offset_x = usize::try_from(i64::from(region.x) - i64::from(self.origin.x))
            .map_err(|_| VisionError::new("capture_region_invalid", "帧坐标无效"))?;
        let offset_y = usize::try_from(i64::from(region.y) - i64::from(self.origin.y))
            .map_err(|_| VisionError::new("capture_region_invalid", "帧坐标无效"))?;
        let row_bytes = usize::try_from(region.width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or_else(|| VisionError::new("capture_region_invalid", "帧宽度溢出"))?;
        let source_stride = usize::try_from(self.width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or_else(|| VisionError::new("capture_region_invalid", "帧宽度溢出"))?;
        let region_height = usize::try_from(region.height)
            .map_err(|_| VisionError::new("capture_region_invalid", "帧高度溢出"))?;
        let total = row_bytes
            .checked_mul(region_height)
            .ok_or_else(|| VisionError::new("capture_region_invalid", "帧尺寸溢出"))?;
        let mut pixels = Vec::with_capacity(total);
        for row in 0..region.height {
            let row = usize::try_from(row)
                .map_err(|_| VisionError::new("capture_region_invalid", "帧行号溢出"))?;
            let start = (offset_y + row)
                .checked_mul(source_stride)
                .and_then(|value| value.checked_add(offset_x * 4))
                .ok_or_else(|| VisionError::new("capture_region_invalid", "帧偏移溢出"))?;
            let end = start
                .checked_add(row_bytes)
                .ok_or_else(|| VisionError::new("capture_region_invalid", "帧偏移溢出"))?;
            let slice = self.pixels_bgra.get(start..end).ok_or_else(|| {
                VisionError::new("capture_region_invalid", "捕获帧像素数据不完整")
            })?;
            pixels.extend_from_slice(slice);
        }
        let valid_regions = self
            .valid_regions
            .iter()
            .filter_map(|valid| valid.intersection(&region))
            .collect::<Vec<_>>();
        Self::from_bgra_with_valid_regions(
            Point {
                x: region.x,
                y: region.y,
            },
            region.width,
            region.height,
            pixels,
            valid_regions,
        )
    }

    pub fn pixels_bgra(&self) -> &[u8] {
        &self.pixels_bgra
    }

    pub fn valid_regions(&self) -> &[ScreenRect] {
        &self.valid_regions
    }

    pub fn is_region_fully_valid(&self, region: ScreenRect) -> bool {
        if region.width == 0 || region.height == 0 {
            return false;
        }
        let Ok(frame_region) =
            ScreenRect::new(self.origin.x, self.origin.y, self.width, self.height)
        else {
            return false;
        };
        if !frame_region.contains(&region).unwrap_or(false) {
            return false;
        }

        let Ok(region_bottom) = region.bottom_i64() else {
            return false;
        };
        let mut y_breaks = vec![i64::from(region.y), region_bottom];
        for valid in self.valid_regions.iter() {
            let top = i64::from(valid.y).max(i64::from(region.y));
            let bottom = valid.bottom_i64().unwrap_or(i64::MIN).min(region_bottom);
            if top < bottom {
                y_breaks.push(top);
                y_breaks.push(bottom);
            }
        }
        y_breaks.sort_unstable();
        y_breaks.dedup();
        let region_left = i64::from(region.x);
        let region_right = region.right_i64().unwrap_or(i64::MIN);

        for pair in y_breaks.windows(2) {
            let top = pair[0];
            let bottom = pair[1];
            if top >= bottom {
                continue;
            }
            let mut spans = self
                .valid_regions
                .iter()
                .filter_map(|valid| {
                    let valid_bottom = valid.bottom_i64().ok()?;
                    if i64::from(valid.y) <= top && valid_bottom >= bottom {
                        Some((
                            i64::from(valid.x).max(region_left),
                            valid.right_i64().ok()?.min(region_right),
                        ))
                    } else {
                        None
                    }
                })
                .filter(|(left, right)| left < right)
                .collect::<Vec<_>>();
            spans.sort_unstable_by_key(|(left, _)| *left);
            let mut covered_until = region_left;
            for (left, right) in spans {
                if left > covered_until {
                    return false;
                }
                covered_until = covered_until.max(right);
                if covered_until >= region_right {
                    break;
                }
            }
            if covered_until < region_right {
                return false;
            }
        }
        true
    }
}

pub trait WindowProvider: Send + Sync {
    fn active_window(&self) -> Result<Option<WindowInfo>, VisionError>;
    fn find_windows(&self, title_query: &str) -> Result<Vec<WindowInfo>, VisionError>;
    fn window_rect(&self, title_query: &str) -> Result<Option<ScreenRect>, VisionError>;
}

pub trait CaptureBackend: Send {
    fn capture_region(&mut self, region: ScreenRect) -> Result<CaptureFrame, VisionError>;
    fn capture_window(&mut self, window: WindowId) -> Result<CaptureFrame, VisionError>;
    fn reset(&mut self);

    fn capture_region_with_cancel(
        &mut self,
        region: ScreenRect,
        _cancel: &AtomicBool,
    ) -> Result<CaptureFrame, VisionError> {
        self.capture_region(region)
    }

    fn capture_window_with_cancel(
        &mut self,
        window: WindowId,
        _cancel: &AtomicBool,
    ) -> Result<CaptureFrame, VisionError> {
        self.capture_window(window)
    }
}

pub trait VisionMatcher: Send + Sync {
    fn pixel_matches(
        &self,
        frame: &CaptureFrame,
        point: Point,
        expected: RgbColor,
        tolerance: u8,
    ) -> Result<bool, VisionError>;

    fn find_template(
        &self,
        frame: &CaptureFrame,
        template: &CaptureFrame,
        threshold: f32,
    ) -> Result<Option<ImageMatch>, VisionError>;

    fn find_template_with_options(
        &self,
        frame: &CaptureFrame,
        template: &CaptureFrame,
        threshold: f32,
        options: &MatcherOptions,
    ) -> Result<MatcherResult, VisionError> {
        let started = std::time::Instant::now();
        let image = self.find_template(frame, template, threshold)?;
        let mut diagnostics = VisionDiagnostics::for_mode(options.mode);
        diagnostics.total_ms = started.elapsed().as_millis() as u64;
        diagnostics.single_match_ms = diagnostics.total_ms;
        Ok(MatcherResult { image, diagnostics })
    }

    fn find_prepared_template(
        &self,
        frame: &CaptureFrame,
        template: &CaptureFrame,
        _prepared: Option<&super::vision::PreparedTemplate>,
        threshold: f32,
        options: &MatcherOptions,
    ) -> Result<MatcherResult, VisionError> {
        self.find_template_with_options(frame, template, threshold, options)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum MatcherMode {
    #[default]
    Auto,
    Exact,
    Fast,
}

impl MatcherMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Exact => "exact",
            Self::Fast => "fast",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatcherOptions {
    pub mode: MatcherMode,
    pub prefer_last: bool,
    pub max_candidates: usize,
    pub scale_min: f32,
    pub scale_max: f32,
    pub scale_step: Option<f32>,
    /// Runtime-only hint populated by the previous-hit fast path. It is not
    /// exposed through the Rhai options map and never expands the requested
    /// scale range.
    pub(crate) preferred_scale: Option<f32>,
}

impl Default for MatcherOptions {
    fn default() -> Self {
        Self {
            mode: MatcherMode::Auto,
            prefer_last: true,
            max_candidates: 8,
            scale_min: 0.67,
            scale_max: 2.0,
            scale_step: None,
            preferred_scale: None,
        }
    }
}

impl MatcherOptions {
    pub fn validate(&self) -> Result<(), VisionError> {
        if !(1..=32).contains(&self.max_candidates) {
            return Err(VisionError::new(
                "vision_match_options_invalid",
                "候选数量必须在 1 到 32 之间",
            ));
        }
        if !self.scale_min.is_finite() || !self.scale_max.is_finite() {
            return Err(VisionError::new(
                "vision_scale_invalid",
                "scale_min 和 scale_max 必须是有限数字",
            ));
        }
        if !(MIN_MATCH_SCALE..=MAX_MATCH_SCALE).contains(&self.scale_min)
            || !(MIN_MATCH_SCALE..=MAX_MATCH_SCALE).contains(&self.scale_max)
        {
            return Err(VisionError::new(
                "vision_scale_invalid",
                "scale_min 和 scale_max 必须在 0.5 到 2.0 倍之间",
            ));
        }
        if self.scale_min > self.scale_max {
            return Err(VisionError::new(
                "vision_scale_range_invalid",
                "scale_min 不能大于 scale_max",
            ));
        }
        if let Some(step) = self.scale_step {
            if !step.is_finite() || step <= 0.0 {
                return Err(VisionError::new(
                    "vision_scale_step_invalid",
                    "scale_step 必须是大于 0 的有限数字",
                ));
            }
            if step > MAX_MATCH_SCALE - MIN_MATCH_SCALE {
                return Err(VisionError::new(
                    "vision_scale_step_invalid",
                    "scale_step 不能大于 1.5",
                ));
            }
        }
        self.scale_candidates()?;
        Ok(())
    }

    pub fn scale_candidates(&self) -> Result<Vec<f32>, VisionError> {
        let mut candidates = Vec::new();
        if self.mode == MatcherMode::Exact {
            candidates.push(1.0);
            return Ok(candidates);
        }

        if let Some(step) = self.scale_step {
            let mut scale = self.scale_min;
            for _ in 0..=MAX_SCALE_CANDIDATES {
                if scale > self.scale_max + 0.0005 {
                    break;
                }
                push_scale_candidate(&mut candidates, scale.min(self.scale_max));
                if candidates.len() > MAX_SCALE_CANDIDATES {
                    return Err(VisionError::new(
                        "vision_scale_candidates_invalid",
                        "scale_step 生成的尺寸候选不能超过 16 个",
                    ));
                }
                let next = scale + step;
                if next <= scale {
                    return Err(VisionError::new(
                        "vision_scale_step_invalid",
                        "scale_step 生成的尺寸候选无效",
                    ));
                }
                scale = next;
            }
            push_scale_candidate(&mut candidates, self.scale_max);
        } else {
            for scale in [0.67, 0.80, 0.83, 1.0, 1.20, 1.25, 1.50, 1.75, 2.0] {
                if (self.scale_min..=self.scale_max).contains(&scale) {
                    push_scale_candidate(&mut candidates, scale);
                }
            }
            push_scale_candidate(&mut candidates, self.scale_min);
            push_scale_candidate(&mut candidates, self.scale_max);
        }
        candidates.sort_by(|left, right| left.total_cmp(right));
        if candidates.is_empty() || candidates.len() > MAX_SCALE_CANDIDATES {
            return Err(VisionError::new(
                "vision_scale_candidates_invalid",
                "尺寸候选数量必须在 1 到 16 个之间",
            ));
        }
        if let Some(preferred) = self.preferred_scale {
            if preferred.is_finite() {
                if let Some(index) = candidates
                    .iter()
                    .position(|scale| (*scale - preferred).abs() < 0.0005)
                {
                    let preferred = candidates.remove(index);
                    candidates.insert(0, preferred);
                }
            }
        }
        Ok(candidates)
    }
}

fn push_scale_candidate(candidates: &mut Vec<f32>, scale: f32) {
    if !candidates
        .iter()
        .any(|current| (*current - scale).abs() < 0.0005)
    {
        candidates.push(scale);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VisionDiagnostics {
    pub total_ms: u64,
    pub capture_ms: u64,
    pub prepare_ms: u64,
    pub coarse_ms: u64,
    pub refine_ms: u64,
    pub fallback_ms: u64,
    pub candidate_count: usize,
    pub coarse_score: f32,
    pub refined_score: f32,
    pub previous_hit_used: bool,
    pub fallback_used: bool,
    pub matcher_mode: String,
    pub matched_scale: Option<f32>,
    pub scale_candidates: Vec<f32>,
    pub scale_search_ms: u64,
    pub matched_width: u32,
    pub matched_height: u32,
    pub robust_verify_used: bool,
    pub robust_verify_ms: u64,
    pub robust_score: Option<f32>,
    pub valid_tile_count: usize,
    pub discarded_tile_count: usize,
    pub alpha_mask_used: bool,
    pub anchor_recovery_used: bool,
    pub anchor_candidate_count: usize,
    pub preferred_scale_hit: bool,
    pub single_match_ms: u64,
    pub wait_total_ms: u64,
}

impl Default for VisionDiagnostics {
    fn default() -> Self {
        Self::for_mode(MatcherMode::Auto)
    }
}

impl VisionDiagnostics {
    pub fn for_mode(mode: MatcherMode) -> Self {
        Self {
            total_ms: 0,
            capture_ms: 0,
            prepare_ms: 0,
            coarse_ms: 0,
            refine_ms: 0,
            fallback_ms: 0,
            candidate_count: 0,
            coarse_score: 0.0,
            refined_score: 0.0,
            previous_hit_used: false,
            fallback_used: false,
            matcher_mode: mode.as_str().to_string(),
            matched_scale: None,
            scale_candidates: Vec::new(),
            scale_search_ms: 0,
            matched_width: 0,
            matched_height: 0,
            robust_verify_used: false,
            robust_verify_ms: 0,
            robust_score: None,
            valid_tile_count: 0,
            discarded_tile_count: 0,
            alpha_mask_used: false,
            anchor_recovery_used: false,
            anchor_candidate_count: 0,
            preferred_scale_hit: false,
            single_match_ms: 0,
            wait_total_ms: 0,
        }
    }

    pub fn add_attempt(&mut self, other: &Self) {
        self.capture_ms = self.capture_ms.saturating_add(other.capture_ms);
        self.prepare_ms = self.prepare_ms.saturating_add(other.prepare_ms);
        self.coarse_ms = self.coarse_ms.saturating_add(other.coarse_ms);
        self.refine_ms = self.refine_ms.saturating_add(other.refine_ms);
        self.fallback_ms = self.fallback_ms.saturating_add(other.fallback_ms);
        self.candidate_count = self.candidate_count.max(other.candidate_count);
        self.coarse_score = self.coarse_score.max(other.coarse_score);
        self.refined_score = self.refined_score.max(other.refined_score);
        self.previous_hit_used |= other.previous_hit_used;
        self.fallback_used |= other.fallback_used;
        self.matcher_mode = other.matcher_mode.clone();
        if self.scale_candidates.is_empty() {
            self.scale_candidates = other.scale_candidates.clone();
        }
        self.scale_search_ms = self.scale_search_ms.saturating_add(other.scale_search_ms);
        self.robust_verify_used |= other.robust_verify_used;
        self.robust_verify_ms = self.robust_verify_ms.saturating_add(other.robust_verify_ms);
        self.valid_tile_count = self.valid_tile_count.max(other.valid_tile_count);
        self.discarded_tile_count = self.discarded_tile_count.max(other.discarded_tile_count);
        self.alpha_mask_used |= other.alpha_mask_used;
        self.anchor_recovery_used |= other.anchor_recovery_used;
        self.anchor_candidate_count = self
            .anchor_candidate_count
            .saturating_add(other.anchor_candidate_count);
        self.preferred_scale_hit |= other.preferred_scale_hit;
        self.single_match_ms = self.single_match_ms.max(other.single_match_ms);
        self.wait_total_ms = self.wait_total_ms.max(other.wait_total_ms);
        if other.robust_score.is_some() {
            self.robust_score = other.robust_score;
        }
        if other.matched_scale.is_some() {
            self.matched_scale = other.matched_scale;
            self.matched_width = other.matched_width;
            self.matched_height = other.matched_height;
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatcherResult {
    pub image: Option<ImageMatch>,
    pub diagnostics: VisionDiagnostics,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VisionSearchResult {
    pub image: Option<ImageMatch>,
    pub diagnostics: VisionDiagnostics,
}

pub trait VisionApi: Send + Sync {
    fn active_window_title(&self) -> Result<String, VisionError>;
    fn window_exists(&self, title_query: &str) -> Result<bool, VisionError>;
    fn window_rect(&self, title_query: &str) -> Result<WindowRectValue, VisionError>;
    fn wait_window(
        &self,
        title_query: &str,
        options: VisionPollOptions<'_>,
    ) -> Result<bool, VisionError>;
    fn pixel_matches(
        &self,
        point: Point,
        expected: RgbColor,
        tolerance: u8,
        cancel: &AtomicBool,
    ) -> Result<bool, VisionError>;
    fn wait_pixel(
        &self,
        point: Point,
        expected: RgbColor,
        tolerance: u8,
        options: VisionPollOptions<'_>,
    ) -> Result<bool, VisionError>;
    fn find_image(
        &self,
        file_name: &str,
        region: ScreenRect,
        threshold: f32,
        cancel: &AtomicBool,
    ) -> Result<Option<ImageMatch>, VisionError>;

    fn find_image_diagnostic(
        &self,
        file_name: &str,
        region: ScreenRect,
        threshold: f32,
        cancel: &AtomicBool,
        options: &MatcherOptions,
    ) -> Result<VisionSearchResult, VisionError> {
        let started = std::time::Instant::now();
        let image = self.find_image(file_name, region, threshold, cancel)?;
        let mut diagnostics = VisionDiagnostics::for_mode(options.mode);
        diagnostics.total_ms = started.elapsed().as_millis() as u64;
        diagnostics.single_match_ms = diagnostics.total_ms;
        Ok(VisionSearchResult { image, diagnostics })
    }
    fn wait_image(
        &self,
        file_name: &str,
        region: ScreenRect,
        threshold: f32,
        options: VisionPollOptions<'_>,
    ) -> Result<Option<ImageMatch>, VisionError>;

    fn wait_image_diagnostic(
        &self,
        file_name: &str,
        region: ScreenRect,
        threshold: f32,
        options: VisionPollOptions<'_>,
        matcher_options: &MatcherOptions,
    ) -> Result<VisionSearchResult, VisionError> {
        let started = std::time::Instant::now();
        let image = self.wait_image(file_name, region, threshold, options)?;
        let mut diagnostics = VisionDiagnostics::for_mode(matcher_options.mode);
        diagnostics.total_ms = started.elapsed().as_millis() as u64;
        diagnostics.wait_total_ms = diagnostics.total_ms;
        Ok(VisionSearchResult { image, diagnostics })
    }
}

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: WindowId,
    pub title: String,
    pub rect: ScreenRect,
    pub visible: bool,
    pub minimized: bool,
    pub process_id: Option<u32>,
}

pub struct VisionPollBudget {
    count: AtomicUsize,
    progress: Option<Arc<dyn Fn(usize) + Send + Sync>>,
}

impl VisionPollBudget {
    pub fn new(progress: Option<Arc<dyn Fn(usize) + Send + Sync>>) -> Self {
        Self {
            count: AtomicUsize::new(0),
            progress,
        }
    }

    pub fn consume(&self) -> Result<usize, VisionError> {
        let next = self.count.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        if next > MAX_VISION_OPERATIONS {
            return Err(VisionError::new(
                "vision_operation_limit",
                "视觉轮询超过最大操作数，请增大轮询间隔或缩短等待时间",
            ));
        }
        if let Some(progress) = &self.progress {
            progress(next);
        }
        Ok(next)
    }

    pub fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

pub struct VisionPollOptions<'a> {
    pub timeout: Duration,
    pub poll: Duration,
    pub cancel: &'a AtomicBool,
    pub budget: &'a VisionPollBudget,
}

impl<'a> VisionPollOptions<'a> {
    pub const fn new(
        timeout: Duration,
        poll: Duration,
        cancel: &'a AtomicBool,
        budget: &'a VisionPollBudget,
    ) -> Self {
        Self {
            timeout,
            poll,
            cancel,
            budget,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_monitor_coordinates_and_intersection_are_safe() {
        let monitor = ScreenRect::from_parts(-1920, -200, 1920, 1080);
        let region = ScreenRect::from_parts(-100, 20, 100, 100);
        assert!(monitor.contains(&region).expect("contains"));
        assert_eq!(monitor.intersection(&region), Some(region));
        let outside = ScreenRect::from_parts(0, 900, 100, 100);
        assert_eq!(monitor.intersection(&outside), None);
    }

    #[test]
    fn screen_rect_rejects_zero_and_oversized_regions() {
        assert_eq!(
            ScreenRect::new(0, 0, 0, 1).expect_err("zero width").code,
            "capture_region_invalid"
        );
        assert_eq!(
            ScreenRect::new(0, 0, MAX_CAPTURE_WIDTH, MAX_CAPTURE_HEIGHT + 1)
                .expect_err("oversized region")
                .code,
            "capture_region_invalid"
        );
    }

    #[test]
    fn scale_candidates_cover_common_windows_dpi_values_and_validate_bounds() {
        let options = MatcherOptions::default();
        assert_eq!(
            options.scale_candidates().expect("default scales"),
            vec![0.67, 0.8, 0.83, 1.0, 1.2, 1.25, 1.5, 1.75, 2.0]
        );
        let mut stepped = MatcherOptions {
            scale_min: 0.8,
            scale_max: 1.2,
            scale_step: Some(0.2),
            ..options.clone()
        };
        assert_eq!(
            stepped.scale_candidates().expect("stepped scales"),
            vec![0.8, 1.0, 1.2]
        );
        stepped.scale_min = 2.1;
        assert_eq!(
            stepped.validate().expect_err("invalid scale").code,
            "vision_scale_invalid"
        );
        stepped.scale_min = 1.2;
        stepped.scale_max = 0.8;
        assert_eq!(
            stepped.validate().expect_err("reversed range").code,
            "vision_scale_range_invalid"
        );

        let mut non_finite = options.clone();
        non_finite.scale_min = f32::NAN;
        assert_eq!(
            non_finite.validate().expect_err("non-finite scale").code,
            "vision_scale_invalid"
        );

        let mut too_many = options.clone();
        too_many.scale_min = MIN_MATCH_SCALE;
        too_many.scale_max = MAX_MATCH_SCALE;
        too_many.scale_step = Some(0.01);
        assert_eq!(
            too_many
                .validate()
                .expect_err("too many scale candidates")
                .code,
            "vision_scale_candidates_invalid"
        );

        let mut preferred = options;
        preferred.preferred_scale = Some(1.25);
        assert_eq!(
            preferred.scale_candidates().expect("preferred scales")[0],
            1.25
        );
    }
}
