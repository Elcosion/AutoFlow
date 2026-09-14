import { useEffect, useMemo, useState } from "react";
import { PageHeader } from "../components/PageHeader";
import { toErrorMessage, useAppConfig } from "../lib/config";
import {
  deleteBehaviorProfileV2,
  deleteBehaviorSessionV2,
  exportBehaviorProfileV2,
  exportBehaviorSessionV2,
  generateBehaviorApi,
  getBehaviorRecordingStatus,
  retrainBehaviorProfileV2,
  startBehaviorRecording,
  stopBehaviorRecording,
  type BehaviorApi,
  type BehaviorRecordingStatus,
} from "../lib/tauri";
import type { BehaviorProfileV2 } from "../types/config";

const idleStatus: BehaviorRecordingStatus = {
  active: false,
  captureStarted: false,
  durationMs: 0,
  eventCount: 0,
  keyboardEvents: 0,
  mouseEvents: 0,
  wheelEvents: 0,
  capped: false,
  persistingRawSession: false,
};

function formatDuration(milliseconds: number): string {
  const seconds = Math.floor(milliseconds / 1000);
  const minutes = Math.floor(seconds / 60);
  return minutes > 0
    ? `${minutes}m ${(seconds % 60).toString().padStart(2, "0")}s`
    : `${seconds}s`;
}

function formatNumber(value: number): string {
  return new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 1 }).format(
    value,
  );
}

function qualityLabel(quality: BehaviorProfileV2["coverage"]["quality"]): string {
  return quality === "good"
    ? "良好"
    : quality === "usable"
      ? "可用"
      : "不足";
}

