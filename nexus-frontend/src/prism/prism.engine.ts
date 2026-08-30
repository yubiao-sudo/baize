// ==========================================================================
// 棱镜层 · Prism Theme Engine
// --------------------------------------------------------------------------
// 与旧前端 tokens.css + workshop.css 分层模型完全不同：
//   - 用运行时动态 CSS 变量注入替代"加载不同 CSS 文件"
//   - 每个主题是一份 Token Map（颜色/折射角度/噪点浓度/流体速度）
//   - 提供 prism(themeId) 切换，由 Prism 信号广播给所有 Cell 重新上色
//   - 内置四种预设：晨光(aurora)、星云(nebula)、玻璃花园(garden)、深海流(floe)
//     —— 刻意不使用"水晶工坊"(紫晶/磨砂玻璃)、"终末地"(工业黄/条纹/冷灰) 色板
// ==========================================================================
import { prismBus } from "../bus/prism.bus";
import type { PrismThemeManifest } from "../bus/prism.types";

export interface PrismTokens {
  /** 基底底色（最暗） */
  bg_void: string;
  /** 页面背景（带折射的渐变叠加层） */
  bg_sheen_1: string;
  bg_sheen_2: string;
  bg_sheen_3: string;
  /** 棱镜主色（折射轴色） */
  prism_axis: string;
  prism_face_r: string; // 红光面
  prism_face_g: string; // 绿光面
  prism_face_b: string; // 蓝光面
  /** 文字色阶 */
  ink_primary: string;
  ink_secondary: string;
  ink_faint: string;
  /** 细胞边框 / 切面色 */
  cell_edge: string;
  cell_glow: string;
  /** 交互色 */
  action: string;
  action_hover: string;
  danger: string;
  success: string;
  /** 动画参数（单位秒） */
  refraction_speed: number;
  /** 噪点浓度（0-1） */
  grain_amount: number;
}

export interface PrismTheme extends PrismThemeManifest {
  tokens: PrismTokens;
}

// --------------------------------------------------------------------
// 四个完全不同的主题（没有终末地工业黄，也没有水晶工坊紫晶极光）
// --------------------------------------------------------------------
const aurora: PrismTheme = {
  id: "aurora",
  name: "晨光折射",
  description: "清晨日光穿透棱镜，粉橙金三色横向折射，轻盈温暖。",
  tokens: {
    bg_void: "#13161e",
    bg_sheen_1: "radial-gradient(60% 40% at 15% 10%, rgba(255, 175, 120, 0.22), transparent 70%)",
    bg_sheen_2: "radial-gradient(55% 45% at 90% 30%, rgba(255, 118, 170, 0.18), transparent 70%)",
    bg_sheen_3: "radial-gradient(80% 60% at 50% 110%, rgba(120, 220, 255, 0.18), transparent 70%)",
    prism_axis: "#ffcf6b",
    prism_face_r: "#ff7a59",
    prism_face_g: "#ffd06b",
    prism_face_b: "#73d2ff",
    ink_primary: "#fff7ea",
    ink_secondary: "#cdc4b4",
    ink_faint: "#8a8176",
    cell_edge: "rgba(255, 207, 107, 0.28)",
    cell_glow: "rgba(255, 176, 110, 0.22)",
    action: "#ffb05e",
    action_hover: "#ffc980",
    danger: "#ff6b8a",
    success: "#7be1a7",
    refraction_speed: 14,
    grain_amount: 0.06,
  },
};

const nebula: PrismTheme = {
  id: "nebula",
  name: "星云色散",
  description: "深空紫蓝背景 + 红绿蓝三色星点色散，冷冽神秘。",
  tokens: {
    bg_void: "#0a0a1a",
    bg_sheen_1: "radial-gradient(50% 40% at 20% 20%, rgba(120, 80, 255, 0.35), transparent 70%)",
    bg_sheen_2: "radial-gradient(50% 45% at 85% 75%, rgba(80, 200, 255, 0.25), transparent 70%)",
    bg_sheen_3: "radial-gradient(70% 55% at 45% 45%, rgba(255, 90, 160, 0.18), transparent 70%)",
    prism_axis: "#b58bff",
    prism_face_r: "#ff5aa0",
    prism_face_g: "#6cffbf",
    prism_face_b: "#5aa8ff",
    ink_primary: "#eef1ff",
    ink_secondary: "#aab0cc",
    ink_faint: "#6a7190",
    cell_edge: "rgba(181, 139, 255, 0.28)",
    cell_glow: "rgba(120, 80, 255, 0.32)",
    action: "#b58bff",
    action_hover: "#c9a6ff",
    danger: "#ff5a7a",
    success: "#6cffbf",
    refraction_speed: 20,
    grain_amount: 0.09,
  },
};

