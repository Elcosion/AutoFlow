use super::*;
use crate::behavior::BehaviorEvent;
use crate::MouseButton;

fn mouse_move(timestamp_ms: u64, x: i32, y: i32) -> BehaviorEvent {
    BehaviorEvent::MouseMove { timestamp_ms, x, y }
}

fn mouse_button(timestamp_ms: u64, button: u8, is_down: bool, x: i32, y: i32) -> BehaviorEvent {
    BehaviorEvent::MouseButton {
        timestamp_ms,
        button,
        is_down,
        x,
        y,
    }
}

fn make_session(events: Vec<BehaviorEvent>) -> BehaviorSessionV2 {
    BehaviorSessionV2 {
        id: "session-test".to_string(),
        name: "test".to_string(),
        api_version: BEHAVIOR_V2_API_VERSION,
        created_at_ms: 1,
        duration_ms: events
            .last()
            .map(|event| match event {
                BehaviorEvent::Key { timestamp_ms, .. }
                | BehaviorEvent::MouseMove { timestamp_ms, .. }
                | BehaviorEvent::MouseButton { timestamp_ms, .. }
                | BehaviorEvent::Wheel { timestamp_ms, .. } => *timestamp_ms,
            })
            .unwrap_or(0),
        task_tag: None,
        capture_metadata: BehaviorCaptureMetadata::default(),
        raw_events: events,
    }
}

fn trajectory_episode(trajectory: &PointerTrajectory) -> PointerMoveEpisode {
    let mut timestamp_ms = 0u64;
    let samples = trajectory
        .points
        .iter()
        .map(|point| {
            timestamp_ms = timestamp_ms.saturating_add(point.delay_ms);
            PointerSample {
                timestamp_ms,
                x: point.x,
                y: point.y,
            }
        })
        .collect();
    PointerMoveEpisode::from_samples(samples, false, None, 0).unwrap()
}

fn varied_profile() -> BehaviorProfileV2 {
    let mut events = Vec::new();
    for episode in 0..6u64 {
        let base = episode * 1_000;
        let bend = if episode % 2 == 0 { 18 } else { -12 };
        events.extend([
            mouse_move(base, 0, 0),
            mouse_move(base + 35, 15, bend),
            mouse_move(base + 85, 52, bend * 2),
            mouse_move(base + 150, 82, bend),
            mouse_move(base + 220, 100, 0),
            mouse_button(base + 280, 1, true, 100, 0),
            mouse_button(base + 320 + episode * 5, 1, false, 100, 0),
        ]);
    }
    train_behavior_profile(&make_session(events)).unwrap()
}

fn straight_episode() -> PointerMoveEpisode {
    PointerMoveEpisode::from_samples(
        vec![
            PointerSample {
                timestamp_ms: 0,
                x: 0,
                y: 0,
            },
            PointerSample {
                timestamp_ms: 50,
                x: 20,
                y: 0,
            },
            PointerSample {
                timestamp_ms: 75,
                x: 80,
                y: 0,
            },
            PointerSample {
                timestamp_ms: 125,
                x: 100,
                y: 0,
            },
            PointerSample {
                timestamp_ms: 200,
                x: 100,
                y: 0,
            },
        ],
        false,
        None,
        150,
    )
    .expect("straight episode")
}

#[test]
fn segmentation_splits_dwell_and_associates_click() {
    let events = vec![
        mouse_move(0, 0, 0),
        mouse_move(20, 20, 0),
        mouse_move(50, 40, 0),
        mouse_move(300, 45, 0),
        mouse_move(330, 70, 0),
        mouse_button(450, 1, true, 70, 0),
        mouse_button(500, 1, false, 70, 0),
    ];
    let result = segment_mouse_actions(&events, &SegmentationConfig::default()).unwrap();
    assert_eq!(result.pointer_moves.len(), 2);
    assert!(result.pointer_moves[0].endpoint_dwell_ms >= 200);
    assert!(result.pointer_moves[1].followed_by_click);
    assert_eq!(result.clicks.len(), 1);
    assert_eq!(result.clicks[0].pointer_move_episode_index, Some(1));
    assert_eq!(result.clicks[0].hold_ms, 50);
    assert!(result.clicks[0].pre_click_dwell_ms >= 100);
}

