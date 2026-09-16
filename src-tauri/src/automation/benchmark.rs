use super::types::{CaptureFrame, MatcherOptions, Point, ScreenRect, VisionMatcher};
use super::vision::{ImageProcVisionMatcher, PreparedTemplate};
use image::imageops::{resize, FilterType};
use std::sync::Arc;
use std::time::Instant;

const WIDTH: u32 = 1_920;
const HEIGHT: u32 = 1_080;
const TEMPLATE_WIDTH: u32 = 45;
const TEMPLATE_HEIGHT: u32 = 41;
const THRESHOLD: f32 = 0.92;
const HIT_AT_END: (u32, u32) = (WIDTH - TEMPLATE_WIDTH, HEIGHT - TEMPLATE_HEIGHT);
const MOVED_TO: (u32, u32) = (812, 437);

#[derive(Debug)]
struct BenchCase {
    name: &'static str,
    frame: CaptureFrame,
    expected: Option<(u32, u32)>,
    expected_scale: Option<f32>,
}

pub fn run_vision_benchmark() -> String {
    let template = synthetic_template();
    let template_started = Instant::now();
    let prepared = PreparedTemplate::from_frame(
        "benchmark-template",
        "benchmark.png",
        "synthetic-fixed-seed",
        Arc::new(template.clone()),
    )
    .expect("synthetic template preparation");
    let prepare_ms = template_started.elapsed().as_millis() as u64;
    let matcher = ImageProcVisionMatcher::new();
    let options = MatcherOptions::default();

    // Keep the original scale-one benchmark cases as a regression baseline.
    let hit = scaled_case("hit", 1.0, 0xA11CE, HIT_AT_END, &template);
    let miss = BenchCase {
        name: "miss",
        frame: synthetic_frame(0xA11CE, None, &template),
        expected: None,
        expected_scale: None,
    };
    let moved_hit = scaled_case("moved-hit", 1.0, 0xA11CE, MOVED_TO, &template);

    let scale_080 = scaled_case(
        "scale-0.80-hit",
        0.80,
        0xA11CE,
        scale_target(0.80),
        &template,
    );
    let scale_100 = scaled_case(
        "scale-1.00-hit",
        1.00,
        0xA11CE,
        scale_target(1.00),
        &template,
    );
    let scale_125 = scaled_case(
        "scale-1.25-hit",
        1.25,
        0xA11CE,
        scale_target(1.25),
        &template,
    );
    let scale_150 = scaled_case(
        "scale-1.50-hit",
        1.50,
        0xA11CE,
        scale_target(1.50),
        &template,
    );
    let scale_175 = scaled_case(
        "scale-1.75-hit",
        1.75,
        0xA11CE,
        scale_target(1.75),
        &template,
    );
    let scale_200 = scaled_case("scale-2.00-hit", 2.0, 0xA11CE, scale_target(2.0), &template);
    let moved = scaled_case("scale-moved-hit", 1.25, 0xBEEF, MOVED_TO, &template);
    let changed = scaled_case(
        "scale-changed-hit",
        1.50,
        0xCAFE,
        scale_target(1.50),
        &template,
    );
    let multiscale_miss = BenchCase {
        name: "multiscale-miss",
        frame: synthetic_frame(0xA11CE, None, &template),
        expected: None,
        expected_scale: None,
    };
    let changed_bottom_left = changed_bottom_left_case(
        "changed-bottom-left-hit",
        1.75,
        0xBADC0DE,
        scale_target(1.75),
        &template,
    );
    let changed_bottom_left_moved = changed_bottom_left_case(
        "changed-bottom-left-moved-hit",
        2.0,
        0xBADC0DE,
        MOVED_TO,
        &template,
    );
    let similar_distractor = similar_distractor_case(&template);
    let robust_miss = robust_miss_case(&template);
    let anchor_recovery = anchor_recovery_case(&template);

    let mut report = String::new();
    report.push_str("vision_benchmark=synthetic\n");
    report.push_str(&format!(
        "dimensions={}x{} template={}x{} threshold={} seed=0xA11CE\n",
        WIDTH, HEIGHT, TEMPLATE_WIDTH, TEMPLATE_HEIGHT, THRESHOLD
    ));
    report.push_str(&format!("prepared_template_ms={prepare_ms}\n"));

    for case in [
        &hit,
        &miss,
        &scale_080,
        &scale_100,
        &scale_125,
        &scale_150,
        &scale_175,
        &scale_200,
        &multiscale_miss,
        &changed_bottom_left,
        &changed_bottom_left_moved,
        &similar_distractor,
        &robust_miss,
        &anchor_recovery,
    ] {
        append_case(
            &mut report,
            &matcher,
            &prepared,
            &options,
            case,
            prepare_ms,
            false,
        );
    }

    let original_repeat_target = hit.expected.expect("original repeat target");
    let original_repeat_region =
        previous_region(original_repeat_target, (TEMPLATE_WIDTH, TEMPLATE_HEIGHT));
    let original_repeat_started = Instant::now();
    let original_repeat_frame = hit
        .frame
        .crop(original_repeat_region)
        .expect("original repeat ROI");
    let mut original_repeat_result = matcher
        .find_prepared_template(
            &original_repeat_frame,
            prepared.frame(),
            Some(&prepared),
            THRESHOLD,
            &options,
        )
        .expect("original repeat optimized match");
    original_repeat_result.diagnostics.previous_hit_used = true;
    original_repeat_result.diagnostics.total_ms =
        original_repeat_started.elapsed().as_millis() as u64;
    original_repeat_result.diagnostics.single_match_ms =
        original_repeat_result.diagnostics.total_ms;
    let original_repeat = BenchCase {
        name: "repeat-hit",
        frame: hit.frame.clone(),
        expected: hit.expected,
        expected_scale: hit.expected_scale,
    };
    append_result(
        &mut report,
        &matcher,
        &prepared,
        &options,
        &original_repeat,
        original_repeat_result,
        0,
    );

    append_case(
        &mut report,
        &matcher,
        &prepared,
        &options,
        &moved_hit,
        0,
        false,
    );

    let repeat_target = scale_125.expected.expect("repeat target");
    let repeat_dimensions = scaled_dimensions(1.25);
    let repeat_search_region = previous_region(repeat_target, repeat_dimensions);
    let repeat_started = Instant::now();
    let repeat_frame = scale_125
        .frame
        .crop(repeat_search_region)
        .expect("repeat ROI");
    let mut repeat_result = matcher
        .find_prepared_template(
            &repeat_frame,
            prepared.frame(),
            Some(&prepared),
            THRESHOLD,
            &options,
        )
        .expect("repeat optimized match");
    repeat_result.diagnostics.previous_hit_used = true;
    repeat_result.diagnostics.total_ms = repeat_started.elapsed().as_millis() as u64;
    repeat_result.diagnostics.single_match_ms = repeat_result.diagnostics.total_ms;
    append_result(
        &mut report,
        &matcher,
        &prepared,
        &options,
        &scale_125,
        repeat_result,
        0,
    );

    let moved_started = Instant::now();
    let moved_previous_frame = moved.frame.crop(repeat_search_region).expect("moved ROI");
    let previous = matcher
        .find_prepared_template(
            &moved_previous_frame,
            prepared.frame(),
            Some(&prepared),
            THRESHOLD,
            &options,
        )
        .expect("moved previous match");
    let full = matcher
        .find_prepared_template(
            &moved.frame,
            prepared.frame(),
            Some(&prepared),
            THRESHOLD,
            &options,
        )
        .expect("moved full match");
    let mut moved_result = full;
    moved_result.diagnostics.add_attempt(&previous.diagnostics);
    moved_result.diagnostics.previous_hit_used = true;
    moved_result.diagnostics.total_ms = moved_started.elapsed().as_millis() as u64;
    moved_result.diagnostics.single_match_ms = moved_result.diagnostics.total_ms;
    append_result(
        &mut report,
        &matcher,
        &prepared,
        &options,
        &moved,
        moved_result,
        0,
    );

    let changed_started = Instant::now();
    let changed_target = changed.expected.expect("changed target");
    let changed_previous_region = previous_region(changed_target, scaled_dimensions(1.0));
    let changed_previous_frame = changed
        .frame
        .crop(changed_previous_region)
        .expect("changed ROI");
    let mut changed_result = matcher
        .find_prepared_template(
            &changed_previous_frame,
            prepared.frame(),
            Some(&prepared),
            THRESHOLD,
            &options,
        )
        .expect("changed previous match");
    changed_result.diagnostics.previous_hit_used = true;
    changed_result.diagnostics.total_ms = changed_started.elapsed().as_millis() as u64;
    changed_result.diagnostics.single_match_ms = changed_result.diagnostics.total_ms;
    append_result(
        &mut report,
        &matcher,
        &prepared,
        &options,
        &changed,
        changed_result,
        0,
    );

    append_repeat_case(&mut report, &matcher, &prepared, &options, &scale_175, 1.75);
    append_repeat_case(&mut report, &matcher, &prepared, &options, &scale_200, 2.0);

    let alpha_template = alpha_masked_template(&template);
    let alpha_prepared = PreparedTemplate::from_frame(
        "benchmark-alpha-template",
        "benchmark-alpha.png",
        "synthetic-alpha-fixed-seed",
        Arc::new(alpha_template.clone()),
    )
    .expect("alpha template preparation");
    let alpha_case = BenchCase {
        name: "alpha-masked-hit",
        frame: alpha_masked_frame(0xA11CE, scale_target(1.0), &alpha_template),
        expected: Some(scale_target(1.0)),
        expected_scale: Some(1.0),
    };
    append_case(
        &mut report,
        &matcher,
        &alpha_prepared,
        &options,
        &alpha_case,
        prepare_ms,
        false,
    );

    report
}

