import type { BehaviorProfileV2 } from "../types/config";

export type BucketTrainingView = {
  ready: boolean;
  sampleCount: number;
  minimumSamples: number;
  expectedFallback: boolean;
  fallbackReason?: string;
};

export function getBucketTrainingView(
  validSampleCount: number,
  minimumSamples: number,
): BucketTrainingView {
  const sampleCount = Number.isFinite(validSampleCount)
    ? Math.max(0, Math.floor(validSampleCount))
    : 0;
  const minimum = Math.max(1, Math.floor(minimumSamples));
  const ready = sampleCount >= minimum;
  return {
    ready,
    sampleCount,
    minimumSamples: minimum,
    expectedFallback: !ready,
    ...(ready ? {} : { fallbackReason: "insufficient_samples" }),
  };
}

export function getBehaviorQualityView(profile: BehaviorProfileV2) {
  const validEpisodeCount = Math.max(
    0,
    Math.floor(profile.coverage.validPointerEpisodeCount),
  );
  const eligibleEpisodeCount = Math.max(
    0,
    Math.floor(profile.coverage.eligibleEpisodeCount),
  );
  const eligibleCoverage = Math.min(
    1,
    Math.max(
      0,
      Number.isFinite(profile.coverage.eligibleCoverage)
        ? profile.coverage.eligibleCoverage
        : validEpisodeCount > 0
          ? eligibleEpisodeCount / validEpisodeCount
          : 0,
    ),
  );
  return {
    quality: profile.coverage.quality,
    validEpisodeCount,
    eligibleEpisodeCount,
    eligibleCoverage,
    minBucketSamples: profile.modelConfig.minBucketSamples,
    minQualityEpisodes: profile.modelConfig.minQualityEpisodes,
    usableQualityEpisodes: profile.modelConfig.usableQualityEpisodes,
    goodQualityEpisodes: profile.modelConfig.goodQualityEpisodes,
  };
}

export function getTargetWidthTrainingView(profile: BehaviorProfileV2) {
  const hasObservedTargetWidth = profile.pointerModel.buckets.some(
    (bucket) => bucket.key.targetWidth !== "unknown",
  );
  return {
    hasObservedTargetWidth,
    message: hasObservedTargetWidth
      ? "训练数据包含目标几何 bucket"
      : "训练阶段未采集目标几何；运行时 targetWidth 只能使用 width wildcard/fallback",
  };
}