#[test]
fn segmentation_rejects_unordered_timestamps_and_drops_duplicates() {
    let unordered = vec![mouse_move(10, 0, 0), mouse_move(5, 10, 0)];
    let error = segment_mouse_actions(&unordered, &SegmentationConfig::default()).unwrap_err();
    assert_eq!(error.code, "behavior_v2_timestamp_order");

    let duplicate = vec![
        mouse_move(0, 0, 0),
        mouse_move(10, 10, 0),
        mouse_move(20, 10, 0),
        mouse_move(30, 20, 0),
    ];
    let result = segment_mouse_actions(&duplicate, &SegmentationConfig::default()).unwrap();
    assert_eq!(result.discarded_events.len(), 1);
    assert!(result.pointer_moves[0].path_length_px.is_finite());
}

#[test]
fn features_distinguish_straight_curve_peak_and_correction() {
    let straight = extract_pointer_features(&straight_episode()).unwrap();
    assert!(straight.path_efficiency > 0.99);
    assert!(straight.maximum_lateral_deviation_px < 0.01);
    assert!(straight.time_to_peak_ratio > 0.0);
    assert!(straight.deceleration_phase_ratio > 0.0);

    let curved = PointerMoveEpisode::from_samples(
        vec![
            PointerSample {
                timestamp_ms: 0,
                x: 0,
                y: 0,
            },
            PointerSample {
                timestamp_ms: 40,
                x: 30,
                y: 20,
            },
            PointerSample {
                timestamp_ms: 80,
                x: 60,
                y: 30,
            },
            PointerSample {
                timestamp_ms: 120,
                x: 100,
                y: 0,
            },
        ],
        false,
        None,
        0,
    )
    .unwrap();
    let curved_features = extract_pointer_features(&curved).unwrap();
    assert!(curved_features.maximum_lateral_deviation_px > 1.0);
    assert!(curved_features.path_efficiency < 1.0);

    let overshoot = PointerMoveEpisode::from_samples(
        vec![
            PointerSample {
                timestamp_ms: 0,
                x: 0,
                y: 0,
            },
            PointerSample {
                timestamp_ms: 100,
                x: 100,
                y: 0,
            },
            PointerSample {
                timestamp_ms: 120,
                x: 125,
                y: 0,
            },
            PointerSample {
                timestamp_ms: 150,
                x: 100,
                y: 0,
            },
        ],
        false,
        None,
        0,
    )
    .unwrap();
    let overshoot_features = extract_pointer_features(&overshoot).unwrap();
    assert_eq!(overshoot_features.overshoot_count, 1);
    assert!(overshoot_features.overshoot_distance_px >= 25.0);
    assert!(overshoot_features.correction_count >= 1);
    assert!(overshoot_features.finite());
}

#[test]
fn v2_profile_serializes_without_raw_events_and_models_clicks() {
    let mut events = Vec::new();
    for episode in 0..4u64 {
        let base = episode * 1_000;
        events.extend([
            mouse_move(base, 0, 0),
            mouse_move(base + 40, 30, 0),
            mouse_move(base + 80, 70, 0),
            mouse_button(base + 180, 1, true, 70, 0),
            mouse_button(base + 230, 1, false, 70, 0),
        ]);
    }
    let session = make_session(events);
    let session_serialized = serde_json::to_value(&session).unwrap();
    let session_round_trip: BehaviorSessionV2 = serde_json::from_value(session_serialized).unwrap();
    session_round_trip.validate().unwrap();
    let profile = train_behavior_profile(&session).unwrap();
    assert_eq!(profile.api_version, 2);
    assert!(profile.coverage.valid_pointer_episode_count >= 4);
    assert_eq!(profile.coverage.click_associated_pointer_episode_count, 4);
    assert_eq!(profile.coverage.click_episode_count, 4);
    assert_eq!(profile.click_model.buckets[0].button, MouseButton::Left);
    let serialized = serde_json::to_value(&profile).unwrap();
    assert!(serialized.get("rawEvents").is_none());
    let round_trip: BehaviorProfileV2 = serde_json::from_value(serialized).unwrap();
    round_trip.validate().unwrap();

    let mut legacy = serde_json::to_value(&profile).unwrap();
    let object = legacy.as_object_mut().expect("profile object");
    object.remove("sourceRetention");
    object.remove("modelConfig");
    if let Some(buckets) = object
        .get_mut("pointerModel")
        .and_then(|model| model.get_mut("buckets"))
        .and_then(serde_json::Value::as_array_mut)
    {
        for bucket in buckets {
            if let Some(bucket) = bucket.as_object_mut() {
                bucket.remove("fallbackLevel");
                bucket.remove("exemplars");
            }
        }
    }
    if let Some(buckets) = object
        .get_mut("clickModel")
        .and_then(|model| model.get_mut("buckets"))
        .and_then(serde_json::Value::as_array_mut)
    {
        for bucket in buckets {
            if let Some(bucket) = bucket.as_object_mut() {
                bucket.remove("fallbackLevel");
            }
        }
    }
    let migrated: BehaviorProfileV2 = serde_json::from_value(legacy).unwrap();
    migrated.validate().unwrap();
    assert_eq!(migrated.source_retention, SourceRetention::Persisted);
}

