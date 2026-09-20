use super::types::{
    CaptureFrame, ImageMatch, MatcherMode, MatcherOptions, MatcherResult, Point, RgbColor,
    ScreenRect, VisionDiagnostics, VisionError, VisionMatcher, MAX_MATCH_SCALE,
    MAX_TEMPLATE_HEIGHT, MAX_TEMPLATE_PIXELS, MAX_TEMPLATE_WIDTH, MIN_MATCH_SCALE,
};
use image::imageops::{crop_imm, resize, FilterType};
use image::{GrayImage, Luma};
use imageproc::template_matching::{match_template, match_template_parallel, MatchTemplateMethod};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

const REFINE_RADIUS: u32 = 10;
const COARSE_GATE_MARGIN: f32 = 0.22;
const COARSE_MIN_CONFIDENCE: f32 = 0.35;
const COARSE_UNCERTAINTY_CONFIDENCE: f32 = 0.88;
const REFINE_UNCERTAINTY_MARGIN: f32 = 0.04;
const LOW_VARIANCE_FLOOR: f64 = 1.0;
const MAX_SCALED_TEMPLATE_CACHE_ENTRIES: usize = 16;
const MAX_SCALED_TEMPLATE_CACHE_BYTES: usize = 8 * 1024 * 1024;
const COARSE_TEMPLATE_MIN_SIDE: u32 = 8;
const ROBUST_GRID_SIZE: u32 = 3;
const ROBUST_MIN_VALID_TILES: usize = 4;
const ROBUST_MIN_PASS_TILES: usize = 3;
const ROBUST_MAX_DISCARDED_TILES: usize = 1;
const ROBUST_TILE_VARIANCE_FLOOR: f64 = 1.0;
const ROBUST_TILE_GRADIENT_FLOOR: f64 = 6.0;
const ROBUST_TILE_GATE_MARGIN: f32 = 0.18;
const ROBUST_COMBINED_WEIGHT: f32 = 0.70;
const ROBUST_PASS_COUNT_WEIGHT: f32 = 0.15;
const ROBUST_MAX_LOCAL_POSITIONS: usize = 6;
const MAX_ANCHOR_SCALES: usize = 4;
const MAX_ANCHOR_TILES: usize = 2;
const MAX_ANCHOR_POSITIONS_PER_TILE: usize = 4;
const MAX_ANCHOR_CANDIDATES: usize = 16;

#[derive(Debug)]
struct CachedScaledTemplate {
    template: Arc<PreparedScale>,
    last_used: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ScaledTemplateKey {
    resource_id: String,
    file_name: String,
    fingerprint: String,
    scale_bits: u32,
    width: u32,
    height: u32,
}

#[derive(Debug)]
pub struct PreparedScale {
    scale: f32,
    width: u32,
    height: u32,
    gray: GrayImage,
    alpha: GrayImage,
    half_gray: Option<GrayImage>,
    quarter_gray: Option<GrayImage>,
    sum: f64,
    sum_squares: f64,
    variance: f64,
    weight: f64,
    tiles: Vec<PreparedTile>,
    alpha_mask_used: bool,
}

#[derive(Debug, Clone)]
struct PreparedTile {
    row: u32,
    column: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    sum: f64,
    sum_squares: f64,
    weight: f64,
    variance: f64,
    gradient_energy: f64,
    valid: bool,
}

impl PreparedScale {
    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn gray(&self) -> &GrayImage {
        &self.gray
    }

    pub fn alpha(&self) -> &GrayImage {
        &self.alpha
    }

    pub fn half_gray(&self) -> Option<&GrayImage> {
        self.half_gray.as_ref()
    }

    pub fn coarse_gray(&self, factor: u32) -> Option<&GrayImage> {
        match factor {
            2 => self.half_gray.as_ref(),
            4 => self.quarter_gray.as_ref(),
            _ => None,
        }
    }

    pub fn variance(&self) -> f64 {
        self.variance
    }

    pub fn sum(&self) -> f64 {
        self.sum
    }

    pub fn sum_squares(&self) -> f64 {
        self.sum_squares
    }

    fn tiles(&self) -> &[PreparedTile] {
        &self.tiles
    }

    pub fn alpha_mask_used(&self) -> bool {
        self.alpha_mask_used
    }

