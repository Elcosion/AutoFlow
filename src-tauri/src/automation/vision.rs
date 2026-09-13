use super::types::{
    CaptureFrame, ImageMatch, Point, RgbColor, VisionError, VisionMatcher, MAX_TEMPLATE_HEIGHT,
    MAX_TEMPLATE_PIXELS, MAX_TEMPLATE_WIDTH,
};
use image::GrayImage;
use imageproc::template_matching::{match_template, MatchTemplateMethod};

#[derive(Debug, Default)]
pub struct ImageProcVisionMatcher;

impl ImageProcVisionMatcher {
    pub fn new() -> Self {
        Self
    }

    fn gray(frame: &CaptureFrame) -> Result<GrayImage, VisionError> {
        let pixels = frame.pixels_bgra();
        let width = frame.width;
        let height = frame.height;
        let expected = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| VisionError::new("capture_region_invalid", "图像帧尺寸溢出"))?;
        if pixels.len() != expected {
            return Err(VisionError::new(
                "capture_region_invalid",
                "图像帧像素数据不完整",
            ));
        }
        let mut gray = Vec::with_capacity(expected / 4);
        for pixel in pixels.as_chunks::<4>().0 {
            // Integer BT.601 luminance keeps matching deterministic and avoids
            // allocating an RGBA image solely for grayscale conversion.
            let value = (u16::from(pixel[2]) * 77
                + u16::from(pixel[1]) * 150
                + u16::from(pixel[0]) * 29
                + 128)
                / 256;
            gray.push(value as u8);
        }
        GrayImage::from_raw(width, height, gray)
            .ok_or_else(|| VisionError::new("capture_region_invalid", "无法构造灰度帧"))
    }
}

impl VisionMatcher for ImageProcVisionMatcher {
    fn pixel_matches(
        &self,
        frame: &CaptureFrame,
        point: Point,
        expected: RgbColor,
        tolerance: u8,
    ) -> Result<bool, VisionError> {
        let Some(actual) = frame.pixel(point) else {
            return Err(VisionError::new(
                "capture_region_invalid",
                "像素坐标不在捕获帧范围内",
            ));
        };
        let within = |left: u8, right: u8| {
            (i16::from(left) - i16::from(right)).unsigned_abs() <= u16::from(tolerance)
        };
        Ok(within(actual.red, expected.red)
            && within(actual.green, expected.green)
            && within(actual.blue, expected.blue))
    }

    fn find_template(
        &self,
        frame: &CaptureFrame,
        template: &CaptureFrame,
        threshold: f32,
    ) -> Result<Option<ImageMatch>, VisionError> {
        validate_threshold(threshold)?;
        if template.width == 0
            || template.height == 0
            || template.width > MAX_TEMPLATE_WIDTH
            || template.height > MAX_TEMPLATE_HEIGHT
            || u64::from(template.width) * u64::from(template.height) > MAX_TEMPLATE_PIXELS
        {
            return Err(VisionError::new(
                "asset_decode_failed",
                "模板尺寸超过允许范围",
            ));
        }
        if template.width > frame.width || template.height > frame.height {
            return Err(VisionError::new(
                "template_larger_than_region",
                "模板不能大于搜索区域",
            ));
        }
        let image = Self::gray(frame)?;
        let template_image = Self::gray(template)?;
        let scores = match_template(
            &image,
            &template_image,
            MatchTemplateMethod::CrossCorrelationNormalized,
        );
        let mut best: Option<(u32, u32, f32)> = None;
        for (x, y, pixel) in scores.enumerate_pixels() {
            let score = pixel.0[0];
            if !score.is_finite() {
                continue;
            }
            if best.is_none_or(|(_, _, current)| score > current) {
                best = Some((x, y, score));
            }
        }
        let Some((x, y, score)) = best else {
            return Ok(None);
        };
        if score < threshold {
            return Ok(None);
        }
        let match_x = i64::from(frame.origin.x)
            .checked_add(i64::from(x))
            .ok_or_else(|| VisionError::new("capture_region_invalid", "匹配坐标溢出"))?;
        let match_y = i64::from(frame.origin.y)
            .checked_add(i64::from(y))
            .ok_or_else(|| VisionError::new("capture_region_invalid", "匹配坐标溢出"))?;
        let center_x = match_x
            .checked_add(i64::from(template.width / 2))
            .ok_or_else(|| VisionError::new("capture_region_invalid", "匹配坐标溢出"))?;
        let center_y = match_y
            .checked_add(i64::from(template.height / 2))
            .ok_or_else(|| VisionError::new("capture_region_invalid", "匹配坐标溢出"))?;
        Ok(Some(ImageMatch {
            x: i32::try_from(match_x)
                .map_err(|_| VisionError::new("capture_region_invalid", "匹配坐标超出屏幕范围"))?,
            y: i32::try_from(match_y)
                .map_err(|_| VisionError::new("capture_region_invalid", "匹配坐标超出屏幕范围"))?,
            width: template.width,
            height: template.height,
            center_x: i32::try_from(center_x).map_err(|_| {
                VisionError::new("capture_region_invalid", "匹配中心坐标超出屏幕范围")
            })?,
            center_y: i32::try_from(center_y).map_err(|_| {
                VisionError::new("capture_region_invalid", "匹配中心坐标超出屏幕范围")
            })?,
            score,
        }))
    }
}

