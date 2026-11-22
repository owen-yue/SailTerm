// 内置终端配色预设 + 主题解析。
// id 落库（后端 appearance.theme），颜色只在展示区解析为 xterm ITheme——不进后端、不含凭证。
import type { ITheme } from "@xterm/xterm";
import type { CustomTheme, TriggerColor } from "../types";
import { t } from "./i18n";

/** 与后端 `commands/settings.rs` 的默认值保持一致（前端首屏未加载完成时用它兜底）。 */
export const DEFAULT_THEME = "sailterm-dark";
export const DEFAULT_FONT_SIZE = 13;
export const DEFAULT_FONT_FAMILY = "'Cascadia Code', 'JetBrains Mono', Consolas, monospace";

/** 自定义主题的保留 id：选中它时用导入的 CustomTheme 颜色，而非内置预设。 */
export const CUSTOM_THEME_ID = "custom";

/** 一个内置主题预设：id（持久化到设置）+ 展示名 + xterm ITheme 配色。 */
export interface ThemePreset {
  id: string;
  name: string;
  theme: ITheme;
}

/**
 * 内置终端配色预设。首个 `sailterm-dark` 即 `Terminal.tsx` 的原硬编码配色，作为默认值；
 * 其余为社区常见配色（含 github-light、solarized-light 两个亮色，与各自暗色配成明暗对），
 * 满足「亮暗主题」诉求。亮色预设的 bg 亮度 >0.55，applyAppChrome 据此自动切整窗亮色。
 */