fn append_case(
    report: &mut String,
    matcher: &ImageProcVisionMatcher,
    prepared: &PreparedTemplate,
    options: &MatcherOptions,
    case: &BenchCase,
    prepare_ms: u64,
    previous_hit_used: bool,
) {
    let started = Instant::now();
    let result = matcher
        .find_prepared_template(
            &case.frame,
            prepared.frame(),
            Some(prepared),
            THRESHOLD,
            options,
        )
        .expect("synthetic optimized match");
    let mut result = result;
    result.diagnostics.prepare_ms = prepare_ms;
    result.diagnostics.previous_hit_used = previous_hit_used;
    result.diagnostics.total_ms = started.elapsed().as_millis() as u64;
    result.diagnostics.single_match_ms = result.diagnostics.total_ms;
    append_result(report, matcher, prepared, options, case, result, prepare_ms);
}

fn append_repeat_case(
    report: &mut String,
    matcher: &ImageProcVisionMatcher,
    prepared: &PreparedTemplate,
    options: &MatcherOptions,
    case: &BenchCase,
    scale: f32,
) {
    let target = case.expected.expect("repeat target");
    let repeat_region = previous_region(target, scaled_dimensions(scale));
    let repeat_frame = case.frame.crop(repeat_region).expect("repeat ROI");
    let mut repeat_options = options.clone();
    repeat_options.preferred_scale = Some(scale);
    let started = Instant::now();
    let mut result = matcher
        .find_prepared_template(
            &repeat_frame,
            prepared.frame(),
            Some(prepared),
            THRESHOLD,
            &repeat_options,
        )
        .expect("repeat optimized match");
    result.diagnostics.previous_hit_used = true;
    result.diagnostics.total_ms = started.elapsed().as_millis() as u64;
    result.diagnostics.single_match_ms = result.diagnostics.total_ms;
    let repeat_case = BenchCase {
        name: if (scale - 1.75).abs() < 0.001 {
            "scale-1.75-repeat"
        } else {
            "scale-2.00-repeat"
        },
        frame: case.frame.clone(),
        expected: case.expected,
        expected_scale: case.expected_scale,
    };
    append_result(
        report,
        matcher,
        prepared,
        &repeat_options,
        &repeat_case,
        result,
        0,
    );
}

