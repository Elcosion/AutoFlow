import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const html = readFileSync(resolve("index.html"), "utf8");
const css = readFileSync(resolve("src/styles-premium.css"), "utf8");
const markerScript = html.match(
  /<script data-autoflow-window-marker>([\s\S]*?)<\/script>/,
)?.[1];

function markedWindow(search: string) {
  const attributes = new Map<string, string>();
  const run = new Function(
    "window",
    "document",
    "URLSearchParams",
    markerScript ?? "",
  );
  run(
    { location: { search } },
    {
      documentElement: {
        setAttribute(name: string, value: string) {
          attributes.set(name, value);
        },
      },
    },
    URLSearchParams,
  );
  return attributes.get("data-autoflow-window");
}

describe("playback overlay prepaint document marker", () => {
  it("marks only the exact playback-overlay query value", () => {
    expect(markerScript).toBeTruthy();
    expect(markedWindow("?window=playback-overlay")).toBe("playback-overlay");
    expect(markedWindow("?window=playback-overlay&extra=1")).toBe(
      "playback-overlay",
    );
    expect(markedWindow("?window=playback-overlay-extra")).toBeUndefined();
    expect(markedWindow("?window=Playback-Overlay")).toBeUndefined();
    expect(markedWindow("?other=playback-overlay")).toBeUndefined();
    expect(markerScript).not.toContain("removeAttribute");
  });

  it("scopes transparent roots to the marker without changing the main theme or card", () => {
    expect(css).toContain('html[data-autoflow-window="playback-overlay"]');
    expect(css).toContain('html[data-autoflow-window="playback-overlay"] body');
    expect(css).toContain(
      'html[data-autoflow-window="playback-overlay"] #root',
    );
    expect(css).toContain("background-image: none !important");
    expect(css).not.toContain("body.playback-overlay-body {");
    expect(css).toContain("background: #0b1117;");
    expect(css).toContain(".playback-overlay-card {");
    expect(css).toContain("background: rgba(17, 31, 39, 0.94);");
  });
});
