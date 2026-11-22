import { type ReactNode } from "react";
import { useI18n } from "../../lib/i18n";
import { useEscClose, useMaskDismiss } from "../../lib/useOverlay";
import Button from "../common/Button";

interface Props {
  /** 弹窗标题（如「新建模型」「编辑密钥」）。 */
  title: string;
  /** 关闭（取消 / X / 点遮罩 / Esc 统一走这里）。 */
  onClose: () => void;
  /** 提交（表单 onSubmit 或回车触发）。 */
  onSubmit: () => void;
  /** 主按钮文案，默认取 `common.save`。 */
  okText?: string;
  /** 取消按钮文案，默认取 `common.cancel`。 */
  cancelText?: string;
  /** 底部左侧的附加操作（如「测试连通」），与右侧取消/保存分置。 */
  extraActions?: ReactNode;
  /** 提交中禁用主按钮。 */
  submitting?: boolean;
  /**
   * 异步流程进行中（如保存后自动测试）：主按钮转圈且不可重复提交，
   * 同时屏蔽取消/遮罩/Esc，避免结果未回就关窗丢失回执。
   */
  loading?: boolean;
  /** 弹窗宽度，默认 480。 */
  width?: number;
  /** 表单正文（字段）。 */
  children: ReactNode;
}

/**
 * 全局统一的「新增 / 编辑」表单弹窗。
 *
 * 设置中心里各处管理页（模型 / 密钥 / 记忆 / 片段 / 触发器 / 隧道 / 插件）此前都在列表
 * 下方内联展开一大块表单，既撑长页面又各写各的。本组件把「编辑表单」收敛成一种形态：
 * 聚焦的居中弹窗，列表留在背后。遮罩 z-index 高于设置模态（50），可稳妥盖在其上；
 * 内部复用 `.ss-form` 字段样式，视觉与其它设置项一致。
 */
export default function FormDialog({
  title,
  onClose,
  onSubmit,
  okText,
  cancelText,
  extraActions,
  submitting = false,
  loading = false,
  width = 480,
  children,
}: Props) {
  const t = useI18n();
  const busy = submitting || loading;
  /** 统一出口：异步进行中不允许退出弹窗。 */
  const requestClose = () => {
    if (!busy) onClose();
  };

  /** × 是「强制关闭」出口：即便提交/测试在途也放行。后端命令永不 settle 时（历史反复
   *  出现的卡死），这是用户不依赖 IPC 超时就能脱身的兜底；异步结果本就落 store/toast，
   *  关窗不丢回执。遮罩与 Esc 仍走 requestClose（提交中防误关丢表单）。 */
  const forceClose = () => onClose();

  // Esc 与遮罩均走统一出口（提交中不得退出），并登记进弹层栈只关最顶层。
  useEscClose(requestClose);

  return (
    <div className="modal-mask fd-mask" {...useMaskDismiss(requestClose)}>
      <div className="modal form-dialog" style={{ width }} onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <h2>{title}</h2>
          <button type="button" className="modal-x" onClick={forceClose} aria-label={t("common.close")}>
            ×
          </button>
        </div>
        <form
          className="fd-form"
          onSubmit={(e) => {
            e.preventDefault();
            if (!busy) onSubmit();
          }}
        >
          <div className="fd-body ss-form">{children}</div>
          <div className="form-actions fd-foot">
            {extraActions && <div className="fd-foot-l">{extraActions}</div>}
            <Button variant="ghost" disabled={busy} onClick={requestClose}>
              {cancelText ?? t("common.cancel")}
            </Button>
            <Button type="submit" loading={loading} disabled={submitting}>
              {okText ?? t("common.save")}
            </Button>
          </div>
        </form>
      </div>
    </div>
  );
}