fn append_result(
    report: &mut String,
    matcher: &ImageProcVisionMatcher,
    prepared: &PreparedTemplate,
    options: &MatcherOptions,
    case: &BenchCase,
    result: super::types::MatcherResult,
    _prepare_ms: u64,
) {
    let baseline_started = Instant::now();
    let baseline = matcher
        .find_template_baseline(&case.frame, prepared.frame(), THRESHOLD)
        .expect("synthetic baseline match");
    let baseline_ms = baseline_started.elapsed().as_millis() as u64;
    let (optimized_x, optimized_y) = result
        .image
        .as_ref()
        .map(|image| (image.x - case.frame.origin.x, image.y - case.frame.origin.y))
        .unwrap_or((-1, -1));
    let (baseline_x, baseline_y) = baseline
        .as_ref()
        .map(|image| (image.x - case.frame.origin.x, image.y - case.frame.origin.y))
        .unwrap_or((-1, -1));
    let expected = case
        .expected
        .map(|(x, y)| format!("{x},{y}"))
        .unwrap_or_else(|| "none".to_string());
    let diagnostics = &result.diagnostics;
    let expected_scale = case
        .expected_scale
        .map(|scale| format!("{scale:.2}"))
        .unwrap_or_else(|| "none".to_string());
    let matched_scale = diagnostics
        .matched_scale
        .map(|scale| format!("{scale:.4}"))
        .unwrap_or_else(|| "none".to_string());
    let scale_candidates = diagnostics
        .scale_candidates
        .iter()
        .map(|scale| format!("{scale:.2}"))
        .collect::<Vec<_>>()
        .join(",");
    let score = result
        .image
        .as_ref()
        .map(|image| format!("{:.6}", image.score))
        .unwrap_or_else(|| "none".to_string());
    report.push_str(&format!(
        "case={} expected_xy={} optimized_xy={},{} expected_scale={} matched_scale={} baseline_match_ms={} baseline_xy={},{} total_match_ms={} single_match_ms={} scale_search_ms={} capture_ms={} prepare_ms={} coarse_match_ms={} refine_match_ms={} robust_verify_ms={} fallback_match_ms={} coarse_score={:.6} refined_score={:.6} robust_score={} valid_tile_count={} discarded_tile_count={} alpha_mask_used={} anchor_recovery_used={} anchor_candidate_count={} preferred_scale_hit={} candidate_count={} scale_candidates={} previous_hit_used={} fallback_used={} matcher_mode={} options_max_candidates={} matched_width={} matched_height={} wait_total_ms={} score={}\n",
        case.name,
        expected,
        optimized_x,
        optimized_y,
        expected_scale,
        matched_scale,
        baseline_ms,
        baseline_x,
        baseline_y,
        diagnostics.total_ms,
        diagnostics.single_match_ms,
        diagnostics.scale_search_ms,
        diagnostics.capture_ms,
        diagnostics.prepare_ms,
        diagnostics.coarse_ms,
        diagnostics.refine_ms,
        diagnostics.robust_verify_ms,
        diagnostics.fallback_ms,
        diagnostics.coarse_score,
        diagnostics.refined_score,
        diagnostics
            .robust_score
            .map(|score| format!("{score:.6}"))
            .unwrap_or_else(|| "none".to_string()),
        diagnostics.valid_tile_count,
        diagnostics.discarded_tile_count,
        diagnostics.alpha_mask_used,
        diagnostics.anchor_recovery_used,
        diagnostics.anchor_candidate_count,
        diagnostics.preferred_scale_hit,
        diagnostics.candidate_count,
        scale_candidates,
        diagnostics.previous_hit_used,
        diagnostics.fallback_used,
        diagnostics.matcher_mode,
        options.max_candidates,
        diagnostics.matched_width,
        diagnostics.matched_height,
        diagnostics.wait_total_ms,
        score,
    ));
}

