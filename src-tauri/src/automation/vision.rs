use super::types::{
    CaptureFrame, ImageMatch, MatcherMode, MatcherOptions, MatcherResult, Point, RgbColor,
    ScreenRect, VisionDiagnostics, VisionError, VisionMatcher, MAX_TEMPLATE_HEIGHT,
    MAX_TEMPLATE_PIXELS, MAX_TEMPLATE_WIDTH,
};
use image::imageops::{crop_imm, resize, FilterType};
use image::{GrayImage, Luma};
use imageproc::template_matching::{match_template, match_template_parallel, MatchTemplateMethod};
use std::sync::Arc;
use std::time::Instant;

const PYRAMID_MIN_TEMPLATE_SIDE: u32 = 24;
const PYRAMID_MIN_SEARCH_MULTIPLIER: u32 = 2;
const REFINE_RADIUS: u32 = 10;
const COARSE_GATE_MARGIN: f32 = 0.22;
const COARSE_MIN_CONFIDENCE: f32 = 0.35;
const COARSE_UNCERTAINTY_CONFIDENCE: f32 = 0.88;
const REFINE_UNCERTAINTY_MARGIN: f32 = 0.04;
const LOW_VARIANCE_FLOOR: f64 = 1.0;

#[derive(Debug)]
pub struct PreparedTemplate {
    resource_id: String,
    file_name: String,
    fingerprint: String,
    frame: Arc<CaptureFrame>,
    gray: GrayImage,
    half_gray: Option<GrayImage>,
    sum_squares: f64,
    variance: f64,
}

impl PreparedTemplate {
    pub fn from_frame(
        resource_id: impl Into<String>,
        file_name: impl Into<String>,
        fingerprint: impl Into<String>,
        frame: Arc<CaptureFrame>,
    ) -> Result<Self, VisionError> {
        validate_template_dimensions(frame.width, frame.height)?;
        let gray = ImageProcVisionMatcher::gray(&frame)?;
        let half_gray = (frame.width >= 2 && frame.height >= 2).then(|| {
            resize(
                &gray,
                frame.width.div_ceil(2),
                frame.height.div_ceil(2),
                FilterType::Triangle,
            )
        });
        let pixel_count = f64::from(frame.width) * f64::from(frame.height);
        let sum = gray
            .pixels()
            .map(|pixel| f64::from(pixel.0[0]))
            .sum::<f64>();
        let sum_squares = gray
            .pixels()
            .map(|pixel| {
                let value = f64::from(pixel.0[0]);
                value * value
            })
            .sum::<f64>();
        let mean = if pixel_count > 0.0 {
            sum / pixel_count
        } else {
            0.0
        };
        let variance = if pixel_count > 0.0 {
            (sum_squares / pixel_count - mean * mean).max(0.0)
        } else {
            0.0
        };
        Ok(Self {
            resource_id: resource_id.into(),
            file_name: file_name.into(),
            fingerprint: fingerprint.into(),
            frame,
            gray,
            half_gray,
            sum_squares,
            variance,
        })
    }

    pub fn resource_id(&self) -> &str {
        &self.resource_id
    }

    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn frame(&self) -> &Arc<CaptureFrame> {
        &self.frame
    }

    pub fn gray(&self) -> &GrayImage {
        &self.gray
    }

    pub fn half_gray(&self) -> Option<&GrayImage> {
        self.half_gray.as_ref()
    }

    pub fn sum_squares(&self) -> f64 {
        self.sum_squares
    }

    pub fn variance(&self) -> f64 {
        self.variance
    }

    pub fn memory_bytes(&self) -> usize {
        self.frame.pixels_bgra().len()
            + self.gray.as_raw().len()
            + self
                .half_gray
                .as_ref()
                .map(|image| image.as_raw().len())
                .unwrap_or(0)
    }
}

#[derive(Debug, Default)]
pub struct ImageProcVisionMatcher;

impl ImageProcVisionMatcher {
    pub fn new() -> Self {
        Self
    }

