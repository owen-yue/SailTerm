import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import { formatBytes } from "../../lib/format";
import { sessions as sshApi } from "../../lib/tauri";
import { errMsg } from "../../lib/tauri/client";
import { sessionStore, uiPrefsStore, useStore } from "../../stores";
import { useI18n } from "../../lib/i18n";
import type { DiskPartition, MonitorSample } from "../../types";
import { sessionLabel } from "../../types";
import { hasCap } from "../../lib/capabilities";
import HoverPopover from "../common/HoverPopover";
import { IconInfo } from "../common/icons";
import SysInfoCard from "./SysInfoCard";

const HISTORY = 60;
/** 固定时间窗口宽度（点数）：每个点占 1/60 宽度，点少时线段从右侧生长、左侧留网格，
 * 不再把少量点拉伸铺满全宽（那会把折线拉成跨屏大斜线，观感差）。 */
const SPARK_SLOTS = 60;

interface Point extends MonitorSample {
  t: number;
}

const pct = (a: number, b: number) => (b > 0 ? (a / b) * 100 : null);

/** 16px 线稿图标：比 emoji 清晰、跟主题色，避免「图标太简易太小」。 */
const ICO: Record<string, ReactNode> = {
  cpu: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3">
      <rect x="4" y="4" width="8" height="8" rx="1.5" />
      <rect x="6.5" y="6.5" width="3" height="3" />
      <path d="M6 4V2M10 4V2M6 14v-2M10 14v-2M4 6H2M4 10H2M14 6h-2M14 10h-2" />
    </svg>
  ),
  mem: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3">
      <rect x="2" y="4.5" width="12" height="6" rx="1" />
      <path d="M5 4.5v3M8 4.5v3M11 4.5v3M4 10.5v2M12 10.5v2" />
    </svg>
  ),
  disk: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3">
      <circle cx="8" cy="8" r="6" />
      <circle cx="8" cy="8" r="1.6" />
      <path d="M8 2v2.2M8 11.8V14" />
    </svg>
  ),
  io: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3">
      <path d="M5 13V3M5 3 3 5.2M5 3l2 2.2" />
      <path d="M11 3v10M11 13l-2-2.2M11 13l2-2.2" />
    </svg>
  ),
  net: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3">
      <circle cx="8" cy="8" r="6" />
      <path d="M2 8h12M8 2c2.5 2.6 2.5 9.4 0 12M8 2c-2.5 2.6-2.5 9.4 0 12" />
    </svg>
  ),
};

function derive(prev: Point, cur: Point) {
  const dt = Math.max(1, (cur.t - prev.t) / 1000);
  const dTotal = cur.cpuTotal - prev.cpuTotal;
  const dIdle = cur.cpuIdle - prev.cpuIdle;
  return {
    cpu:
      dTotal > 0
        ? pct(dTotal - dIdle, dTotal)
        : pct(cur.cpuTotal - cur.cpuIdle, cur.cpuTotal),
    mem: pct(cur.memTotal - cur.memAvailable, cur.memTotal),
    disk: pct(cur.diskUsed, cur.diskTotal),
    rx: Math.max(0, cur.netRx - prev.netRx) / dt,
    tx: Math.max(0, cur.netTx - prev.netTx) / dt,
  };
}

