import { useEffect, useMemo, useRef, useState } from "react";
import { errMsg } from "../../lib/tauri/client";
import { svc } from "../../lib/tauri";
import { hasCap } from "../../lib/capabilities";
import { t, useI18n } from "../../lib/i18n";
import { sessionStore, toast, useStore } from "../../stores";
import { sessionLabel, type SvcAction, type SvcInfo } from "../../types";
import ConfirmDialog from "../dialog/ConfirmDialog";
import { IconRefresh } from "../common/icons";

/** 可排序列（服务全集一次拉回、无截断样本，前端全列即时重排即为全局序）。 */
type SvcCol = "name" | "state" | "loaded" | "enabled" | "pid";

/** 徽标三态：running / stopped / failed；state 空串（SysV 判不了）显「未知」；
 * active 但非 running（如 exited）如实显 systemd 原词（跨语言稳定术语，不硬译）。 */
function badge(s: SvcInfo): { cls: string; label: string } {
  if (s.state === "failed") return { cls: "fail", label: t("svc.failed") };
  if (s.state === "active") {
    if (s.sub === "running") return { cls: "run", label: t("svc.running") };
    return { cls: "stop", label: s.sub || t("svc.stopped") };
  }
  if (!s.state) return { cls: "stop", label: t("svc.unknown") };
  return { cls: "stop", label: t("svc.stopped") };
}

/** 动作按固定列序排列，状态只决定哪些可用——同名按钮在所有行的相对位置一致，鼠标不用逐行找位置。
 * 各态可用性（语义同前）：运行中=停/重/载，失败=停/重，已停=启/重，判不了（SysV）=启/停/重。 */
const SVC_ACTION_ORDER: SvcAction[] = ["start", "stop", "restart", "reload"];
function actionsFor(s: SvcInfo): SvcAction[] {
  const allowed =
    s.state === "active"
      ? ["stop", "restart", "reload"]
      : s.state === "failed"
        ? ["stop", "restart"]
        : s.state === "inactive"
          ? ["start", "restart"]
          : ["start", "stop", "restart"];
  return SVC_ACTION_ORDER.filter((a) => allowed.includes(a));
}

/**
 * 服务面板：当前会话的服务列表 + 启停。
 *
 * 与进程面板同档（都是 exec 派生采样），可见性复用 `hasCap(kind,"monitor")`。
 * 进入有效会话即采样一次；服务状态变化低频，不做自动轮询，手动「刷新」单采一次。
 * 启停是写操作：前端确认框即人工审批，后端再兜服务名白名单 + 策略黑名单（与 kill 同构）。
 */
