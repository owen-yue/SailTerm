import { useEffect, useMemo, useRef, useState } from "react";
import { formatBytes } from "../../lib/format";
import { errMsg } from "../../lib/tauri/client";
import { proc } from "../../lib/tauri";
import { hasCap } from "../../lib/capabilities";
import { useI18n } from "../../lib/i18n";
import { sessionStore, toast, useStore } from "../../stores";
import { sessionLabel, type ProcCol, type ProcInfo, type ProcSort } from "../../types";
import ConfirmDialog from "../dialog/ConfirmDialog";
import { IconRefresh } from "../common/icons";

/**
 * 后端能全局排序的列（见 commands/process.rs 的 sort_by）：这三列把口径一并下发，
 * 取回的就是该列真正的热点前 N；其余列后端不认，只能在本批样本内重排。
 */
const PUSHDOWN: ProcCol[] = ["pid", "cpu", "mem"];

/** 秒 → 人读时长（天/时/分/秒取前两级，够定位又不啰嗦）。 */
function fmtElapsed(sec: number): string {
  if (sec <= 0) return "—";
  const d = Math.floor(sec / 86400);
  const h = Math.floor((sec % 86400) / 3600);
  const m = Math.floor((sec % 3600) / 60);
  const s = Math.floor(sec % 60);
  if (d) return `${d}d${h}h`;
  if (h) return `${h}h${m}m`;
  if (m) return `${m}m${s}s`;
  return `${s}s`;
}

/**
 * 🧩 进程管理器（PR-5）：当前会话的进程列表 + 结束进程。
 *
 * 与监控同为 exec 派生的只读采样，可见性复用 `hasCap(kind,"monitor")`。
 * 进入有效会话即采样一次（打开就有数据，不再空白等手动刷新）；切会话/改排序即重采；
 * 手动「刷新」按钮单采一次，打开自动刷新后额外每 3s 一采，离开/关会话即停。
 * kill 是写操作：前端确认框即人工审批，后端再兜 pid≤1 + 策略黑名单。
 */
