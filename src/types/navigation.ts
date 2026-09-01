export type RouteId =
  "home" | "hotkeys" | "text-expansion" | "macros" | "settings";

export type NavItem = {
  id: RouteId;
  label: string;
  description: string;
  icon: string;
};

export const navItems: NavItem[] = [
  { id: "home", label: "首页", description: "运行概览", icon: "◌" },
  { id: "hotkeys", label: "快捷键", description: "按键规则", icon: "⌘" },
  {
    id: "text-expansion",
    label: "文本扩展",
    description: "常用短语",
    icon: "Aa",
  },
  { id: "macros", label: "宏", description: "录制与编辑", icon: "◉" },
  { id: "settings", label: "设置", description: "安全与偏好", icon: "⚙" },
];
