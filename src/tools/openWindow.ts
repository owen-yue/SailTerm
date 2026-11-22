// 工具独立窗口（多窗口 EXT）：点工具 → 弹一个单独的子窗口（经 `lib/tauri/window` 适配），主窗口照常可操作。
//
// 与「会话弹出窗口」(lib/windows.ts) 同源套路：新窗口加载同一前端（`/?tool=<id>`），
// main.tsx 据此路由到 ToolWindow；每窗口独立 JS 上下文，工具状态天然隔离、可并排多开。
// label 冲突 = 该工具窗口已在 → 直接聚焦，不重复创建。后端 net_* 命令与事件对所有窗口可用
// （capability `tools` 授权 `tool-*` 窗口）。
import { toast } from "../stores/toastStore";
import { t } from "../lib/i18n";
import { createDetachedWindow, focusDetachedWindow, type Rgba } from "../lib/tauri/window";
import { toolById } from "./registry";

/** 工具窗口 label（capability `tools` 按 `tool-*` 通配授权）。 */
export function toolWindowLabel(id: string): string {
  return `tool-${id}`;
}

/**
 * 读当前（主）窗口 body 的实际背景色——随主题变化——转成 Tauri Color，
 * 用于新窗口创建时的原生 backgroundColor，消除 WebView 首帧未上屏前的白屏闪烁。
 * 取不到时兜底为深色主题 `#0b0e12`。
 */
function themeBgColor(): Rgba {
  const raw = getComputedStyle(document.body).backgroundColor;
  const nums = raw.match(/\d+(?:\.\d+)?/g);
  if (nums && nums.length >= 3) {
    const a = nums.length >= 4 ? Math.round(parseFloat(nums[3]) * 255) : 255;
    return [Number(nums[0]), Number(nums[1]), Number(nums[2]), a];
  }
  return [11, 14, 18, 255];
}

/** 打开（或聚焦已开的）工具独立窗口。已在则置顶，不重复创建。 */
export async function openToolWindow(id: string, title: string): Promise<void> {
  const label = toolWindowLabel(id);
  if (await focusDetachedWindow(label)) return;
  // 宽栏两栏工具（如录制库）在注册表里自带更宽的默认尺寸，其余沿用 860×640。
  const size = toolById(id)?.winSize;
  createDetachedWindow({
    label,
    url: `/?tool=${encodeURIComponent(id)}`,
    title,
    width: size?.width ?? 860,
    height: size?.height ?? 640,
    minWidth: 480,
    minHeight: 360,
    // 固定居中弹出：不指定时由 OS 决定初始位置，每次落在不同点
    center: true,
    // 原生背景预设为当前主题底色：新窗口首帧在内容绘制前不再闪白屏
    backgroundColor: themeBgColor(),
    onError: (payload) => {
      toast.show(t("wd.popFail").replace("{m}", JSON.stringify(payload)));
    },
  });
}