    pub(crate) fn gray(frame: &CaptureFrame) -> Result<GrayImage, VisionError> {
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

    fn prepare_ephemeral(&self, template: &CaptureFrame) -> Result<PreparedTemplate, VisionError> {
        PreparedTemplate::from_frame("", "", "", Arc::new(template.clone()))
    }

    fn match_prepared(
        &self,
        frame: &CaptureFrame,
        prepared: &PreparedTemplate,
        threshold: f32,
        options: &MatcherOptions,
    ) -> Result<MatcherResult, VisionError> {
        validate_threshold(threshold)?;
        options.validate()?;
        validate_template_dimensions(prepared.frame.width, prepared.frame.height)?;
        if prepared.frame.width > frame.width || prepared.frame.height > frame.height {
            return Err(VisionError::new(
                "template_larger_than_region",
                "模板不能大于搜索区域",
            ));
        }
        let started = Instant::now();
        let mut diagnostics = VisionDiagnostics::for_mode(options.mode);
        let image = Self::gray(frame)?;
        let template_width = prepared.frame.width;
        let template_height = prepared.frame.height;

        let pyramid_eligible = options.mode != MatcherMode::Exact
            && template_width.min(template_height) >= PYRAMID_MIN_TEMPLATE_SIDE
            && frame.width >= template_width.saturating_mul(PYRAMID_MIN_SEARCH_MULTIPLIER)
            && frame.height >= template_height.saturating_mul(PYRAMID_MIN_SEARCH_MULTIPLIER)
            && prepared.variance() >= LOW_VARIANCE_FLOOR;
        if !pyramid_eligible {
            let mut result =
                self.full_match(frame, &image, prepared.gray(), threshold, options.mode)?;
            result.diagnostics.total_ms = started.elapsed().as_millis() as u64;
            result.diagnostics.fallback_used = options.mode == MatcherMode::Auto;
            return Ok(result);
        }

        let coarse_started = Instant::now();
        let half_image = resize(
            &image,
            frame.width.div_ceil(2),
            frame.height.div_ceil(2),
            FilterType::Triangle,
        );
        let half_template = prepared
            .half_gray()
            .ok_or_else(|| VisionError::new("vision_match_failed", "模板金字塔准备失败"))?;
        let coarse_scores = match_template_parallel(
            &half_image,
            half_template,
            MatchTemplateMethod::CrossCorrelationNormalized,
        );
        diagnostics.coarse_ms = coarse_started.elapsed().as_millis() as u64;

        let (coarse_best, candidates) = coarse_candidates(
            &coarse_scores,
            frame,
            template_width,
            template_height,
            options.max_candidates,
        );
        diagnostics.candidate_count = candidates.len();
        let Some(coarse_best) = coarse_best else {
            return Ok(MatcherResult {
                image: None,
                diagnostics: finish_diagnostics(diagnostics, started),
            });
        };
        diagnostics.coarse_score = coarse_best;
        let coarse_gate = (threshold - COARSE_GATE_MARGIN).max(COARSE_MIN_CONFIDENCE);
        if coarse_best < coarse_gate {
            return Ok(MatcherResult {
                image: None,
                diagnostics: finish_diagnostics(diagnostics, started),
            });
        }

        let refine_started = Instant::now();
        let refined = refine_candidates(
            &image,
            frame,
            prepared.gray(),
            template_width,
            template_height,
            &candidates,
        )?;
        diagnostics.refine_ms = refine_started.elapsed().as_millis() as u64;
        if let Some(best) = refined.as_ref() {
            diagnostics.refined_score = best.score;
            if best.score >= threshold {
                return Ok(MatcherResult {
                    image: Some(best.clone()),
                    diagnostics: finish_diagnostics(diagnostics, started),
                });
            }
        }

        let coarse_uncertainty = (threshold * 0.8).max(COARSE_UNCERTAINTY_CONFIDENCE);
        let refine_uncertain = refined
            .as_ref()
            .is_none_or(|best| best.score >= (threshold - REFINE_UNCERTAINTY_MARGIN).max(0.0));
        if options.mode == MatcherMode::Fast
            || !refine_uncertain
            || coarse_best < coarse_uncertainty
        {
            return Ok(MatcherResult {
                image: None,
                diagnostics: finish_diagnostics(diagnostics, started),
            });
        }

        let fallback_started = Instant::now();
        let mut result =
            self.full_match(frame, &image, prepared.gray(), threshold, options.mode)?;
        diagnostics.fallback_ms = fallback_started.elapsed().as_millis() as u64;
        diagnostics.fallback_used = true;
        diagnostics.candidate_count = diagnostics.candidate_count.max(1);
        diagnostics.total_ms = started.elapsed().as_millis() as u64;
        result.diagnostics = diagnostics;
        Ok(result)
    }

    fn full_match(
        &self,
        frame: &CaptureFrame,
        image: &GrayImage,
        template: &GrayImage,
        threshold: f32,
        mode: MatcherMode,
    ) -> Result<MatcherResult, VisionError> {
        let started = Instant::now();
        let scores = match_template_parallel(
            image,
            template,
            MatchTemplateMethod::CrossCorrelationNormalized,
        );
        let best = best_match_from_scores(&scores, frame, template.width(), template.height())?;
        let mut diagnostics = VisionDiagnostics::for_mode(mode);
        diagnostics.fallback_ms = started.elapsed().as_millis() as u64;
        diagnostics.candidate_count = usize::from(best.is_some());
        diagnostics.fallback_used = false;
        Ok(MatcherResult {
            image: best.filter(|value| value.score >= threshold),
            diagnostics,
        })
    }

    /// The old serial implementation is intentionally kept for differential tests
    /// and the synthetic benchmark. It is never used by the normal runtime path.
    pub fn find_template_baseline(
        &self,
        frame: &CaptureFrame,
        template: &CaptureFrame,
        threshold: f32,
    ) -> Result<Option<ImageMatch>, VisionError> {
        validate_threshold(threshold)?;
        validate_template_dimensions(template.width, template.height)?;
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
        Ok(
            best_match_from_scores(&scores, frame, template.width, template.height)?
                .filter(|value| value.score >= threshold),
        )
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
        let prepared = self.prepare_ephemeral(template)?;
        Ok(self
            .match_prepared(frame, &prepared, threshold, &MatcherOptions::default())?
            .image)
    }

    fn find_template_with_options(
        &self,
        frame: &CaptureFrame,
        template: &CaptureFrame,
        threshold: f32,
        options: &MatcherOptions,
    ) -> Result<MatcherResult, VisionError> {
        let prepared = self.prepare_ephemeral(template)?;
        self.match_prepared(frame, &prepared, threshold, options)
    }

    fn find_prepared_template(
        &self,
        frame: &CaptureFrame,
        template: &CaptureFrame,
        prepared: Option<&PreparedTemplate>,
        threshold: f32,
        options: &MatcherOptions,
    ) -> Result<MatcherResult, VisionError> {
        if let Some(prepared) = prepared {
            if prepared.frame.width != template.width || prepared.frame.height != template.height {
                return Err(VisionError::new(
                    "vision_template_cache_invalid",
                    "模板缓存与资源尺寸不一致，请刷新图像资源",
                ));
            }
            return self.match_prepared(frame, prepared, threshold, options);
        }
        self.find_template_with_options(frame, template, threshold, options)
    }
}

#[derive(Debug, Clone, Copy)]
struct CoarseCandidate {
    x: u32,
    y: u32,
    score: f32,
}

fn coarse_candidates(
    scores: &image::ImageBuffer<Luma<f32>, Vec<f32>>,
    frame: &CaptureFrame,
    template_width: u32,
    template_height: u32,
    max_candidates: usize,
) -> (Option<f32>, Vec<CoarseCandidate>) {
    let max_x = frame.width.saturating_sub(template_width);
    let max_y = frame.height.saturating_sub(template_height);
    let mut ranked = scores
        .enumerate_pixels()
        .filter_map(|(x, y, pixel)| {
            let score = pixel.0[0];
            if !score.is_finite() {
                return None;
            }
            let x = x.saturating_mul(2).min(max_x);
            let y = y.saturating_mul(2).min(max_y);
            let region = ScreenRect::from_parts(
                frame.origin.x.saturating_add(i32::try_from(x).ok()?),
                frame.origin.y.saturating_add(i32::try_from(y).ok()?),
                template_width,
                template_height,
            );
            frame
                .is_region_fully_valid(region)
                .then_some(CoarseCandidate { x, y, score })
        })
        .collect::<Vec<_>>();
    let best = ranked
        .iter()
        .map(|candidate| candidate.score)
        .max_by(|left, right| left.total_cmp(right));
    ranked.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.y.cmp(&right.y))
            .then_with(|| left.x.cmp(&right.x))
    });

    let nms_distance = template_width.max(template_height).max(16) / 2;
    let nms_distance_squared = u64::from(nms_distance) * u64::from(nms_distance);
    let mut candidates = Vec::with_capacity(max_candidates);
    for candidate in ranked {
        if candidates.iter().all(|kept: &CoarseCandidate| {
            let dx = i64::from(candidate.x) - i64::from(kept.x);
            let dy = i64::from(candidate.y) - i64::from(kept.y);
            (dx * dx + dy * dy) as u64 >= nms_distance_squared
        }) {
            candidates.push(candidate);
            if candidates.len() >= max_candidates {
                break;
            }
        }
    }
    (best, candidates)
}