export function BehaviorPage() {
  const { config, loading, saving, error, setError, persist, refresh } =
    useAppConfig();
  const [status, setStatus] = useState(idleStatus);
  const [sessionName, setSessionName] = useState("我的鼠标操作");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [api, setApi] = useState<BehaviorApi | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    setSelectedId(config.activeBehaviorProfileV2Id);
  }, [config.activeBehaviorProfileV2Id]);

  useEffect(() => {
    let active = true;
    const refreshStatus = () => {
      void getBehaviorRecordingStatus()
        .then((next) => {
          if (active) setStatus(next);
        })
        .catch(() => undefined);
    };
    refreshStatus();
    const timer = window.setInterval(refreshStatus, 500);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, []);

  const selectedProfile = useMemo(
    () =>
      config.behaviorProfilesV2.find((profile) => profile.id === selectedId) ??
      null,
    [config.behaviorProfilesV2, selectedId],
  );

  const showNotice = (message: string) => {
    setNotice(message);
    window.setTimeout(() => setNotice(null), 2400);
  };

  const start = async () => {
    try {
      await startBehaviorRecording(sessionName);
      showNotice("已开始采集原始操作，请在目标程序中完成一组鼠标移动和点击");
    } catch (reason) {
      setError(toErrorMessage(reason));
    }
  };

  const stop = async () => {
    try {
      const profile = await stopBehaviorRecording();
      await refresh();
      setSelectedId(profile.id);
      showNotice("V2 会话与去敏行为档案已分开保存");
    } catch (reason) {
      setError(toErrorMessage(reason));
    }
  };

  const updatePolicy = async (patch: Partial<typeof config.behaviorPolicy>) => {
    try {
      await persist({
        ...config,
        behaviorPolicy: { ...config.behaviorPolicy, ...patch },
      });
      showNotice("运行时行为策略已更新");
    } catch {
      // useAppConfig exposes the structured error on the page.
    }
  };

  const updateRetention = async (retainBehaviorRecords: boolean) => {
    try {
      await persist({ ...config, retainBehaviorRecords });
      showNotice(
        retainBehaviorRecords
          ? "已开启原始行为记录，V2 训练会保留可复核会话"
          : "已关闭原始行为记录，V2 仍会训练但不保留原始会话",
      );
    } catch {
      // useAppConfig exposes the structured error on the page.
    }
  };

  const selectProfile = (profile: BehaviorProfileV2) => {
    setSelectedId(profile.id);
    void persist({
      ...config,
      activeBehaviorProfileV2Id: profile.id,
      behaviorPolicy: { ...config.behaviorPolicy, profileId: profile.id },
    }).then(() => showNotice("当前宏默认档案已切换"));
  };

  const downloadText = (fileName: string, content: string) => {
    const blob = new Blob([content], { type: "application/json;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = fileName;
    link.click();
    window.setTimeout(() => URL.revokeObjectURL(url), 0);
  };

  const exportProfile = async (profile: BehaviorProfileV2) => {
    try {
      downloadText(`${profile.id}.json`, await exportBehaviorProfileV2(profile.id));
      showNotice("V2 Profile JSON 已导出");
    } catch (reason) {
      setError(toErrorMessage(reason));
    }
  };

  const deleteProfile = async (profile: BehaviorProfileV2) => {
    const inUse =
      config.activeBehaviorProfileV2Id === profile.id ||
      config.behaviorPolicy.profileId === profile.id ||
      config.macros.some(
        (macro) => macro.behaviorPolicy?.profileId === profile.id,
      );
    const message = inUse
      ? "该 V2 Profile 仍被全局策略或宏引用，确认后会清除引用并切回全局策略。继续删除吗？"
      : "确认删除这个 V2 Profile 吗？原始 Session 不会因此自动删除。";
    if (!window.confirm(message)) return;
    try {
      const next = await deleteBehaviorProfileV2(profile.id, true);
      await refresh();
      setSelectedId(
        next.activeBehaviorProfileV2Id ?? next.behaviorProfilesV2[0]?.id ?? null,
      );
      showNotice("V2 Profile 已删除");
    } catch (reason) {
      setError(toErrorMessage(reason));
    }
  };

  const exportSession = async (sessionId: string) => {
    try {
      downloadText(`${sessionId}.json`, await exportBehaviorSessionV2(sessionId));
      showNotice("原始 Session JSON 已导出");
    } catch (reason) {
      setError(toErrorMessage(reason));
    }
  };

  const deleteSession = async (sessionId: string) => {
    const inUse = config.behaviorProfilesV2.some((profile) =>
      profile.sourceSessionIds.includes(sessionId),
    );
    const message = inUse
      ? "该 Session 仍是 Profile 的训练来源，确认后会删除原始 Session 并将关联 Profile 标记为仅保留模型。继续吗？"
      : "确认删除这个原始 Session 吗？该操作不会删除 Profile。";
    if (!window.confirm(message)) return;
    try {
      await deleteBehaviorSessionV2(sessionId, true);
      await refresh();
      showNotice("原始 Session 已删除");
    } catch (reason) {
      setError(toErrorMessage(reason));
    }
  };

  const retrainSession = async (sessionId: string) => {
    try {
      const profile = await retrainBehaviorProfileV2(sessionId);
      await refresh();
      setSelectedId(profile.id);
      showNotice("已从保留的 Session 重训 V2 Profile");
    } catch (reason) {
      setError(toErrorMessage(reason));
    }
  };

  const createApi = async () => {
    if (!selectedProfile) return;
    try {
      setApi(await generateBehaviorApi(selectedProfile.id));
      showNotice("已生成原生 Rhai V2 API 示例");
    } catch (reason) {
      setError(toErrorMessage(reason));
    }
  };

  const copyApi = async () => {
    if (!api) return;
    try {
      await navigator.clipboard.writeText(api.source);
      showNotice("API 示例已复制");
    } catch {
      setError("剪贴板不可用，请手动复制 API 示例");
    }
  };

  return (
    <div className="page-stack behavior-page">
      <PageHeader
        eyebrow="V2 行为训练"
        title="鼠标移动与点击模型"
        description="保存原始训练会话，提取可解释的运动学特征，并按距离、方向和点击上下文生成轨迹。"
      />

      {error ? (
        <div className="error-banner">
          <strong>{error}</strong>
          <button onClick={() => setError(null)} type="button">
            关闭
          </button>
        </div>
      ) : null}
      {notice ? <div className="success-banner">✓ {notice}</div> : null}

      <div className="behavior-grid">
        <section className="settings-card">
          <div className="settings-card-heading">
            <div>
              <span className="editor-kicker">原始会话</span>
              <h2>{status.active ? "正在采集" : "采集一组训练数据"}</h2>
            </div>
            <span className={`behavior-status ${status.active ? "is-live" : ""}`}>
              <i /> {status.active ? "采集中" : "空闲"}
            </span>
          </div>
          <div className="form-section">
            <label>
              <span>会话名称</span>
              <input
                disabled={status.active}
                maxLength={64}
                onChange={(event) => setSessionName(event.target.value)}
                value={sessionName}
              />
            </label>
          </div>
          <div className="behavior-recording-actions">
            {status.active ? (
              <button className="button button-danger" onClick={() => void stop()} type="button">
                停止并训练 V2 档案
              </button>
            ) : (
              <button
                className="button button-primary"
                disabled={loading || !sessionName.trim()}
                onClick={() => void start()}
                type="button"
              >
                开始采集
              </button>
            )}
            <span className="form-help">建议包含多段移动、停顿、移动后点击和不同距离。</span>
          </div>
          <label className="behavior-retention-toggle">
            <input
              checked={config.retainBehaviorRecords}
              disabled={status.active || saving}
              onChange={(event) => void updateRetention(event.target.checked)}
              type="checkbox"
            />
            <span>
              持久化原始行为会话（关闭后仍可训练 V2，只是不保留可复核 rawEvents）
            </span>
          </label>
          <div className="behavior-metric-grid">
            <div><strong>{formatDuration(status.durationMs)}</strong><span>会话时长</span></div>
            <div><strong>{formatNumber(status.eventCount)}</strong><span>原始事件</span></div>
            <div><strong>{formatNumber(status.mouseEvents)}</strong><span>鼠标事件</span></div>
            <div><strong>{formatNumber(status.keyboardEvents)}</strong><span>键盘事件</span></div>
            <div><strong>{formatNumber(status.wheelEvents)}</strong><span>滚轮事件</span></div>
          </div>
          <p className="form-help behavior-privacy-note">
            录制期间始终在有界内存中采集事件用于训练；此开关只决定是否将 V2 session 写入磁盘。BehaviorProfileV2 不包含 rawEvents。
          </p>
        </section>

        <section className="settings-card">
          <div className="settings-card-heading">
            <div>
              <span className="editor-kicker">运行时策略</span>
              <h2>绑定 V2 行为档案</h2>
            </div>
            <button
              aria-pressed={config.behaviorPolicy.enabled}
              className={`large-toggle ${config.behaviorPolicy.enabled ? "on" : ""}`}
              disabled={!selectedProfile || status.active}
              onClick={() => void updatePolicy({ enabled: !config.behaviorPolicy.enabled })}
              type="button"
            ><i /></button>
          </div>
          <div className="form-section">
            <label>
              <span>Timing strength：{Math.round(config.behaviorPolicy.timingStrength * 100)}%</span>
              <input
                max="1" min="0" step="0.05" type="range"
                value={config.behaviorPolicy.timingStrength}
                onChange={(event) => void updatePolicy({ timingStrength: Number(event.target.value) })}
              />
            </label>
            <label>
              <span>Pointer path strength：{Math.round(config.behaviorPolicy.pointerPathStrength * 100)}%</span>
              <input
                max="1" min="0" step="0.05" type="range"
                value={config.behaviorPolicy.pointerPathStrength}
                onChange={(event) => void updatePolicy({ pointerPathStrength: Number(event.target.value) })}
              />
            </label>
            <label>
              <span>Pause strength：{Math.round(config.behaviorPolicy.pauseStrength * 100)}%</span>
              <input
                max="1" min="0" step="0.05" type="range"
                value={config.behaviorPolicy.pauseStrength}
                onChange={(event) => void updatePolicy({ pauseStrength: Number(event.target.value) })}
              />
            </label>
            <label>
              <span>Correction strength：{Math.round(config.behaviorPolicy.correctionStrength * 100)}%</span>
              <input
                max="1" min="0" step="0.05" type="range"
                value={config.behaviorPolicy.correctionStrength}
                onChange={(event) => void updatePolicy({ correctionStrength: Number(event.target.value) })}
              />
            </label>
            <label>
              <span>Speed scale：{config.behaviorPolicy.speedScale.toFixed(2)}×</span>
              <input
                max="4" min="0.1" step="0.05" type="range"
                value={config.behaviorPolicy.speedScale}
                onChange={(event) => void updatePolicy({ speedScale: Number(event.target.value) })}
              />
            </label>
            <label>
              <span>固定随机种子（留空为每次会话随机）</span>
              <input
                inputMode="numeric"
                min="0"
                onChange={(event) => {
                  const value = event.target.value.trim();
                  void updatePolicy({
                    seed: value === "" ? undefined : Math.max(0, Math.floor(Number(value) || 0)),
                  });
                }}
                placeholder="session seed"
                type="number"
                value={config.behaviorPolicy.seed ?? ""}
              />
            </label>
          </div>
          <p className="form-help">0 强度保持原始输入路径；策略绑定在宏上时优先使用宏设置。targetWidth 仅是运行时目标宽度提示，本版训练采集尚未获得真实目标几何，因此训练 bucket 的宽度通常为 unknown。</p>
          <div className="behavior-profile-list">
            <div className="behavior-list-heading">
              <strong>V2 模型档案</strong><span>{config.behaviorProfilesV2.length} 个</span>
            </div>
            {config.behaviorProfilesV2.length === 0 ? (
              <div className="behavior-empty">完成一次采集后，V2 档案会显示在这里。</div>
            ) : (
              config.behaviorProfilesV2.map((profile) => (
                <button
                  className={`behavior-profile-row ${selectedId === profile.id ? "is-selected" : ""}`}
                  key={profile.id}
                  onClick={() => selectProfile(profile)}
                  type="button"
                >
                  <strong>{profile.name}</strong>
                  <span>{qualityLabel(profile.coverage.quality)} · {profile.coverage.validPointerEpisodeCount} 条有效轨迹 · {formatNumber(profile.coverage.rawEventCount)} 个事件</span>
                </button>
              ))
            )}
          </div>
        </section>
      </div>

      {selectedProfile ? (
        <section className="settings-card">
          <div className="settings-card-heading">
            <div>
              <span className="editor-kicker">模型质量</span>
              <h2>{selectedProfile.name} · {qualityLabel(selectedProfile.coverage.quality)}</h2>
            </div>
            <span className="form-help">
              {selectedProfile.sourceRetention === "persisted" ? "已保留原始 Session" : "仅保留去敏 Profile"} · 真实训练数据：{selectedProfile.coverage.validPointerEpisodeCount} 条
            </span>
          </div>
          <div className="behavior-metric-grid">
            <div><strong>{selectedProfile.coverage.pointerEpisodeCount}</strong><span>切分轨迹</span></div>
            <div><strong>{selectedProfile.coverage.clickAssociatedPointerEpisodeCount}</strong><span>与点击关联轨迹</span></div>
            <div><strong>{selectedProfile.coverage.clickEpisodeCount}</strong><span>点击片段</span></div>
            <div><strong>{selectedProfile.coverage.bucketCoverage.length}</strong><span>距离/方向 buckets</span></div>
            <div><strong>{selectedProfile.clickModel.buckets.length}</strong><span>点击时序 buckets</span></div>
            <div><strong>{selectedProfile.coverage.discardedEventCount}</strong><span>丢弃事件</span></div>
          </div>
          <p className="form-help">
            模型阈值：每个 bucket 至少 {selectedProfile.modelConfig.minBucketSamples} 个样本；质量等级按 {selectedProfile.modelConfig.minQualityEpisodes}/{selectedProfile.modelConfig.usableQualityEpisodes}/{selectedProfile.modelConfig.goodQualityEpisodes} 条有效轨迹计算。运行时 fallbackLevel 与 fallbackReason 只反映请求上下文，不会伪装成训练覆盖率。
          </p>
          <div className="behavior-profile-list">
            <div className="behavior-list-heading"><strong>Bucket coverage</strong><span>运行时默认回退不会计入训练覆盖率</span></div>
            {selectedProfile.coverage.bucketCoverage.map((bucket) => (
              <div className="behavior-profile-row" key={bucket.bucket}>
                <strong>{bucket.bucket}</strong>
                <span>{bucket.validSampleCount} 条有效样本 · {Math.round(bucket.coverage * 100)}% coverage · 训练 fallbackLevel {bucket.fallbackLevel}</span>
              </div>
            ))}
          </div>
          <div className="behavior-profile-list">
            <div className="behavior-list-heading"><strong>Click timing model</strong><span>{selectedProfile.clickModel.buckets.length} 个 button/context buckets</span></div>
            {selectedProfile.clickModel.buckets.map((bucket) => (
              <div className="behavior-profile-row" key={`${bucket.button}-${bucket.followedByMove}`}>
                <strong>{bucket.button} · afterMove={bucket.followedByMove ? "true" : "false"}</strong>
                <span>{bucket.validSampleCount} 条有效样本 · 训练 fallbackLevel {bucket.fallbackLevel}</span>
              </div>
            ))}
          </div>
          {Object.keys(selectedProfile.coverage.discardedReasons).length > 0 ? (
            <p className="form-help">
              丢弃原因：{Object.entries(selectedProfile.coverage.discardedReasons).map(([reason, count]) => `${reason}=${count}`).join("，")}
            </p>
          ) : null}
          <div className="behavior-data-actions">
            <span className="form-help">运行时 fallback 会在结果中带有 fallbackReason。</span>
            <button className="button button-secondary" onClick={() => void createApi()} type="button">生成 Rhai API</button>
            <button className="button button-secondary" onClick={() => void exportProfile(selectedProfile)} type="button">导出 Profile</button>
            <button className="danger-link" onClick={() => void deleteProfile(selectedProfile)} type="button">删除 Profile</button>
          </div>
        </section>
      ) : null}

      <section className="settings-card">
        <div className="settings-card-heading">
          <div>
            <span className="editor-kicker">原始训练 Session</span>
            <h2>可复核数据 · {config.behaviorSessionsV2.length} 个</h2>
          </div>
          <span className="form-help">仅显示持久化 raw events；关闭保留开关的录制不会出现在这里。</span>
        </div>
        {config.behaviorSessionsV2.length === 0 ? (
          <div className="behavior-empty">当前没有保留的原始 Session。Profile 仍可以来自关闭持久化后的内存训练。</div>
        ) : (
          <div className="behavior-profile-list">
            {config.behaviorSessionsV2.map((session) => (
              <div className="behavior-profile-row" key={session.id}>
                <div>
                  <strong>{session.name}</strong>
                  <span>{session.id} · {formatDuration(session.durationMs)} · {formatNumber(session.rawEvents.length)} 个 raw events</span>
                </div>
                <div className="behavior-data-actions">
                  <button className="button button-secondary" onClick={() => void exportSession(session.id)} type="button">导出</button>
                  <button className="button button-secondary" onClick={() => void retrainSession(session.id)} type="button">重训</button>
                  <button className="danger-link" onClick={() => void deleteSession(session.id)} type="button">删除</button>
                </div>
              </div>
            ))}
          </div>
        )}
      </section>

      {api ? (
        <section className="settings-card behavior-api-card">
          <div className="settings-card-heading">
            <h2>原生 Rhai V2 API</h2>
            <button className="button button-primary" onClick={() => void copyApi()} type="button">复制</button>
          </div>
          <pre className="behavior-api-source">{api.source}</pre>
        </section>
      ) : null}

      {saving ? <span className="floating-saving">正在保存…</span> : null}
    </div>
  );
}
