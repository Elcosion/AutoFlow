import { describe, expect, it } from "vitest";
import { normalizeRoute, routeHash } from "./routes";

describe("route helpers", () => {
  it("normalizes valid hash routes", () => {
    expect(normalizeRoute("#/macros")).toBe("macros");
    expect(normalizeRoute("text-expansion")).toBe("text-expansion");
  });

  it("falls back to home for an unknown route", () => {
    expect(normalizeRoute("#/not-a-page")).toBe("home");
    expect(normalizeRoute(undefined)).toBe("home");
  });

  it("creates stable hash links", () => {
    expect(routeHash("settings")).toBe("#/settings");
  });
});