function Spark({ data, color }: { data: (number | null)[]; color: string }) {
  // 折线图：最新点钉右缘，槽宽固定；超出窗口左侧自然滑出
  const last = data.length - 1;
  const pts = data.map((v, i) => {
    if (v == null) return null;
    const x = 100 - (last - i) * (100 / SPARK_SLOTS);
    if (x < -0.5) return null; // 窗口外的旧点裁掉，避免线段画出 viewBox 拉伸视觉
    const y = 100 - Math.max(0, Math.min(100, v));
    return `${x.toFixed(2)},${y.toFixed(2)}`;
  });
  const line = pts.filter(Boolean).join(" ");
  const area = pts.filter(Boolean) as string[];
  const firstX = area.length ? area[0].split(",")[0] : "0";
  const lastX = area.length ? area[area.length - 1].split(",")[0] : "100";
  const fill = area.length > 1 ? `${firstX},100 ${area.join(" ")} ${lastX},100` : "";
  return (
    <svg className="mon-spark" viewBox="0 0 100 100" preserveAspectRatio="none">
      <defs>
        <linearGradient id={`g-${color.replace(/[^a-z0-9]/gi, "")}`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={color} stopOpacity="0.45" />
          <stop offset="100%" stopColor={color} stopOpacity="0.02" />
        </linearGradient>
      </defs>
      {/* 背景网格 */}
      <line x1="0" y1="25" x2="100" y2="25" stroke="currentColor" strokeOpacity="0.06" vectorEffect="non-scaling-stroke" />
      <line x1="0" y1="50" x2="100" y2="50" stroke="currentColor" strokeOpacity="0.06" vectorEffect="non-scaling-stroke" />
      <line x1="0" y1="75" x2="100" y2="75" stroke="currentColor" strokeOpacity="0.06" vectorEffect="non-scaling-stroke" />
      {fill && (
        <polygon
          points={fill}
          fill={`url(#g-${color.replace(/[^a-z0-9]/gi, "")})`}
          stroke="none"
        />
      )}
      {line && (
        <polyline
          fill="none"
          stroke={color}
          strokeWidth="1.8"
          strokeLinejoin="round"
          strokeLinecap="round"
          points={line}
          vectorEffect="non-scaling-stroke"
        />
      )}
    </svg>
  );
}

/**
 * 磁盘分区：每个真实挂载点一张紧凑卡（磁盘图标 + 挂载点 + 百分比 + 进度条 + 用量），
 * 排在独立的一行里，与顶部指标卡同款观感。补单值「磁盘占用」卡只看根分区的盲区——
 * 多数据盘服务器（如 /mysql/data）此前完全看不到。按使用率降序，条与百分比按阈值
 * 变色（≥90 红 / ≥80 琥珀 / 其余品牌色）便于爆盘预警。
 */
function DiskGrid({ parts }: { parts: DiskPartition[] }) {
  const t = useI18n();
  const rows = [...parts].sort((a, b) => b.used / (b.total || 1) - a.used / (a.total || 1));
  return (
    <div className="mon-disks">
      <div className="mon-disks-head">{t("mon.diskPart")}</div>
      <div className="mon-grid">
        {rows.map((p) => {
          const use = p.total > 0 ? (p.used / p.total) * 100 : 0;
          const color = use >= 90 ? "#ef4444" : use >= 80 ? "#f59e0b" : "var(--brand)";
          return (
            <div className="mon-card mon-disk-card" key={`${p.device}:${p.mount}`}>
              <div className="mon-card-head">
                <span className="mon-ico" style={{ color }}>{ICO.disk}</span>
                <span className="mon-lbl" title={p.device}>{p.mount}</span>
                <strong>{use.toFixed(0)}%</strong>
              </div>
              <div className="mon-disk-bar">
                <i style={{ width: `${Math.min(100, use)}%`, background: color }} />
              </div>
              <div className="mon-sub">{formatBytes(p.used)} / {formatBytes(p.total)}</div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function Metric({
  label,
  value,
  sub,
  bar,
  series,
  color,
  icon,
}: {
  label: string;
  value: string;
  sub: string;
  bar: number | null;
  series: (number | null)[];
  color: string;
  icon: ReactNode;
}) {
  return (
    <div className="mon-card">
      <div className="mon-card-head">
        <span className="mon-ico" style={{ color }}>{icon}</span>
        <span className="mon-lbl">{label}</span>
        <strong>{value}</strong>
      </div>
      <Spark data={series} color={color} />
      {bar != null && (
        <div className="bar">
          <i style={{ width: `${Math.min(100, bar)}%`, background: color }} />
        </div>
      )}
      <div className="mon-sub">{sub}</div>
    </div>
  );
}

/**
 * 模块级采样池：历史按会话存在组件外，切走下栏页签/收起面板曲线不丢；
 * ticker 在组件卸载后仍后台续采（回到监控页看到的是最新状态），
 * 会话关闭时自动停采并清理池。
 */
interface Pool {
  points: Point[];
  error: string;
  timer: number;
  intervalMs: number;
}
const pools = new Map<string, Pool>();
const poolListeners = new Map<string, Set<() => void>>();

function notifyPool(sid: string) {
  poolListeners.get(sid)?.forEach((fn) => fn());
}

/** 单会话采样订阅（采样间隔由 uiPrefs.monitorInterval 驱动，改设置即时生效）。 */
function useSample(sessionId: string, live: boolean) {
  const [, bump] = useState(0);
  const { monitorInterval } = useStore(uiPrefsStore);
  const intervalMs = Math.max(1000, monitorInterval * 1000);
  useEffect(() => {
    if (!sessionId || !live) return;
    let pool = pools.get(sessionId);
    // 间隔改了：重建 ticker（已采历史保留在池里，不清空）
    if (pool && pool.intervalMs !== intervalMs) {
      window.clearInterval(pool.timer);
      pool.timer = 0;
    }
    if (!pool || !pool.timer) {
      const p: Pool = pool ?? { points: [], error: "", timer: 0, intervalMs };
      p.intervalMs = intervalMs;
      pools.set(sessionId, p);
      const tick = () => {
        // 会话已关闭：停掉后台采样并回收池
        if (!sessionStore.get().tabs.some((t) => t.id === sessionId)) {
          window.clearInterval(p.timer);
          pools.delete(sessionId);
          notifyPool(sessionId);
          return;
        }
        sshApi
          .monitor(sessionId)
          .then((s) => {
            p.error = "";
            p.points = [...p.points, { ...s, t: Date.now() }].slice(-HISTORY);
          })
          .catch((e) => {
            p.error = errMsg(e);
          })
          .finally(() => notifyPool(sessionId));
      };
      tick();
      p.timer = window.setInterval(tick, intervalMs);
      pool = p;
    }
    const sync = () => bump((n) => n + 1);
    const set = poolListeners.get(sessionId) ?? new Set<() => void>();
    set.add(sync);
    poolListeners.set(sessionId, set);
    return () => {
      set.delete(sync);
    };
  }, [sessionId, live, intervalMs]);
  const pool = pools.get(sessionId);
  return { points: pool?.points ?? [], error: pool?.error ?? "" };
}

/**
 * 📈 监控：默认**当前会话**四卡曲线；「总览」一览所有已连接 SSH 的负载。
 * 只读 exec 采样，无主机标识。
 */
export default function MonitorPanel() {
  const t = useI18n();
  const { tabs, activeId } = useStore(sessionStore);
  const tab = tabs.find((x) => x.id === activeId) ?? null;
  // 采样要一条命令执行通道（能力表里的 monitor = SSH + 本地）；FTP/Telnet 等采不到，不显数据也不该显空壳
  const sessionId = tab && hasCap(tab.kind, "monitor") && tab.state === "connected" ? tab.id : "";

  return (
    <div className="mon">
      <div className="mon-head">
        <span className="fp-title">{t("mon.title")}</span>
        <span className="tp-sess">{tab ? sessionLabel(tab, t) : "—"}</span>
        <span className="tp-sess">{tab ? (tab.kind === "local" ? t("mon.local") : t("mon.remote")) : ""}</span>
        {/* 系统信息卡片入口（PR-1）：hover 展开，后端同会话缓存。只在有采集能力的会话上挂。 */}
        {sessionId && (
          <HoverPopover
            align="end"
            width={420}
            className="si-trigger"
            content={<SysInfoCard sessionId={sessionId} />}
          >
            <span className="si-trigger-inner" title={t("sys.info.title")}>
              <IconInfo />
            </span>
          </HoverPopover>
        )}
      </div>
      {sessionId ? (
        <Detail sessionId={sessionId} />
      ) : (
        <div className="ss-empty">{t("mon.empty")}</div>
      )}
    </div>
  );
}

function Detail({ sessionId }: { sessionId: string }) {
  const t = useI18n();
  const { points, error } = useSample(sessionId, true);
  const last = points[points.length - 1];
  const prev = points[points.length - 2];
  const m = last && prev ? derive(prev, last) : null;
  const series = (f: (d: ReturnType<typeof derive>, i: number) => number | null) =>
    points.map((p, i) => (i === 0 ? null : f(derive(points[i - 1], p), i)));
  const memS = points.map((p) => pct(p.memTotal - p.memAvailable, p.memTotal));
  // 采样间隔秒数：IO 增量要除以它才是真速率（原把整个间隔的增量直接标成 /s）
  const dtSec = last && prev ? Math.max(1, (last.t - prev.t) / 1000) : 1;
  // BSD/macOS 无 shell 可达的累计 IO 字节计数（探测显式打 NA）：不可得时如实标
  // 「无累计 IO 计数」，不拿磁盘容量占用凑数假装速率（缺省旧样本/Linux 仍按可得）。
  const ioNA = last?.ioSupported === false;
  const ioRate = (cur: number | undefined, pv: number | undefined) =>
    Math.max(0, (cur ?? 0) - (pv ?? 0)) / dtSec;

  return (
    <>
      {error && <div className="form-error">{error}</div>}
      {!last && !error && <div className="ss-empty">{t("mon.sampling")}</div>}
      {last && (
        <>
        <div className="mon-grid">
          <Metric
            label="CPU"
            icon={ICO.cpu}
            value={m?.cpu != null ? `${m.cpu.toFixed(1)}%` : "—"}
            sub={t("mon.cpuSub")}
            bar={m?.cpu ?? null}
            series={series((d) => d.cpu)}
            color="var(--brand)"
          />
          <Metric
            label="MEM"
            icon={ICO.mem}
            value={
              last.memTotal > 0
                ? `${pct(last.memTotal - last.memAvailable, last.memTotal)!.toFixed(1)}%`
                : "—"
            }
            sub={last.memTotal > 0 ? `${formatBytes(last.memTotal - last.memAvailable)} / ${formatBytes(last.memTotal)}` : "—"}
            bar={last.memTotal > 0 ? pct(last.memTotal - last.memAvailable, last.memTotal) : null}
            series={memS}
            color="#38bdf8"
          />
          <Metric
            label={t("mon.diskIo")}
            icon={ICO.io}
            value={
              ioNA
                ? "—"
                : m
                  ? `${formatBytes(
                      ioRate(last.diskRead, prev?.diskRead) + ioRate(last.diskWrite, prev?.diskWrite),
                    )}/s`
                  : "—"
            }
            sub={
              ioNA
                ? t("mon.ioNA")
                : m
                  ? t("mon.ioRw")
                      .replace("{r}", formatBytes(ioRate(last.diskRead, prev?.diskRead)))
                      .replace("{w}", formatBytes(ioRate(last.diskWrite, prev?.diskWrite)))
                  : t("mon.ioIdle")
            }
            bar={last.diskTotal > 0 ? pct(last.diskUsed, last.diskTotal) : null}
            series={
              ioNA
                ? points.map(() => null)
                : series((_d, i) => {
                    // 用相邻点算 IO 速率（字节/秒）映射到 0-100
                    const cur = points[i];
                    const pv = points[i - 1];
                    if (!cur || !pv) return null;
                    const gap = Math.max(1, (cur.t - pv.t) / 1000);
                    const bytes =
                      Math.max(0, (cur.diskRead ?? 0) - (pv.diskRead ?? 0)) +
                      Math.max(0, (cur.diskWrite ?? 0) - (pv.diskWrite ?? 0));
                    return Math.min(100, (bytes / gap / (1024 * 1024)) * 20);
                  })
            }
            color="#f59e0b"
          />
          <Metric
            label="NET"
            icon={ICO.net}
            value={m ? `↓${formatBytes(m.rx)}/s` : "—"}
            sub={m ? `↑ ${formatBytes(m.tx)}/s` : "—"}
            bar={null}
            series={series((d) => Math.min(100, ((d.rx + d.tx) / (1024 * 1024)) * 100))}
            color="#2dd4bf"
          />
        </div>
        {(last.diskPartitions?.length ?? 0) > 0 && <DiskGrid parts={last.diskPartitions!} />}
        </>
      )}
    </>
  );
}

/** 总览：每个已连接 SSH 一行紧凑负载。 */
