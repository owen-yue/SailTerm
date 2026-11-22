// 自动更新前端封装（Tauri v2 updater 插件）。
//
// 发布模型：GitHub Releases 托管 latest.json 清单 + 各平台 minisign 签名产物；应用启动后按需
// 主动 `check()` 比对版本，有更新则 `downloadAndInstall()`，装完复用后端 `restart_app` 重启生效。
// 安全：产物经 tauri.conf.json 里 plugins.updater.pubkey 的 minisign 公钥验签，签名不符即拒装。
// 端点/公钥未配置（占位）时 check() 会抛错，调用方需捕获并轻提示，不影响其余功能。
import { check, type Update } from "@tauri-apps/plugin-updater";

/** 透传插件的 Update 句柄类型，供调用方保存以便后续 install（不必直接依赖插件包）。 */
export type { Update };

/** 一条可用更新的展示信息。 */
export interface UpdateInfo {
  version: string;
  /** 更新说明（changelog，可能为空）。 */
  body: string | null;
  /** 发布日期（ISO 串，可能为空）。 */
  date: string | null;
}

export const updater = {
  /**
   * 拉取并比对最新版本。有更新返回 `Update` 句柄 + 元信息（供后续 install）；已是最新返回 null。
   * 网络失败 / 端点或公钥未配置会抛错，由调用方 catch。
   */
  async check(): Promise<{ update: Update; info: UpdateInfo } | null> {
    const update = await check();
    if (!update) return null;
    return {
      update,
      info: { version: update.version, body: update.body ?? null, date: update.date ?? null },
    };
  },

  /**
   * 下载并安装更新；`onProgress` 回传 0–100 的下载百分比（无总长时不回调）。
   * 完成后不自动重启，交由调用方决定时机（装完提示用户「立即重启」）。
   */
  async install(update: Update, onProgress?: (percent: number) => void): Promise<void> {
    let total = 0;
    let downloaded = 0;
    await update.downloadAndInstall((ev) => {
      switch (ev.event) {
        case "Started":
          total = ev.data.contentLength ?? 0;
          downloaded = 0;
          break;
        case "Progress":
          downloaded += ev.data.chunkLength;
          if (total) onProgress?.(Math.min(100, Math.round((downloaded / total) * 100)));
          break;
        case "Finished":
          onProgress?.(100);
          break;
      }
    });
  },
};