pub fn validate_threshold(threshold: f32) -> Result<(), VisionError> {
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        return Err(VisionError::new(
            "vision_threshold_invalid",
            "匹配阈值必须在 0.0 到 1.0 之间",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(origin: Point, width: u32, height: u32, pixels: &[[u8; 4]]) -> CaptureFrame {
        CaptureFrame::from_bgra(
            origin,
            width,
            height,
            pixels.iter().flatten().copied().collect(),
        )
        .expect("test frame")
    }

    #[test]
    fn rgb_tolerance_is_inclusive_at_the_boundary() {
        let frame = frame(Point { x: -3, y: 8 }, 1, 1, &[[30, 20, 10, 255]]);
        let matcher = ImageProcVisionMatcher::new();
        assert!(matcher
            .pixel_matches(
                &frame,
                Point { x: -3, y: 8 },
                RgbColor {
                    red: 15,
                    green: 20,
                    blue: 30
                },
                5,
            )
            .expect("pixel match"));
        assert!(!matcher
            .pixel_matches(
                &frame,
                Point { x: -3, y: 8 },
                RgbColor {
                    red: 16,
                    green: 20,
                    blue: 30
                },
                5,
            )
            .expect("pixel mismatch"));
    }

    #[test]
    fn template_matching_finds_exact_template_and_rejects_mismatch() {
        let image = frame(
            Point { x: -10, y: 20 },
            4,
            3,
            &[
                [0, 0, 0, 255],
                [0, 0, 0, 255],
                [0, 0, 0, 255],
                [0, 0, 0, 255],
                [0, 0, 0, 255],
                [10, 20, 30, 255],
                [40, 50, 60, 255],
                [0, 0, 0, 255],
                [0, 0, 0, 255],
                [70, 80, 90, 255],
                [100, 110, 120, 255],
                [0, 0, 0, 255],
            ],
        );
        let template = frame(
            Point { x: 0, y: 0 },
            2,
            2,
            &[
                [10, 20, 30, 255],
                [40, 50, 60, 255],
                [70, 80, 90, 255],
                [100, 110, 120, 255],
            ],
        );
        let matcher = ImageProcVisionMatcher::new();
        let found = matcher
            .find_template(&image, &template, 0.99)
            .expect("template match")
            .expect("exact template");
        assert_eq!((found.x, found.y), (-9, 21));
        assert!(matcher
            .find_template(&image, &template, 1.0)
            .expect("template mismatch")
            .is_some());
    }

    #[test]
    fn template_larger_than_region_and_invalid_threshold_are_rejected() {
        let small = frame(Point { x: 0, y: 0 }, 1, 1, &[[0, 0, 0, 255]]);
        let large = frame(Point { x: 0, y: 0 }, 2, 2, &[[0, 0, 0, 255]; 4]);
        let matcher = ImageProcVisionMatcher::new();
        assert_eq!(
            matcher
                .find_template(&small, &large, 0.8)
                .expect_err("larger template")
                .code,
            "template_larger_than_region"
        );
        assert_eq!(
            validate_threshold(1.1).expect_err("invalid threshold").code,
            "vision_threshold_invalid"
        );
    }
}
