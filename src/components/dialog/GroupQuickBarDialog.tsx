// 分组命令池弹窗：侧栏分组行右键 / 终端 ＋ 面板「组快捷命令…」打开。
// 绑 `group:<groupId>` 池：勾选 = 进池（组内会话的 ＋ 面板组区可见），
// 上不上栏由各会话在 ＋ 面板里勾选或一键应用决定（无隐含继承）。
import { useEffect } from "react";
import { groupKeyOf, quickBarActions, quickBarStore, snippetActions, snippetStore, useStore } from "../../stores";
import type { Snippet } from "../../types";
import { useEscClose, useMaskDismiss } from "../../lib/useOverlay";
import { useI18n } from "../../lib/i18n";
import { useSnippetDelete } from "../../lib/useSnippetDelete";
import QuickPickList from "../terminal/QuickPickList";

interface Props {
  groupId: string;
  groupName: string;
  onClose: () => void;
}

export default function GroupQuickBarDialog({ groupId, groupName, onClose }: Props) {
  const t = useI18n();
  const { byProfile } = useStore(quickBarStore);
  const { items } = useStore(snippetStore);
  useEscClose(onClose);

  // 片段库可能还没拉过（侧栏入口不经过 QuickBar 挂载），空时补一次刷新（幂等）。
  useEffect(() => {
    if (items.length === 0 && !snippetStore.get().loading) void snippetActions.refresh();
  }, [items.length]);

  const key = groupKeyOf(groupId);
  const ids = byProfile[key] ?? [];
  // 池内命令（保持池顺序）；已删片段跳过
  const list = ids
    .map((id) => items.find((s) => s.id === id))
    .filter((s): s is Snippet => Boolean(s));

  const toggle = (s: Snippet, on: boolean) => {
    if (on) quickBarActions.removeSnippet(key, s.id);
    else quickBarActions.addSnippet(key, s.id);
  };

  // 行尾 × 删除：与 ＋ 面板 / 设置页片段库同一套气泡就近确认（useSnippetDelete），删的是库本体
  const delSnippet = useSnippetDelete();

  return (
    <div className="modal-mask fd-mask" {...useMaskDismiss(onClose)}>
      <div className="modal form-dialog" style={{ width: 420 }} onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <h2>{`${groupName} · ${t("qbar.groupCmds")}`}</h2>
          <button type="button" className="modal-x" onClick={onClose} aria-label={t("common.close")}>
            ×
          </button>
        </div>
        <div className="gqb-body">
          {items.length === 0 ? (
            <div className="qbar-picker-empty">{t("qbar.pickEmptyLib")}</div>
          ) : list.length === 0 ? (
            <div className="qbar-picker-empty">{t("qbar.groupEmpty")}</div>
          ) : (
            <QuickPickList
              list={list}
              checkedIds={ids}
              onToggle={toggle}
              onRemove={(s, a) => delSnippet.confirm(s, a.x, a.y)}
            />
          )}
        </div>
        {delSnippet.element}
      </div>
    </div>
  );
}