    fn memory_bytes(&self) -> usize {
        self.gray.as_raw().len()
            + self.alpha.as_raw().len()
            + self
                .half_gray
                .as_ref()
                .map(|image| image.as_raw().len())
                .unwrap_or(0)
            + self
                .quarter_gray
                .as_ref()
                .map(|image| image.as_raw().len())
                .unwrap_or(0)
    }
}

#[derive(Debug)]
pub struct PreparedTemplate {
    resource_id: String,
    file_name: String,
    fingerprint: String,
    frame: Arc<CaptureFrame>,
    gray: GrayImage,
    alpha: GrayImage,
    half_gray: Option<GrayImage>,
    sum: f64,
    sum_squares: f64,
    variance: f64,
    alpha_mask_used: bool,
    scaled_templates: Mutex<HashMap<ScaledTemplateKey, CachedScaledTemplate>>,
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
        let alpha = ImageProcVisionMatcher::alpha_channel(&frame)?;
        let half_gray = (frame.width >= 2 && frame.height >= 2).then(|| {
            resize(
                &gray,
                frame.width.div_ceil(2),
                frame.height.div_ceil(2),
                FilterType::Triangle,
            )
        });
        let alpha_mask_used = ImageProcVisionMatcher::alpha_has_mask(&alpha);
        let (sum, sum_squares, variance) =
            ImageProcVisionMatcher::template_stats(&gray, &alpha, alpha_mask_used);
        Ok(Self {
            resource_id: resource_id.into(),
            file_name: file_name.into(),
            fingerprint: fingerprint.into(),
            frame,
            gray,
            alpha,
            half_gray,
            sum,
            sum_squares,
            variance,
            alpha_mask_used,
            scaled_templates: Mutex::new(HashMap::new()),
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

    pub fn sum(&self) -> f64 {
        self.sum
    }

    pub fn variance(&self) -> f64 {
        self.variance
    }

    pub fn alpha_mask_used(&self) -> bool {
        self.alpha_mask_used
    }

    pub fn scaled_template(&self, scale: f32) -> Result<Arc<PreparedScale>, VisionError> {
        validate_scale(scale)?;
        let width = scaled_dimension(self.frame.width, scale)?;
        let height = scaled_dimension(self.frame.height, scale)?;
        validate_template_dimensions(width, height)?;
        let cache_key = ScaledTemplateKey {
            resource_id: self.resource_id.clone(),
            file_name: self.file_name.clone(),
            fingerprint: self.fingerprint.clone(),
            scale_bits: scale.to_bits(),
            width,
            height,
        };
        if let Ok(mut cache) = self.scaled_templates.lock() {
            if let Some(cached) = cache.get_mut(&cache_key) {
                cached.last_used = SystemTime::now();
                return Ok(Arc::clone(&cached.template));
            }
        }

        let gray = if width == self.frame.width && height == self.frame.height {
            self.gray.clone()
        } else {
            resize(&self.gray, width, height, FilterType::Triangle)
        };
        let alpha = if width == self.frame.width && height == self.frame.height {
            self.alpha.clone()
        } else {
            resize(&self.alpha, width, height, FilterType::Triangle)
        };
        let prepared = Arc::new(ImageProcVisionMatcher::build_prepared_scale(
            scale, gray, alpha,
        ));

        if prepared.memory_bytes() <= MAX_SCALED_TEMPLATE_CACHE_BYTES {
            if let Ok(mut cache) = self.scaled_templates.lock() {
                let prepared_bytes = prepared.memory_bytes();
                while cache.len() >= MAX_SCALED_TEMPLATE_CACHE_ENTRIES
                    || cache
                        .values()
                        .map(|cached| cached.template.memory_bytes())
                        .sum::<usize>()
                        .saturating_add(prepared_bytes)
                        > MAX_SCALED_TEMPLATE_CACHE_BYTES
                {
                    let Some(oldest) = cache
                        .iter()
                        .min_by_key(|(_, cached)| cached.last_used)
                        .map(|(key, _)| key.clone())
                    else {
                        break;
                    };
                    cache.remove(&oldest);
                }
                cache.insert(
                    cache_key,
                    CachedScaledTemplate {
                        template: Arc::clone(&prepared),
                        last_used: SystemTime::now(),
                    },
                );
            }
        }
        Ok(prepared)
    }

    pub fn memory_bytes(&self) -> usize {
        self.frame.pixels_bgra().len()
            + self.gray.as_raw().len()
            + self.alpha.as_raw().len()
            + self
                .half_gray
                .as_ref()
                .map(|image| image.as_raw().len())
                .unwrap_or(0)
            + self
                .scaled_templates
                .lock()
                .map(|cache| {
                    cache
                        .values()
                        .map(|cached| cached.template.memory_bytes())
                        .sum::<usize>()
                })
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
    fn alpha_channel(frame: &CaptureFrame) -> Result<GrayImage, VisionError> {
        let expected = usize::try_from(frame.width)
            .ok()
            .and_then(|width| {
                usize::try_from(frame.height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| VisionError::new("capture_region_invalid", "图像帧尺寸溢出"))?;
        if frame.pixels_bgra().len() != expected {
            return Err(VisionError::new(
                "capture_region_invalid",
                "图像帧像素数据不完整",
            ));
        }
        let alpha = frame
            .pixels_bgra()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[3])
            .collect::<Vec<_>>();
        GrayImage::from_raw(frame.width, frame.height, alpha)
            .ok_or_else(|| VisionError::new("capture_region_invalid", "无法构造 alpha 通道"))
    }

    fn alpha_has_mask(alpha: &GrayImage) -> bool {
        alpha.pixels().any(|pixel| pixel.0[0] < u8::MAX)
    }

    fn build_prepared_scale(scale: f32, gray: GrayImage, alpha: GrayImage) -> PreparedScale {
        let width = gray.width();
        let height = gray.height();
        let half_gray = (width >= 2 && height >= 2).then(|| {
            resize(
                &gray,
                width.div_ceil(2),
                height.div_ceil(2),
                FilterType::Triangle,
            )
        });
        let quarter_gray = (width >= 4 && height >= 4).then(|| {
            resize(
                &gray,
                width.div_ceil(4),
                height.div_ceil(4),
                FilterType::Triangle,
            )
        });
        let alpha_mask_used = Self::alpha_has_mask(&alpha);
        let (sum, sum_squares, variance) = Self::template_stats(&gray, &alpha, alpha_mask_used);
        let weight = Self::template_weight(&alpha, alpha_mask_used);
        let tiles = Self::build_tiles(&gray, &alpha, alpha_mask_used);
        PreparedScale {
            scale,
            width,
            height,
            gray,
            alpha,
            half_gray,
            quarter_gray,
            sum,
            sum_squares,
            variance,
            weight,
            tiles,
            alpha_mask_used,
        }
    }

    fn template_stats(gray: &GrayImage, alpha: &GrayImage, masked: bool) -> (f64, f64, f64) {
        if masked {
            Self::weighted_gray_stats(gray, alpha)
        } else {
            gray_stats(gray)
        }
    }

    fn template_weight(alpha: &GrayImage, masked: bool) -> f64 {
        if masked {
            alpha
                .pixels()
                .map(|pixel| f64::from(pixel.0[0]) / 255.0)
                .sum()
        } else {
            f64::from(alpha.width()) * f64::from(alpha.height())
        }
    }

    fn weighted_gray_stats(gray: &GrayImage, alpha: &GrayImage) -> (f64, f64, f64) {
        let mut weight = 0.0;
        let mut sum = 0.0;
        let mut sum_squares = 0.0;
        for (gray_pixel, alpha_pixel) in gray.pixels().zip(alpha.pixels()) {
            let weight_at_pixel = f64::from(alpha_pixel.0[0]) / 255.0;
            if weight_at_pixel <= 0.0 {
                continue;
            }
            let value = f64::from(gray_pixel.0[0]);
            weight += weight_at_pixel;
            sum += weight_at_pixel * value;
            sum_squares += weight_at_pixel * value * value;
        }
        if weight <= 0.0 {
            return (0.0, 0.0, 0.0);
        }
        (
            sum,
            sum_squares,
            (sum_squares / weight - (sum / weight).powi(2)).max(0.0),
        )
    }

    fn build_tiles(gray: &GrayImage, alpha: &GrayImage, masked: bool) -> Vec<PreparedTile> {
        let mut tiles = Vec::with_capacity((ROBUST_GRID_SIZE * ROBUST_GRID_SIZE) as usize);
        for row in 0..ROBUST_GRID_SIZE {
            for column in 0..ROBUST_GRID_SIZE {
                let x = ((u64::from(gray.width()) * u64::from(column))
                    / u64::from(ROBUST_GRID_SIZE))
                .min(u64::from(u32::MAX)) as u32;
                let next_x = ((u64::from(gray.width()) * u64::from(column + 1))
                    / u64::from(ROBUST_GRID_SIZE))
                .min(u64::from(u32::MAX)) as u32;
                let y = ((u64::from(gray.height()) * u64::from(row)) / u64::from(ROBUST_GRID_SIZE))
                    .min(u64::from(u32::MAX)) as u32;
                let next_y = ((u64::from(gray.height()) * u64::from(row + 1))
                    / u64::from(ROBUST_GRID_SIZE))
                .min(u64::from(u32::MAX)) as u32;
                let width = next_x.saturating_sub(x);
                let height = next_y.saturating_sub(y);
                if width == 0 || height == 0 {
                    continue;
                }
                let mut weight = 0.0;
                let mut sum = 0.0;
                let mut sum_squares = 0.0;
                let mut gradient_energy = 0.0;
                let mut sample_count = 0_u32;
                for tile_y in y..next_y {
                    for tile_x in x..next_x {
                        let alpha_value = if masked {
                            f64::from(alpha.get_pixel(tile_x, tile_y).0[0]) / 255.0
                        } else {
                            1.0
                        };
                        if alpha_value <= 0.0 {
                            continue;
                        }
                        let value = f64::from(gray.get_pixel(tile_x, tile_y).0[0]);
                        weight += alpha_value;
                        sum += alpha_value * value;
                        sum_squares += alpha_value * value * value;
                        sample_count = sample_count.saturating_add(1);
                        if tile_x + 1 < next_x {
                            let right = f64::from(gray.get_pixel(tile_x + 1, tile_y).0[0]);
                            gradient_energy += (value - right).abs() * alpha_value;
                        }
                        if tile_y + 1 < next_y {
                            let below = f64::from(gray.get_pixel(tile_x, tile_y + 1).0[0]);
                            gradient_energy += (value - below).abs() * alpha_value;
                        }
                    }
                }
                let variance = if weight > 0.0 {
                    (sum_squares / weight - (sum / weight).powi(2)).max(0.0)
                } else {
                    0.0
                };
                let normalized_gradient = if weight > 0.0 {
                    gradient_energy / weight
                } else {
                    0.0
                };
                let valid = sample_count >= 4
                    && (variance >= ROBUST_TILE_VARIANCE_FLOOR
                        || normalized_gradient >= ROBUST_TILE_GRADIENT_FLOOR);
                tiles.push(PreparedTile {
                    row,
                    column,
                    x,
                    y,
                    width,
                    height,
                    sum,
                    sum_squares,
                    weight,
                    variance,
                    gradient_energy: normalized_gradient,
                    valid,
                });
            }
        }
        tiles
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
        diagnostics.alpha_mask_used = prepared.alpha_mask_used();
        let scales = options.scale_candidates()?;
        diagnostics.scale_candidates = scales.clone();
        let image = Self::gray(frame)?;
        let direct_only = options.mode == MatcherMode::Exact
            || prepared.variance() < LOW_VARIANCE_FLOOR
            || prepared.frame.width.min(prepared.frame.height) < COARSE_TEMPLATE_MIN_SIDE;
        if direct_only {
            // A fixed-scale safe path is required for low-variance and tiny
            // templates because normalized coarse scores are undefined. Do
            // not advertise scales that this path did not actually test.
            diagnostics.scale_candidates = vec![1.0];
            if options.mode == MatcherMode::Fast {
                // Fast mode never turns an ineligible template into an
                // expensive full-area fallback. Returning no hit is safer
                // than accepting an unbounded or undefined score.
                return Ok(MatcherResult {
                    image: None,
                    diagnostics: finish_diagnostics(diagnostics, started),
                });
            }
            let scale_started = Instant::now();
            let mut best = None;
            for scale in [1.0] {
                let scaled = prepared.scaled_template(scale)?;
                if scaled.width() > frame.width || scaled.height() > frame.height {
                    continue;
                }
                let candidate = self.full_match_scaled(
                    frame,
                    &image,
                    &scaled,
                    threshold,
                    options.mode,
                    options.mode != MatcherMode::Exact,
                )?;
                if candidate.as_ref().is_some_and(|candidate| {
                    best.as_ref().is_none_or(|current: &RefinedCandidate| {
                        candidate.image.score > current.image.score
                    })
                }) {
                    best = candidate;
                }
            }
            let direct_elapsed = scale_started.elapsed().as_millis() as u64;
            diagnostics.scale_search_ms = direct_elapsed;
            diagnostics.refine_ms = direct_elapsed;
            if options.mode == MatcherMode::Auto {
                diagnostics.fallback_ms = direct_elapsed;
                diagnostics.fallback_used = true;
            }
            diagnostics.candidate_count = usize::from(best.is_some());
            if let Some(best) = best {
                diagnostics.refined_score = best.image.score;
                if best.accepted && best.image.score >= threshold {
                    set_match_diagnostics(&mut diagnostics, &best);
                    return Ok(MatcherResult {
                        image: Some(best.image),
                        diagnostics: finish_diagnostics(diagnostics, started),
                    });
                }
            }
            return Ok(MatcherResult {
                image: None,
                diagnostics: finish_diagnostics(diagnostics, started),
            });
        }

        let coarse_started = Instant::now();
        // A quarter-resolution pass keeps the bounded seven-scale search
        // practical for a normal 1920x1080 desktop. Smaller synthetic/test
        // frames keep the older half-resolution path so the local refine
        // radius remains precise relative to the frame.
        let coarse_factor = if frame.width >= 1024
            && frame.height >= 720
            && prepared.frame.width.min(prepared.frame.height) >= 24
        {
            4
        } else {
            2
        };
        let coarse_image = resize(
            &image,
            frame.width.div_ceil(coarse_factor),
            frame.height.div_ceil(coarse_factor),
            FilterType::Triangle,
        );
        let mut coarse_best = None;
        let mut all_candidates = Vec::new();
        for (scale_index, scale) in scales.iter().enumerate() {
            let scaled = prepared.scaled_template(*scale)?;
            let Some(coarse_template) = scaled.coarse_gray(coarse_factor) else {
                continue;
            };
            if scaled.width() > frame.width
                || scaled.height() > frame.height
                || coarse_template.width() > coarse_image.width()
                || coarse_template.height() > coarse_image.height()
            {
                continue;
            }
            let coarse_scores = match_template_parallel(
                &coarse_image,
                coarse_template,
                MatchTemplateMethod::CrossCorrelationNormalized,
            );
            let (best, per_scale_candidates) = coarse_candidates_for_scale(
                &coarse_scores,
                frame,
                Arc::clone(&scaled),
                options.max_candidates,
                coarse_factor,
            );
            coarse_best = match (coarse_best, best) {
                (Some(left), Some(right)) => Some(left.max(right)),
                (None, value) | (value, None) => value,
            };
            if scale_index == 0
                && options
                    .preferred_scale
                    .is_some_and(|preferred| (preferred - *scale).abs() < 0.0005)
            {
                let preferred_candidates =
                    cross_scale_nms(per_scale_candidates.clone(), options.max_candidates);
                let preferred_refine_started = Instant::now();
                let preferred_refined = refine_candidates_with_mode(
                    &image,
                    frame,
                    &preferred_candidates,
                    threshold,
                    options.mode,
                    false,
                )?;
                diagnostics.coarse_ms = coarse_started.elapsed().as_millis() as u64;
                diagnostics.candidate_count = preferred_candidates.len();
                diagnostics.coarse_score = best.unwrap_or(0.0);
                diagnostics.robust_verify_ms = diagnostics
                    .robust_verify_ms
                    .saturating_add(preferred_refined.robust_verify_ms);
                diagnostics.robust_verify_used |= preferred_refined.robust_verify_used;
                diagnostics.refine_ms = preferred_refine_started.elapsed().as_millis() as u64;
                if preferred_refined
                    .best
                    .as_ref()
                    .is_some_and(|candidate| candidate.accepted)
                {
                    let candidate = preferred_refined.best.expect("preferred match");
                    diagnostics.preferred_scale_hit = true;
                    diagnostics.refined_score = candidate.image.score;
                    set_match_diagnostics(&mut diagnostics, &candidate);
                    diagnostics.scale_search_ms = started.elapsed().as_millis() as u64;
                    return Ok(MatcherResult {
                        image: Some(candidate.image),
                        diagnostics: finish_diagnostics(diagnostics, started),
                    });
                }
            }
            all_candidates.extend(per_scale_candidates);
        }
        diagnostics.coarse_ms = coarse_started.elapsed().as_millis() as u64;
        let Some(coarse_best) = coarse_best else {
            diagnostics.scale_search_ms = started.elapsed().as_millis() as u64;
            return Ok(MatcherResult {
                image: None,
                diagnostics: finish_diagnostics(diagnostics, started),
            });
        };
        // `max_candidates` is a hard total budget. The NMS stage first keeps
        // one best coarse hypothesis for each scale while that budget allows,
        // then fills remaining slots only with non-overlapping hypotheses.
        let anchor_candidates_source = all_candidates.clone();
        let candidates = cross_scale_nms(all_candidates, options.max_candidates);
        diagnostics.candidate_count = candidates.len();
        diagnostics.coarse_score = coarse_best;
        let coarse_gate = (threshold - COARSE_GATE_MARGIN).max(COARSE_MIN_CONFIDENCE);
        if candidates.is_empty() || (coarse_best < coarse_gate && options.mode == MatcherMode::Fast)
        {
            diagnostics.scale_search_ms = started.elapsed().as_millis() as u64;
            return Ok(MatcherResult {
                image: None,
                diagnostics: finish_diagnostics(diagnostics, started),
            });
        }

        let refine_started = Instant::now();
        let mut refined = refine_candidates_with_mode(
            &image,
            frame,
            &candidates,
            threshold,
            options.mode,
            false,
        )?;
        diagnostics.refine_ms = refine_started.elapsed().as_millis() as u64;
        diagnostics.robust_verify_ms = diagnostics
            .robust_verify_ms
            .saturating_add(refined.robust_verify_ms);
        diagnostics.robust_verify_used |= refined.robust_verify_used;
        diagnostics.scale_search_ms = started.elapsed().as_millis() as u64;
        if let Some(best) = refined.best.as_ref() {
            diagnostics.refined_score = best.image.score;
            if best.accepted && best.image.score >= threshold {
                set_match_diagnostics(&mut diagnostics, best);
                return Ok(MatcherResult {
                    image: Some(best.image.clone()),
                    diagnostics: finish_diagnostics(diagnostics, started),
                });
            }
        }

        let needs_anchor_recovery = options.mode == MatcherMode::Auto
            && (coarse_best < coarse_gate
                || refined
                    .best
                    .as_ref()
                    .is_some_and(|best| !best.accepted && best.robust_verify_used));
        if needs_anchor_recovery {
            let anchor_started = Instant::now();
            let (anchor_candidates, anchor_count) = anchor_recovery_candidates(
                &coarse_image,
                frame,
                &anchor_candidates_source,
                AnchorRecoveryOptions {
                    coarse_factor,
                    max_candidates: options.max_candidates,
                    preferred_scale: options.preferred_scale,
                },
            )?;
            diagnostics.anchor_recovery_used = anchor_count > 0;
            diagnostics.anchor_candidate_count = anchor_count;
            diagnostics.coarse_ms = diagnostics
                .coarse_ms
                .saturating_add(anchor_started.elapsed().as_millis() as u64);
            if !anchor_candidates.is_empty() {
                let recovered_candidates =
                    anchor_recovery_nms(anchor_candidates, options.max_candidates);
                diagnostics.candidate_count =
                    diagnostics.candidate_count.max(recovered_candidates.len());
                let recovered_started = Instant::now();
                let recovered = refine_candidates_with_mode(
                    &image,
                    frame,
                    &recovered_candidates,
                    threshold,
                    options.mode,
                    true,
                )?;
                diagnostics.refine_ms = diagnostics
                    .refine_ms
                    .saturating_add(recovered_started.elapsed().as_millis() as u64);
                diagnostics.robust_verify_ms = diagnostics
                    .robust_verify_ms
                    .saturating_add(recovered.robust_verify_ms);
                diagnostics.robust_verify_used |= recovered.robust_verify_used;
                if recovered.best.as_ref().is_some_and(|best| best.accepted) {
                    refined = recovered;
                    let best = refined.best.as_ref().expect("accepted recovery");
                    diagnostics.refined_score = best.image.score;
                    set_match_diagnostics(&mut diagnostics, best);
                    return Ok(MatcherResult {
                        image: Some(best.image.clone()),
                        diagnostics: finish_diagnostics(diagnostics, started),
                    });
                }
                if recovered.best.as_ref().is_some_and(|best| {
                    refined
                        .best
                        .as_ref()
                        .is_none_or(|current| best.image.score > current.image.score)
                }) {
                    refined = recovered;
                }
            }
        }

        let coarse_uncertainty = (threshold * 0.8).max(COARSE_UNCERTAINTY_CONFIDENCE);
        let refine_uncertain = refined.best.as_ref().is_none_or(|best| {
            best.image.score >= (threshold - REFINE_UNCERTAINTY_MARGIN).max(0.0)
        });
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
        let mut fallback_best = None;
        let mut fallback_scales = Vec::new();
        for candidate in candidates.iter().take(2) {
            if fallback_scales
                .iter()
                .any(|scale: &f32| (*scale - candidate.scale()).abs() < 0.0005)
            {
                continue;
            }
            fallback_scales.push(candidate.scale());
            let full = self.full_match_scaled(
                frame,
                &image,
                candidate.template(),
                threshold,
                options.mode,
                true,
            )?;
            if full.as_ref().is_some_and(|full| {
                fallback_best
                    .as_ref()
                    .is_none_or(|current: &RefinedCandidate| full.image.score > current.image.score)
            }) {
                fallback_best = full;
            }
        }
        diagnostics.fallback_ms = fallback_started.elapsed().as_millis() as u64;
        diagnostics.scale_search_ms = diagnostics
            .scale_search_ms
            .saturating_add(diagnostics.fallback_ms);
        diagnostics.fallback_used = true;
        if let Some(best) = fallback_best {
            if best.accepted && best.image.score >= threshold {
                diagnostics.refined_score = diagnostics.refined_score.max(best.image.score);
                set_match_diagnostics(&mut diagnostics, &best);
                return Ok(MatcherResult {
                    image: Some(best.image),
                    diagnostics: finish_diagnostics(diagnostics, started),
                });
            }
        }
        Ok(MatcherResult {
            image: None,
            diagnostics: finish_diagnostics(diagnostics, started),
        })
    }

    fn full_match_scaled(
        &self,
        frame: &CaptureFrame,
        image: &GrayImage,
        template: &PreparedScale,
        threshold: f32,
        mode: MatcherMode,
        verify_zero_mean: bool,
    ) -> Result<Option<RefinedCandidate>, VisionError> {
        let scores = match_template_parallel(
            image,
            template.gray(),
            MatchTemplateMethod::CrossCorrelationNormalized,
        );
        let Some(best) =
            best_match_from_scores(&scores, frame, template.width(), template.height())?
        else {
            return Ok(None);
        };
        let local_x = u32::try_from(i64::from(best.x) - i64::from(frame.origin.x))
            .map_err(|_| VisionError::new("vision_match_failed", "匹配坐标超出帧范围"))?;
        let local_y = u32::try_from(i64::from(best.y) - i64::from(frame.origin.y))
            .map_err(|_| VisionError::new("vision_match_failed", "匹配坐标超出帧范围"))?;
        let evaluation = if verify_zero_mean {
            evaluate_candidate_at(image, template, local_x, local_y, threshold, mode)
        } else {
            let score = if template.alpha_mask_used() {
                weighted_ncc_at(image, template, local_x, local_y).unwrap_or(best.score)
            } else {
                best.score
            };
            Some(CandidateEvaluation {
                full_score: score,
                combined_score: score,
                robust_score: None,
                valid_tile_count: 0,
                discarded_tile_count: 0,
                robust_verify_used: false,
                accepted: score >= threshold,
            })
        };
        let Some(evaluation) = evaluation else {
            return Ok(None);
        };
        Ok(Some(RefinedCandidate {
            image: ImageMatch {
                score: evaluation.combined_score,
                ..best
            },
            scale: template.scale(),
            full_score: evaluation.full_score,
            robust_score: evaluation.robust_score,
            valid_tile_count: evaluation.valid_tile_count,
            discarded_tile_count: evaluation.discarded_tile_count,
            robust_verify_used: evaluation.robust_verify_used,
            alpha_mask_used: template.alpha_mask_used(),
            accepted: evaluation.accepted,
        }))
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

#[derive(Debug, Clone)]
struct CoarseCandidate {
    x: u32,
    y: u32,
    score: f32,
    template: Arc<PreparedScale>,
}

impl CoarseCandidate {
    fn scale(&self) -> f32 {
        self.template.scale()
    }

    fn template(&self) -> &PreparedScale {
        &self.template
    }
}

#[derive(Debug, Clone)]
struct RefinedCandidate {
    image: ImageMatch,
    scale: f32,
    full_score: f32,
    robust_score: Option<f32>,
    valid_tile_count: usize,
    discarded_tile_count: usize,
    robust_verify_used: bool,
    alpha_mask_used: bool,
    accepted: bool,
}

#[derive(Debug, Clone, Copy)]
struct RobustVerification {
    score: f32,
    pass_ratio: f32,
    valid_tile_count: usize,
    discarded_tile_count: usize,
    accepted: bool,
}

#[derive(Debug)]
struct RefineResult {
    best: Option<RefinedCandidate>,
    robust_verify_ms: u64,
    robust_verify_used: bool,
}

fn coarse_candidates_for_scale(
    scores: &image::ImageBuffer<Luma<f32>, Vec<f32>>,
    frame: &CaptureFrame,
    template: Arc<PreparedScale>,
    max_candidates: usize,
    coarse_factor: u32,
) -> (Option<f32>, Vec<CoarseCandidate>) {
    let template_width = template.width();
    let template_height = template.height();
    let max_x = frame.width.saturating_sub(template_width);
    let max_y = frame.height.saturating_sub(template_height);
    let mut ranked = scores
        .enumerate_pixels()
        .filter_map(|(x, y, pixel)| {
            let score = pixel.0[0];
            if !score.is_finite() {
                return None;
            }
            let x = x.saturating_mul(coarse_factor).min(max_x);
            let y = y.saturating_mul(coarse_factor).min(max_y);
            let region = ScreenRect::from_parts(
                frame.origin.x.saturating_add(i32::try_from(x).ok()?),
                frame.origin.y.saturating_add(i32::try_from(y).ok()?),
                template_width,
                template_height,
            );
            frame
                .is_region_fully_valid(region)
                .then_some(CoarseCandidate {
                    x,
                    y,
                    score,
                    template: Arc::clone(&template),
                })
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

fn cross_scale_nms(
    mut candidates: Vec<CoarseCandidate>,
    max_candidates: usize,
) -> Vec<CoarseCandidate> {
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.y.cmp(&right.y))
            .then_with(|| left.x.cmp(&right.x))
            .then_with(|| left.scale().total_cmp(&right.scale()))
    });
    let mut selected = Vec::with_capacity(max_candidates);
    let mut selected_scales = Vec::<u32>::new();
    let distinct_scale_count = candidates
        .iter()
        .map(|candidate| candidate.scale().to_bits())
        .collect::<std::collections::HashSet<_>>()
        .len();
    let unique_scale_budget = if distinct_scale_count > max_candidates {
        max_candidates.saturating_sub(1).max(1)
    } else {
        max_candidates
    };
    for candidate in &candidates {
        if selected_scales.len() >= unique_scale_budget {
            break;
        }
        let scale_key = candidate.scale().to_bits();
        let already_selected = selected_scales.contains(&scale_key);
        if !already_selected {
            selected_scales.push(scale_key);
            selected.push(candidate.clone());
        }
    }
    if selected.len() < max_candidates {
        for candidate in candidates {
            if selected.len() >= max_candidates {
                break;
            }
            if selected
                .iter()
                .all(|kept: &CoarseCandidate| !cross_scale_overlap(&candidate, kept))
            {
                selected.push(candidate);
            }
        }
    }
    selected.sort_by(|left, right| right.score.total_cmp(&left.score));
    selected
}

fn anchor_recovery_nms(
    mut candidates: Vec<CoarseCandidate>,
    max_candidates: usize,
) -> Vec<CoarseCandidate> {
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.y.cmp(&right.y))
            .then_with(|| left.x.cmp(&right.x))
            .then_with(|| left.scale().total_cmp(&right.scale()))
    });
    let mut selected = Vec::with_capacity(max_candidates);
    let mut selected_scales = Vec::<u32>::new();
    for candidate in &candidates {
        if selected.len() >= max_candidates {
            break;
        }
        if selected_scales.contains(&candidate.scale().to_bits()) {
            continue;
        }
        selected_scales.push(candidate.scale().to_bits());
        selected.push(candidate.clone());
    }
    if selected.len() < max_candidates {
        for candidate in candidates {
            if selected.len() >= max_candidates {
                break;
            }
            if selected
                .iter()
                .all(|kept: &CoarseCandidate| !cross_scale_overlap(&candidate, kept))
            {
                selected.push(candidate);
            }
        }
    }
    selected
}

fn cross_scale_overlap(left: &CoarseCandidate, right: &CoarseCandidate) -> bool {
    let left_width = left.template.width();
    let left_height = left.template.height();
    let right_width = right.template.width();
    let right_height = right.template.height();
    let left_right = u64::from(left.x) + u64::from(left_width);
    let left_bottom = u64::from(left.y) + u64::from(left_height);
    let right_right = u64::from(right.x) + u64::from(right_width);
    let right_bottom = u64::from(right.y) + u64::from(right_height);
    let overlap_width = left_right
        .min(right_right)
        .saturating_sub(u64::from(left.x).max(u64::from(right.x)));
    let overlap_height = left_bottom
        .min(right_bottom)
        .saturating_sub(u64::from(left.y).max(u64::from(right.y)));
    let overlap = overlap_width.saturating_mul(overlap_height);
    let smaller_area = u64::from(left_width.min(right_width))
        .saturating_mul(u64::from(left_height.min(right_height)));
    if smaller_area > 0 && overlap.saturating_mul(2) >= smaller_area {
        return true;
    }
    let left_center_x = u64::from(left.x) * 2 + u64::from(left_width);
    let left_center_y = u64::from(left.y) * 2 + u64::from(left_height);
    let right_center_x = u64::from(right.x) * 2 + u64::from(right_width);
    let right_center_y = u64::from(right.y) * 2 + u64::from(right_height);
    let dx = left_center_x.abs_diff(right_center_x);
    let dy = left_center_y.abs_diff(right_center_y);
    let max_distance = u64::from(
        left_width
            .max(left_height)
            .max(right_width)
            .max(right_height),
    );
    let scale_ratio = left.scale().max(right.scale()) / left.scale().min(right.scale());
    scale_ratio <= 1.6
        && dx.saturating_mul(dx) + dy.saturating_mul(dy)
            <= max_distance.saturating_mul(max_distance)
}

#[derive(Debug, Clone, Copy)]
struct AnchorRecoveryOptions {
    coarse_factor: u32,
    max_candidates: usize,
    preferred_scale: Option<f32>,
}

fn anchor_recovery_candidates(
    coarse_image: &GrayImage,
    frame: &CaptureFrame,
    existing: &[CoarseCandidate],
    options: AnchorRecoveryOptions,
) -> Result<(Vec<CoarseCandidate>, usize), VisionError> {
    let mut likely_scales = Vec::<CoarseCandidate>::new();
    let mut seen_scales = Vec::<u32>::new();
    let mut ranked_existing = existing.to_vec();
    ranked_existing.sort_by(|left, right| right.score.total_cmp(&left.score));
    if let Some(preferred_scale) = options.preferred_scale {
        if let Some(candidate) = ranked_existing
            .iter()
            .find(|candidate| (candidate.scale() - preferred_scale).abs() < 0.0005)
        {
            seen_scales.push(candidate.scale().to_bits());
            likely_scales.push(candidate.clone());
        }
    }
    let high_score_budget = MAX_ANCHOR_SCALES.saturating_sub(2).max(1);
    for candidate in &ranked_existing {
        if seen_scales.contains(&candidate.scale().to_bits()) {
            continue;
        }
        seen_scales.push(candidate.scale().to_bits());
        likely_scales.push(candidate.clone());
        if likely_scales.len() >= high_score_budget {
            break;
        }
    }
    let mut high_scale = ranked_existing.clone();
    high_scale.sort_by(|left, right| {
        right
            .scale()
            .total_cmp(&left.scale())
            .then_with(|| right.score.total_cmp(&left.score))
    });
    for candidate in high_scale {
        if seen_scales.contains(&candidate.scale().to_bits()) {
            continue;
        }
        seen_scales.push(candidate.scale().to_bits());
        likely_scales.push(candidate);
        if likely_scales.len() >= MAX_ANCHOR_SCALES {
            break;
        }
    }
    if likely_scales.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let recovery_limit = MAX_ANCHOR_CANDIDATES.min(options.max_candidates.saturating_mul(2).max(1));
    let mut by_scale = Vec::<Vec<CoarseCandidate>>::new();
    for likely in likely_scales {
        let template = likely.template();
        let Some(coarse_template) = template.coarse_gray(options.coarse_factor) else {
            continue;
        };
        let mut anchors = template
            .tiles()
            .iter()
            .filter(|tile| tile.valid)
            .collect::<Vec<_>>();
        anchors.sort_by(|left, right| {
            let left_information = left.variance + left.gradient_energy;
            let right_information = right.variance + right.gradient_energy;
            let left_stability = 1.0
                + f64::from(ROBUST_GRID_SIZE - 1 - left.row) * 0.25
                + f64::from(left.column) * 0.08
                - if left.row == ROBUST_GRID_SIZE - 1 && left.column == 0 {
                    0.28
                } else {
                    0.0
                };
            let right_stability = 1.0
                + f64::from(ROBUST_GRID_SIZE - 1 - right.row) * 0.25
                + f64::from(right.column) * 0.08
                - if right.row == ROBUST_GRID_SIZE - 1 && right.column == 0 {
                    0.28
                } else {
                    0.0
                };
            (right_information * right_stability).total_cmp(&(left_information * left_stability))
        });
        let mut selected_anchors = Vec::new();
        for anchor in anchors {
            if selected_anchors.iter().all(|selected: &&PreparedTile| {
                selected.row.abs_diff(anchor.row) + selected.column.abs_diff(anchor.column) >= 2
            }) {
                selected_anchors.push(anchor);
            }
            if selected_anchors.len() >= MAX_ANCHOR_TILES {
                break;
            }
        }
        let mut scale_recovered = Vec::new();
        for anchor in selected_anchors {
            let anchor_x = anchor.x / options.coarse_factor;
            let anchor_y = anchor.y / options.coarse_factor;
            let anchor_width = (anchor.width / options.coarse_factor).max(1);
            let anchor_height = (anchor.height / options.coarse_factor).max(1);
            if anchor_x.saturating_add(anchor_width) > coarse_template.width()
                || anchor_y.saturating_add(anchor_height) > coarse_template.height()
            {
                continue;
            }
            let anchor_template = crop_imm(
                coarse_template,
                anchor_x,
                anchor_y,
                anchor_width,
                anchor_height,
            )
            .to_image();
            let anchor_scores = match_template_parallel(
                coarse_image,
                &anchor_template,
                MatchTemplateMethod::CrossCorrelationNormalized,
            );
            let max_x = frame.width.saturating_sub(template.width());
            let max_y = frame.height.saturating_sub(template.height());
            let mut ranked = anchor_scores
                .enumerate_pixels()
                .filter_map(|(x, y, pixel)| {
                    let score = pixel.0[0];
                    if !score.is_finite() {
                        return None;
                    }
                    let full_anchor_x = x.saturating_mul(options.coarse_factor);
                    let full_anchor_y = y.saturating_mul(options.coarse_factor);
                    let full_x = full_anchor_x.saturating_sub(anchor.x).min(max_x);
                    let full_y = full_anchor_y.saturating_sub(anchor.y).min(max_y);
                    let region = ScreenRect::from_parts(
                        frame.origin.x.checked_add(i32::try_from(full_x).ok()?)?,
                        frame.origin.y.checked_add(i32::try_from(full_y).ok()?)?,
                        template.width(),
                        template.height(),
                    );
                    frame
                        .is_region_fully_valid(region)
                        .then_some(CoarseCandidate {
                            x: full_x,
                            y: full_y,
                            score,
                            template: Arc::clone(&likely.template),
                        })
                })
                .collect::<Vec<_>>();
            ranked.sort_by(|left, right| right.score.total_cmp(&left.score));
            for candidate in ranked {
                if scale_recovered
                    .iter()
                    .all(|kept: &CoarseCandidate| !cross_scale_overlap(&candidate, kept))
                {
                    scale_recovered.push(candidate);
                    if scale_recovered.len() >= MAX_ANCHOR_POSITIONS_PER_TILE * MAX_ANCHOR_TILES {
                        break;
                    }
                }
            }
        }
        if !scale_recovered.is_empty() {
            by_scale.push(scale_recovered);
        }
    }
    let mut recovered = Vec::with_capacity(recovery_limit);
    for position in 0..MAX_ANCHOR_POSITIONS_PER_TILE * MAX_ANCHOR_TILES {
        for scale_candidates in &by_scale {
            if recovered.len() >= recovery_limit {
                break;
            }
            let Some(candidate) = scale_candidates.get(position) else {
                continue;
            };
            if position == 0
                || recovered
                    .iter()
                    .all(|kept: &CoarseCandidate| !cross_scale_overlap(candidate, kept))
            {
                recovered.push(candidate.clone());
            }
        }
        if recovered.len() >= recovery_limit {
            break;
        }
    }
    let generated = recovered.len();
    recovered.sort_by(|left, right| right.score.total_cmp(&left.score));
    recovered.truncate(MAX_ANCHOR_CANDIDATES);
    Ok((recovered, generated.min(MAX_ANCHOR_CANDIDATES)))
}

fn refine_candidates_with_mode(
    image: &GrayImage,
    frame: &CaptureFrame,
    candidates: &[CoarseCandidate],
    threshold: f32,
    mode: MatcherMode,
    anchor_neighborhood: bool,
) -> Result<RefineResult, VisionError> {
    let mut best = None;
    let robust_started = Instant::now();
    let mut robust_verify_used = false;
    for candidate in candidates {
        let template = candidate.template();
        let template_width = template.width();
        let template_height = template.height();
        let max_x = image.width().saturating_sub(template_width);
        let max_y = image.height().saturating_sub(template_height);
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
            template.gray(),
            MatchTemplateMethod::CrossCorrelationNormalized,
        );
        let mut local_positions = scores
            .enumerate_pixels()
            .filter_map(|(x, y, pixel)| {
                let absolute_x = left.saturating_add(x);
                let absolute_y = top.saturating_add(y);
                let score = pixel.0[0];
                if !score.is_finite() {
                    return None;
                }
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
            .collect::<Vec<_>>();
        local_positions.sort_by(|left, right| right.2.total_cmp(&left.2));
        local_positions.truncate(ROBUST_MAX_LOCAL_POSITIONS);
        if !local_positions
            .iter()
            .any(|(x, y, _)| *x == center_x && *y == center_y)
        {
            // Anchor recovery supplies a position inferred from a stable tile.
            // Keep that position for the final full/robust verification even
            // when the damaged full template ranks it below local distractors.
            local_positions.push((center_x, center_y, candidate.score));
        }
        if anchor_neighborhood {
            for offset_y in -2_i32..=2 {
                for offset_x in -2_i32..=2 {
                    let x = i64::from(center_x) + i64::from(offset_x);
                    let y = i64::from(center_y) + i64::from(offset_y);
                    if x < 0 || y < 0 || x > i64::from(max_x) || y > i64::from(max_y) {
                        continue;
                    }
                    let position = (x as u32, y as u32, candidate.score);
                    if !local_positions.iter().any(|(local_x, local_y, _)| {
                        *local_x == position.0 && *local_y == position.1
                    }) {
                        local_positions.push(position);
                    }
                }
            }
        }
        for (x, y, _coarse_score) in local_positions {
            let Some(evaluation) = evaluate_candidate_at(image, template, x, y, threshold, mode)
            else {
                continue;
            };
            robust_verify_used |= evaluation.robust_verify_used;
            let refined = RefinedCandidate {
                image: make_image_match(
                    frame,
                    template_width,
                    template_height,
                    x,
                    y,
                    evaluation.combined_score,
                )?,
                scale: candidate.scale(),
                full_score: evaluation.full_score,
                robust_score: evaluation.robust_score,
                valid_tile_count: evaluation.valid_tile_count,
                discarded_tile_count: evaluation.discarded_tile_count,
                robust_verify_used: evaluation.robust_verify_used,
                alpha_mask_used: template.alpha_mask_used(),
                accepted: evaluation.accepted,
            };
            let should_replace = best.as_ref().is_none_or(|current: &RefinedCandidate| {
                (refined.accepted && !current.accepted)
                    || (refined.accepted == current.accepted
                        && if refined.accepted {
                            refined.full_score > current.full_score
                        } else {
                            refined.image.score > current.image.score
                        })
            });
            if should_replace {
                best = Some(refined);
            }
        }
    }
    Ok(RefineResult {
        best,
        robust_verify_ms: if robust_verify_used {
            robust_started.elapsed().as_millis() as u64
        } else {
            0
        },
        robust_verify_used,
    })
}

#[derive(Debug, Clone, Copy)]
struct CandidateEvaluation {
    full_score: f32,
    combined_score: f32,
    robust_score: Option<f32>,
    valid_tile_count: usize,
    discarded_tile_count: usize,
    robust_verify_used: bool,
    accepted: bool,
}

fn evaluate_candidate_at(
    image: &GrayImage,
    template: &PreparedScale,
    x: u32,
    y: u32,
    threshold: f32,
    mode: MatcherMode,
) -> Option<CandidateEvaluation> {
    let full_score = if template.alpha_mask_used() {
        weighted_ncc_at(image, template, x, y)
    } else {
        normalized_ncc_at(
            image,
            template.gray(),
            template.sum(),
            template.sum_squares(),
            x,
            y,
        )
    }?;
    if !full_score.is_finite() {
        return None;
    }
    if mode == MatcherMode::Exact {
        return Some(CandidateEvaluation {
            full_score,
            combined_score: full_score,
            robust_score: None,
            valid_tile_count: 0,
            discarded_tile_count: 0,
            robust_verify_used: false,
            accepted: full_score >= threshold,
        });
    }
    let valid_tile_count = template.tiles().iter().filter(|tile| tile.valid).count();
    if valid_tile_count < ROBUST_MIN_VALID_TILES {
        return Some(CandidateEvaluation {
            full_score,
            combined_score: full_score,
            robust_score: None,
            valid_tile_count,
            discarded_tile_count: 0,
            robust_verify_used: false,
            accepted: full_score >= threshold,
        });
    }
    let robust = robust_verify_at(image, template, x, y, threshold);
    let combined_score = (full_score * (1.0 - ROBUST_COMBINED_WEIGHT - ROBUST_PASS_COUNT_WEIGHT)
        + robust.score * ROBUST_COMBINED_WEIGHT
        + robust.pass_ratio * ROBUST_PASS_COUNT_WEIGHT)
        .clamp(-1.0, 1.0);
    Some(CandidateEvaluation {
        full_score,
        combined_score,
        robust_score: Some(robust.score),
        valid_tile_count: robust.valid_tile_count,
        discarded_tile_count: robust.discarded_tile_count,
        robust_verify_used: true,
        accepted: robust.accepted && combined_score >= threshold,
    })
}

fn robust_verify_at(
    image: &GrayImage,
    template: &PreparedScale,
    x: u32,
    y: u32,
    threshold: f32,
) -> RobustVerification {
    let valid_tiles = template
        .tiles()
        .iter()
        .filter(|tile| tile.valid)
        .collect::<Vec<_>>();
    let valid_tile_count = valid_tiles.len();
    let tile_gate = (threshold - ROBUST_TILE_GATE_MARGIN).max(0.45);
    let mut scores = Vec::with_capacity(valid_tile_count);
    let mut passed_positions = Vec::new();
    for tile in valid_tiles {
        let score = weighted_tile_ncc_at(image, template, tile, x, y).unwrap_or(-1.0);
        if score >= tile_gate {
            passed_positions.push((tile.row, tile.column));
        }
        scores.push(score);
    }
    scores.sort_by(|left, right| right.total_cmp(left));
    let discarded_tile_count = scores
        .len()
        .saturating_div(5)
        .min(ROBUST_MAX_DISCARDED_TILES);
    let kept_end = scores.len().saturating_sub(discarded_tile_count);
    let kept = scores
        .get(..kept_end)
        .filter(|scores| !scores.is_empty())
        .unwrap_or(&[]);
    let score = if kept.is_empty() {
        0.0
    } else {
        kept[kept.len() / 2]
    };
    let unique_rows = passed_positions
        .iter()
        .map(|(row, _)| *row)
        .collect::<std::collections::HashSet<_>>()
        .len();
    let unique_columns = passed_positions
        .iter()
        .map(|(_, column)| *column)
        .collect::<std::collections::HashSet<_>>()
        .len();
    let passed_tile_count = passed_positions.len();
    let pass_ratio = if valid_tile_count == 0 {
        0.0
    } else {
        passed_tile_count as f32 / valid_tile_count as f32
    };
    let spatially_separated =
        passed_tile_count >= ROBUST_MIN_PASS_TILES && unique_rows >= 2 && unique_columns >= 2;
    RobustVerification {
        score: score.clamp(-1.0, 1.0),
        pass_ratio,
        valid_tile_count,
        discarded_tile_count,
        accepted: passed_tile_count >= ROBUST_MIN_PASS_TILES
            && spatially_separated
            && score >= (threshold - ROBUST_TILE_GATE_MARGIN).max(0.0),
    }
}

fn set_match_diagnostics(diagnostics: &mut VisionDiagnostics, match_result: &RefinedCandidate) {
    diagnostics.matched_scale = Some(match_result.scale);
    diagnostics.matched_width = match_result.image.width;
    diagnostics.matched_height = match_result.image.height;
    diagnostics.robust_verify_used = match_result.robust_verify_used;
    diagnostics.robust_score = match_result.robust_score;
    diagnostics.valid_tile_count = match_result.valid_tile_count;
    diagnostics.discarded_tile_count = match_result.discarded_tile_count;
    diagnostics.alpha_mask_used = match_result.alpha_mask_used;
}

fn scaled_dimension(dimension: u32, scale: f32) -> Result<u32, VisionError> {
    validate_scale(scale)?;
    let scaled = (f64::from(dimension) * f64::from(scale)).round();
    if !scaled.is_finite() || scaled < 1.0 || scaled > f64::from(u32::MAX) {
        return Err(VisionError::new(
            "vision_scale_overflow",
            "缩放后的模板尺寸溢出或小于 1 像素",
        ));
    }
    Ok(scaled as u32)
}

fn validate_scale(scale: f32) -> Result<(), VisionError> {
    if !scale.is_finite() || !(MIN_MATCH_SCALE..=MAX_MATCH_SCALE).contains(&scale) {
        return Err(VisionError::new(
            "vision_scale_invalid",
            "scale 必须在 0.5 到 2.0 倍之间",
        ));
    }
    Ok(())
}

fn gray_stats(image: &GrayImage) -> (f64, f64, f64) {
    let count = f64::from(image.width()) * f64::from(image.height());
    if count == 0.0 {
        return (0.0, 0.0, 0.0);
    }
    let sum = image
        .pixels()
        .map(|pixel| f64::from(pixel.0[0]))
        .sum::<f64>();
    let sum_squares = image
        .pixels()
        .map(|pixel| {
            let value = f64::from(pixel.0[0]);
            value * value
        })
        .sum::<f64>();
    (
        sum,
        sum_squares,
        (sum_squares / count - (sum / count).powi(2)).max(0.0),
    )
}

fn normalized_ncc_at(
    image: &GrayImage,
    template: &GrayImage,
    template_sum: f64,
    template_sum_squares: f64,
    x: u32,
    y: u32,
) -> Option<f32> {
    let width = template.width();
    let height = template.height();
    if width == 0
        || height == 0
        || x.checked_add(width)? > image.width()
        || y.checked_add(height)? > image.height()
    {
        return None;
    }
    let count = f64::from(width) * f64::from(height);
    let template_variance = template_sum_squares - template_sum * template_sum / count;
    if !template_variance.is_finite() || template_variance <= 1.0 {
        return None;
    }
    let mut image_sum = 0.0;
    let mut image_sum_squares = 0.0;
    let mut cross = 0.0;
    for template_y in 0..height {
        for template_x in 0..width {
            let template_value = f64::from(template.get_pixel(template_x, template_y).0[0]);
            let image_value = f64::from(image.get_pixel(x + template_x, y + template_y).0[0]);
            image_sum += image_value;
            image_sum_squares += image_value * image_value;
            cross += template_value * image_value;
        }
    }
    let image_variance = image_sum_squares - image_sum * image_sum / count;
    if !image_variance.is_finite() || image_variance <= 1.0 {
        return None;
    }
    let numerator = cross - template_sum * image_sum / count;
    let denominator = (template_variance * image_variance).sqrt();
    if !denominator.is_finite() || denominator <= f64::EPSILON {
        return None;
    }
    Some((numerator / denominator).clamp(-1.0, 1.0) as f32)
}

fn weighted_ncc_at(image: &GrayImage, template: &PreparedScale, x: u32, y: u32) -> Option<f32> {
    weighted_ncc_region_at(
        image,
        template.gray(),
        template.alpha(),
        WeightedRegion {
            template_x: 0,
            template_y: 0,
            width: template.width(),
            height: template.height(),
            sum: template.sum(),
            sum_squares: template.sum_squares(),
            weight: template.weight,
        },
        x,
        y,
    )
}

fn weighted_tile_ncc_at(
    image: &GrayImage,
    template: &PreparedScale,
    tile: &PreparedTile,
    x: u32,
    y: u32,
) -> Option<f32> {
    weighted_ncc_region_at(
        image,
        template.gray(),
        template.alpha(),
        WeightedRegion {
            template_x: tile.x,
            template_y: tile.y,
            width: tile.width,
            height: tile.height,
            sum: tile.sum,
            sum_squares: tile.sum_squares,
            weight: tile.weight,
        },
        x.saturating_add(tile.x),
        y.saturating_add(tile.y),
    )
}

#[derive(Debug, Clone, Copy)]
struct WeightedRegion {
    template_x: u32,
    template_y: u32,
    width: u32,
    height: u32,
    sum: f64,
    sum_squares: f64,
    weight: f64,
}

fn weighted_ncc_region_at(
    image: &GrayImage,
    template: &GrayImage,
    alpha: &GrayImage,
    region: WeightedRegion,
    image_x: u32,
    image_y: u32,
) -> Option<f32> {
    if region.width == 0
        || region.height == 0
        || region.template_x.checked_add(region.width)? > template.width()
        || region.template_y.checked_add(region.height)? > template.height()
        || image_x.checked_add(region.width)? > image.width()
        || image_y.checked_add(region.height)? > image.height()
        || !region.weight.is_finite()
        || region.weight <= 1.0
    {
        return None;
    }
    let template_variance = region.sum_squares - region.sum * region.sum / region.weight;
    if !template_variance.is_finite() || template_variance <= 1.0 {
        return None;
    }
    let mut image_sum = 0.0;
    let mut image_sum_squares = 0.0;
    let mut cross = 0.0;
    for offset_y in 0..region.height {
        for offset_x in 0..region.width {
            let template_x_at = region.template_x + offset_x;
            let template_y_at = region.template_y + offset_y;
            let pixel_weight =
                f64::from(alpha.get_pixel(template_x_at, template_y_at).0[0]) / 255.0;
            if pixel_weight <= 0.0 {
                continue;
            }
            let template_value = f64::from(template.get_pixel(template_x_at, template_y_at).0[0]);
            let image_value =
                f64::from(image.get_pixel(image_x + offset_x, image_y + offset_y).0[0]);
            image_sum += pixel_weight * image_value;
            image_sum_squares += pixel_weight * image_value * image_value;
            cross += pixel_weight * template_value * image_value;
        }
    }
    let image_variance = image_sum_squares - image_sum * image_sum / region.weight;
    if !image_variance.is_finite() || image_variance <= 1.0 {
        return None;
    }
    let numerator = cross - region.sum * image_sum / region.weight;
    let denominator = (template_variance * image_variance).sqrt();
    if !denominator.is_finite() || denominator <= f64::EPSILON {
        return None;
    }
    Some((numerator / denominator).clamp(-1.0, 1.0) as f32)
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
    diagnostics.single_match_ms = diagnostics.total_ms;
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

    fn scaled_patterned_frame(
        origin: Point,
        width: u32,
        height: u32,
        template: &CaptureFrame,
        target: (u32, u32),
        scale: f32,
        seed: u32,
    ) -> CaptureFrame {
        let base = patterned_frame(
            origin,
            width,
            height,
            &[template.width as u8, template.height as u8],
            &vec![[0, 0, 0, 255]; (template.width * template.height) as usize],
            None,
            seed,
        );
        let gray = ImageProcVisionMatcher::gray(template).expect("template gray");
        let scaled_width = (f64::from(template.width) * f64::from(scale)).round() as u32;
        let scaled_height = (f64::from(template.height) * f64::from(scale)).round() as u32;
        let scaled = resize(&gray, scaled_width, scaled_height, FilterType::Triangle);
        let mut pixels = base.pixels_bgra().to_vec();
        for y in 0..scaled_height {
            for x in 0..scaled_width {
                let value = scaled.get_pixel(x, y).0[0];
                let index = (((target.1 + y) * width + target.0 + x) * 4) as usize;
                pixels[index..index + 4].copy_from_slice(&[value, value, value, 255]);
            }
        }
        CaptureFrame::from_bgra(origin, width, height, pixels).expect("scaled patterned frame")
    }

    fn changed_bottom_left_frame(
        mut frame: CaptureFrame,
        template: &CaptureFrame,
        target: (u32, u32),
        scale: f32,
    ) -> CaptureFrame {
        let (width, height) = scaled_dimensions_for_template(template, scale);
        let mut pixels = frame.pixels_bgra().to_vec();
        let changed_left = width.saturating_mul(2) / 3;
        let changed_top = height.saturating_mul(2) / 3;
        for y in changed_top..height {
            for x in 0..changed_left {
                let value = x
                    .wrapping_mul(29)
                    .wrapping_add(y.wrapping_mul(47))
                    .wrapping_add(191) as u8;
                let index = (((target.1 + y) * frame.width + target.0 + x) * 4) as usize;
                pixels[index..index + 4].copy_from_slice(&[
                    value,
                    value.wrapping_add(73),
                    value.wrapping_mul(3).wrapping_add(11),
                    255,
                ]);
            }
        }
        frame = CaptureFrame::from_bgra(frame.origin, frame.width, frame.height, pixels)
            .expect("changed bottom-left frame");
        frame
    }

    fn scaled_dimensions_for_template(template: &CaptureFrame, scale: f32) -> (u32, u32) {
        (
            (f64::from(template.width) * f64::from(scale)).round() as u32,
            (f64::from(template.height) * f64::from(scale)).round() as u32,
        )
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
            let optimized_result = matcher
                .find_template_with_options(&image, &template, 0.99, &MatcherOptions::default())
                .expect("optimized");
            let optimized = optimized_result.image.expect("optimized differential hit");
            assert_eq!((baseline.x, baseline.y), (optimized.x, optimized.y));
        }
    }

    #[test]
    fn multiscale_matching_reports_real_scale_and_dimensions() {
        let (template, _, _) = test_template();
        let matcher = ImageProcVisionMatcher::new();
        for scale in [0.67_f32, 0.80, 1.0, 1.20, 1.25, 1.50, 1.75, 2.0] {
            let target = (60, 30);
            let image = scaled_patterned_frame(
                Point { x: -20, y: 10 },
                160,
                120,
                &template,
                target,
                scale,
                0xC0DE,
            );
            let result = matcher
                .find_template_with_options(&image, &template, 0.99, &MatcherOptions::default())
                .expect("multiscale match");
            let found = result.image.expect("scaled template hit");
            assert_eq!((found.x, found.y), (40, 40));
            assert_eq!(
                (found.width, found.height),
                (
                    (f64::from(template.width) * f64::from(scale)).round() as u32,
                    (f64::from(template.height) * f64::from(scale)).round() as u32,
                )
            );
            assert!(
                (result.diagnostics.matched_scale.expect("matched scale") - scale).abs() < 0.01,
                "scale={scale} diagnostics={:?}",
                result.diagnostics
            );
        }
    }

    #[test]
    fn robust_verification_tolerates_changed_bottom_left_corner() {
        let (template, _, _) = test_template();
        let target = (48, 28);
        let scale = 1.75;
        let base = scaled_patterned_frame(
            Point { x: 0, y: 0 },
            160,
            120,
            &template,
            target,
            scale,
            0xD00D,
        );
        let image = changed_bottom_left_frame(base, &template, target, scale);
        let result = ImageProcVisionMatcher::new()
            .find_template_with_options(&image, &template, 0.92, &MatcherOptions::default())
            .expect("robust corner match");
        let found = result.image.expect("corner change should remain a hit");
        assert_eq!((found.x, found.y), (target.0 as i32, target.1 as i32));
        assert!((result.diagnostics.matched_scale.expect("scale") - scale).abs() < 0.01);
        assert!(result.diagnostics.robust_verify_used);
        assert!(result.diagnostics.robust_score.expect("robust score") > 0.8);
        assert!(result.diagnostics.valid_tile_count >= ROBUST_MIN_VALID_TILES);
        assert!(result.diagnostics.discarded_tile_count <= ROBUST_MAX_DISCARDED_TILES);
    }

    #[test]
    fn robust_verification_rejects_a_single_similar_tile() {
        let (template, template_pixels, size) = test_template();
        let target = (54_u32, 32_u32);
        let mut image = patterned_frame(
            Point { x: 0, y: 0 },
            160,
            120,
            &size,
            &template_pixels,
            None,
            0xA55A,
        );
        let mut pixels = image.pixels_bgra().to_vec();
        for y in 0..u32::from(size[1]) / 3 {
            for x in 0..u32::from(size[0]) / 3 {
                let source_index = ((y * u32::from(size[0]) + x) * 4) as usize;
                let target_index = (((target.1 + y) * image.width + target.0 + x) * 4) as usize;
                pixels[target_index..target_index + 4]
                    .copy_from_slice(&template_pixels[source_index / 4]);
            }
        }
        image = CaptureFrame::from_bgra(Point { x: 0, y: 0 }, 160, 120, pixels)
            .expect("single tile frame");
        let result = ImageProcVisionMatcher::new()
            .find_template_with_options(&image, &template, 0.92, &MatcherOptions::default())
            .expect("single tile search");
        assert!(result.image.is_none());
        assert!(result.diagnostics.robust_score.is_none());
        assert!(result.diagnostics.robust_verify_used);
    }

    #[test]
    fn robust_verification_rejects_two_spatially_insufficient_tiles() {
        let (template, template_pixels, size) = test_template();
        let target = (54_u32, 32_u32);
        let mut image = patterned_frame(
            Point { x: 0, y: 0 },
            160,
            120,
            &size,
            &template_pixels,
            None,
            0xBEEF,
        );
        let mut pixels = image.pixels_bgra().to_vec();
        for (tile_x, tile_y) in [(0_u32, 0_u32), (16_u32, 16_u32)] {
            for y in 0..8 {
                for x in 0..8 {
                    let source_index =
                        (((tile_y + y) * u32::from(size[0]) + tile_x + x) * 4) as usize;
                    let target_index =
                        (((target.1 + tile_y + y) * image.width + target.0 + tile_x + x) * 4)
                            as usize;
                    pixels[target_index..target_index + 4]
                        .copy_from_slice(&template_pixels[source_index / 4]);
                }
            }
        }
        image = CaptureFrame::from_bgra(Point { x: 0, y: 0 }, 160, 120, pixels)
            .expect("two tile frame");
        let result = ImageProcVisionMatcher::new()
            .find_template_with_options(&image, &template, 0.92, &MatcherOptions::default())
            .expect("two tile search");
        assert!(result.image.is_none());
        assert!(result.diagnostics.robust_verify_used);
        assert!(result.diagnostics.robust_score.is_none());
    }

    #[test]
    fn alpha_zero_region_is_ignored_but_opaque_changes_are_not() {
        let (_, mut template_pixels, size) = test_template();
        for y in 16..24 {
            for x in 0..8 {
                template_pixels[(y * 24 + x) as usize][3] = 0;
            }
        }
        let template = frame(Point { x: 0, y: 0 }, 24, 24, &template_pixels);
        let target = (52_u32, 34_u32);
        let mut image = patterned_frame(
            Point { x: 0, y: 0 },
            160,
            120,
            &size,
            &template_pixels,
            Some(target),
            0xBADA,
        );
        let mut pixels = image.pixels_bgra().to_vec();
        for y in 16..24 {
            for x in 0..8 {
                let index = (((target.1 + y) * image.width + target.0 + x) * 4) as usize;
                pixels[index..index + 4].copy_from_slice(&[3, 191, 17, 255]);
            }
        }
        image = CaptureFrame::from_bgra(Point { x: 0, y: 0 }, 160, 120, pixels)
            .expect("alpha ignored frame");
        let prepared = PreparedTemplate::from_frame(
            "alpha-test",
            "alpha.png",
            "alpha-fingerprint",
            Arc::new(template.clone()),
        )
        .expect("alpha prepared template");
        let scaled = prepared
            .scaled_template(1.75)
            .expect("scaled alpha template");
        assert_eq!((scaled.width(), scaled.height()), (42, 42));
        assert_eq!((scaled.alpha().width(), scaled.alpha().height()), (42, 42));
        assert!(scaled.alpha_mask_used());
        let result = ImageProcVisionMatcher::new()
            .find_prepared_template(
                &image,
                prepared.frame(),
                Some(&prepared),
                0.99,
                &MatcherOptions::default(),
            )
            .expect("alpha match");
        let found = result.image.expect("transparent region should be ignored");
        assert_eq!((found.x, found.y), (target.0 as i32, target.1 as i32));
        assert!(result.diagnostics.alpha_mask_used);
        assert!(result.diagnostics.robust_score.expect("alpha score") > 0.95);

        let mut opaque_changed = image.pixels_bgra().to_vec();
        for y in 8..16 {
            for x in 8..16 {
                let index = (((target.1 + y) * image.width + target.0 + x) * 4) as usize;
                opaque_changed[index..index + 4].copy_from_slice(&[4, 7, 231, 255]);
            }
        }
        let opaque_changed =
            CaptureFrame::from_bgra(Point { x: 0, y: 0 }, 160, 120, opaque_changed)
                .expect("opaque change frame");
        let rejected = ImageProcVisionMatcher::new()
            .find_prepared_template(
                &opaque_changed,
                prepared.frame(),
                Some(&prepared),
                0.99,
                &MatcherOptions::default(),
            )
            .expect("opaque change search");
        assert!(rejected.image.is_none());
        assert!(rejected.diagnostics.robust_score.is_none());
    }

    #[test]
    fn auto_anchor_recovery_verifies_stable_tiles_and_fast_skips_recovery() {
        let (template, _, _) = test_template();
        let scale = 1.75;
        let target = (48_u32, 28_u32);
        let base = scaled_patterned_frame(
            Point { x: 0, y: 0 },
            160,
            120,
            &template,
            target,
            scale,
            0xA55A,
        );
        let (width, height) = scaled_dimensions_for_template(&template, scale);
        let mut pixels = base.pixels_bgra().to_vec();
        for y in height * 2 / 3..height {
            for x in 0..width / 3 {
                let index = (((target.1 + y) * base.width + target.0 + x) * 4) as usize;
                pixels[index..index + 4].copy_from_slice(&[0, 0, 0, 255]);
            }
        }
        let image = CaptureFrame::from_bgra(Point { x: 0, y: 0 }, 160, 120, pixels)
            .expect("anchor recovery frame");
        let matcher = ImageProcVisionMatcher::new();
        let result = matcher
            .find_template_with_options(&image, &template, 0.92, &MatcherOptions::default())
            .expect("anchor recovery search");
        let found = result.image.expect("anchor recovery hit");
        assert_eq!((found.x, found.y), (target.0 as i32, target.1 as i32));
        assert!(result.diagnostics.anchor_recovery_used);
        assert!(result.diagnostics.anchor_candidate_count > 0);
        assert!(
            result
                .diagnostics
                .robust_score
                .expect("anchor robust score")
                > 0.95
        );

        let fast = matcher
            .find_template_with_options(
                &image,
                &template,
                0.92,
                &MatcherOptions {
                    mode: MatcherMode::Fast,
                    ..MatcherOptions::default()
                },
            )
            .expect("fast anchor search");
        assert!(!fast.diagnostics.anchor_recovery_used);
    }

    #[test]
    fn matcher_modes_keep_exact_fixed_and_fast_without_fallback() {
        let (template, _, _) = test_template();
        let image = scaled_patterned_frame(
            Point { x: 0, y: 0 },
            160,
            120,
            &template,
            (60, 30),
            1.25,
            0xFACE,
        );
        let matcher = ImageProcVisionMatcher::new();
        let exact_scale_one_image = scaled_patterned_frame(
            Point { x: 0, y: 0 },
            160,
            120,
            &template,
            (60, 30),
            1.0,
            0xFACE,
        );
        let exact_scale_one_result = matcher
            .find_template_with_options(
                &exact_scale_one_image,
                &template,
                0.99,
                &MatcherOptions {
                    mode: MatcherMode::Exact,
                    ..MatcherOptions::default()
                },
            )
            .expect("exact scale-one mode");
        let exact_scale_one = exact_scale_one_result.image.expect("exact scale-one hit");
        assert_eq!(
            (
                exact_scale_one.x,
                exact_scale_one.y,
                exact_scale_one.width,
                exact_scale_one.height
            ),
            (60, 30, 24, 24)
        );
        assert_eq!(
            exact_scale_one_result.diagnostics.scale_candidates,
            vec![1.0]
        );
        assert!(!exact_scale_one_result.diagnostics.fallback_used);
        let exact = matcher
            .find_template_with_options(
                &image,
                &template,
                0.99,
                &MatcherOptions {
                    mode: MatcherMode::Exact,
                    ..MatcherOptions::default()
                },
            )
            .expect("exact mode");
        assert!(exact.image.is_none());
        assert_eq!(exact.diagnostics.scale_candidates, vec![1.0]);
        assert!(!exact.diagnostics.fallback_used);
        assert_eq!(exact.diagnostics.coarse_ms, 0);

        let fast_result = matcher
            .find_template_with_options(
                &image,
                &template,
                0.99,
                &MatcherOptions {
                    mode: MatcherMode::Fast,
                    ..MatcherOptions::default()
                },
            )
            .expect("fast mode");
        let fast = fast_result.image.expect("fast multiscale hit");
        assert_eq!((fast.x, fast.y, fast.width, fast.height), (60, 30, 30, 30));
        assert!(!fast_result.diagnostics.fallback_used);
        assert!((fast_result.diagnostics.matched_scale.expect("fast scale") - 1.25).abs() < 0.01);
    }

    #[test]
    fn highest_original_score_wins_across_scale_candidates() {
        let (template, _, _) = test_template();
        let true_target = (70_u32, 30_u32);
        let weaker_target = (10_u32, 12_u32);
        let mut image = scaled_patterned_frame(
            Point { x: 0, y: 0 },
            160,
            120,
            &template,
            true_target,
            1.25,
            0x1234,
        );
        let gray = ImageProcVisionMatcher::gray(&template).expect("template gray");
        let weaker = resize(&gray, 19, 19, FilterType::Triangle);
        let mut pixels = image.pixels_bgra().to_vec();
        for y in 0..weaker.height() {
            for x in 0..weaker.width() {
                let value = if x == 3 && y == 3 {
                    weaker.get_pixel(x, y).0[0].wrapping_add(70)
                } else {
                    weaker.get_pixel(x, y).0[0]
                };
                let target_index =
                    (((weaker_target.1 + y) * 160 + weaker_target.0 + x) * 4) as usize;
                pixels[target_index..target_index + 4].copy_from_slice(&[value, value, value, 255]);
            }
        }
        image = CaptureFrame::from_bgra(Point { x: 0, y: 0 }, 160, 120, pixels)
            .expect("multi-scale candidates");

        let result = ImageProcVisionMatcher::new()
            .find_template_with_options(&image, &template, 0.99, &MatcherOptions::default())
            .expect("multi-scale candidate match");
        let found = result.image.expect("highest scale candidate");
        assert_eq!(
            (found.x, found.y),
            (true_target.0 as i32, true_target.1 as i32)
        );
        assert_eq!((found.width, found.height), (30, 30));
        assert!((result.diagnostics.matched_scale.expect("winning scale") - 1.25).abs() < 0.01);
    }

    #[test]
    fn scaled_template_cache_is_reused_and_bounded() {
        let (template, _, _) = test_template();
        let prepared = PreparedTemplate::from_frame(
            "asset-id",
            "button.png",
            "fingerprint-a",
            Arc::new(template),
        )
        .expect("prepared template");
        let first = prepared.scaled_template(1.25).expect("first scale");
        let second = prepared.scaled_template(1.25).expect("cached scale");
        assert!(Arc::ptr_eq(&first, &second));
        for index in 0..31 {
            let scale = 0.5 + index as f32 * 0.05;
            prepared.scaled_template(scale).expect("bounded scale");
        }
        let cache = prepared.scaled_templates.lock().expect("scale cache");
        assert!(cache.len() <= MAX_SCALED_TEMPLATE_CACHE_ENTRIES);
        assert!(
            cache
                .values()
                .map(|cached| cached.template.memory_bytes())
                .sum::<usize>()
                <= MAX_SCALED_TEMPLATE_CACHE_BYTES
        );
    }

    #[test]
    fn scaled_template_larger_than_frame_and_miss_diagnostics_are_safe() {
        let (template, _, _) = test_template();
        let frame = patterned_frame(
            Point { x: 0, y: 0 },
            24,
            24,
            &[24, 24],
            &vec![[5, 11, 23, 255]; 24 * 24],
            None,
            0xCAFE,
        );
        let result = ImageProcVisionMatcher::new()
            .find_template_with_options(
                &frame,
                &template,
                0.99,
                &MatcherOptions {
                    scale_min: 1.5,
                    scale_max: 1.5,
                    ..MatcherOptions::default()
                },
            )
            .expect("scaled template should be skipped safely");
        assert!(result.image.is_none());
        assert!(result.diagnostics.matched_scale.is_none());
        assert_eq!(
            (
                result.diagnostics.matched_width,
                result.diagnostics.matched_height
            ),
            (0, 0)
        );
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
                    ..MatcherOptions::default()
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