fn scaled_dimensions(scale: f32) -> (u32, u32) {
    (
        (f64::from(TEMPLATE_WIDTH) * f64::from(scale)).round() as u32,
        (f64::from(TEMPLATE_HEIGHT) * f64::from(scale)).round() as u32,
    )
}

fn scale_target(scale: f32) -> (u32, u32) {
    let (width, height) = scaled_dimensions(scale);
    (WIDTH - width - 1, HEIGHT - height - 1)
}

fn scaled_case(
    name: &'static str,
    scale: f32,
    seed: u32,
    target: (u32, u32),
    template: &CaptureFrame,
) -> BenchCase {
    BenchCase {
        name,
        frame: scaled_synthetic_frame(seed, target, scale, template),
        expected: Some(target),
        expected_scale: Some(scale),
    }
}

fn changed_bottom_left_case(
    name: &'static str,
    scale: f32,
    seed: u32,
    target: (u32, u32),
    template: &CaptureFrame,
) -> BenchCase {
    let base = scaled_synthetic_frame(seed, target, scale, template);
    let (width, height) = scaled_dimensions(scale);
    let changed_left = width.saturating_mul(2) / 3;
    let changed_top = height.saturating_mul(2) / 3;
    let mut pixels = base.pixels_bgra().to_vec();
    for y in changed_top..height {
        for x in 0..changed_left {
            let value = x
                .wrapping_mul(31)
                .wrapping_add(y.wrapping_mul(43))
                .wrapping_add(173) as u8;
            let index = (((target.1 + y) * WIDTH + target.0 + x) * 4) as usize;
            pixels[index..index + 4].copy_from_slice(&[
                value,
                value.wrapping_add(71),
                value.wrapping_mul(5).wrapping_add(13),
                255,
            ]);
        }
    }
    BenchCase {
        name,
        frame: CaptureFrame::from_bgra(Point { x: 0, y: 0 }, WIDTH, HEIGHT, pixels)
            .expect("changed bottom-left frame"),
        expected: Some(target),
        expected_scale: Some(scale),
    }
}

