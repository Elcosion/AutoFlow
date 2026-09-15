use super::types::{CaptureFrame, MatcherOptions, Point, ScreenRect, VisionMatcher};
use super::vision::{ImageProcVisionMatcher, PreparedTemplate};
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

    let hit = BenchCase {
        name: "hit",
        frame: synthetic_frame(0xA11CE, Some(HIT_AT_END), &template),
        expected: Some(HIT_AT_END),
    };
    let miss = BenchCase {
        name: "miss",
        frame: synthetic_frame(0xA11CE, None, &template),
        expected: None,
    };
    let repeat = BenchCase {
        name: "repeat-hit",
        frame: hit.frame.clone(),
        expected: Some(HIT_AT_END),
    };
    let moved = BenchCase {
        name: "moved-hit",
        frame: synthetic_frame(0xA11CE, Some(MOVED_TO), &template),
        expected: Some(MOVED_TO),
    };

    let mut report = String::new();
    report.push_str("vision_benchmark=synthetic\n");
    report.push_str(&format!(
        "dimensions={}x{} template={}x{} threshold={} seed=0xA11CE\n",
        WIDTH, HEIGHT, TEMPLATE_WIDTH, TEMPLATE_HEIGHT, THRESHOLD
    ));
    report.push_str(&format!("prepared_template_ms={prepare_ms}\n"));

    append_case(
        &mut report,
        &matcher,
        &prepared,
        &options,
        &hit,
        prepare_ms,
        false,
    );
    append_case(&mut report, &matcher, &prepared, &options, &miss, 0, false);

    let previous_region = previous_region(HIT_AT_END);
    let repeat_started = Instant::now();
    let repeat_frame = repeat.frame.crop(previous_region).expect("repeat ROI");
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
    append_result(
        &mut report,
        &matcher,
        &prepared,
        &options,
        &repeat,
        repeat_result,
        0,
    );

    let moved_started = Instant::now();
    let moved_previous_frame = moved.frame.crop(previous_region).expect("moved ROI");
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
    append_result(
        &mut report,
        &matcher,
        &prepared,
        &options,
        &moved,
        moved_result,
        0,
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
    append_result(report, matcher, prepared, options, case, result, prepare_ms);
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
    let score = result
        .image
        .as_ref()
        .map(|image| format!("{:.6}", image.score))
        .unwrap_or_else(|| "none".to_string());
    report.push_str(&format!(
        "case={} expected={} baseline_match_ms={} baseline_xy={},{} optimized_xy={},{} total_match_ms={} capture_ms={} prepare_ms={} coarse_match_ms={} refine_match_ms={} fallback_match_ms={} candidate_count={} coarse_score={:.6} refined_score={:.6} previous_hit_used={} fallback_used={} matcher_mode={} options_max_candidates={} score={}\n",
        case.name,
        expected,
        baseline_ms,
        baseline_x,
        baseline_y,
        optimized_x,
        optimized_y,
        diagnostics.total_ms,
        diagnostics.capture_ms,
        diagnostics.prepare_ms,
        diagnostics.coarse_ms,
        diagnostics.refine_ms,
        diagnostics.fallback_ms,
        diagnostics.candidate_count,
        diagnostics.coarse_score,
        diagnostics.refined_score,
        diagnostics.previous_hit_used,
        diagnostics.fallback_used,
        diagnostics.matcher_mode,
        options.max_candidates,
        score,
    ));
}

fn previous_region((x, y): (u32, u32)) -> ScreenRect {
    let margin_x = TEMPLATE_WIDTH.saturating_mul(2).max(64);
    let margin_y = TEMPLATE_HEIGHT.saturating_mul(2).max(64);
    let left = x.saturating_sub(margin_x);
    let top = y.saturating_sub(margin_y);
    let right = x
        .saturating_add(TEMPLATE_WIDTH)
        .saturating_add(margin_x)
        .min(WIDTH);
    let bottom = y
        .saturating_add(TEMPLATE_HEIGHT)
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

fn next_byte(state: &mut u32) -> u8 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    (*state >> 24) as u8
}
