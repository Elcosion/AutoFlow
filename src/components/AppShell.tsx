import { useEffect, useState, type ReactNode } from "react";
import { navItems, type RouteId } from "../types/navigation";
import { emergencyStop, getConfig } from "../lib/tauri";
import { WindowTitlebar } from "./WindowTitlebar";

type AppShellProps = {
  route: RouteId;
  onNavigate: (route: RouteId) => void;
  children: ReactNode;
};

export function AppShell({ route, onNavigate, children }: AppShellProps) {
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [navigationAutoCollapse, setNavigationAutoCollapse] = useState(false);

  useEffect(() => {
    let active = true;
    void getConfig()
      .then((config) => {
        if (!active) return;
        setNavigationAutoCollapse(config.navigationAutoCollapse);
        setSidebarCollapsed(config.navigationAutoCollapse);
      })
      .catch(() => undefined);
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    const handlePreferenceChange = (event: Event) => {
      const nextValue = Boolean((event as CustomEvent<boolean>).detail);
      setNavigationAutoCollapse(nextValue);
      setSidebarCollapsed(nextValue);
    };
    window.addEventListener(
      "autoflow:navigation-auto-collapse",
      handlePreferenceChange,
    );
    return () =>
      window.removeEventListener(
        "autoflow:navigation-auto-collapse",
        handlePreferenceChange,
      );
  }, []);

  const stopNow = () => void emergencyStop();
  return (
    <div className="app-window">
      <WindowTitlebar />
      <div
        className={`app-shell ${sidebarCollapsed ? "sidebar-collapsed" : ""}`}
      >
        <aside
          className="sidebar"
          onMouseEnter={() => {
            if (navigationAutoCollapse) setSidebarCollapsed(false);
          }}
          onMouseLeave={() => {
            if (navigationAutoCollapse) setSidebarCollapsed(true);
          }}
        >
          <div className="brand-block">
            <div className="brand-mark" aria-hidden="true">
              <span />
              <span />
              <span />
            </div>
            <div>
              <div className="brand-name">AutoFlow</div>
            </div>
            <button
              aria-label={sidebarCollapsed ? "展开导航栏" : "收起导航栏"}
              aria-pressed={sidebarCollapsed}
              className="sidebar-collapse-toggle"
              onClick={() => setSidebarCollapsed((current) => !current)}
              title={sidebarCollapsed ? "展开导航栏" : "收起导航栏"}
              type="button"
            >
              {sidebarCollapsed ? "→" : "←"}
            </button>
          </div>
          <nav className="main-nav" aria-label="主导航">
            <div className="nav-label">工作台</div>
            {navItems.slice(0, 5).map((item) => (
              <button
                className={`nav-item ${route === item.id ? "is-active" : ""}`}
                key={item.id}
                onClick={() => onNavigate(item.id)}
                type="button"
              >
                <span className="nav-icon" aria-hidden="true">
                  {item.icon}
                </span>
                <span className="nav-copy">
                  <span>{item.label}</span>
                </span>
              </button>
            ))}
            <div className="nav-label nav-label-settings">偏好</div>
            <button
              className={`nav-item ${route === "settings" ? "is-active" : ""}`}
              onClick={() => onNavigate("settings")}
              type="button"
            >
              <span className="nav-icon" aria-hidden="true">
                ⚙
              </span>
              <span className="nav-copy">
                <span>设置</span>
              </span>
            </button>
          </nav>
          <div className="sidebar-safety-action">
            <button className="topbar-stop" onClick={stopNow} type="button">
              <span>!</span> F12 停止
            </button>
          </div>
        </aside>
        <main className="main-content">
          <div className="page-content">{children}</div>
        </main>
      </div>
    </div>
  );
}
