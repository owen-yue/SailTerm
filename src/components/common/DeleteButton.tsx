// 行内「删除」图标按钮：垃圾桶 + 悬停提示「删除」，统一 icon-btn danger 观感（去浏览器化、跨行对齐）。
// 各列表/设置页的行尾删除都复用它，避免每处重复 title/aria-label/getBoundingClientRect 那套样板。
// 确认浮层由调用方渲染（各处确认文案/前置校验不同）：本组件只负责按钮本身，
// 点击时把锚点（按钮左缘 x、下缘 y+6）回传给调用方定位就近气泡。
import { IconTrash } from "./icons";
import { useI18n } from "../../lib/i18n";

interface Props {
  /** 点击回调：给出就近气泡的锚点坐标（左缘 x、下缘 y+6）。 */
  onConfirm: (x: number, y: number) => void;
  /** 覆盖默认「删除」提示（如「移除」等近义场景）。 */
  title?: string;
  disabled?: boolean;
  /** 追加类名（个别行内布局微调用）。 */
  className?: string;
}

export default function DeleteButton({ onConfirm, title, disabled, className }: Props) {
  const tr = useI18n();
  const label = title ?? tr("common.delete");
  return (
    <button
      type="button"
      className={`icon-btn danger${className ? ` ${className}` : ""}`}
      title={label}
      aria-label={label}
      disabled={disabled}
      onClick={(e) => {
        // 行整行可点时，阻止冒泡避免误触发选中/回放。
        e.stopPropagation();
        const r = e.currentTarget.getBoundingClientRect();
        onConfirm(r.left, r.bottom + 6);
      }}
    >
      <IconTrash />
    </button>
  );
}
