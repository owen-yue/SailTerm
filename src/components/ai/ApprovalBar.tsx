import { useEffect } from "react";
import { aiChat, aiStore, sessionStore, useStore } from "../../stores";
import { IconInfo } from "../common/icons";
import Chevron from "../common/Chevron";
import { useI18n } from "../../lib/i18n";
import { useBarFold } from "../../lib/useBarFold";
import { kbdLabel } from "../../lib/kbd";
import { sessionLabel, type AiApprovalPrompt } from "../../types";

/**
 * 授权确认条：**唯一的裁决入口**，同一条 UI 挂两个位置（两种模式的确认框不该长得不一样）。
 * - Agent/Shell 档 → 终端里贴在提示符正下方（Terminal.tsx），离敲命令的地方最近；
 * - 聊天档 → 挂在自己的聊天窗口内、输入框上方（AiDrawer.tsx，带 `inline`）：这一档的命令走
 *   独立 exec 通道、不占用户终端，于是审批也不该去打扰终端画面。
 *
 * 门道没变：五要素详情、编辑改后命令、「总是允许」、超时后端按拒绝（fail-closed）。
 */
export default function ApprovalBar({
  sessionId,
  inline = false,
  surface,
  keysOff = false,
}: {
  sessionId: string;
  inline?: boolean;
  /** 只渲染该「来源轨」的审批（chat / agent）：不传则不按轨过滤。避免切页签时跨轨泄漏。 */
  surface?: "chat" | "agent";
  /**
   * 备用屏（vim/less/top）时由调用方置 true：不接管 Ctrl+Enter。
   * 全屏程序自己可能用这个组合键，此时抢过来等于把用户按键吃掉。
   */
  keysOff?: boolean;
}) {
  const t = useI18n();
  const { approvals } = useStore(aiStore);
  const { tabs } = useStore(sessionStore);
  // 按轨严格过滤：只渲染本轨的审批。surface 已在 pushApproval 经 surfaceOf 归一为恒非空（缺 track 时兜底猜单一轨），
  // 故此处直接比对即可——既不跨轨泄漏，也不会因滤空而永久挂死。`surface == null` 指调用方未指定轨（不过滤）。
  const pending = approvals.filter(
    (a) => a.sessionId === sessionId && (surface == null || a.surface === surface),
  );
  const top: AiApprovalPrompt | undefined = pending[0];
  const danger = top?.risk === "DANGEROUS";
  const [foldPref, toggleFold] = useBarFold();
  // 只有挂在面板里的两处（inline）能收起：终端浮层那条是「离敲命令最近的唯一裁决入口」，
  // 折叠会弱化命令可见性，故保持常驻展开。收起态下 Ctrl+Enter 仍然有效（上面的监听没关），
  // 只是不再展示键位提示。
  const folded = inline && foldPref;

  // 鼠标不在手边时的退路：有 pending 审批才登记这对组合键（平时一个按键也不多接）。
  // 只接 Ctrl/Cmd+Enter（±Shift = 拒绝）：它不是 readline 文本键，不与 shell 抢字符；
  // 焦点在聊天输入框/自填框里时不抢——那里 Ctrl+Enter 是用户自己的换行与发送语义。
  const topId = top?.requestId ?? null;
  useEffect(() => {
    if (!topId || keysOff) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Enter" || !(e.ctrlKey || e.metaKey) || e.altKey) return;
      if (e.defaultPrevented) return;
      const el = document.activeElement as HTMLElement | null;
      // xterm 的辅助 textarea 代表「用户正在终端里」，这正是要给键盘退路的场景，故不算输入框
      const inField =
        !!el && !el.closest(".xterm") && !!el.closest("input,textarea,[contenteditable]");
      if (inField) return;
      e.preventDefault();
      e.stopPropagation();
      void aiChat.respondApproval(topId, !e.shiftKey);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [topId, keysOff]);

  if (!top) return null;

  const tab = tabs.find((x) => x.id === sessionId);
  const host = tab ? sessionLabel(tab, t) : "";
  return (
    <div
      className={`agent-float${inline ? " inline" : ""}${folded ? " folded" : ""}`}
      data-danger={danger}
      // 危险条要打断读屏（默认 status 的 polite 会被慢悠悠地忽略）
      role={danger ? "alert" : "status"}
    >
      <div className="agent-float-main">
        {/* 窄面板下目的与命令都会被省略号截断（CSS ellipsis）：把全文挂到 title，悬停即读全，
            完整五要素另有 ⓘ 详情卡。 */}
        <div className="agent-float-why" title={top.purpose || t("ab.defaultPurpose")}>
          <span className="agent-float-tag">{danger ? t("ab.danger") : t("ab.needAuth")}</span>
          {top.purpose || t("ab.defaultPurpose")}
          {pending.length > 1 && (
            <span className="agent-float-tag">{t("ab.more").replace("{n}", String(pending.length - 1))}</span>
          )}
        </div>
        <div className="agent-float-q" title={top.summary}>{top.summary.slice(0, 96)}</div>
        {/* 「批的是哪台机器的命令」是审批的第一要素，此前只能靠页签颜色和一条省略号猜；
            键位提示是可发现性的一半——不知道 Ctrl+Enter 就等于没有键盘退路。 */}
        <div className="agent-float-meta">
          {host && (
            <span className="agent-float-host" title={t("ab.hostTip")}>
              {host}
            </span>
          )}
          {!keysOff && <span className="agent-float-kbd">{kbdLabel(t("ab.kbdHint"))}</span>}
        </div>
      </div>
      <div className="agent-float-actions">
      <button
        type="button"
        className="ag-detail"
        title={t("ab.detailTip")}
        onClick={() => aiChat.showDetail(top.requestId)}
      >
        <IconInfo />
      </button>
      <button
        type="button"
        className="ag-always"
        title={t("ag.alwaysTip")}
        onClick={() => void aiChat.respondApproval(top.requestId, true, undefined, true)}
      >
        {t("ag.always")}
      </button>
      <button
        type="button"
        className="ag-exec"
        data-danger={danger}
        onClick={() => void aiChat.respondApproval(top.requestId, true)}
      >
        {t("ag.run")}
      </button>
      <button
        type="button"
        className="ag-reject"
        onClick={() => void aiChat.respondApproval(top.requestId, false)}
      >
        {t("ad.deny")}
      </button>
      {inline && (
        <button
          type="button"
          className="bar-fold"
          aria-expanded={!folded}
          title={t(folded ? "common.expandAnswer" : "common.collapse")}
          onClick={toggleFold}
        >
          <Chevron dir={folded ? "up" : "down"} />
        </button>
      )}
      </div>
    </div>
  );
}