export const THEME_PRESETS: ThemePreset[] = [
  {
    id: "sailterm-dark",
    name: "theme.sailtermDark",
    theme: {
      background: "#0a0e13",
      foreground: "#d7e2ec",
      cursor: "#2dd4bf",
      selectionBackground: "#243040",
      black: "#0a0e13",
      red: "#e5484d",
      green: "#46a758",
      yellow: "#ffc53d",
      blue: "#3e63dd",
      magenta: "#8e4ec6",
      cyan: "#2dd4bf",
      white: "#d7e2ec",
      brightBlack: "#5b6b7b",
      brightRed: "#ff6369",
      brightGreen: "#5bb96b",
      brightYellow: "#ffd666",
      brightBlue: "#6b8aff",
      brightMagenta: "#b57ce8",
      brightCyan: "#57e6d6",
      brightWhite: "#ffffff",
    },
  },
  {
    id: "one-dark",
    name: "theme.oneDark",
    theme: {
      background: "#282c34",
      foreground: "#abb2bf",
      cursor: "#528bff",
      selectionBackground: "#3e4451",
      black: "#282c34",
      red: "#e06c75",
      green: "#98c379",
      yellow: "#e5c07b",
      blue: "#61afef",
      magenta: "#c678dd",
      cyan: "#56b6c2",
      white: "#abb2bf",
      brightBlack: "#5c6370",
      brightRed: "#e06c75",
      brightGreen: "#98c379",
      brightYellow: "#e5c07b",
      brightBlue: "#61afef",
      brightMagenta: "#c678dd",
      brightCyan: "#56b6c2",
      brightWhite: "#ffffff",
    },
  },
  {
    id: "dracula",
    name: "theme.dracula",
    theme: {
      background: "#282a36",
      foreground: "#f8f8f2",
      cursor: "#f8f8f2",
      selectionBackground: "#44475a",
      black: "#21222c",
      red: "#ff5555",
      green: "#50fa7b",
      yellow: "#f1fa8c",
      blue: "#bd93f9",
      magenta: "#ff79c6",
      cyan: "#8be9fd",
      white: "#f8f8f2",
      brightBlack: "#6272a4",
      brightRed: "#ff6e6e",
      brightGreen: "#69ff94",
      brightYellow: "#ffffa5",
      brightBlue: "#d6acff",
      brightMagenta: "#ff92df",
      brightCyan: "#a4ffff",
      brightWhite: "#ffffff",
    },
  },
  {
    id: "solarized-dark",
    name: "theme.solarizedDark",
    theme: {
      background: "#002b36",
      foreground: "#839496",
      cursor: "#93a1a1",
      selectionBackground: "#073642",
      black: "#073642",
      red: "#dc322f",
      green: "#859900",
      yellow: "#b58900",
      blue: "#268bd2",
      magenta: "#d33682",
      cyan: "#2aa198",
      white: "#eee8d5",
      brightBlack: "#002b36",
      brightRed: "#cb4b16",
      brightGreen: "#586e75",
      brightYellow: "#657b83",
      brightBlue: "#839496",
      brightMagenta: "#6c71c4",
      brightCyan: "#93a1a1",
      brightWhite: "#fdf6e3",
    },
  },
  {
    id: "monokai",
    name: "theme.monokai",
    theme: {
      background: "#272822",
      foreground: "#f8f8f2",
      cursor: "#f8f8f0",
      selectionBackground: "#49483e",
      black: "#272822",
      red: "#f92672",
      green: "#a6e22e",
      yellow: "#f4bf75",
      blue: "#66d9ef",
      magenta: "#ae81ff",
      cyan: "#a1efe4",
      white: "#f8f8f2",
      brightBlack: "#75715e",
      brightRed: "#f92672",
      brightGreen: "#a6e22e",
      brightYellow: "#f4bf75",
      brightBlue: "#66d9ef",
      brightMagenta: "#ae81ff",
      brightCyan: "#a1efe4",
      brightWhite: "#f9f8f5",
    },
  },
  {
    id: "nord",
    name: "theme.nord",
    theme: {
      background: "#2e3440",
      foreground: "#d8dee9",
      cursor: "#d8dee9",
      selectionBackground: "#434c5e",
      black: "#3b4252",
      red: "#bf616a",
      green: "#a3be8c",
      yellow: "#ebcb8b",
      blue: "#81a1c1",
      magenta: "#b48ead",
      cyan: "#88c0d0",
      white: "#e5e9f0",
      brightBlack: "#4c566a",
      brightRed: "#bf616a",
      brightGreen: "#a3be8c",
      brightYellow: "#ebcb8b",
      brightBlue: "#81a1c1",
      brightMagenta: "#b48ead",
      brightCyan: "#8fbcbb",
      brightWhite: "#eceff4",
    },
  },
  {
    id: "gruvbox-dark",
    name: "theme.gruvboxDark",
    theme: {
      background: "#282828",
      foreground: "#ebdbb2",
      cursor: "#ebdbb2",
      selectionBackground: "#504945",
      black: "#282828",
      red: "#cc241d",
      green: "#98971a",
      yellow: "#d79921",
      blue: "#458588",
      magenta: "#b16286",
      cyan: "#689d6a",
      white: "#a89984",
      brightBlack: "#928374",
      brightRed: "#fb4934",
      brightGreen: "#b8bb26",
      brightYellow: "#fabd2f",
      brightBlue: "#83a598",
      brightMagenta: "#d3869b",
      brightCyan: "#8ec07c",
      brightWhite: "#ebdbb2",
    },
  },
  {
    id: "github-light",
    name: "theme.githubLight",
    theme: {
      background: "#ffffff",
      foreground: "#24292f",
      cursor: "#044f8e",
      selectionBackground: "#dbebfb",
      black: "#24292f",
      red: "#cf222e",
      green: "#116329",
      yellow: "#4d2d00",
      blue: "#0969da",
      magenta: "#8250df",
      cyan: "#1b7c83",
      white: "#6e7781",
      brightBlack: "#57606a",
      brightRed: "#a40e26",
      brightGreen: "#1a7f37",
      brightYellow: "#633c01",
      brightBlue: "#218bff",
      brightMagenta: "#a475f9",
      brightCyan: "#3192aa",
      brightWhite: "#8c959f",
    },
  },
  {
    id: "solarized-light",
    name: "theme.solarizedLight",
    theme: {
      background: "#fdf6e3",
      foreground: "#586e75",
      cursor: "#657b83",
      selectionBackground: "#eee8d5",
      black: "#073642",
      red: "#dc322f",
      green: "#859900",
      yellow: "#b58900",
      blue: "#268bd2",
      magenta: "#d33682",
      cyan: "#2aa198",
      white: "#eee8d5",
      brightBlack: "#586e75",
      brightRed: "#cb4b16",
      brightGreen: "#657b83",
      brightYellow: "#93a1a1",
      brightBlue: "#839496",
      brightMagenta: "#6c71c4",
      brightCyan: "#93a1a1",
      brightWhite: "#fdf6e3",
    },
  },
];