export default function ServicesPanel() {
  const tr = useI18n();
  const { tabs, activeId } = useStore(sessionStore);
  const tab = tabs.find((x) => x.id === activeId) ?? null;
  const sessionId =
    tab && hasCap(tab.kind, "monitor") && tab.state === "connected" ? tab.id : "";

  const [svcs, setSvcs] = useState<SvcInfo[]>([]);
  const [sortKey, setSortKey] = useState<SvcCol | null>(null);
  const [sortDir, setSortDir] = useState<"asc" | "desc">("asc");
  const [filter, setFilter] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [pending, setPending] = useState<{ svc: SvcInfo; action: SvcAction } | null>(null);
  const reqSeq = useRef(0);

  const load = () => {
    if (!sessionId) return;
    const seq = ++reqSeq.current;
    setLoading(true);
    svc
      .list(sessionId)
      .then((v) => {
        if (seq !== reqSeq.current) return; // 迟到的旧响应丢弃，防切换会话串台
        setError("");
        setSvcs(v);
      })
      .catch((e) => {
        if (seq !== reqSeq.current) return;
        setError(errMsg(e));
      })
      .finally(() => {
        if (seq === reqSeq.current) setLoading(false);
      });
  };

  // 进入有效会话即采样一次（与进程面板同惯例）；切会话清空列表并作废在途请求。
  useEffect(() => {
    if (!sessionId) {
      reqSeq.current++;
      setSvcs([]);
      setError("");
      return;
    }
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId]);

  const doControl = async (s: SvcInfo, action: SvcAction) => {
    setPending(null);
    // 通知标来源：服务操作是连接级事件，铃铛历史里要能看出是哪台机器
    const hostTab = sessionStore.get().tabs.find((x) => x.id === sessionId);
    const src = hostTab ? sessionLabel(hostTab, tr) : undefined;
    try {
      const r = await svc.control(sessionId, s.name, action);
      if (r?.settled === true) {
        toast.success(tr(`svc.done.${action}`).replace("{name}", s.name), src);
      } else if (r?.settled === false) {
        // macOS 核验未达预期：stop 后被 KeepAlive 拉起 / start 后作业立刻退出——如实提示，不报假成功。
        toast.warn(
          tr("svc.unsettled")
            .replace("{name}", s.name)
            .replace("{action}", tr(`svc.${action}`))
            .replace("{state}", r.state ?? ""),
          src,
        );
      } else {
        // Linux / 核验不可用：维持旧提示（命令已成功投递，systemd 同步收敛可信）。
        toast.success(tr(`svc.done.${action}`).replace("{name}", s.name), src);
      }
      load();
    } catch (e) {
      toast.show(errMsg(e), "error", src);
    }
  };

  const q = filter.trim().toLowerCase();
  const shown = q ? svcs.filter((s) => s.name.toLowerCase().includes(q)) : svcs;

  const clickSort = (col: SvcCol) => {
    if (col === sortKey) setSortDir((d) => (d === "asc" ? "desc" : "asc"));
    else {
      setSortKey(col);
      setSortDir("asc");
    }
  };

  const sorted = useMemo(() => {
    // 未选列：后端原序（名称升序）；同值回退名称升序，保证稳定可读。
    if (!sortKey) return shown;
    const sign = sortDir === "asc" ? 1 : -1;
    const cmp = (a: SvcInfo, b: SvcInfo): number => {
      switch (sortKey) {
        case "pid": return a.pid - b.pid;
        case "name": return a.name.localeCompare(b.name);
        case "state": return a.state.localeCompare(b.state);
        case "loaded": return a.loaded.localeCompare(b.loaded);
        case "enabled": return a.enabled.localeCompare(b.enabled);
      }
    };
    return [...shown].sort((a, b) => sign * (cmp(a, b) || a.name.localeCompare(b.name)));
  }, [shown, sortKey, sortDir]);

  const th = (col: SvcCol, label: string, num = false) => (
    <button
      type="button"
      className={`sp-th${num ? " sp-num" : ""}`}
      data-active={sortKey === col}
      onClick={() => clickSort(col)}
    >
      {label}
      <span className="fp-sort-ind">{sortKey === col ? (sortDir === "asc" ? "▲" : "▼") : "↕"}</span>
    </button>
  );

  return (
    <div className="sp">
      <div className="sp-head">
        <span className="fp-title">{tr("svc.title")}</span>
        <span className="tp-sess">{tab ? sessionLabel(tab, tr) : "—"}</span>
        <input
          className="sp-filter"
          placeholder={tr("svc.filter.ph")}
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <button
          type="button"
          className="icon-btn"
          title={tr("svc.refresh")}
          disabled={!sessionId || loading}
          onClick={() => load()}
        >
          <IconRefresh />
        </button>
      </div>

      {!sessionId ? (
        <div className="ss-empty">{tr("svc.unsupported")}</div>
      ) : (
        <>
          {error && <div className="form-error">{error}</div>}
          <div className="sp-table">
            <div className="sp-row sp-head-row">
              {th("name", tr("svc.col.name"))}
              {th("state", tr("svc.col.state"))}
              {th("loaded", tr("svc.col.loaded"))}
              {th("enabled", tr("svc.col.enabled"))}
              {th("pid", tr("svc.col.pid"), true)}
              <span className="sp-th sp-acts" />
            </div>
            {sorted.length === 0 ? (
              <div className="ss-empty">{loading ? "…" : tr("svc.empty")}</div>
            ) : (
              sorted.map((s) => {
                const b = badge(s);
                return (
                  <div className="sp-row" key={s.name}>
                    <span className="sp-td sp-name" title={s.name}>
                      {s.name}
                    </span>
                    <span className={`sp-td sp-badge ${b.cls}`}>
                      <i className="sp-dot" />
                      {b.label}
                    </span>
                    <span className="sp-td">{s.loaded || "—"}</span>
                    <span className="sp-td">{s.enabled || "—"}</span>
                    <span className="sp-td sp-num">{s.pid > 0 ? s.pid : "—"}</span>
                    <span className="sp-td sp-acts">
                      {actionsFor(s).map((a) => (
                        <button
                          type="button"
                          key={a}
                          className={`sp-btn${a === "stop" ? " danger" : ""}`}
                          title={tr(`svc.${a}`)}
                          onClick={() => setPending({ svc: s, action: a })}
                        >
                          {tr(`svc.${a}`)}
                        </button>
                      ))}
                    </span>
                  </div>
                );
              })
            )}
          </div>
        </>
      )}

      {pending && (
        <ConfirmDialog
          title={tr(`svc.${pending.action}`)}
          message={tr(`svc.confirm.${pending.action}`).replace("{name}", pending.svc.name)}
          warning={
            pending.action === "stop" || pending.action === "restart" || pending.action === "reload"
              ? tr(`svc.warn.${pending.action}`)
              : undefined
          }
          okText={tr(`svc.${pending.action}`)}
          onOk={() => void doControl(pending.svc, pending.action)}
          onClose={() => setPending(null)}
        />
      )}
    </div>
  );
}