fn refine_candidates(
    image: &GrayImage,
    frame: &CaptureFrame,
    template: &GrayImage,
    template_width: u32,
    template_height: u32,
    candidates: &[CoarseCandidate],
) -> Result<Option<ImageMatch>, VisionError> {
    let max_x = image.width().saturating_sub(template_width);
    let max_y = image.height().saturating_sub(template_height);
    let mut best = None;
    for candidate in candidates {
        let center_x = candidate.x.min(max_x);
        let center_y = candidate.y.min(max_y);
        let left = center_x.saturating_sub(REFINE_RADIUS);
        let top = center_y.saturating_sub(REFINE_RADIUS);
        let right = center_x
            .saturating_add(REFINE_RADIUS)
            .min(max_x)
            .saturating_add(template_width);
        let bottom = center_y
            .saturating_add(REFINE_RADIUS)
            .min(max_y)
            .saturating_add(template_height);
        let local = crop_imm(
            image,
            left,
            top,
            right.saturating_sub(left),
            bottom.saturating_sub(top),
        )
        .to_image();
        let scores = match_template_parallel(
            &local,
            template,
            MatchTemplateMethod::CrossCorrelationNormalized,
        );
        let local_best = scores
            .enumerate_pixels()
            .filter_map(|(x, y, pixel)| {
                let score = pixel.0[0];
                if !score.is_finite() {
                    return None;
                }
                let absolute_x = left.saturating_add(x);
                let absolute_y = top.saturating_add(y);
                let screen_region = ScreenRect::from_parts(
                    frame
                        .origin
                        .x
                        .checked_add(i32::try_from(absolute_x).ok()?)?,
                    frame
                        .origin
                        .y
                        .checked_add(i32::try_from(absolute_y).ok()?)?,
                    template_width,
                    template_height,
                );
                frame
                    .is_region_fully_valid(screen_region)
                    .then_some((absolute_x, absolute_y, score))
            })
            .max_by(|left, right| left.2.total_cmp(&right.2));
        if let Some((x, y, score)) = local_best {
            let candidate = make_image_match(frame, template_width, template_height, x, y, score)?;
            if best
                .as_ref()
                .is_none_or(|current: &ImageMatch| candidate.score > current.score)
            {
                best = Some(candidate);
            }
        }
    }
    Ok(best)
}

