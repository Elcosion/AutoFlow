export type HotkeyActionType = "remap" | "launch";

export type HotkeyAction = {
  type: HotkeyActionType;
  target: string;
};

export type HotkeyRule = {
  id: string;
  name: string;
  enabled: boolean;
  triggerKeys: string[];
  action: HotkeyAction;
};

export type TextExpansionRule = {
  id: string;
  name: string;
  abbreviation: string;
  replacement: string;
  enabled: boolean;
  caseSensitive: boolean;
  sensitive: boolean;
};

export type MacroMode = "once" | "repeat" | "hold" | "toggle";
export type KeyAction = "down" | "up";
export type MouseButton = "left" | "right" | "middle" | "x1" | "x2";

export type MacroStep =
  | { type: "delay"; durationMs: number; durationMaxMs?: number }
  | { type: "key"; key: string; action: KeyAction }
  | {
      type: "mouseButton";
      button: MouseButton;
      action: KeyAction;
      x: number;
      y: number;
    }
  | { type: "mouseMove"; x: number; y: number }
  | { type: "wheel"; deltaX: number; deltaY: number }
  | { type: "text"; text: string };

export type MacroTarget = {
  processName: string;
};

export type MacroRule = {
  id: string;
  name: string;
  enabled: boolean;
  triggerKeys: string[];
  mode: MacroMode;
  repeatCount: number;
  speed: number;
  target?: MacroTarget;
  steps: MacroStep[];
};

export type AppConfig = {
  schemaVersion: number;
  globalEnabled: boolean;
  emergencyStop: string;
  navigationAutoCollapse: boolean;
  launchAtStartup: boolean;
  hotkeys: HotkeyRule[];
  textExpansions: TextExpansionRule[];
  macros: MacroRule[];
};

export const defaultConfig: AppConfig = {
  schemaVersion: 1,
  globalEnabled: true,
  emergencyStop: "F12",
  navigationAutoCollapse: false,
  launchAtStartup: false,
  hotkeys: [
    {
      id: "capslock-to-escape",
      name: "CapsLock 改为 Esc",
      enabled: false,
      triggerKeys: ["CapsLock"],
      action: { type: "remap", target: "Esc" },
    },
    {
      id: "open-terminal",
      name: "打开 Windows Terminal",
      enabled: false,
      triggerKeys: ["Ctrl", "Alt", "T"],
      action: { type: "launch", target: "wt.exe" },
    },
  ],
  textExpansions: [
    {
      id: "common-email",
      name: "常用邮箱",
      abbreviation: "@@",
      replacement: "example@example.com",
      enabled: false,
      caseSensitive: false,
      sensitive: false,
    },
  ],
  macros: [],
};

export function newRuleId(prefix: string): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return `${prefix}-${crypto.randomUUID()}`;
  }
  return `${prefix}-${Date.now()}`;
}
