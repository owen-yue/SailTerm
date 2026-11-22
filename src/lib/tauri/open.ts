// 在系统默认浏览器打开外部链接（走 tauri_plugin_opener）。
//
// 跟 updater.ts 一样直接调插件前端 API（先例已存），不再包一层后端命令：反过来说，
// 一个能开任意 URL 的 IPC 命令比受 scope 约束的插件权限面更大。`opener:default` 已在
// 三个 capability 里放行（allow-open-url + allow-default-urls，覆盖 http/https/mailto/tel）；
// 协议白名单在解析层就卡住了（见 src/lib/markdown.ts 的 safeHref）。
import { openUrl } from "@tauri-apps/plugin-opener";

/** 打开外部链接；被 scope 拒或系统无默认浏览器时返回 false，由调用方决定怎么提示。 */
export async function openExternal(url: string): Promise<boolean> {
  try {
    await openUrl(url);
    return true;
  } catch {
    return false;
  }
}
