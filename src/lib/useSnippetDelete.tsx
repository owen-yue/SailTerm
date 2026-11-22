// 片段删除的气泡就近确认（共享行为层）：快捷栏 ＋ 弹框与分组池入口两处同套。
// 用法：const delSnippet = useSnippetDelete(); → 行尾 × 处 delSnippet.confirm(s, x, y)，
// JSX 里渲染 {delSnippet.element}。删的是片段库本体，各池/栏的陈旧引用由调用方既有 effect 清理。
import { useState } from "react";
import type { Snippet } from "../types";
import { snippetActions } from "../stores";
import { t } from "./i18n";
import Popconfirm from "../components/common/Popconfirm";

export function useSnippetDelete() {
  // 待删片段 + 气泡锚点（视口坐标）：与侧栏删连接同口径的就地确认。
  const [pending, setPending] = useState<{ id: string; name: string; x: number; y: number } | null>(
    null,
  );

  /** 锚点 = 气泡左上角视口坐标（各调用方沿用既有口径：触发元素底边 + 6px）。 */
  function confirm(s: Snippet, x: number, y: number): void {
    setPending({ id: s.id, name: s.name, x, y });
  }

  const element = pending ? (
    <Popconfirm
      x={pending.x}
      y={pending.y}
      title={t("sn.delMsg").replace("{n}", pending.name)}
      onConfirm={() => {
        const id = pending.id;
        setPending(null);
        void snippetActions.remove(id);
      }}
      onClose={() => setPending(null)}
    />
  ) : null;

  return { confirm, element };
}