#[test]
fn insufficient_bucket_is_explicitly_marked_as_fallback() {
    let profile = train_behavior_profile(&make_session(vec![
        mouse_move(0, 0, 0),
        mouse_move(50, 20, 0),
        mouse_move(100, 50, 0),
    ]))
    .unwrap();
    assert_eq!(profile.coverage.quality, ModelQuality::Insufficient);

    let context = PointerActionContext::new((0, 0), (50, 0), None, false, 1.0, Some(3)).unwrap();
    let trajectory = generate_pointer_trajectory(&profile, &context, None).unwrap();
    assert!(!trajectory.sampled_features.trained);
    assert_eq!(
        trajectory.fallback_reason.as_deref(),
        Some("insufficient_samples_in_exact_bucket")
    );
}

#[test]
fn generated_trajectory_stays_in_training_quantiles_and_has_jerk_profile() {
    let mut events = Vec::new();
    for episode in 0..12u64 {
        let base = episode * 1_000;
        events.extend([
            mouse_move(base, 0, 0),
            mouse_move(base + 50, 10, 0),
            mouse_move(base + 100, 50, 0),
            mouse_move(base + 200, 100, 0),
        ]);
    }
    let profile = train_behavior_profile(&make_session(events)).unwrap();
    let bucket = profile
        .pointer_model
        .buckets
        .iter()
        .find(|bucket| {
            bucket.key.distance == DistanceBucket::Short
                && bucket.key.direction == DirectionBucket::E
                && !bucket.key.followed_by_click
        })
        .expect("known training bucket");
    let mut generated = Vec::new();
    for seed in 0..8 {
        let context =
            PointerActionContext::new((0, 0), (100, 0), None, false, 1.0, Some(seed)).unwrap();
        let trajectory = generate_pointer_trajectory(&profile, &context, None).unwrap();
        assert!(trajectory.fallback_reason.is_none());
        generated.push(extract_pointer_features(&trajectory_episode(&trajectory)).unwrap());
    }

    let movement_time = bucket.features.movement_time_ms.p50;
    let peak_speed = bucket.features.peak_speed.p50;
    assert!(generated.iter().all(|features| {
        (features.movement_time_ms - movement_time).abs() <= 5.0
            && features.movement_time_ms >= bucket.features.movement_time_ms.p10 - 5.0
            && features.movement_time_ms <= bucket.features.movement_time_ms.p90 + 5.0
            && features.peak_speed >= peak_speed * 0.8
            && features.peak_speed <= peak_speed * 1.2
            && features.path_efficiency > 0.98
    }));
    assert!(generated
        .iter()
        .all(|features| features.peak_speed > features.mean_speed * 1.25));
}

