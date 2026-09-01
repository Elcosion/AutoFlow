import { useAppConfig } from "../lib/config";
import type { HotkeyRule, MacroRule, TextExpansionRule } from "../types/config";

type HomeRoute = "hotkeys" | "text-expansion" | "macros";

type HomePageProps = {
  onNavigate: (route: HomeRoute) => void;
};

function hotkeySummary(rule: HotkeyRule) {
  return `${rule.triggerKeys.join(" + ")} → ${
    rule.action.type === "launch" ? "启动程序" : rule.action.target
  }`;
}

function textSummary(rule: TextExpansionRule) {
  const replacement = rule.sensitive
    ? "••••••••"
    : rule.replacement || "未填写内容";
  return `${rule.abbreviation} → ${replacement}`;
}

function macroSummary(rule: MacroRule) {
  return `${rule.triggerKeys.join(" + ")} · ${rule.steps.length} 步 · ${
    rule.mode === "once"
      ? "单次"
      : rule.mode === "repeat"
        ? `${rule.repeatCount} 次`
        : rule.mode === "hold"
          ? "按住循环"
          : "开关循环"
  }`;
}

type ControlColumnProps = {
  icon: string;
  title: string;
  route: HomeRoute;
  rules: Array<HotkeyRule | TextExpansionRule | MacroRule>;
  enabledCount: number;
  emptyLabel: string;
  onNavigate: (route: HomeRoute) => void;
  onToggle: (id: string) => void;
  summary: (rule: HotkeyRule | TextExpansionRule | MacroRule) => string;
};

function ControlColumn({
  icon,
  title,
  route,
  rules,
  enabledCount,
  emptyLabel,
  onNavigate,
  onToggle,
  summary,
}: ControlColumnProps) {
  return (
    <section className="home-control-column">
      <div className="home-control-header">
        <button
          className="home-control-title"
          onClick={() => onNavigate(route)}
          type="button"
        >
          <span className="dashboard-card-icon">{icon}</span>
          <span>
            <strong>{title}</strong>
            <small>
              {enabledCount} / {rules.length} 条启用
            </small>
          </span>
          <span className="card-arrow">→</span>
        </button>
      </div>
      <div className="home-control-list">
        {rules.length === 0 ? (
          <div className="home-control-empty">
            <span>{emptyLabel}</span>
            <button
              className="text-button"
              onClick={() => onNavigate(route)}
              type="button"
            >
              去添加 →
            </button>
          </div>
        ) : (
          rules.map((rule) => (
            <div className="home-control-row" key={rule.id}>
              <button
                className="home-control-name"
                onClick={() => onNavigate(route)}
                type="button"
              >
                <strong>{rule.name}</strong>
                <small>{summary(rule)}</small>
              </button>
              <button
                aria-label={`${rule.name}${rule.enabled ? "停用" : "启用"}`}
                aria-pressed={rule.enabled}
                className={`mini-toggle ${rule.enabled ? "on" : ""}`}
                onClick={() => onToggle(rule.id)}
                type="button"
              >
                <i />
              </button>
            </div>
          ))
        )}
      </div>
    </section>
  );
}

export function HomePage({ onNavigate }: HomePageProps) {
  const { config, error, saving, setError, persist } = useAppConfig();
  const enabledHotkeys = config.hotkeys.filter((rule) => rule.enabled).length;
  const enabledText = config.textExpansions.filter(
    (rule) => rule.enabled,
  ).length;
  const enabledMacros = config.macros.filter((macro) => macro.enabled).length;

  const toggleHotkey = (id: string) => {
    void persist({
      ...config,
      hotkeys: config.hotkeys.map((rule) =>
        rule.id === id ? { ...rule, enabled: !rule.enabled } : rule,
      ),
    }).catch(() => undefined);
  };

  const toggleTextExpansion = (id: string) => {
    void persist({
      ...config,
      textExpansions: config.textExpansions.map((rule) =>
        rule.id === id ? { ...rule, enabled: !rule.enabled } : rule,
      ),
    }).catch(() => undefined);
  };

  const toggleMacro = (id: string) => {
    void persist({
      ...config,
      macros: config.macros.map((rule) =>
        rule.id === id ? { ...rule, enabled: !rule.enabled } : rule,
      ),
    }).catch(() => undefined);
  };

  return (
    <div className="home-page">
      <section className="hero-simple">
        <div className="hero-copy">
          <div className="eyebrow">工作台</div>
          <h1>AutoFlow</h1>
          <p>把重复操作收进三个清晰、随时可控的工作区。</p>
        </div>
        <nav aria-label="功能入口" className="home-navigation">
          <button
            className="home-navigation-button"
            onClick={() => onNavigate("hotkeys")}
            type="button"
          >
            <span>
              <strong>快捷键</strong>
              <small>{enabledHotkeys} 条正在工作</small>
            </span>
            <b>→</b>
          </button>
          <button
            className="home-navigation-button"
            onClick={() => onNavigate("text-expansion")}
            type="button"
          >
            <span>
              <strong>文本扩展</strong>
              <small>{enabledText} 条正在工作</small>
            </span>
            <b>→</b>
          </button>
          <button
            className="home-navigation-button"
            onClick={() => onNavigate("macros")}
            type="button"
          >
            <span>
              <strong>宏录制</strong>
              <small>{enabledMacros} 个正在工作</small>
            </span>
            <b>→</b>
          </button>
        </nav>
      </section>

      {error ? (
        <div className="error-banner">
          <strong>{error}</strong>
          <button onClick={() => setError(null)} type="button">
            知道了
          </button>
        </div>
      ) : null}

      <section className="home-section">
        <div className="section-heading">
          <div>
            <h2>规则状态</h2>
            <p>{saving ? "正在保存开关状态…" : "直接在这里启用或停用规则"}</p>
          </div>
        </div>
        <div className="home-control-grid">
          <ControlColumn
            emptyLabel="还没有快捷键"
            enabledCount={enabledHotkeys}
            icon="⌘"
            onNavigate={onNavigate}
            onToggle={toggleHotkey}
            route="hotkeys"
            rules={config.hotkeys}
            summary={(rule) => hotkeySummary(rule as HotkeyRule)}
            title="快捷键"
          />
          <ControlColumn
            emptyLabel="还没有文本扩展"
            enabledCount={enabledText}
            icon="Aa"
            onNavigate={onNavigate}
            onToggle={toggleTextExpansion}
            route="text-expansion"
            rules={config.textExpansions}
            summary={(rule) => textSummary(rule as TextExpansionRule)}
            title="文本扩展"
          />
          <ControlColumn
            emptyLabel="还没有宏"
            enabledCount={enabledMacros}
            icon="◉"
            onNavigate={onNavigate}
            onToggle={toggleMacro}
            route="macros"
            rules={config.macros}
            summary={(rule) => macroSummary(rule as MacroRule)}
            title="宏录制"
          />
        </div>
      </section>
    </div>
  );
}
