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
pub const MAX_ASSET_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_ASSET_NAME_LENGTH: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(pub isize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

impl Clone for CaptureFrame {
    fn clone(&self) -> Self {
        Self {
            origin: self.origin,
            width: self.width,
            height: self.height,
            pixels_bgra: Arc::clone(&self.pixels_bgra),
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
        Ok(Self {
            origin,
            width,
            height,
            pixels_bgra: pixels.into(),
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
        Self::from_bgra(
            Point {
                x: region.x,
                y: region.y,
            },
            region.width,
            region.height,
            pixels,
        )
    }

    pub fn pixels_bgra(&self) -> &[u8] {
        &self.pixels_bgra
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
}

pub trait VisionApi: Send + Sync {
    fn active_window_title(&self) -> Result<String, VisionError>;
    fn window_exists(&self, title_query: &str) -> Result<bool, VisionError>;
    fn window_rect(&self, title_query: &str) -> Result<WindowRectValue, VisionError>;
    fn wait_window(
        &self,
        title_query: &str,
        timeout: Duration,
        poll: Duration,
        cancel: &AtomicBool,
        budget: &VisionPollBudget,
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
        timeout: Duration,
        poll: Duration,
        cancel: &AtomicBool,
        budget: &VisionPollBudget,
    ) -> Result<bool, VisionError>;
    fn find_image(
        &self,
        asset_id: &str,
        region: ScreenRect,
        threshold: f32,
        cancel: &AtomicBool,
    ) -> Result<Option<ImageMatch>, VisionError>;
    fn wait_image(
        &self,
        asset_id: &str,
        region: ScreenRect,
        threshold: f32,
        timeout: Duration,
        poll: Duration,
        cancel: &AtomicBool,
        budget: &VisionPollBudget,
    ) -> Result<Option<ImageMatch>, VisionError>;
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
