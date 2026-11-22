import type { ReactNode } from "react";
import Select, { type SelectOption } from "./Select";
import { IconEdit } from "./icons";

interface Props {
  value: string;
  options: SelectOption[];
  onChange: (value: string) => void;
  /** 底部「添加」动作行的文案（作为参数传入，如「添加模型」）。 */
  addLabel: string;
  /** 点击底部「添加」行的回调（不改变当前选中）。 */
  onAdd: () => void;
  /**
   * 悬停某行时右侧淡入的「编辑」按钮回调（携该行 value）；不改变当前选中。
   * 不传（`undefined`）则行内不显示编辑 affordance。
   */
  onEdit?: (value: string) => void;
  /** 编辑按钮的 title / aria-label（图标按钮需可读文本）。 */
  editTitle?: string;
  /** 编辑行的图标（默认铅笔），特殊场景可替换。 */
  editIcon?: ReactNode;
  className?: string;
  title?: string;
  placeholder?: string;
  disabled?: boolean;
}

/**
 * 可增可改的下拉框（在通用 Select 之上封装两个固定语义的动作）：
 * - 每行悬停淡入「编辑」图标 → `onEdit(value)`；
 * - 弹层底部分隔行「添加…」 → `onAdd()`。
 * 具体做什么（打开哪个弹框、编辑/新增哪条）全部由调用方通过回调决定，组件本身不含业务。
 * 供模型选择等「列表 + 就地管理」场景复用（AiDrawer / AgentPanel）。
 */
export default function EditableSelect({
  value,
  options,
  onChange,
  addLabel,
  onAdd,
  onEdit,
  editTitle,
  editIcon,
  className,
  title,
  placeholder,
  disabled,
}: Props) {
  return (
    <Select
      className={className}
      value={value}
      options={options}
      onChange={onChange}
      title={title}
      placeholder={placeholder}
      disabled={disabled}
      optionAction={
        onEdit
          ? { icon: editIcon ?? <IconEdit />, title: editTitle ?? addLabel, onSelect: onEdit }
          : undefined
      }
      footerAction={{ label: addLabel, onSelect: onAdd }}
    />
  );
}
