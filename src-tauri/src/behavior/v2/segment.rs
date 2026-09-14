use crate::behavior::BehaviorEvent;
use crate::AppError;
use std::collections::HashMap;

use super::events::{
    behavior_event_parts, mouse_button_from_id, ClickEpisode, DiscardedEvent, PointerMoveEpisode,
    PointerSample, SegmentationConfig, SegmentationResult,
};

/// Split the raw hook stream into pointer movements and click episodes.
///
/// The splitter intentionally does not infer a movement from a whole session:
/// a dwell gap, a click, or another input kind closes the current movement.
pub fn segment_mouse_actions(
    events: &[BehaviorEvent],
    config: &SegmentationConfig,
) -> Result<SegmentationResult, AppError> {
    config.validate()?;
    if events.len() > config.max_event_count {
        return Err(AppError::invalid(
            "behavior_v2_event_limit",
            "原始事件数量超过动作切分安全上限",
        ));
    }

    let mut pointer_moves = Vec::<PointerMoveEpisode>::new();
    let mut clicks = Vec::<ClickEpisode>::new();
    let mut discarded_events = Vec::new();
    let mut current_samples = Vec::<PointerSample>::new();
    let mut pending_buttons = HashMap::<u8, PendingClick>::new();
    let mut last_click_up = None::<(usize, u64)>;
    let mut last_down_by_button = HashMap::<u8, u64>::new();
    let mut last_mouse_sample = None::<PointerSample>;
    let mut previous_timestamp = None::<u64>;
    let mut accepted_event_count = 0usize;

    for (event_index, event) in events.iter().enumerate() {
        let (timestamp, _) = behavior_event_parts(event);
        if previous_timestamp.is_some_and(|previous| timestamp < previous) {
            return Err(AppError::invalid(
                "behavior_v2_timestamp_order",
                format!("第 {} 个事件的时间戳早于前一个事件", event_index),
            ));
        }
        previous_timestamp = Some(timestamp);

        if let Some((click_index, click_up_at)) = last_click_up {
            if timestamp >= click_up_at {
                if let Some(click) = clicks.get_mut(click_index) {
                    click.post_click_dwell_ms = timestamp.saturating_sub(click_up_at);
                }
                last_click_up = None;
            }
        }

        match event {
            BehaviorEvent::MouseMove { x, y, .. } => {
                if !valid_coordinate(*x, *y, config.max_coordinate_abs) {
                    discarded_events.push(DiscardedEvent {
                        event_index,
                        reason: "coordinate_out_of_range".to_string(),
                    });
                    continue;
                }
                let sample = PointerSample {
                    timestamp_ms: timestamp,
                    x: *x,
                    y: *y,
                };
                if last_mouse_sample.is_some_and(|previous| previous.x == *x && previous.y == *y) {
                    discarded_events.push(DiscardedEvent {
                        event_index,
                        reason: "duplicate_coordinate".to_string(),
                    });
                    continue;
                }
                if let Some(previous) = current_samples.last().copied() {
                    let gap = timestamp.saturating_sub(previous.timestamp_ms);
                    if gap > config.max_event_gap_ms || gap > config.move_end_dwell_ms {
                        finish_current_move(
                            &mut current_samples,
                            &mut pointer_moves,
                            &mut discarded_events,
                            event_index,
                            config,
                            gap,
                        );
                    }
                }
                current_samples.push(sample);
                last_mouse_sample = Some(sample);
                accepted_event_count += 1;
            }
            BehaviorEvent::MouseButton {
                button,
                is_down,
                x,
                y,
                ..
            } => {
                if !valid_coordinate(*x, *y, config.max_coordinate_abs) {
                    discarded_events.push(DiscardedEvent {
                        event_index,
                        reason: "coordinate_out_of_range".to_string(),
                    });
                    continue;
                }
                if mouse_button_from_id(*button).is_none() {
                    discarded_events.push(DiscardedEvent {
                        event_index,
                        reason: "unknown_mouse_button".to_string(),
                    });
                    continue;
                }
                if *is_down {
                    if pending_buttons.contains_key(button) {
                        discarded_events.push(DiscardedEvent {
                            event_index,
                            reason: "overlapping_button_down".to_string(),
                        });
                        continue;
                    }
                    let association = finalize_before_click(
                        &mut current_samples,
                        &mut pointer_moves,
                        &mut discarded_events,
                        event_index,
                        timestamp,
                        config,
                        last_mouse_sample,
                    );
                    let is_double_click = last_down_by_button
                        .get(button)
                        .is_some_and(|previous| timestamp.saturating_sub(*previous) <= 500);
                    last_down_by_button.insert(*button, timestamp);
                    let pre_click_dwell_ms =
                        association.as_ref().map(|(_, dwell)| *dwell).unwrap_or(0);
                    if let Some((move_index, _)) = association {
                        if let Some(pointer_move) = pointer_moves.get_mut(move_index) {
                            pointer_move.followed_by_click = true;
                        }
                    }
                    pending_buttons.insert(
                        *button,
                        PendingClick {
                            button: *button,
                            x: *x,
                            y: *y,
                            clicked_at: timestamp,
                            pre_click_dwell_ms,
                            pointer_move_episode_index: association.map(|(index, _)| index),
                            is_double_click,
                        },
                    );
                    accepted_event_count += 1;
                } else if let Some(pending) = pending_buttons.remove(button) {
                    let Some(button) = mouse_button_from_id(pending.button) else {
                        discarded_events.push(DiscardedEvent {
                            event_index,
                            reason: "unknown_mouse_button".to_string(),
                        });
                        continue;
                    };
                    let click_index = clicks.len();
                    clicks.push(ClickEpisode {
                        button,
                        x: pending.x,
                        y: pending.y,
                        clicked_at: pending.clicked_at,
                        pre_click_dwell_ms: pending.pre_click_dwell_ms,
                        hold_ms: timestamp.saturating_sub(pending.clicked_at),
                        post_click_dwell_ms: 0,
                        pointer_move_episode_index: pending.pointer_move_episode_index,
                        is_double_click: pending.is_double_click,
                    });
                    last_click_up = Some((click_index, timestamp));
                    accepted_event_count += 1;
                } else {
                    discarded_events.push(DiscardedEvent {
                        event_index,
                        reason: "unmatched_button_up".to_string(),
                    });
                }
            }
            BehaviorEvent::Key { .. } | BehaviorEvent::Wheel { .. } => {
                if let Some(previous) = current_samples.last().copied() {
                    let dwell = timestamp.saturating_sub(previous.timestamp_ms);
                    finish_current_move(
                        &mut current_samples,
                        &mut pointer_moves,
                        &mut discarded_events,
                        event_index,
                        config,
                        dwell,
                    );
                }
                accepted_event_count += 1;
            }
        }
    }

    if !current_samples.is_empty() {
        finish_current_move(
            &mut current_samples,
            &mut pointer_moves,
            &mut discarded_events,
            events.len().saturating_sub(1),
            config,
            0,
        );
    }

    for (button, pending) in pending_buttons {
        discarded_events.push(DiscardedEvent {
            event_index: events.len().saturating_sub(1),
            reason: format!("missing_button_up_{button}"),
        });
        let _ = pending;
    }

    if let Some((click_index, click_up_at)) = last_click_up {
        if let Some(click) = clicks.get_mut(click_index) {
            click.post_click_dwell_ms = events
                .last()
                .map(|event| behavior_event_parts(event).0.saturating_sub(click_up_at))
                .unwrap_or(0);
        }
    }

    Ok(SegmentationResult {
        pointer_moves,
        clicks,
        discarded_events,
        accepted_event_count,
    })
}

