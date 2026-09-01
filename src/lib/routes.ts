import type { RouteId } from "../types/navigation";

const routeIds: RouteId[] = [
  "home",
  "hotkeys",
  "text-expansion",
  "macros",
  "settings",
];

export function normalizeRoute(value: string | null | undefined): RouteId {
  const candidate = value?.replace(/^#\/?/, "") as RouteId | undefined;
  return candidate && routeIds.includes(candidate) ? candidate : "home";
}

export function routeHash(route: RouteId): string {
  return `#/${route}`;
}
