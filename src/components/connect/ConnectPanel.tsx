import ConnectForm from "./ConnectForm";
import { useI18n } from "../../lib/i18n";
import { useEscClose } from "../../lib/useOverlay";
import type { CSSProperties } from "react";
import type { Profile } from "../../types";

interface Props {
  /** 编辑既有连接时传入；新建为 null。 */
  profile: Profile | null;
  /** 面板左偏移（px）：贴在左侧栏右边缘，由 App 按 rail+侧栏+分隔条宽度计算。 */
  left?: number;
  /** 面板当前是否可见（false = 切到了密钥库/工具区，表单只是 display:none 隐着保挂载）。 */
  visible?: boolean;
  onClose: () => void;
}

/**
 * 连接编辑侧面板（master-detail）：
 * 在左侧栏旁边滑出的全高面板，替代原有居中弹窗。
 */
export default function ConnectPanel({ profile, left = 0, visible = true, onClose }: Props) {
  const t = useI18n();
  // Esc 关面板：沿用全仓弹层栈约定（只关最顶层），面板开着时子菜单/确认气泡优先消费。
  // 不可见时必须退出 Esc 登记：表单在密钥库/工具区背后保持挂载，否则在那些页面按
  // 一下 Esc 会把没保存的半张表静默丢掉（用户看不见是谁被关了）。
  useEscClose(onClose, visible);
  return (
    <aside className="conn-panel" style={{ "--cp-left": `${left}px` } as CSSProperties}>
      <div className="cp-head">
        <h3>{profile ? t("dlg.editConn") : t("dlg.newConn")}</h3>
        <button type="button" className="cp-close" onClick={onClose} aria-label={t("common.close")}>
          ×
        </button>
      </div>
      <ConnectForm profile={profile} onDone={onClose} />
    </aside>
  );
}
