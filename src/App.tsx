import { useEffect, useState } from "react";
import { AppShell } from "./components/AppShell";
import { normalizeRoute, routeHash } from "./lib/routes";
import { HomePage } from "./pages/HomePage";
import { HotkeysPage } from "./pages/HotkeysPage";
import { MacrosPage } from "./pages/MacrosPage";
import { SettingsPage } from "./pages/SettingsPage";
import { TextExpansionPage } from "./pages/TextExpansionPage";
import type { RouteId } from "./types/navigation";

function currentRoute(): RouteId {
  return normalizeRoute(window.location.hash);
}

function useHashRoute(): [RouteId, (nextRoute: RouteId) => void] {
  const [route, setRoute] = useState<RouteId>(currentRoute);

  useEffect(() => {
    if (!window.location.hash)
      window.history.replaceState(null, "", routeHash("home"));
    const handleHashChange = () => setRoute(currentRoute());
    window.addEventListener("hashchange", handleHashChange);
    return () => window.removeEventListener("hashchange", handleHashChange);
  }, []);

  const navigate = (nextRoute: RouteId) => {
    if (nextRoute === route) return;
    window.location.hash = routeHash(nextRoute);
    setRoute(nextRoute);
  };

  return [route, navigate];
}

function pageForRoute(route: RouteId, onNavigate: (route: RouteId) => void) {
  switch (route) {
    case "hotkeys":
      return <HotkeysPage />;
    case "text-expansion":
      return <TextExpansionPage />;
    case "macros":
      return <MacrosPage />;
    case "settings":
      return <SettingsPage />;
    case "home":
    default:
      return <HomePage onNavigate={onNavigate} />;
  }
}

export default function App() {
  const [route, navigate] = useHashRoute();
  return (
    <AppShell route={route} onNavigate={navigate}>
      {pageForRoute(route, navigate)}
    </AppShell>
  );
}