#[test]
fn trajectory_is_seeded_bounded_and_uses_context_bucket() {
    let mut events = Vec::new();
    for episode in 0..4u64 {
        let base = episode * 1_000;
        events.extend([
            mouse_move(base, 0, 0),
            mouse_move(base + 30, 30, 0),
            mouse_move(base + 80, 70, 0),
        ]);
    }
    for episode in 0..4u64 {
        let base = 10_000 + episode * 2_000;
        events.extend([
            mouse_move(base, 0, 0),
            mouse_move(base + 100, 300, 0),
            mouse_move(base + 240, 900, 0),
        ]);
    }
    let profile = train_behavior_profile(&make_session(events)).unwrap();
    let context = PointerActionContext::new((0, 0), (70, 0), None, false, 1.0, Some(7)).unwrap();
    let first = generate_pointer_trajectory(&profile, &context, None).unwrap();
    let second = generate_pointer_trajectory(&profile, &context, None).unwrap();
    assert_eq!(first, second);
    assert_eq!(
        first.points.first().map(|point| (point.x, point.y)),
        Some((0, 0))
    );
    assert_eq!(
        first.points.last().map(|point| (point.x, point.y)),
        Some((70, 0))
    );
    assert!(first.points.iter().all(|point| point.delay_ms <= 5_000));
    assert!(first.points.len() > 2);

    let long_context =
        PointerActionContext::new((0, 0), (900, 0), None, false, 1.0, Some(7)).unwrap();
    let long = generate_pointer_trajectory(&profile, &long_context, None).unwrap();
    assert_ne!(first.bucket, long.bucket);

    let disabled = PointerActionContext::new((0, 0), (900, 0), None, false, 0.0, Some(7)).unwrap();
    let raw = generate_pointer_trajectory(&profile, &disabled, None).unwrap();
    assert_eq!(raw.points.len(), 2);
    assert!(raw.points.iter().all(|point| point.delay_ms == 0));
}

#[test]
fn disabled_click_plan_has_no_dwell_or_hold() {
    let profile = train_behavior_profile(&make_session(vec![
        mouse_move(0, 0, 0),
        mouse_move(40, 40, 0),
        mouse_button(140, 1, true, 40, 0),
        mouse_button(190, 1, false, 40, 0),
    ]))
    .unwrap();
    let plan = sample_click_plan(&profile, MouseButton::Left, true, 0.0, Some(9), None).unwrap();
    assert_eq!(plan.pre_click_dwell_ms, 0);
    assert_eq!(plan.hold_ms, 0);
    assert_eq!(plan.post_click_dwell_ms, 0);
    assert_eq!(plan.fallback_reason.as_deref(), Some("behavior_disabled"));
}

#[test]
fn runtime_seed_and_action_index_control_reproducible_variation() {
    let profile = varied_profile();
    let policy = BehaviorPolicy {
        enabled: true,
        profile_id: Some(profile.id.clone()),
        timing_strength: 1.0,
        pointer_path_strength: 1.0,
        pause_strength: 1.0,
        correction_strength: 1.0,
        speed_scale: 1.0,
        seed: Some(1234),
    };
    let mut first =
        BehaviorRuntimeV2::with_runtime_seed(profile.clone(), policy.clone(), 1234).unwrap();
    let first_move = first
        .pointer_trajectory((0, 0), (100, 0), None, true, None)
        .unwrap();
    let first_click = first.click_plan(MouseButton::Left, true, None).unwrap();
    let second_move = first
        .pointer_trajectory((0, 0), (100, 0), None, true, None)
        .unwrap();
    let second_click = first.click_plan(MouseButton::Left, true, None).unwrap();
    assert_eq!(first.action_index(), 4);
    assert_ne!(first_move, second_move);
    assert_ne!(first_click, second_click);
    assert_eq!(
        first_move
            .diagnostic
            .as_ref()
            .map(|value| value.action_index),
        Some(0)
    );
    assert_eq!(
        first_click
            .diagnostic
            .as_ref()
            .map(|value| value.action_index),
        Some(1)
    );
    assert_eq!(
        first_move
            .diagnostic
            .as_ref()
            .map(|value| value.runtime_seed),
        Some(1234)
    );
    assert_ne!(
        first_move
            .diagnostic
            .as_ref()
            .map(|value| value.action_seed),
        first_click
            .diagnostic
            .as_ref()
            .map(|value| value.action_seed)
    );

    let mut replay = BehaviorRuntimeV2::with_runtime_seed(profile.clone(), policy, 1234).unwrap();
    assert_eq!(
        first_move,
        replay
            .pointer_trajectory((0, 0), (100, 0), None, true, None)
            .unwrap()
    );
    assert_eq!(
        first_click,
        replay.click_plan(MouseButton::Left, true, None).unwrap()
    );

    let unseeded_policy = BehaviorPolicy {
        enabled: true,
        timing_strength: 1.0,
        pointer_path_strength: 1.0,
        pause_strength: 1.0,
        seed: None,
        ..BehaviorPolicy::default()
    };
    let mut unseeded_a = BehaviorRuntimeV2::new(profile.clone(), unseeded_policy.clone()).unwrap();
    let mut unseeded_b = BehaviorRuntimeV2::new(profile, unseeded_policy).unwrap();
    assert_ne!(unseeded_a.runtime_seed(), unseeded_b.runtime_seed());
    let unseeded_a_move = unseeded_a
        .pointer_trajectory((0, 0), (100, 0), None, true, None)
        .unwrap();
    let unseeded_b_move = unseeded_b
        .pointer_trajectory((0, 0), (100, 0), None, true, None)
        .unwrap();
    assert_ne!(
        unseeded_a_move.diagnostic.unwrap().action_seed,
        unseeded_b_move.diagnostic.unwrap().action_seed
    );
}