export default function ProcessPanel() {
  const tr = useI18n();
  const { tabs, activeId } = useStore(sessionStore);
  const tab = tabs.find((x) => x.id === activeId) ?? null;
  const sessionId =
    tab && hasCap(tab.kind, "monitor") && tab.state === "connected" ? tab.id : "";

  const [procs, setProcs] = useState<ProcInfo[]>([]);
  // 排序在前端做：后端按 `backendSort` 取回该列热点前 N，这里按点选的列 + 方向即时重排。
  // 交互与观感统一按文件栏口径（FilePane）：初始不排（沿用后端降序热点序），
  // 点列从升序起，同列再点翻转；指示器单箭头 ▲/▼，未激活显 ↕。
  const [sortKey, setSortKey] = useState<ProcCol | null>(null);
  const [sortDir, setSortDir] = useState<"asc" | "desc">("asc");
  const [filter, setFilter] = useState("");
  const [auto, setAuto] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [pending, setPending] = useState<{ proc: ProcInfo; force: boolean } | null>(null);
  const reqSeq = useRef(0);
  // 定时器/回调里的 load() 需要拿到最新过滤词，但 effect 不能把 filter 纳入依赖（否则每敲一
  // 个字符都重挂定时器）——用 ref 每次渲染同步，load 读 ref 永远是最新值。
  const filterRef = useRef(filter);
  filterRef.current = filter;

  // 样本口径：可下推的列按该列取全局前 N（否则按内存/时长排序时，真正的内存大户可能压根
  // 不在「CPU 前 200」这个样本里，重排出来的是错数据）；其余列回落 cpu 热点样本。
  const backendSort: ProcSort =
    sortKey && PUSHDOWN.includes(sortKey) ? (sortKey as ProcSort) : "cpu";
  // 只能在本批样本内重排的列（界面要如实提示，不冒充全局序）。
  const localOnly = !!sortKey && !PUSHDOWN.includes(sortKey);

  const load = () => {
    if (!sessionId) return;
    const seq = ++reqSeq.current;
    setLoading(true);
    const f = filterRef.current.trim();
    proc
      .list(sessionId, backendSort, 200, f || null)
      .then((v) => {
        if (seq !== reqSeq.current) return; // 迟到的旧响应丢弃，防切换会话/排序串台
        setError("");
        setProcs(v);
      })
      .catch((e) => {
        if (seq !== reqSeq.current) return;
        setError(errMsg(e));
      })
      .finally(() => {
        if (seq === reqSeq.current) setLoading(false);
      });
  };

  // 进入有效会话即采样一次（修「打开列表空」）；改样本口径同样立即重采；
  // 开启自动刷新则额外起 3s 定时器。切会话清空列表并作废在途请求；无有效会话不采。
  useEffect(() => {
    if (!sessionId) {
      reqSeq.current++;
      setProcs([]);
      setError("");
      return;
    }
    load();
    if (!auto) return;
    const t = window.setInterval(() => load(), 3000);
    return () => window.clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId, auto, backendSort]);

  // 过滤词变化：防抖 400ms 下发服务端权威搜索（后端对**全量**样本匹配，不受前 200 截断影响，
  // 「搜什么就看到什么」）；清空过滤词也重采一次，恢复热点前 200 视图。首轮挂载跳过
  // （上面的 effect 已采样一次），避免重复 exec。
  const firstFilter = useRef(true);
  useEffect(() => {
    if (!sessionId) return;
    if (firstFilter.current) {
      firstFilter.current = false;
      return;
    }
    const t = window.setTimeout(() => load(), 400);
    return () => window.clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filter, sessionId]);

  const doKill = async (p: ProcInfo, force: boolean) => {
    setPending(null);
    try {
      const r = await proc.kill(sessionId, p.pid, force ? "KILL" : "TERM");
      const label = p.cmd || String(p.pid);
      const pid = String(p.pid);
      if (r?.gone === true) {
        // 后端核验已不在进程表，才报「已结束」——kill 退出码 0 只代表信号已投递。
        toast.success(tr("proc.killed").replace("{cmd}", label).replace("{pid}", pid));
      } else if (r?.gone === false && (r.state ?? "").startsWith("Z")) {
        // 僵尸态：进程已死但父进程未回收，不会再消耗资源，父进程收尸后自动从列表消失。
        toast.warn(
          tr("proc.killedZombie").replace("{cmd}", label).replace("{pid}", pid),
        );
      } else if (r?.gone === false) {
        // 信号已投递但进程仍在运行：可能忽略 TERM / 正在优雅退出 / 被守护进程拉起。
        toast.warn(tr("proc.killAlive").replace("{cmd}", label).replace("{pid}", pid));
      } else {
        // Windows / 核验不可用：维持旧提示（信号已成功投递）。
        toast.success(tr("proc.killed").replace("{cmd}", label).replace("{pid}", pid));
      }
      load();
    } catch (e) {
      toast.error(errMsg(e));
    }
  };

  const q = filter.trim().toLowerCase();
  const shown = q
    ? procs.filter(
        (p) =>
          p.cmd.toLowerCase().includes(q) ||
          p.user.toLowerCase().includes(q) ||
          String(p.pid) === q,
      )
    : procs;

  // 与文件栏 toggleSort 同构：换列一律从升序起（不再按数值/文本列区分默认方向）。
  const clickSort = (col: ProcCol) => {
    if (col === sortKey) setSortDir((d) => (d === "asc" ? "desc" : "asc"));
    else {
      setSortKey(col);
      setSortDir("asc");
    }
  };

  const sorted = useMemo(() => {
    // 未选列：后端原序（backendSort 降序热点在前），与文件栏 sortKey=null 时同语义
    if (!sortKey) return shown;
    const sign = sortDir === "asc" ? 1 : -1;
    const cmp = (a: ProcInfo, b: ProcInfo): number => {
      switch (sortKey) {
        case "pid": return a.pid - b.pid;
        case "cpu": return (a.cpuPct ?? -1) - (b.cpuPct ?? -1);
        case "mem": return a.memKb - b.memKb;
        case "elapsed": return a.elapsedSec - b.elapsedSec;
        case "user": return a.user.localeCompare(b.user);
        case "state": return a.state.localeCompare(b.state);
        case "cmd": return a.cmd.localeCompare(b.cmd);
      }
    };
    // 同值时回退 pid 升序，保证稳定可读、不自排拖动。
    return [...shown].sort((a, b) => sign * (cmp(a, b) || a.pid - b.pid));
  }, [shown, sortKey, sortDir]);

  // 表头单元格：数值列标 pp-num 右对齐（与单元格一致）；排序指示与文件栏共用
  // .fp-sort-ind 观感（单箭头 ▲/▼/↕，激活列染品牌色）。可下推的列提示常规排序口径，
  // 只能在本批样本内重排的列额外说明局限（点表头不重取，样本仍是该口径的热点前 N）。
  const th = (col: ProcCol, label: string, num = false) => (
    <button
      type="button"
      className={`pp-th${num ? " pp-num" : ""}`}
      data-active={sortKey === col}
      title={localOnly && sortKey === col ? tr("proc.localSortTip") : undefined}
      onClick={() => clickSort(col)}
    >
      {label}
      <span className="fp-sort-ind">{sortKey === col ? (sortDir === "asc" ? "▲" : "▼") : "↕"}</span>
    </button>
  );

  return (
    <div className="pp">
      <div className="pp-head">
        <span className="fp-title">{tr("proc.title")}</span>
        <span className="tp-sess">{tab ? sessionLabel(tab, tr) : "—"}</span>
        <input
          className="pp-filter"
          placeholder={tr("proc.filter.ph")}
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <button
          type="button"
          className={`pp-auto${auto ? " on" : ""}`}
          title={tr("proc.autoTip")}
          onClick={() => setAuto((a) => !a)}
        >
          {tr("proc.auto")}
        </button>
        <button
          type="button"
          className="icon-btn"
          title={tr("proc.refresh")}
          disabled={!sessionId || loading}
          onClick={() => load()}
        >
          <IconRefresh />
        </button>
      </div>

      {!sessionId ? (
        <div className="ss-empty">{tr("proc.unsupported")}</div>
      ) : (
        <>
          {error && <div className="form-error">{error}</div>}
          {!filter.trim() && procs.length >= 200 && (
            <div className="pp-hint">{tr("proc.truncHint")}</div>
          )}
          <div className="pp-table">
            <div className="pp-row pp-head-row">
              {th("pid", "PID", true)}
              {th("user", tr("proc.col.user"))}
              {th("state", tr("proc.col.state"))}
              {th("cpu", tr("proc.col.cpu"), true)}
              {th("mem", tr("proc.col.mem"), true)}
              {th("elapsed", tr("proc.col.elapsed"), true)}
              {th("cmd", tr("proc.col.cmd"))}
              <span className="pp-th pp-acts" />
            </div>
            {sorted.length === 0 ? (
              <div className="ss-empty">
                {loading ? "…" : filter.trim() ? tr("proc.noMatch") : tr("proc.empty")}
              </div>
            ) : (
              sorted.map((p) => (
                <div className="pp-row" key={p.pid}>
                  <span className="pp-td pp-pid">{p.pid}</span>
                  <span className="pp-td pp-user" title={p.user}>
                    {p.user || "—"}
                  </span>
                  <span className="pp-td">{p.state || "—"}</span>
                  <span className="pp-td pp-num">
                    {p.cpuPct != null ? `${p.cpuPct.toFixed(1)}%` : "—"}
                  </span>
                  <span className="pp-td pp-num">{formatBytes(p.memKb * 1024)}</span>
                  <span className="pp-td pp-num">{fmtElapsed(p.elapsedSec)}</span>
                  <span className="pp-td pp-cmd" title={p.cmd}>
                    {p.cmd}
                  </span>
                  <span className="pp-td pp-acts">
                    <button
                      type="button"
                      className="pp-btn"
                      title={tr("proc.kill")}
                      onClick={() => setPending({ proc: p, force: false })}
                    >
                      {tr("proc.kill")}
                    </button>
                    <button
                      type="button"
                      className="pp-btn danger"
                      title={tr("proc.killForce")}
                      onClick={() => setPending({ proc: p, force: true })}
                    >
                      {tr("proc.killForceShort")}
                    </button>
                  </span>
                </div>
              ))
            )}
          </div>
        </>
      )}

      {pending && (
        <ConfirmDialog
          title={tr("proc.kill")}
          message={
            tr("proc.confirmMsg")
              .replace("{cmd}", pending.proc.cmd || String(pending.proc.pid))
              .replace("{pid}", String(pending.proc.pid))
          }
          warning={
            pending.force ? tr("proc.confirmWarnForce") : tr("proc.confirmWarnTerm")
          }
          okText={pending.force ? tr("proc.killForce") : tr("proc.kill")}
          onOk={() => void doKill(pending.proc, pending.force)}
          onClose={() => setPending(null)}
        />
      )}
    </div>
  );
}
