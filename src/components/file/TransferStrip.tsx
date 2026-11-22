import { useMemo } from "react";
import { baseName, formatBytes, percent } from "../../lib/format";
import { activeCount, aggregateProgress, isFinal, panel, transferStore, useStore } from "../../stores";
import { useI18n } from "../../lib/i18n";

/**
 * 文件视图底部内嵌的传输条：有进行中的传输时出现，展示聚合进度与最近几个任务，
 * 点击直接跳到底部面板的「传输」页。用户在文件页就能看到下载进展，不必来回切页签。
 *
 * 文件视图现在每个会话一份，故只显示**本会话**的任务；否则在 A 会话的文件页里会看到 B 会话的进度。
 * `preparing` 占位计数是全局的（后端未分会话），宁可多显一次“准备中”，也不漏掉反馈。
 */
export default function TransferStrip({ session }: { session: string }) {
  const tr = useI18n();
  const { tasks, preparing } = useStore(transferStore);
  const mine = useMemo(() => tasks.filter((t) => t.sessionId === session), [tasks, session]);
  const live = useMemo(() => mine.filter((t) => !isFinal(t.status)), [mine]);
  // 真任务行尚未落地、但提交外壳已置占位（串行提交首个 stat 往返期间）：也要让传输条现身。
  if (live.length === 0 && preparing === 0) return null;

  // 聚合反映「整批」进度（含已完成）：只算在传的文件会让大批次里条子长期贴 0%、看着像不动。
  const { done, total, pct } = aggregateProgress(mine);
  // 最展示的“最近几个”按 createdAt 升序（先传的在前），与传输面板“活跃置顶·先传在上”口径一致。
  const shown = [...live].sort((a, b) => a.createdAt - b.createdAt).slice(0, 3);

  return (
    <div
      className="tx-strip"
      role="button"
      tabIndex={0}
      title={tr("ts.open")}
      onClick={() => panel.show("transfers")}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") panel.show("transfers");
      }}
    >
      <span className="tx-strip-head">
        <b className="tx-n">{tr("ts.active").replace("{n}", String(activeCount(mine)))}</b>
        <span className="tx-total">
          {formatBytes(done)}
          {total > 0 ? ` / ${formatBytes(total)}` : ""}
        </span>
        <span className="tx-more">{tr("ts.open")} ›</span>
      </span>
      <span className="tx-agg">
        <i style={{ width: `${pct}%` }} />
      </span>
      <span className="tx-rows">
        {live.length === 0 && preparing > 0 && (
          <span className="tx-item">
            <b className="tx-n">{tr("ts.preparing")}</b>
          </span>
        )}
        {shown.map((t) => (
          <span key={t.id} className="tx-item" data-status={t.status}>
            <span className="tx-nm" title={t.remotePath}>
              {t.direction === "upload" ? "↑" : "↓"}{" "}
              {baseName(t.direction === "upload" ? t.localPath : t.remotePath)}
            </span>
            <span className="tx-bar">
              <i style={{ width: `${percent(t.doneBytes, t.totalBytes)}%` }} />
            </span>
            <span className="tx-pct">{percent(t.doneBytes, t.totalBytes)}%</span>
          </span>
        ))}
        {live.length > shown.length && (
          <span className="tx-more">{tr("ts.more").replace("{n}", String(live.length - shown.length))}</span>
        )}
      </span>
    </div>
  );
}