fn similar_distractor_case(template: &CaptureFrame) -> BenchCase {
    let target = (1_420, 720);
    let distractor = (180, 180);
    let mut frame = scaled_synthetic_frame(0x1234, target, 1.0, template);
    let mut pixels = frame.pixels_bgra().to_vec();
    for y in 0..TEMPLATE_HEIGHT {
        for x in 0..TEMPLATE_WIDTH {
            let source_index = ((y * TEMPLATE_WIDTH + x) * 4) as usize;
            let target_index = (((distractor.1 + y) * WIDTH + distractor.0 + x) * 4) as usize;
            pixels[target_index..target_index + 4]
                .copy_from_slice(&template.pixels_bgra()[source_index..source_index + 4]);
        }
    }
    let changed_index = (((distractor.1 + 5) * WIDTH + distractor.0 + 5) * 4) as usize;
    pixels[changed_index] = pixels[changed_index].wrapping_add(80);
    frame = CaptureFrame::from_bgra(Point { x: 0, y: 0 }, WIDTH, HEIGHT, pixels)
        .expect("similar distractor frame");
    BenchCase {
        name: "similar-distractor",
        frame,
        expected: Some(target),
        expected_scale: Some(1.0),
    }
}

fn robust_miss_case(template: &CaptureFrame) -> BenchCase {
    let target = (1_100, 620);
    let mut frame = synthetic_frame(0x55AA, None, template);
    let mut pixels = frame.pixels_bgra().to_vec();
    for y in 0..TEMPLATE_HEIGHT / 3 {
        for x in 0..TEMPLATE_WIDTH / 3 {
            let source_index = ((y * TEMPLATE_WIDTH + x) * 4) as usize;
            let target_index = (((target.1 + y) * WIDTH + target.0 + x) * 4) as usize;
            pixels[target_index..target_index + 4]
                .copy_from_slice(&template.pixels_bgra()[source_index..source_index + 4]);
        }
    }
    frame = CaptureFrame::from_bgra(Point { x: 0, y: 0 }, WIDTH, HEIGHT, pixels)
        .expect("robust miss frame");
    BenchCase {
        name: "robust-miss",
        frame,
        expected: None,
        expected_scale: None,
    }
}

fn anchor_recovery_case(template: &CaptureFrame) -> BenchCase {
    let scale = 1.75;
    let target = scale_target(scale);
    let base = scaled_synthetic_frame(0xA55A, target, scale, template);
    let (width, height) = scaled_dimensions(scale);
    let mut pixels = base.pixels_bgra().to_vec();
    for y in height.saturating_mul(2) / 3..height {
        for x in 0..width / 3 {
            let index = (((target.1 + y) * WIDTH + target.0 + x) * 4) as usize;
            pixels[index..index + 4].copy_from_slice(&[0, 0, 0, 255]);
        }
    }
    BenchCase {
        name: "anchor-recovery-hit",
        frame: CaptureFrame::from_bgra(Point { x: 0, y: 0 }, WIDTH, HEIGHT, pixels)
            .expect("anchor recovery frame"),
        expected: Some(target),
        expected_scale: Some(scale),
    }
}

fn alpha_masked_template(template: &CaptureFrame) -> CaptureFrame {
    let mut pixels = template.pixels_bgra().to_vec();
    for y in TEMPLATE_HEIGHT * 2 / 3..TEMPLATE_HEIGHT {
        for x in 0..TEMPLATE_WIDTH / 2 {
            let index = ((y * TEMPLATE_WIDTH + x) * 4 + 3) as usize;
            pixels[index] = 0;
        }
    }
    CaptureFrame::from_bgra(
        Point { x: 0, y: 0 },
        TEMPLATE_WIDTH,
        TEMPLATE_HEIGHT,
        pixels,
    )
    .expect("alpha masked template")
}

