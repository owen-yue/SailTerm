// 快捷命令新建/编辑弹框：＋ 面板（或任何入口）背后列表保持原位，表单聚焦在独立模态里。
// 旧做法把表单内联展开在分类区下方，一大块表单把已有命令整个挤下去，重开还带旧草稿——
// 弹窗每次开关都重挂载，天然无残留。
// 字段、校验、自动命名全在共享 SnippetEditor；本组件只负责壳（标题/遮罩/Esc）与归属提示。
import { createPortal } from "react-dom";
import type { Snippet } from "../../types";
import { useI18n } from "../../lib/i18n";
import { useEscClose, useMaskDismiss } from "../../lib/useOverlay";
import SnippetEditor from "./SnippetEditor";

interface Props {
  /** 编辑目标；null = 新建。关闭即卸载，重开永远是一张白表。 */
  editing: Snippet | null;
  /** 归属分类名（全局 / 组 / 本连接），拼在标题里说明新建会进哪个池。 */
  scopeName: string;
  /** 保存成功并关闭（落库在 SnippetEditor，父级据此勾进池/关窗）。 */
  onSave: (s: Snippet) => void;
  onCancel: () => void;
}

export default function SnippetFormDialog({ editing, scopeName, onSave, onCancel }: Props) {
  const t = useI18n();
  // 后挂载者位于 Esc 栈顶（useOverlay 约定）：一次 Esc 只关本弹框，＋ 面板留在身后。
  useEscClose(onCancel);
  return createPortal(
    <div className="modal-mask" {...useMaskDismiss(onCancel)}>
      <div className="modal form-dialog" style={{ width: 460 }} onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <h2>{`${editing ? t("qbar.editTitle") : t("qbar.newTitle")} · ${scopeName}`}</h2>
          <button type="button" className="modal-x" onClick={onCancel} aria-label={t("common.close")}>
            ×
          </button>
        </div>
        <div className="sfd-body">
          <SnippetEditor editing={editing} onSave={onSave} onCancel={onCancel} />
        </div>
      </div>
    </div>,
    document.body,
  );
}