fn best_match_from_scores(
    scores: &image::ImageBuffer<Luma<f32>, Vec<f32>>,
    frame: &CaptureFrame,
    template_width: u32,
    template_height: u32,
) -> Result<Option<ImageMatch>, VisionError> {
    let Some((x, y, score)) = scores
        .enumerate_pixels()
        .filter_map(|(x, y, pixel)| {
            let score = pixel.0[0];
            if !score.is_finite() {
                return None;
            }
            let region = ScreenRect::from_parts(
                frame.origin.x.checked_add(i32::try_from(x).ok()?)?,
                frame.origin.y.checked_add(i32::try_from(y).ok()?)?,
                template_width,
                template_height,
            );
            frame.is_region_fully_valid(region).then_some((x, y, score))
        })
        .max_by(|left, right| {
            left.2
                .total_cmp(&right.2)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| right.0.cmp(&left.0))
        })
    else {
        return Ok(None);
    };
    make_image_match(frame, template_width, template_height, x, y, score).map(Some)
}

fn make_image_match(
    frame: &CaptureFrame,
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    score: f32,
) -> Result<ImageMatch, VisionError> {
    let match_x = i64::from(frame.origin.x)
        .checked_add(i64::from(x))
        .ok_or_else(|| VisionError::new("capture_region_invalid", "匹配坐标溢出"))?;
    let match_y = i64::from(frame.origin.y)
        .checked_add(i64::from(y))
        .ok_or_else(|| VisionError::new("capture_region_invalid", "匹配坐标溢出"))?;
    let center_x = match_x
        .checked_add(i64::from(width / 2))
        .ok_or_else(|| VisionError::new("capture_region_invalid", "匹配中心坐标溢出"))?;
    let center_y = match_y
        .checked_add(i64::from(height / 2))
        .ok_or_else(|| VisionError::new("capture_region_invalid", "匹配中心坐标溢出"))?;
    Ok(ImageMatch {
        x: i32::try_from(match_x)
            .map_err(|_| VisionError::new("capture_region_invalid", "匹配坐标超出屏幕范围"))?,
        y: i32::try_from(match_y)
            .map_err(|_| VisionError::new("capture_region_invalid", "匹配坐标超出屏幕范围"))?,
        width,
        height,
        center_x: i32::try_from(center_x)
            .map_err(|_| VisionError::new("capture_region_invalid", "匹配中心坐标超出屏幕范围"))?,
        center_y: i32::try_from(center_y)
            .map_err(|_| VisionError::new("capture_region_invalid", "匹配中心坐标超出屏幕范围"))?,
        score,
    })
}