fn alpha_masked_frame(seed: u32, target: (u32, u32), template: &CaptureFrame) -> CaptureFrame {
    let base = synthetic_frame(seed, None, template);
    let mut pixels = base.pixels_bgra().to_vec();
    for y in 0..template.height {
        for x in 0..template.width {
            let source_index = ((y * template.width + x) * 4) as usize;
            if template.pixels_bgra()[source_index + 3] == 0 {
                continue;
            }
            let target_index = (((target.1 + y) * WIDTH + target.0 + x) * 4) as usize;
            pixels[target_index..target_index + 4]
                .copy_from_slice(&template.pixels_bgra()[source_index..source_index + 4]);
        }
    }
    CaptureFrame::from_bgra(Point { x: 0, y: 0 }, WIDTH, HEIGHT, pixels)
        .expect("alpha masked frame")
}

fn previous_region(
    (x, y): (u32, u32),
    (template_width, template_height): (u32, u32),
) -> ScreenRect {
    let margin_x = template_width.saturating_mul(2).max(64);
    let margin_y = template_height.saturating_mul(2).max(64);
    let left = x.saturating_sub(margin_x);
    let top = y.saturating_sub(margin_y);
    let right = x
        .saturating_add(template_width)
        .saturating_add(margin_x)
        .min(WIDTH);
    let bottom = y
        .saturating_add(template_height)
        .saturating_add(margin_y)
        .min(HEIGHT);
    ScreenRect::from_parts(left as i32, top as i32, right - left, bottom - top)
}

fn synthetic_template() -> CaptureFrame {
    let mut state = 0x51A7_2026_u32;
    let mut pixels = Vec::with_capacity((TEMPLATE_WIDTH * TEMPLATE_HEIGHT * 4) as usize);
    for _ in 0..TEMPLATE_WIDTH * TEMPLATE_HEIGHT {
        let value = next_byte(&mut state);
        pixels.extend_from_slice(&[
            value.wrapping_add(17),
            value.wrapping_mul(3).wrapping_add(31),
            value.wrapping_mul(5).wrapping_add(47),
            255,
        ]);
    }
    CaptureFrame::from_bgra(
        Point { x: 0, y: 0 },
        TEMPLATE_WIDTH,
        TEMPLATE_HEIGHT,
        pixels,
    )
    .expect("synthetic template")
}

fn synthetic_frame(seed: u32, target: Option<(u32, u32)>, template: &CaptureFrame) -> CaptureFrame {
    let mut state = seed;
    let mut pixels = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for _ in 0..WIDTH * HEIGHT {
        let value = next_byte(&mut state);
        pixels.extend_from_slice(&[
            value.wrapping_add(5),
            value.wrapping_mul(7).wrapping_add(11),
            value.wrapping_mul(13).wrapping_add(23),
            255,
        ]);
    }
    if let Some((target_x, target_y)) = target {
        for y in 0..TEMPLATE_HEIGHT {
            for x in 0..TEMPLATE_WIDTH {
                let source_index = ((y * TEMPLATE_WIDTH + x) * 4) as usize;
                let target_index = (((target_y + y) * WIDTH + target_x + x) * 4) as usize;
                pixels[target_index..target_index + 4]
                    .copy_from_slice(&template.pixels_bgra()[source_index..source_index + 4]);
            }
        }
    }
    CaptureFrame::from_bgra(Point { x: 0, y: 0 }, WIDTH, HEIGHT, pixels).expect("synthetic frame")
}

fn scaled_synthetic_frame(
    seed: u32,
    target: (u32, u32),
    scale: f32,
    template: &CaptureFrame,
) -> CaptureFrame {
    let base = synthetic_frame(seed, None, template);
    let gray = ImageProcVisionMatcher::gray(template).expect("benchmark template gray");
    let (scaled_width, scaled_height) = scaled_dimensions(scale);
    let scaled = resize(&gray, scaled_width, scaled_height, FilterType::Triangle);
    let mut pixels = base.pixels_bgra().to_vec();
    for y in 0..scaled_height {
        for x in 0..scaled_width {
            let value = scaled.get_pixel(x, y).0[0];
            let index = (((target.1 + y) * WIDTH + target.0 + x) * 4) as usize;
            pixels[index..index + 4].copy_from_slice(&[value, value, value, 255]);
        }
    }
    CaptureFrame::from_bgra(Point { x: 0, y: 0 }, WIDTH, HEIGHT, pixels)
        .expect("scaled synthetic frame")
}

fn next_byte(state: &mut u32) -> u8 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    (*state >> 24) as u8
}