const garden: PrismTheme = {
  id: "garden",
  name: "玻璃花园",
  description: "苔绿+珊瑚+琥珀的自然色散，像阳光穿过多肉温室玻璃。",
  tokens: {
    bg_void: "#0f1512",
    bg_sheen_1: "radial-gradient(55% 45% at 10% 90%, rgba(120, 230, 160, 0.22), transparent 70%)",
    bg_sheen_2: "radial-gradient(50% 40% at 90% 10%, rgba(255, 170, 120, 0.20), transparent 70%)",
    bg_sheen_3: "radial-gradient(80% 55% at 50% 50%, rgba(255, 120, 160, 0.12), transparent 70%)",
    prism_axis: "#78e6a0",
    prism_face_r: "#ffaa78",
    prism_face_g: "#78e6a0",
    prism_face_b: "#78cfff",
    ink_primary: "#eefbf2",
    ink_secondary: "#b3c7ba",
    ink_faint: "#6e8274",
    cell_edge: "rgba(120, 230, 160, 0.28)",
    cell_glow: "rgba(120, 230, 160, 0.20)",
    action: "#78e6a0",
    action_hover: "#9df0b8",
    danger: "#ff7a8a",
    success: "#78e6a0",
    refraction_speed: 18,
    grain_amount: 0.05,
  },
};

const floe: PrismTheme = {
  id: "floe",
  name: "深海流冰",
  description: "深海青+冰白色散，流动的切变光像水下浮冰。",
  tokens: {
    bg_void: "#071119",
    bg_sheen_1: "radial-gradient(55% 45% at 10% 30%, rgba(100, 200, 220, 0.28), transparent 70%)",
    bg_sheen_2: "radial-gradient(50% 45% at 80% 80%, rgba(180, 240, 255, 0.22), transparent 70%)",
    bg_sheen_3: "radial-gradient(70% 55% at 50% 50%, rgba(120, 180, 255, 0.15), transparent 70%)",
    prism_axis: "#7fe0f0",
    prism_face_r: "#b4f0ff",
    prism_face_g: "#7fe0f0",
    prism_face_b: "#6aa8ff",
    ink_primary: "#ebfaff",
    ink_secondary: "#b4cad4",
    ink_faint: "#6b808a",
    cell_edge: "rgba(127, 224, 240, 0.28)",
    cell_glow: "rgba(127, 224, 240, 0.22)",
    action: "#7fe0f0",
    action_hover: "#a8ebf7",
    danger: "#ff8a9a",
    success: "#7fe0c2",
    refraction_speed: 22,
    grain_amount: 0.08,
  },
};

const THEMES: Record<string, PrismTheme> = { aurora, nebula, garden, floe };

export const listPrismThemes = (): PrismThemeManifest[] =>
  Object.values(THEMES).map(({ id, name, description }) => ({ id, name, description }));

let currentId = "aurora";
export const getActivePrism = (): PrismTheme => THEMES[currentId] ?? aurora;

/** 应用主题：写入 CSS 变量 + 广播 prism 信号 */
export function applyPrism(themeId: string): PrismTheme {
  const theme = THEMES[themeId] ?? aurora;
  currentId = theme.id;
  const root = document.documentElement;
  const t = theme.tokens;
  const write = (k: string, v: string | number) => root.style.setProperty(k, String(v));
  write("--prism-bg-void", t.bg_void);
  write("--prism-sheen-1", t.bg_sheen_1);
  write("--prism-sheen-2", t.bg_sheen_2);
  write("--prism-sheen-3", t.bg_sheen_3);
  write("--prism-axis", t.prism_axis);
  write("--prism-face-r", t.prism_face_r);
  write("--prism-face-g", t.prism_face_g);
  write("--prism-face-b", t.prism_face_b);
  write("--prism-ink-1", t.ink_primary);
  write("--prism-ink-2", t.ink_secondary);
  write("--prism-ink-3", t.ink_faint);
  write("--prism-cell-edge", t.cell_edge);
  write("--prism-cell-glow", t.cell_glow);
  write("--prism-action", t.action);
  write("--prism-action-hover", t.action_hover);
  write("--prism-danger", t.danger);
  write("--prism-success", t.success);
  write("--prism-refraction", `${t.refraction_speed}s`);
  write("--prism-grain", String(t.grain_amount));
  prismBus.emit(
    "prism.changed",
    { themeId: theme.id, name: theme.name, tokens: t },
    { tier: "prism", source: "prism.engine" }
  );
  return theme;
}
export function getPrismById(id: string): PrismTheme | null {
  return THEMES[id] ?? null;
}