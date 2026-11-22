// 平台判定（纯 UA，零 Tauri 依赖）。
//
// 主窗口的装饰由 tauri.conf.json（Win/Linux：decorations:false 自绘）与
// tauri.macos.conf.json（macOS：Overlay 标题栏 + 原生红绿灯）分别配置；**程序化创建**的
// 独立窗口（会话弹出 lib/windows.ts、工具 openWindow.ts）不受这两个文件影响，其
// 装饰分流与「隐藏建窗 + 首帧后揭示」已收口在 lib/tauri/window.ts 窗口适配层。
// 本文件只留平台谓词：谓词须与 main.tsx 写入 `<html data-os>` 同源（顶栏靠
// `[data-os="mac"] .topbar { padding-left }` 给左上角按钮留位），勿在别处另判。

/** 是否 macOS：决定用系统原生红绿灯（Overlay 标题栏）还是自绘窗口控件。 */
export function isMacOS(): boolean {
  if (typeof navigator === "undefined") return false;
  return /mac|iphone|ipad/i.test(navigator.userAgent) || navigator.platform === "MacIntel";
}

/** 是否 Windows 平台（本地会话能力注册用：Windows 本地只有聊天形态，见 capabilities.isLocalWindows）。 */
export function isWindowsPlatform(): boolean {
  if (typeof navigator === "undefined") return false;
  return /windows/i.test(navigator.userAgent);
}
