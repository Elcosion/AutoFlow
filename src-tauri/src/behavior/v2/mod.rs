//! Version 2 of the behavior pipeline.
//!
//! The module is intentionally separate from the old aggregate profile code:
//! capture data, segmentation, feature extraction, modeling, sampling, and
//! input execution have different ownership and validation boundaries.

mod events;
mod features;
mod model;
mod policy;
mod runtime;
mod sampler;
mod segment;
mod session;
mod validation;

pub use events::{
    ClickEpisode, DiscardedEvent, PointerMoveEpisode, PointerSample, SegmentationConfig,
    SegmentationResult,
};
pub use features::{
    extract_pointer_features, training_quality_rejection_reason, PointerFeatures,
    MAX_TRAINING_PATH_RATIO, MIN_TRAINING_PATH_EFFICIENCY,
};
pub use model::{
    train_behavior_profile, train_behavior_profile_with_retention, BehaviorModelConfig,
    BehaviorProfileV2, BucketCoverage, ClickBucketModel, ClickModel, CoverageSummary,
    DirectionBucket, DistanceBucket, FeatureDistribution, ModelQuality, PointerBucket,
    PointerBucketModel, PointerFeatureDistributions, PointerModel, SourceRetention,
    TargetWidthBucket,
};
pub use policy::BehaviorPolicy;
pub use runtime::{
    generate_pointer_trajectory, generate_pointer_trajectory_with_policy, sample_click_plan,
    sample_click_plan_with_policy, BehaviorActionDiagnostic, BehaviorRuntimeV2, ClickPlan,
    PointerActionContext, PointerTrajectory, PointerTrajectoryPoint, SampledPointerFeatures,
};
pub use segment::{segment_behavior_events, segment_mouse_actions};
pub use session::{
    BehaviorCaptureMetadata, BehaviorProfileV2File, BehaviorSessionFile, BehaviorSessionV2,
};
pub use validation::{
    validate_coordinate, validate_finite, V2_MAX_COORDINATE_ABS, V2_MAX_SESSION_EVENTS,
};

pub const BEHAVIOR_V2_API_VERSION: u32 = 2;

#[cfg(test)]
mod tests;
