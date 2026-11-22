// 快捷栏勾选行列表（共享体）：QuickBar 的「＋」弹层三个分类区与分组池弹窗共用。
// 行主体点击 = 勾选/取消（✓ 格就在主体里，所见即所得）；破坏性/编辑动作拆成行尾小按钮：
// hover 出 ✎ 编辑（onEdit）与 × 删除（onRemove，就近确认由调用方做），避免误触。
// 语义由调用方定义：list = 本区展示的命令，checkedIds = 打 ✓ 的依据（如「在本连接快捷栏上」）。
import { useI18n } from "../../lib/i18n";
import type { Snippet } from "../../types";

interface Props {
  /** 本区展示的命令列表。 */
  list: Snippet[];
  /** 打 ✓ 的 id 集合依据。 */
  checkedIds: string[];
  /** 行主体点击：on = 切换前该行是否打 ✓。 */
  onToggle: (snippet: Snippet, on: boolean) => void;
  /** ✎ 编辑回调：el = 触发按钮元素（供二级面板跟随其位置）；缺省则不显示编辑。 */
  onEdit?: (snippet: Snippet, el: HTMLElement) => void;
  /** 行尾 × 删除回调：anchor = 气泡确认的锚点视口坐标（缺省则不显示删除）。 */
  onRemove?: (snippet: Snippet, anchor: { x: number; y: number }) => void;
}

export default function QuickPickList({ list, checkedIds, onToggle, onEdit, onRemove }: Props) {
  const t = useI18n();
  return (
    <>
      {list.map((s) => {
        const on = checkedIds.includes(s.id);
        return (
          <div key={s.id} className={on ? "qbar-picker-item on" : "qbar-picker-item"}>
            <button
              type="button"
              className="qbar-picker-item-main"
              title={t(on ? "qbar.uncheckIt" : "qbar.pinIt")}
              onClick={() => onToggle(s, on)}
            >
              <span className={on ? "qbar-picker-check" : "qbar-picker-check off"}>
                {on && (
                  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                    <path d="M20 6 9 17l-5-5" />
                  </svg>
                )}
              </span>
              <span className="qbar-picker-name">{s.name}</span>
              {s.groupName && <span className="qbar-picker-group">{s.groupName}</span>}
            </button>
            {onEdit && (
              <button
                type="button"
                className="qbar-pick-edit"
                title={t("qbar.editIt")}
                onClick={(e) => onEdit(s, e.currentTarget)}
              >
                ✎
              </button>
            )}
            {onRemove && (
              <button
                type="button"
                className="qbar-pick-del"
                title={t("qbar.delIt")}
                onClick={(e) => {
                  const r = e.currentTarget.getBoundingClientRect();
                  onRemove(s, { x: r.left, y: r.bottom + 6 });
                }}
              >
                ×
              </button>
            )}
          </div>
        );
      })}
    </>
  );
}