#[test]
fn fallback_diagnostics_never_claim_training() {
    let profile = varied_profile();
    let policy = BehaviorPolicy {
        enabled: true,
        timing_strength: 1.0,
        pointer_path_strength: 1.0,
        pause_strength: 1.0,
        correction_strength: 1.0,
        ..BehaviorPolicy::default()
    };

    let mut pointer_runtime =
        BehaviorRuntimeV2::with_runtime_seed(profile.clone(), policy.clone(), 91).unwrap();
    let parent_fallback = pointer_runtime
        .pointer_trajectory((0, 0), (100, 100), None, true, None)
        .unwrap();
    let pointer_diagnostic = parent_fallback.diagnostic.as_ref().unwrap();
    assert!(pointer_diagnostic.fallback_level > 0);
    assert!(pointer_diagnostic.fallback_reason.is_some());
    assert!(!pointer_diagnostic.trained);

    let mut click_runtime =
        BehaviorRuntimeV2::with_runtime_seed(profile.clone(), policy.clone(), 91).unwrap();
    let context_fallback = click_runtime
        .click_plan(MouseButton::Left, false, None)
        .unwrap();
    let click_diagnostic = context_fallback.diagnostic.as_ref().unwrap();
    assert_eq!(context_fallback.fallback_level, 2);
    assert_eq!(
        context_fallback.fallback_reason.as_deref(),
        Some("click_context_fallback")
    );
    assert!(!click_diagnostic.trained);
    assert!(click_diagnostic.coverage > 0.0);

    let mut insufficient = profile;
    for bucket in &mut insufficient.click_model.buckets {
        bucket.valid_sample_count = 1;
    }
    let mut insufficient_runtime =
        BehaviorRuntimeV2::with_runtime_seed(insufficient, policy, 91).unwrap();
    let exact_insufficient = insufficient_runtime
        .click_plan(MouseButton::Left, true, None)
        .unwrap();
    assert_eq!(exact_insufficient.fallback_level, 1);
    assert_eq!(
        exact_insufficient.fallback_reason.as_deref(),
        Some("insufficient_samples_in_exact_click_bucket")
    );
    assert!(!exact_insufficient.diagnostic.as_ref().unwrap().trained);
}

