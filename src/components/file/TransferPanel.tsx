import { useEffect, useMemo, useState } from "react";
import { baseName, formatBytes, formatEta, formatSpeed, percent } from "../../lib/format";
import { activeCount, aggregateProgress, isFinal, sessionStore, transferActions, transferStore, useStore } from "../../stores";
import { sessionLabel } from "../../types";
import { useI18n } from "../../lib/i18n";
import Popconfirm from "../common/Popconfirm";
import { IconRefresh, IconTrash } from "../common/icons";

/**
 * 状态文案走词典：旧写法在组件里写死 "OK"/"ERR"/"STOP"，中文界面下队列列一堆英文缩写，
 * 读屏也会按字母逐个念。未登记者回返原始枚举值（后端新增状态时不至于渲染为空）。
 */
const statusText = (tr: (k: string) => string, s: string): string => {
  const key = `tp.status.${s}`;
  const v = tr(key);
  return v === key ? s : v;
};

/**
 * 传输队列：**只显当前会话**——新连接即干净列表，每一行都能重传/续传
 * （旧会话的历史行重传会报「会话不存在或已关闭」，不再露出）。
 * 历史行仍留在库里供审计；删服务器时后端级联清理（db/profile.rs）。
 */
export default function TransferPanel() {
  const tr = useI18n();
  const { tasks, loading } = useStore(transferStore);
  const { tabs, activeId } = useStore(sessionStore);
  /** 清空确认：一键清掉整批结束记录，在垃圾桶按钮下方就近弹气泡确认（不阻断上下文）。 */
  const [clearAnchor, setClearAnchor] = useState<{ x: number; y: number } | null>(null);

  const tab = tabs.find((t) => t.id === activeId) ?? null;
  const sid = tab?.id ?? "";
  // 只看本会话行：其它会话（含重连前的旧 session id）一律不可见
  const shown = useMemo(
    () => (sid ? tasks.filter((t) => t.sessionId === sid) : []),
    [tasks, sid],
  );
  const running = activeCount(shown);
  // 总进度聚合「整批」（含已完成）、与底部传输条同口径：见 aggregateProgress。
  // 旧口径只算未完成任务，会让传完的文件从分子分母同时消失、进度不累积（条子长期贴 0%）。
  const totals = useMemo(() => aggregateProgress(shown), [shown]);
  // 显示顺序：进行中置顶 → 待传 → 已结束；同组按 createdAt 升序（先传的在上）。
  // 后端 list_tasks 是 created_at DESC（最新在上），会让「正在传」的最早建行文件沉到列表底部、
  // 被一堆 0% pending 行淹没——看不到有东西在动。聚合进度与顺序无关，仍用 shown。
  const ordered = useMemo(() => {
    const rank = (s: string) => (s === "running" ? 0 : s === "pending" ? 1 : 2);
    return [...shown].sort((a, b) => rank(a.status) - rank(b.status) || a.createdAt - b.createdAt);
  }, [shown]);

  useEffect(() => {
    if (sid) void transferActions.refresh(sid);
  }, [sid]);

  return (
    <>
    <div className="tp">
      <div className="tp-head">
        <span className="fp-title">{tr("panel.transfers")}</span>
        <span className="tp-sess">{tab ? sessionLabel(tab, tr) : "—"}</span>
        <span className="tp-sum">
          {running > 0 ? `${tr("tp.running").replace("{n}", String(running))} · ` : ""}
          {shown.length}
        </span>
        <span className="fp-acts">
          <button
            type="button"
            className="ghost-link danger"
            title={tr("tp.cancelAllTitle")}
            disabled={running === 0}
            onClick={() => void transferActions.cancelAll(sid)}
          >
            {tr("tp.cancelAll")}
          </button>
          <button
            className="icon-btn"
            title={tr("common.refresh")}
            disabled={loading}
            onClick={() => void transferActions.refresh(sid)}
          >
            <IconRefresh />
          </button>
          <button
            className="icon-btn danger"
            title={tr("tp.clearTitle")}
            disabled={shown.every((t) => !isFinal(t.status))}
            onClick={(e) => {
              // 锚到按钮下方、右缘对齐（气泡宽 244px）；越界由 Popconfirm 自行翻转。
              const r = e.currentTarget.getBoundingClientRect();
              setClearAnchor({ x: r.right - 244, y: r.bottom + 6 });
            }}
          >
            <IconTrash />
          </button>
        </span>
      </div>

      {shown.length > 0 && running > 0 && (
        <div className="tp-agg" title={tr("tp.aggTitle")}>
          <div className="tp-agg-bar">
            <i style={{ width: `${totals.pct}%` }} />
          </div>
          <span className="tp-agg-txt">
            {totals.pct}% · {formatBytes(totals.done)} / {formatBytes(totals.total)}
          </span>
        </div>
      )}

      <div className="tp-list">
        {ordered.map((t) => {
          const pct = percent(t.doneBytes, t.totalBytes);
          const eta = formatEta(t.doneBytes, t.totalBytes, t.speed);
          const name = baseName(t.direction === "upload" ? t.localPath : t.remotePath);
          return (
            <div key={t.id} className="tp-row" data-status={t.status}>
              <span className="tp-dir">{t.direction === "upload" ? "↑" : "↓"}</span>
              <div className="tp-main">
                <div className="tp-line">
                  {t.status === "running" && t.phase && t.phase !== "transfer" && (
                    <span className="tp-phase">
                      {t.phase === "compress" ? `📦 ${tr("bundle.compressing")}` : `📥 ${tr("bundle.extracting")}`}
                    </span>
                  )}
                  <span className="tp-nm" title={`${t.localPath} ↔ ${t.remotePath}`}>
                    {name}
                  </span>
                  <span className="tp-st">{statusText(tr, t.status)}</span>
                </div>
                {t.status === "running" || t.status === "pending" ? (
                  <div className="tp-bar">
                    <i style={{ width: `${pct}%` }} />
                  </div>
                ) : null}
                <div className="tp-meta">
                  <span>
                    {formatBytes(t.doneBytes)}
                    {t.totalBytes > 0 ? ` / ${formatBytes(t.totalBytes)}` : ""}
                  </span>
                  {t.status === "running" && (
                    <span>
                      {formatSpeed(t.speed)}
                      {eta ? ` · ${eta}` : ""}
                    </span>
                  )}
                  {t.error && (
                    <span className="tp-msg" title={t.error}>
                      {t.error}
                    </span>
                  )}
                </div>
              </div>
              <span className="tp-acts">
                {isFinal(t.status) ? (
                  <button className="tp-btn" onClick={() => void transferActions.retry(t.id)}>
                    {t.status === "done" ? tr("tp.retry") : tr("tp.resume")}
                  </button>
                ) : (
                  <button className="tp-btn" onClick={() => void transferActions.cancel(t.id)}>
                    {tr("common.cancel")}
                  </button>
                )}
              </span>
            </div>
          );
        })}
        {shown.length === 0 && (
          <div className="fp-empty">{loading ? "…" : tr("tp.empty")}</div>
        )}
      </div>
    </div>

    {clearAnchor && (
      <Popconfirm
        x={clearAnchor.x}
        y={clearAnchor.y}
        title={tr("tp.clearMsg").replace("{n}", String(shown.filter((t) => isFinal(t.status)).length))}
        description={tr("tp.clearWarn")}
        okText={tr("tp.clearBtn")}
        onConfirm={() => {
          setClearAnchor(null);
          void transferActions.clear(sid);
        }}
        onClose={() => setClearAnchor(null)}
      />
    )}
    </>
  );
}
