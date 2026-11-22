// OS 通知适配层（`lib/tauri` 收口）：把通知发给操作系统通知中心（macOS 横幅）。
//
// 定位：应用内 toast（toastStore）不动，OS 通知是额外的「离屏通道」——用户切到别的
// 应用/窗口时也能知道出事了。
// 安全红线：系统通知中心会被持久化、锁屏可见、跨设备转发，body 必须是
// 调用方生成的通用文案，不得携带主机名/路径/报错原文（策略在 toastStore.dispatchOs）。
// 权限：首次发送前申请一次；被系统拒绝就静默（应用内 toast 仍在，不重复骚扰用户）。
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";

/** 权限状态缓存：undefined = 未查过，查明后只在首次发送时付一次异步代价。 */
let _granted: boolean | undefined;

async function ensurePermission(): Promise<boolean> {
  if (_granted === true) return true;
  try {
    if (_granted === undefined) {
      _granted = await isPermissionGranted();
    }
    if (!_granted) {
      _granted = (await requestPermission()) === "granted";
    }
    return _granted;
  } catch {
    // 非 Tauri 环境（单测/浏览器调试）或插件异常：静默，不影响应用内通知。
    return false;
  }
}

/**
 * 发一条 OS 通知。`body` 为通用文案（不含敏感内容），标题固定应用名。
 * 调用方自行决定去重/时机，这里只管送达或静默失败。
 */
export async function notifyOs(body: string): Promise<void> {
  if (!(await ensurePermission())) return;
  try {
    sendNotification({ title: "SailTerm", body });
  } catch {
    // 发送失败静默：OS 通知是锦上添花，不让它把通知主链路带崩。
  }
}