pub fn segment_behavior_events(
    events: &[BehaviorEvent],
    config: &SegmentationConfig,
) -> Result<SegmentationResult, AppError> {
    segment_mouse_actions(events, config)
}

#[derive(Debug, Clone, Copy)]
struct PendingClick {
    button: u8,
    x: i32,
    y: i32,
    clicked_at: u64,
    pre_click_dwell_ms: u64,
    pointer_move_episode_index: Option<usize>,
    is_double_click: bool,
}

fn finalize_before_click(
    current_samples: &mut Vec<PointerSample>,
    pointer_moves: &mut Vec<PointerMoveEpisode>,
    discarded_events: &mut Vec<DiscardedEvent>,
    event_index: usize,
    click_timestamp: u64,
    config: &SegmentationConfig,
    last_mouse_sample: Option<PointerSample>,
) -> Option<(usize, u64)> {
    let dwell = last_mouse_sample
        .map(|sample| click_timestamp.saturating_sub(sample.timestamp_ms))
        .unwrap_or(0);
    let can_associate = !current_samples.is_empty()
        && dwell <= config.click_association_window_ms
        && current_samples.len() >= config.min_episode_samples;
    let before_len = pointer_moves.len();
    finish_current_move(
        current_samples,
        pointer_moves,
        discarded_events,
        event_index,
        config,
        dwell,
    );
    (can_associate && pointer_moves.len() > before_len).then(|| (pointer_moves.len() - 1, dwell))
}

fn finish_current_move(
    current_samples: &mut Vec<PointerSample>,
    pointer_moves: &mut Vec<PointerMoveEpisode>,
    discarded_events: &mut Vec<DiscardedEvent>,
    event_index: usize,
    config: &SegmentationConfig,
    endpoint_dwell_ms: u64,
) {
    if current_samples.is_empty() {
        return;
    }
    let samples = std::mem::take(current_samples);
    let enough_samples = samples.len() >= config.min_episode_samples;
    let distance = samples
        .first()
        .zip(samples.last())
        .map(|(first, last)| ((last.x - first.x) as f32).hypot((last.y - first.y) as f32))
        .unwrap_or(0.0);
    if !enough_samples {
        discarded_events.push(DiscardedEvent {
            event_index,
            reason: "insufficient_samples".to_string(),
        });
        return;
    }
    if !distance.is_finite() || distance < config.min_episode_distance_px {
        discarded_events.push(DiscardedEvent {
            event_index,
            reason: "episode_too_short".to_string(),
        });
        return;
    }
    match PointerMoveEpisode::from_samples(samples, false, None, endpoint_dwell_ms) {
        Ok(episode) => pointer_moves.push(episode),
        Err(_) => discarded_events.push(DiscardedEvent {
            event_index,
            reason: "invalid_episode".to_string(),
        }),
    }
}

fn valid_coordinate(x: i32, y: i32, maximum: i32) -> bool {
    i64::from(x).abs() <= i64::from(maximum) && i64::from(y).abs() <= i64::from(maximum)
}
