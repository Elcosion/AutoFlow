import { useEffect, useState } from "react";
import { PageHeader } from "../components/PageHeader";
import { useAppConfig } from "../lib/config";
import { emergencyStop } from "../lib/tauri";

export function SettingsPage() {
  const { config, saving, error, setError, persist } = useAppConfig();
  const [notice, setNotice] = useState<string | null>(null);
  const [emergencyStopKey, setEmergencyStopKey] = useState(
    config.emergencyStop,
  );
  useEffect(
    () => setEmergencyStopKey(config.emergencyStop),
    [config.emergencyStop],
  );
  const update = async (patch: Partial<typeof config>, message: string) => {
    try {
      await persist({ ...config, ...patch });
      setNotice(message);
      window.setTimeout(() => setNotice(null), 1800);
      return true;
    } catch {
      /* rendered by hook */
      return false;
    }
  };
  const stopNow = async () => {
    await emergencyStop();
    setNotice("已发送停止信号");
    window.setTimeout(() => setNotice(null), 1800);
  };
  const saveEmergencyStopKey = () => {
    const value = emergencyStopKey.trim().toUpperCase();
    if (value) void update({ emergencyStop: value }, "紧急停止键已保存");
  };
  const setNavigationAutoCollapse = (enabled: boolean) => {
    void update(
      { navigationAutoCollapse: enabled },
      enabled ? "导航栏会在鼠标移出后自动收起" : "导航栏自动收起已关闭",
    ).then((saved) => {
      if (!saved) return;
      window.dispatchEvent(
        new CustomEvent<boolean>("autoflow:navigation-auto-collapse", {
          detail: enabled,
        }),
      );
    });
  };
  return (
    <div className="page-stack">
      <PageHeader title="设置" />
      {error ? (
        <div className="error-banner">
          <strong>{error}</strong>
          <button onClick={() => setError(null)} type="button">
            知道了
          </button>
        </div>
      ) : null}
      {notice ? <div className="success-banner">✓ {notice}</div> : null}
      <div className="settings-grid">
        <section className="settings-card">
          <div className="settings-card-heading">
            <div>
              <span className="editor-kicker">总开关</span>
              <h2>让 AutoFlow 工作</h2>
            </div>
            <button
              className={`large-toggle ${config.globalEnabled ? "on" : ""}`}
              onClick={() =>
                void update(
                  { globalEnabled: !config.globalEnabled },
                  config.globalEnabled ? "全局规则已暂停" : "全局规则已恢复",
                )
              }
              type="button"
              aria-pressed={config.globalEnabled}
            >
              <i />
            </button>
          </div>
          <div
            className={`global-status ${config.globalEnabled ? "active" : "paused"}`}
          >
            <span />
            {config.globalEnabled ? "规则正在工作" : "所有规则已暂停"}
          </div>
        </section>
        <section className="settings-card danger-card">
          <div className="settings-card-heading">
            <div>
              <span className="editor-kicker">安全保护</span>
              <h2>紧急停止</h2>
            </div>
          </div>
          <div className="emergency-row">
            <input
              value={emergencyStopKey}
              onChange={(event) =>
                setEmergencyStopKey(event.target.value.toUpperCase())
              }
              onBlur={saveEmergencyStopKey}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.currentTarget.blur();
                }
              }}
              aria-label="紧急停止键"
            />
            <button
              className="button button-danger"
              onClick={() => void stopNow()}
              type="button"
            >
              立即停止
            </button>
          </div>
          <div className="form-help">
            输入 F12 等键名后按 Enter 或离开输入框保存。
          </div>
        </section>
        <section className="settings-card">
          <div className="settings-card-heading">
            <div>
              <span className="editor-kicker">界面</span>
              <h2>导航栏</h2>
            </div>
            <button
              aria-label="自动收起导航栏"
              aria-pressed={config.navigationAutoCollapse}
              className={`large-toggle ${
                config.navigationAutoCollapse ? "on" : ""
              }`}
              onClick={() =>
                setNavigationAutoCollapse(!config.navigationAutoCollapse)
              }
              type="button"
            >
              <i />
            </button>
          </div>
          <div className="settings-preference-copy">
            <strong>自动收起</strong>
            <span>鼠标移出后收起为图标栏，移回自动展开。</span>
          </div>
        </section>
        <section className="settings-card">
          <div className="settings-card-heading">
            <div>
              <span className="editor-kicker">启动</span>
              <h2>开机自启动</h2>
            </div>
            <button
              aria-label="开机自启动"
              aria-pressed={config.launchAtStartup}
              className={`large-toggle ${config.launchAtStartup ? "on" : ""}`}
              onClick={() =>
                void update(
                  { launchAtStartup: !config.launchAtStartup },
                  config.launchAtStartup
                    ? "已取消开机自启动"
                    : "已设置为开机自启动",
                )
              }
              type="button"
            >
              <i />
            </button>
          </div>
          <div className="settings-preference-copy">
            <strong>登录 Windows 后自动启动</strong>
            <span>仅为当前 Windows 用户写入启动项，可随时在此关闭。</span>
          </div>
        </section>
      </div>
      {saving ? <span className="floating-saving">正在保存…</span> : null}
    </div>
  );
}