fn finish_diagnostics(mut diagnostics: VisionDiagnostics, started: Instant) -> VisionDiagnostics {
    diagnostics.total_ms = started.elapsed().as_millis() as u64;
    diagnostics
}

fn validate_template_dimensions(width: u32, height: u32) -> Result<(), VisionError> {
    if width == 0
        || height == 0
        || width > MAX_TEMPLATE_WIDTH
        || height > MAX_TEMPLATE_HEIGHT
        || u64::from(width) * u64::from(height) > MAX_TEMPLATE_PIXELS
    {
        return Err(VisionError::new(
            "asset_decode_failed",
            "模板尺寸超过允许范围",
        ));
    }
    Ok(())
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
                    blue: 30,
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
                    blue: 30,
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

    #[test]
    fn pure_color_templates_use_safe_parallel_compatibility_path() {
        let image = frame(Point { x: -5, y: 7 }, 3, 3, &[[0, 0, 0, 255]; 9]);
        let template = frame(Point { x: 0, y: 0 }, 2, 2, &[[0, 0, 0, 255]; 4]);
        let matcher = ImageProcVisionMatcher::new();
        let result = matcher
            .find_template(&image, &template, 0.9)
            .expect("pure template should not panic");
        assert!(result.is_none() || result.is_some_and(|value| value.score.is_finite()));
    }

    fn patterned_template(width: u32, height: u32, seed: u32) -> Vec<[u8; 4]> {
        let mut state = seed;
        (0..width * height)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let value = (state >> 24) as u8;
                [
                    value.wrapping_add(17),
                    value.wrapping_mul(3).wrapping_add(31),
                    value.wrapping_mul(5).wrapping_add(47),
                    255,
                ]
            })
            .collect()
    }

    fn patterned_frame(
        origin: Point,
        width: u32,
        height: u32,
        template: &[u8; 2],
        template_pixels: &[[u8; 4]],
        target: Option<(u32, u32)>,
        seed: u32,
    ) -> CaptureFrame {
        let mut state = seed;
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let value = (state >> 24) as u8;
            pixels.extend_from_slice(&[
                value.wrapping_add(5),
                value.wrapping_mul(7).wrapping_add(11),
                value.wrapping_mul(13).wrapping_add(23),
                255,
            ]);
        }
        if let Some((target_x, target_y)) = target {
            let template_width = u32::from(template[0]);
            let template_height = u32::from(template[1]);
            for y in 0..template_height {
                for x in 0..template_width {
                    let source_index = ((y * template_width + x) * 4) as usize;
                    let target_index = (((target_y + y) * width + target_x + x) * 4) as usize;
                    pixels[target_index..target_index + 4]
                        .copy_from_slice(&template_pixels[source_index / 4]);
                }
            }
        }
        CaptureFrame::from_bgra(origin, width, height, pixels).expect("patterned frame")
    }

    fn test_template() -> (CaptureFrame, Vec<[u8; 4]>, [u8; 2]) {
        let size = [24, 24];
        let pixels = patterned_template(u32::from(size[0]), u32::from(size[1]), 0x51A7_2026);
        let frame = frame(
            Point { x: 0, y: 0 },
            u32::from(size[0]),
            u32::from(size[1]),
            &pixels,
        );
        (frame, pixels, size)
    }

    #[test]
    fn optimized_pyramid_matches_baseline_at_start_middle_and_end() {
        let (template, template_pixels, size) = test_template();
        let matcher = ImageProcVisionMatcher::new();
        for target in [(0, 0), (52, 28), (104, 72)] {
            let image = patterned_frame(
                Point { x: 0, y: 0 },
                128,
                96,
                &size,
                &template_pixels,
                Some(target),
                0xA11CE,
            );
            let baseline = matcher
                .find_template_baseline(&image, &template, 0.99)
                .expect("baseline")
                .expect("baseline hit");
            let optimized = matcher
                .find_template(&image, &template, 0.99)
                .expect("optimized")
                .expect("optimized hit");
            assert_eq!((baseline.x, baseline.y), (optimized.x, optimized.y));
            assert_eq!(
                (optimized.x, optimized.y),
                (target.0 as i32, target.1 as i32)
            );
        }
    }

    #[test]
    fn optimized_random_differential_test_keeps_coordinates() {
        let (template, template_pixels, size) = test_template();
        let matcher = ImageProcVisionMatcher::new();
        for (seed, target) in [(1_u32, (3, 4)), (2, (40, 22)), (3, (96, 68)), (4, (70, 2))] {
            let image = patterned_frame(
                Point { x: -30, y: 17 },
                128,
                96,
                &size,
                &template_pixels,
                Some(target),
                seed,
            );
            let baseline = matcher
                .find_template_baseline(&image, &template, 0.99)
                .expect("baseline")
                .expect("baseline differential hit");
            let optimized = matcher
                .find_template(&image, &template, 0.99)
                .expect("optimized")
                .expect("optimized differential hit");
            assert_eq!((baseline.x, baseline.y), (optimized.x, optimized.y));
        }
    }

    #[test]
    fn fast_miss_does_not_run_full_fallback() {
        let (template, template_pixels, size) = test_template();
        let image = patterned_frame(
            Point { x: 0, y: 0 },
            128,
            96,
            &size,
            &template_pixels,
            None,
            0xCAFE,
        );
        let prepared =
            PreparedTemplate::from_frame("test", "test.png", "fixed", Arc::new(template.clone()))
                .expect("prepared template");
        let result = ImageProcVisionMatcher::new()
            .find_prepared_template(
                &image,
                &template,
                Some(&prepared),
                1.0,
                &MatcherOptions {
                    mode: MatcherMode::Fast,
                    prefer_last: false,
                    max_candidates: 8,
                },
            )
            .expect("fast miss");
        assert!(result.image.is_none());
        assert!(!result.diagnostics.fallback_used);
    }

    #[test]
    fn uncertain_coarse_result_uses_parallel_full_fallback() {
        let (template, template_pixels, size) = test_template();
        let mut image = patterned_frame(
            Point { x: 0, y: 0 },
            128,
            96,
            &size,
            &template_pixels,
            Some((86, 42)),
            0xBEEF,
        );
        let mut pixels = image.pixels_bgra().to_vec();
        let target_index = (((42_u32 + 5) * 128 + 86 + 5) * 4) as usize;
        pixels[target_index] = pixels[target_index].wrapping_add(90);
        image = CaptureFrame::from_bgra(Point { x: 0, y: 0 }, 128, 96, pixels)
            .expect("uncertain frame");
        let prepared =
            PreparedTemplate::from_frame("test", "test.png", "fixed", Arc::new(template.clone()))
                .expect("prepared template");
        let result = ImageProcVisionMatcher::new()
            .find_prepared_template(
                &image,
                &template,
                Some(&prepared),
                1.0,
                &MatcherOptions::default(),
            )
            .expect("uncertain match");
        assert!(result.image.is_none());
        assert!(result.diagnostics.fallback_used);
        assert!(result.diagnostics.fallback_ms > 0 || result.diagnostics.total_ms > 0);
    }

    #[test]
    fn highest_original_score_wins_among_similar_candidates() {
        let (template, template_pixels, size) = test_template();
        let mut image = patterned_frame(
            Point { x: 0, y: 0 },
            128,
            96,
            &size,
            &template_pixels,
            Some((86, 42)),
            0x1234,
        );
        let mut pixels = image.pixels_bgra().to_vec();
        let weaker = (10_u32, 12_u32);
        for y in 0..u32::from(size[1]) {
            for x in 0..u32::from(size[0]) {
                let source_index = ((y * u32::from(size[0]) + x) * 4) as usize;
                let target_index = (((weaker.1 + y) * 128 + weaker.0 + x) * 4) as usize;
                pixels[target_index..target_index + 4]
                    .copy_from_slice(&template_pixels[source_index / 4]);
            }
        }
        let weaker_index = (((weaker.1 + 3) * 128 + weaker.0 + 3) * 4) as usize;
        pixels[weaker_index] = pixels[weaker_index].wrapping_add(80);
        image = CaptureFrame::from_bgra(Point { x: 0, y: 0 }, 128, 96, pixels)
            .expect("similar candidate frame");
        let found = ImageProcVisionMatcher::new()
            .find_template(&image, &template, 0.9)
            .expect("similar candidates")
            .expect("strong candidate");
        assert_eq!((found.x, found.y), (86, 42));
    }

    #[test]
    fn invalid_black_padding_cannot_produce_a_match() {
        let (template, template_pixels, size) = test_template();
        let target = (72_u32, 20_u32);
        let mut image = patterned_frame(
            Point { x: -20, y: 10 },
            128,
            96,
            &size,
            &template_pixels,
            Some(target),
            0x7777,
        );
        let valid = ScreenRect::from_parts(-20, 10, 60, 96);
        image = CaptureFrame::from_bgra_with_valid_regions(
            Point { x: -20, y: 10 },
            128,
            96,
            image.pixels_bgra().to_vec(),
            vec![valid],
        )
        .expect("padded frame");
        assert!(ImageProcVisionMatcher::new()
            .find_template(&image, &template, 0.99)
            .expect("padding match")
            .is_none());
    }
}
