import { describe, expect, it } from "vitest";
import {
  getBehaviorQualityView,
  getBucketTrainingView,
  getTargetWidthTrainingView,
} from "./behaviorView";
import { normalizeConfig } from "./tauri";
import type { BehaviorProfileV2 } from "../types/config";

const profile = {
  id: "profile",
  name: "test",
  apiVersion: 2,
  sourceSessionIds: ["session"],
  createdAtMs: 1,
  sourceRetention: "persisted",
  modelConfig: {
    minBucketSamples: 3,
    maxExemplarsPerBucket: 32,
    minQualityEpisodes: 3,
    usableQualityEpisodes: 8,
    goodQualityEpisodes: 16,
    minBucketCoverage: 0.5,
  },
  coverage: {
    rawEventCount: 100,
    pointerEpisodeCount: 35,
    validPointerEpisodeCount: 35,
    clickAssociatedPointerEpisodeCount: 30,
    clickEpisodeCount: 30,
    discardedEventCount: 2,
    discardedReasons: {},
    bucketCoverage: [
      {
        bucket: "short/e/click=true/width=unknown",
        validSampleCount: 2,
        coverage: 2 / 35,
        fallbackLevel: 1,
        trainingReady: false,
        trainingFallbackReason: "insufficient_samples",
      },
    ],
    quality: "good",
    eligibleEpisodeCount: 27,
    eligibleCoverage: 27 / 35,
    qualityFilteredPointerEpisodeCount: 1,
  },
  pointerModel: {
    buckets: [
      {
        key: {
          distance: "short",
          direction: "e",
          followedByClick: true,
          targetWidth: "unknown",
        },
        validSampleCount: 2,
        coverage: 2 / 35,
        fallbackLevel: 1,
        trainingReady: false,
        trainingFallbackReason: "insufficient_samples",
        exemplars: [],
        features: {},
      },
    ],
    totalEpisodeCount: 35,
    validEpisodeCount: 35,
    discardedEpisodeCount: 0,
  },
  clickModel: { buckets: [], totalClickCount: 0, validClickCount: 0 },
} as unknown as BehaviorProfileV2;

describe("behavior model UI view contracts", () => {
  it("distinguishes sufficient and insufficient bucket samples", () => {
    expect(getBucketTrainingView(2, 3)).toMatchObject({
      ready: false,
      sampleCount: 2,
      minimumSamples: 3,
      expectedFallback: true,
      fallbackReason: "insufficient_samples",
    });
    expect(getBucketTrainingView(3, 3)).toMatchObject({
      ready: true,
      expectedFallback: false,
    });
  });

  it("shows eligible coverage using the aggregate, not one bucket", () => {
    expect(getBehaviorQualityView(profile)).toMatchObject({
      quality: "good",
      validEpisodeCount: 35,
      eligibleEpisodeCount: 27,
      eligibleCoverage: 27 / 35,
    });
  });

  it("makes unknown training geometry explicit", () => {
    expect(getTargetWidthTrainingView(profile)).toEqual({
      hasObservedTargetWidth: false,
      message:
        "训练阶段未采集目标几何；运行时 targetWidth 只能使用 width wildcard/fallback",
    });
  });

  it("normalizes legacy quality and sparse fallback metadata", () => {
    const normalized = normalizeConfig({
      behaviorProfilesV2: [
        {
          ...profile,
          coverage: {
            ...profile.coverage,
            quality: "insufficient",
            eligibleEpisodeCount: 0,
            eligibleCoverage: 0,
          },
        },
      ],
    });
    expect(normalized.behaviorProfilesV2[0].coverage.quality).toBe("usable");
    expect(normalized.behaviorProfilesV2[0].coverage.eligibleEpisodeCount).toBe(
      0,
    );
    expect(
      normalized.behaviorProfilesV2[0].coverage.bucketCoverage[0],
    ).toMatchObject({
      trainingReady: false,
      fallbackLevel: 1,
      trainingFallbackReason: "insufficient_samples",
    });
  });
});