/** 把导入的自定义主题颜色映射为 xterm ITheme（键名沿用 ITheme，多余键 xterm 自动忽略）。 */
export function customToTheme(ct: CustomTheme): ITheme {
  return { ...ct.colors } as ITheme;
}

/**
 * 按主题 id 解析出 xterm ITheme：`custom` 且有导入主题→用自定义色；否则查内置预设；
 * 未知 id 一律回落首个预设（fail-safe，绝不因坏配置让终端不可读）。
 */
export function resolveTheme(id: string, custom: CustomTheme | null): ITheme {
  if (id === CUSTOM_THEME_ID && custom) return customToTheme(custom);
  const preset = THEME_PRESETS.find((p) => p.id === id);
  return (preset ?? THEME_PRESETS[0]).theme;
}

/** 主题展示名（UI 用）：内置查表，`custom` 用导入主题名，未知回落 id。 */
export function themeLabel(id: string, custom: CustomTheme | null): string {
  if (id === CUSTOM_THEME_ID) return custom ? t("theme.customWith").replace("{n}", custom.name) : t("theme.custom");
  return t(THEME_PRESETS.find((p) => p.id === id)?.name ?? id);
}

/** 主题预览色（UI 色板用）：取背景/前景/光标三色做小色卡。 */
export function themePreview(id: string, custom: CustomTheme | null): { bg: string; fg: string; cursor: string } {
  const t = resolveTheme(id, custom);
  return { bg: t.background ?? "#000", fg: t.foreground ?? "#fff", cursor: t.cursor ?? t.foreground ?? "#fff" };
}

/** 关键字着色的语义色键清单（与后端 `TRIGGER_COLORS` 一致，UI 颜色选择器直接遍历）。 */
export const TRIGGER_COLORS: readonly TriggerColor[] = ["red", "green", "yellow", "blue", "magenta", "cyan"];

/** 着色取色兜底（暗色调色板）：主题缺该槽位时用，绝不让规则渲成不可读。 */
const HIGHLIGHT_FALLBACK: Record<TriggerColor, string> = {
  red: "#ff6369",
  green: "#5bb96b",
  yellow: "#ffd666",
  blue: "#6b8aff",
  magenta: "#b57ce8",
  cyan: "#57e6d6",
};

/**
 * 关键字着色的实际取色：色键 → 当前主题的 ANSI 槽位。
 *
 * 暗色主题优先取 `bright*` 变体（底黑，基色偏闷）；亮色主题取基色
 * （多个亮色预设的 bright 槽位被故意做成了灰阶，如 solarized-light）。
 */
export function highlightColor(theme: ITheme, key: TriggerColor): string {
  const dark = luminance(theme.background ?? "#000") < 0.5;
  const base = theme[key];
  const bright = dark ? theme[("bright" + key[0].toUpperCase() + key.slice(1)) as keyof ITheme] : undefined;
  return ((bright ?? base) as string | undefined) ?? HIGHLIGHT_FALLBACK[key];
}

