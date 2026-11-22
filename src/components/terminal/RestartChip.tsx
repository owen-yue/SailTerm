import { useI18n } from "../../lib/i18n";
import { app } from "../../lib/tauri";
import { restartActions, restartStore, toast, useStore } from "../../stores";
import { IconRefresh } from "../common/icons";

/**
 * 常驻重启徽标：当有「重启后生效」的设置被改动时出现在右下角，
 * 提供一键重启（复用后端 restart_app），或手动关隐藏（仅收起提示，不撤销改动）。
 * 挂在主窗口根节点，与 <Toasts/> 通知层并列，全局可见。
 *
 * dev 构建下不执行真重启：`restart_app` 会 spawn 独立二进制并退出旧进程，
 * 连带 `tauri dev` 关掉 Vite（1420），新实例连不上 dev server → 白屏。故 dev 仅提示手动重启。
 */
export default function RestartChip() {
  const { pending } = useStore(restartStore);
  const tr = useI18n();
  if (!pending) return null;
  const onRestart = () => {
    if (import.meta.env.DEV) {
      toast.show(tr("restart.devHint"), "warn");
      restartActions.dismiss();
      return;
    }
    void app.restart();
  };
  return (
    <div className="restart-chip" role="status" aria-live="polite">
      <IconRefresh className="rc-ico" />
      <span className="rc-msg">{tr("restart.pending")}</span>
      <button type="button" className="rc-btn" onClick={onRestart}>
        {tr("restart.now")}
      </button>
      <button
        type="button"
        className="rc-x"
        title={tr("common.close")}
        aria-label={tr("common.close")}
        onClick={() => restartActions.dismiss()}
      >
        ×
      </button>
    </div>
  );
}