#[test]
fn correction_strength_and_target_width_bound_runtime_corrections() {
    let mut profile = varied_profile();
    let bucket = profile
        .pointer_model
        .buckets
        .iter_mut()
        .find(|bucket| bucket.key.followed_by_click)
        .expect("varied profile should have a click-associated bucket");
    bucket.exemplars = vec![PointerFeatures {
        movement_time_ms: 220.0,
        distance_px: 100.0,
        path_length_px: 130.0,
        path_efficiency: 0.76,
        mean_speed: 450.0,
        peak_speed: 800.0,
        time_to_peak_ratio: 0.28,
        acceleration_phase_ratio: 0.28,
        deceleration_phase_ratio: 0.72,
        maximum_lateral_deviation_px: 12.0,
        signed_curvature: 0.1,
        endpoint_dwell_ms: 0.0,
        overshoot_count: 1,
        overshoot_distance_px: 40.0,
        correction_count: 1,
        coverage: 1.0,
    }];

    let context =
        PointerActionContext::new((0, 0), (100, 0), Some(20.0), true, 1.0, Some(7)).unwrap();
    let no_correction = BehaviorPolicy {
        enabled: true,
        timing_strength: 1.0,
        pointer_path_strength: 1.0,
        correction_strength: 0.0,
        ..BehaviorPolicy::default()
    };
    let straight =
        generate_pointer_trajectory_with_policy(&profile, &context, &no_correction, None).unwrap();
    assert!(straight
        .points
        .iter()
        .all(|point| point.x <= 100 && point.x >= 0));

    let correction = BehaviorPolicy {
        enabled: true,
        timing_strength: 1.0,
        pointer_path_strength: 1.0,
        correction_strength: 1.0,
        ..BehaviorPolicy::default()
    };
    let corrected =
        generate_pointer_trajectory_with_policy(&profile, &context, &correction, None).unwrap();
    assert_eq!(
        corrected.points.last().map(|point| (point.x, point.y)),
        Some((100, 0))
    );
    assert!(corrected
        .points
        .iter()
        .all(|point| point.x <= 110 && point.x >= 0));
    assert!(corrected.sampled_features.features.time_to_peak_ratio < 0.5);
    assert!(corrected.diagnostic.is_none());

    let mut no_correction_runtime =
        BehaviorRuntimeV2::with_runtime_seed(profile.clone(), no_correction, 7).unwrap();
    let no_correction_runtime_result = no_correction_runtime
        .pointer_trajectory((0, 0), (100, 0), Some(20.0), true, None)
        .unwrap();
    let no_correction_diagnostic = no_correction_runtime_result.diagnostic.unwrap();
    assert!(!no_correction_diagnostic.overshoot_enabled);
    assert!(!no_correction_diagnostic.correction_enabled);

    let mut correction_runtime =
        BehaviorRuntimeV2::with_runtime_seed(profile, correction, 7).unwrap();
    let correction_runtime_result = correction_runtime
        .pointer_trajectory((0, 0), (100, 0), Some(20.0), true, None)
        .unwrap();
    let correction_diagnostic = correction_runtime_result.diagnostic.unwrap();
    assert!(correction_diagnostic.overshoot_enabled);
    assert!(correction_diagnostic.correction_enabled);
}

#[test]
fn training_without_overshoot_does_not_invent_one() {
    let mut events = Vec::new();
    for episode in 0..4u64 {
        let base = episode * 1_000;
        events.extend([
            mouse_move(base, 0, 0),
            mouse_move(base + 40, 30, 0),
            mouse_move(base + 80, 70, 0),
            mouse_move(base + 140, 100, 0),
        ]);
    }
    let profile = train_behavior_profile(&make_session(events)).unwrap();
    let policy = BehaviorPolicy {
        enabled: true,
        timing_strength: 1.0,
        pointer_path_strength: 1.0,
        correction_strength: 1.0,
        ..BehaviorPolicy::default()
    };
    let mut runtime = BehaviorRuntimeV2::with_runtime_seed(profile, policy, 17).unwrap();
    let trajectory = runtime
        .pointer_trajectory((0, 0), (100, 0), None, false, None)
        .unwrap();
    assert!(trajectory
        .points
        .iter()
        .all(|point| point.x >= 0 && point.x <= 100));
    let diagnostic = trajectory.diagnostic.unwrap();
    assert_eq!(diagnostic.overshoot_count, Some(0));
    assert!(!diagnostic.overshoot_enabled);
    assert!(!diagnostic.correction_enabled);
}

#[test]
fn no_click_bucket_keeps_raw_click_timing_and_reports_fallback() {
    let profile = train_behavior_profile(&make_session(vec![
        mouse_move(0, 0, 0),
        mouse_move(40, 40, 0),
        mouse_move(120, 80, 0),
    ]))
    .unwrap();
    let plan = sample_click_plan(&profile, MouseButton::Left, true, 1.0, Some(4), None).unwrap();
    assert_eq!(plan.pre_click_dwell_ms, 0);
    assert_eq!(plan.hold_ms, 0);
    assert_eq!(plan.post_click_dwell_ms, 0);
    assert_eq!(plan.fallback_level, u8::MAX);
    assert_eq!(
        plan.fallback_reason.as_deref(),
        Some("no_trained_click_bucket")
    );
}

#[test]
fn cancellation_stops_a_generated_trajectory() {
    use std::sync::atomic::AtomicBool;
    let profile = train_behavior_profile(&make_session(vec![
        mouse_move(0, 0, 0),
        mouse_move(20, 50, 0),
        mouse_move(40, 100, 0),
    ]))
    .unwrap();
    let cancelled = AtomicBool::new(true);
    let context = PointerActionContext::new((0, 0), (1_000, 0), None, false, 1.0, Some(1)).unwrap();
    let error = generate_pointer_trajectory(&profile, &context, Some(&cancelled)).unwrap_err();
    assert_eq!(error.code, "behavior_cancelled");
}