/** #rgb / #rrggbb → [r,g,b]；解析不了返回 null。兼容三位写法（导入的第三方主题里合法）。 */
function parseHex(hex: string): [number, number, number] | null {
  const m = /^#?([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return null;
  const s = m[1];
  const full = s.length === 3 ? s.split("").map((c) => c + c).join("") : s;
  const n = parseInt(full, 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

/** #rrggbb → HSL：h 取 0–360，s/l 取 0–1。解析不了返回 null。 */
function toHsl(hex: string): [number, number, number] | null {
  const rgb = parseHex(hex);
  if (!rgb) return null;
  const [r, g, b] = rgb.map((c) => c / 255);
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  const d = max - min;
  if (d === 0) return [0, 0, l];
  const s = d / (l > 0.5 ? 2 - max - min : max + min);
  const h = max === r ? (g - b) / d + (g < b ? 6 : 0) : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
  return [h * 60, s, l];
}

/** HSL → #rrggbb。 */
function fromHsl(h: number, s: number, l: number): string {
  const hh = ((h % 360) + 360) % 360; // hue 绕回：360 与 0 同义，不归一就会落错扇区
  const c = (1 - Math.abs(2 * l - 1)) * s;
  const x = c * (1 - Math.abs(((hh / 60) % 2) - 1));
  const m = l - c / 2;
  const seg = hh < 60 ? [c, x, 0] : hh < 120 ? [x, c, 0] : hh < 180 ? [0, c, x] : hh < 240 ? [0, x, c] : hh < 300 ? [x, 0, c] : [c, 0, x];
  return `#${seg.map((v) => Math.round((v + m) * 255).toString(16).padStart(2, "0")).join("")}`;
}

/**
 * WCAG 相对亮度（带 gamma 线性化）。
 * 与下面给 UI 判亮暗用的 `luminance()` 不是一回事——那个是 8-bit 加权粗算，
 * 这个才能拿来算对比度。
 */
function wcagLum(hex: string): number {
  const rgb = parseHex(hex);
  if (!rgb) return 0;
  const [r, g, b] = rgb.map((v) => {
    const c = v / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/** WCAG 对比度（1–21）。用来把「染色会不会把正文糊掉」钉成可断言的数字。 */
export function contrastRatio(a: string, b: string): number {
  const x = wcagLum(a);
  const y = wcagLum(b);
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}

/**
 * 终端搜索命中的两档染色参数（配 `tintFor` 用）：soft = 其它命中，hard = 当前命中。
 * 放这里而不是 Terminal.tsx 里，是为了让标记色/选区色/回归测试共用同一个源头。
 *
 * `keep` 是「这个主题的原生底↔字对比度最多花掉多少」：不能拿同一个 lift 一刷到底——
 * 近黑底（sailterm-dark 原生 14.7）花得起，刷小了就看不清；solarized-dark 只有 4.7，
 * 刷重了正文就没对比度。按预算花，各主题拿到的是“同样多”的可看见度。
 * hard 的 keep 故意置 0（不受预算约束）：它能同时改字色，可读性由 `inkFor` 兜；
 * soft 不能改字色（addon 不递 foregroundColor），只能靠预算自己守住正文。
 */
export const SEARCH_HIT = {
  soft: { sat: 0.85, lift: 0.34, keep: 0.55 },
  hard: { sat: 0.82, lift: 0.62, keep: 0 },
} as const;

/** 强底上的保底墨色：深/浅两套，按底色亮度选反差大的那一边。 */
const INK_DARK = "#0d1117";
const INK_LIGHT = "#f6f8fa";

/**
 * 命中块上的文字色（最小对比度思路：底色把字压住时就改字色）。
 * 底与正文本来就分得开 → 返回 undefined，不动原字符颜色（保留 ANSI 彩色输出）；
 * 否则按块亮度挑深墨/浅墨中对比更高的那个。这样亮块也能读，不用再拿“保守混色”换可读性。
 */
export function inkFor(block: string, fg: string): string | undefined {
  if (contrastRatio(block, fg) >= 4.5) return undefined;
  return contrastRatio(block, INK_DARK) >= contrastRatio(block, INK_LIGHT) ? INK_DARK : INK_LIGHT;
}

/**
 * 搜索命中染色：借强调色的**色相 + 彩度**，亮度按「底→字」的距离走 lift 这一比例。
 *
 * 为什么不用 RGB 直接混（上一版翻车的根因）：暗色主题里的 yellow 往往又亮又淡
 * （`#ffd666` / `#e5c07b` / `#f1fa8c`），按 RGB 比例混就一条对角线走到强调色本身：
 * 强度低时混出一片死灰（one-dark 上 t=0.12 得 `#3f3e3d`），强度高时颜料被稀释、发橙。
 * 拆成色相/彩度/亮度三个旋钮后，“同一个黄、只是深浅不同”在亮暗主题上都能成立；
 * 亮度拉高带来的可读性代价由 `inkFor` 接手（当前命中）或 `keep` 预算兜住（其它命中）。
 *
 * 亮度位移是单调吃掉对比度的，所以 `keep` 可以直接倒探：从 lift 起逐步回退，
 * 直到块与正文的对比度不低于「原生对比度 × keep」为止。
 *
 * @param sat 目标彩度（0–1）
 * @param lift 亮度朝文字色行进的距离上限，按「底→字亮度差」的比例给（0–1）
 * @param keep 至少保住多少比例的原生底↔字对比度（0–1）
 */
export function tintFor(bg: string, accent: string, fg: string, sat: number, lift: number, keep: number): string {
  const hslBg = toHsl(bg);
  if (!hslBg) return bg;
  const hslAc = toHsl(accent);
  // 强调色是个灰阶（部分亮色主题的 bright 槽位故意做成无色）时退回暖金 hue，别染成灰色
  const rawHue = hslAc && hslAc[1] > 0.08 ? hslAc[0] : 45;
  // 夹进琥珀带：dracula / monokai 的 yellow 槽位实为柠檬黄（hue≈65），直接拿它做底色
  // 会染出一片黄绿，看着像报错色而不是标记色
  const hue = Math.min(52, Math.max(32, rawHue));
  const [, , lBg] = hslBg;
  const lFg = toHsl(fg)?.[2] ?? (lBg < 0.5 ? 0.9 : 0.1);
  const budget = contrastRatio(bg, fg) * Math.min(1, Math.max(0, keep));
  let step = Math.min(1, Math.max(0, lift));
  for (;;) {
    const hex = fromHsl(hue, Math.min(1, sat), lBg + (lFg - lBg) * step);
    if (step <= 0 || contrastRatio(hex, fg) >= budget) return hex;
    step -= 0.01;
  }
}

/** 亮度（0–1）：判断亮/暗主题，驱动整窗 chrome。 */
function luminance(hex: string): number {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return 0;
  const n = parseInt(m[1], 16);
  const r = (n >> 16) & 255;
  const g = (n >> 8) & 255;
  const b = n & 255;
  return (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
}

/** 原生窗口标题栏跟 UI 亮暗一致（Windows 浅色标题栏会显得突兀）。 */
async function syncWindowTheme(light: boolean): Promise<void> {
  try {
    const { setWindowTheme } = await import("./tauri/window");
    await setWindowTheme(light ? "light" : "dark");
  } catch {
    // 非 Tauri 环境 / 窗口未就绪时忽略
  }
}

/**
 * 主题缓存键：新窗口（工具/会话弹出）首帧前同步读取以预着色，
 * 避开「等后端 refresh 期间 WKWebView 白底/默认暗底」的闪烁。与后端权威值不符时
 * 随后 refresh() 会覆盖校正，缓存只用于“首帧不至于白/错色”。
 */
const CHROME_CACHE_KEY = "sailterm.chrome";

/** 读取上次应用的主题入参（无缓存/异常返回 null）。 */
export function readChromeCache(): { id: string; custom: CustomTheme | null } | null {
  try {
    const raw = localStorage.getItem(CHROME_CACHE_KEY);
    return raw ? (JSON.parse(raw) as { id: string; custom: CustomTheme | null }) : null;
  } catch {
    return null;
  }
}

/**
 * 终端主题 → 应用 chrome（亮暗 + 强调色 + 一套背景/文字）。
 * 面板/控件只认 CSS 变量，避免「终端换了、UI 还是旧色」的不协调。
 */
export function applyAppChrome(id: string, custom: CustomTheme | null): void {
  const t = resolveTheme(id, custom);
  const bg = t.background ?? "#0b0e12";
  const accent = t.cursor ?? t.cyan ?? "#2dd4bf";
  const root = document.documentElement;
  const light = luminance(bg) > 0.55;
  root.dataset.theme = light ? "light" : "dark";
  void syncWindowTheme(light);
  root.style.setProperty("--brand", accent);
  // 终端真实背景色单独暴露：滚动条轨道/滑块描边、xterm 容器背景用它对齐，
  // 避免亮色主题下「终端白底 vs chrome 灰底」错位露白边
  root.style.setProperty("--term-bg", bg);
  // 命名易踩坑：`--brand` 才是界面强调色（选中态 / 主按钮 / 悬停反馈都用它）；
  // `--accent` 是终端的 ANSI blue 槽位，只给「信息蓝」语义（代码文本、info 级提示、进度填充）。
  // 把选中态写成 --accent，换主题时它跟着终端蓝跑，看着就像另一套配色。
  // 主题没定义 ANSI blue 时回落固定蓝（不是 cursor/cyan）：回落到强调色会让
  // 「信息蓝」与 --brand 同色，挂在它下面的「等你回执」辉光（--await）就与选中指示撞成一团。
  root.style.setProperty("--accent", t.blue ?? "#4a9eff");
  root.style.setProperty("--green", t.green ?? "#2dd4bf");
  root.style.setProperty("--red", t.red ?? "#f43f5e");
  root.style.setProperty("--amber", t.yellow ?? "#f59e0b");
  root.style.setProperty("--btn-ink", light ? "#0b1a22" : "#04222a");
  // 强调色上的文字：按品牌色亮度选黑/白，避免「蓝底黑字」
  const onBrand = luminance(accent) > 0.62 ? "#0b1a22" : "#ffffff";
  root.style.setProperty("--on-brand", onBrand);
  if (light) {
    root.style.setProperty("--bg-0", "#eef2f6");
    root.style.setProperty("--bg-1", "#f8fafc");
    root.style.setProperty("--bg-2", "#e8eef5");
    root.style.setProperty("--bg-3", "#dce4ee");
    root.style.setProperty("--line", "#c9d4e2");
    root.style.setProperty("--line-2", "#b0bfd0");
    root.style.setProperty("--txt", "#15202e");
    root.style.setProperty("--txt-dim", "#3d5168");
    root.style.setProperty("--txt-mute", "#6b7f96");
  } else {
    // 暗色底跟终端 bg，减少「两套黑」割裂
    root.style.setProperty("--bg-0", bg);
    root.style.setProperty("--bg-1", "#10161f");
    root.style.setProperty("--bg-2", "#17202b");
    root.style.setProperty("--bg-3", "#1e2937");
    root.style.setProperty("--line", "#273445");
    root.style.setProperty("--line-2", "#334458");
    root.style.setProperty("--txt", "#e6edf5");
    root.style.setProperty("--txt-dim", "#9aadc2");
    root.style.setProperty("--txt-mute", "#6a7f96");
  }
  // 缓存入参供新窗口同步预着色（main.tsx 首帧前 replay），免等后端 refresh。
  try {
    localStorage.setItem(CHROME_CACHE_KEY, JSON.stringify({ id, custom }));
  } catch {
    /* 隐私模式/配额失败：退化为 refresh 拉到后再着色，不影响正确性 */
  }
}
