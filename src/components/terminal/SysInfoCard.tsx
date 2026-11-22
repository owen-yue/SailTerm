import { useCallback, useEffect, useState } from "react";
import { formatBytes } from "../../lib/format";
import { copyText } from "../../lib/clipboard";
import { system } from "../../lib/tauri";
import { errMsg } from "../../lib/tauri/client";
import { toast } from "../../stores";
import { useI18n } from "../../lib/i18n";
import type { SystemInfo } from "../../types";
import { IconRefresh } from "../common/icons";

interface Props {
  sessionId: string;
}

/** uptime 秒 → "3d 12h 45m"（不足 1 分显"＜1m"）。*/
function fmtUptime(sec: number | undefined): string {
  if (sec == null || sec < 0) return "—";
  if (sec < 60) return "<1m";
  const d = Math.floor(sec / 86400);
  const h = Math.floor((sec % 86400) / 3600);
  const m = Math.floor((sec % 3600) / 60);
  if (d > 0) return `${d}d ${h}h ${m}m`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

/** kb → 已用/总量 展示串（GB/TB 由 formatBytes 处理，1KB=1024B）。 */
function fmtKbPair(usedKb?: number, totalKb?: number): string {
  if (!totalKb || totalKb <= 0) return "—";
  const used = usedKb ?? 0;
  return `${formatBytes(used * 1024)} / ${formatBytes(totalKb * 1024)}`;
}

/**
 * 系统信息卡片：一次采齐、同会话内命中后端缓存；「刷新」按钮传 `refresh=true` 重跑 exec；
 * 「复制」按钮把所有 key=value 拼成多行文本落剪贴板，粘到工单/群里就能交接。
 *
 * 由 MonitorPanel 头部 ℹ️ 图标的 HoverPopover 承载——浮层脱离 overflow 裁切，鼠标可平滑
 * 移到内部按钮而不消失（见 HoverPopover 组件注释）。
 */
export default function SysInfoCard({ sessionId }: Props) {
  const tr = useI18n();
  const [info, setInfo] = useState<SystemInfo | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  const load = useCallback(
    async (refresh: boolean) => {
      setLoading(true);
      setError("");
      try {
        setInfo(await system.info(sessionId, refresh));
      } catch (e) {
        setError(errMsg(e));
      } finally {
        setLoading(false);
      }
    },
    [sessionId],
  );

  // 首次挂载拉一次（走后端缓存，不重复采集）；切会话由父组件重挂载触发
  useEffect(() => {
    void load(false);
  }, [load]);

  const copyAll = async () => {
    if (!info) return;
    const lines: string[] = [];
    const push = (k: string, v: string | number | undefined) => {
      if (v != null && v !== "") lines.push(`${k}=${v}`);
    };
    push("os", info.os);
    push("kernel", info.kernel);
    push("arch", info.arch);
    push("distro", info.distro);
    push("hostname", info.hostname);
    push("ip", info.ip);
    push("shell", info.shell);
    push("cwd", info.cwd);
    push("uptime", fmtUptime(info.uptimeSec));
    push("cpu", info.cpuModel ? `${info.cpuModel} (${info.cpuCores ?? "?"} cores)` : undefined);
    push("mem", fmtKbPair(info.memTotalKb && info.memAvailKb ? info.memTotalKb - info.memAvailKb : undefined, info.memTotalKb));
    push("disk", fmtKbPair(info.diskUsedKb, info.diskTotalKb));
    const ok = await copyText(lines.join("\n"));
    if (ok) toast.success(tr("sys.info.copied"));
    else toast.show(tr("sys.info.copyFail"), "error");
  };

  const row = (label: string, value: string | undefined | null, title?: string) => (
    <div className="si-row" key={label}>
      <span className="si-k">{label}</span>
      <span className="si-v" title={title ?? value ?? ""}>{value || "—"}</span>
    </div>
  );

  return (
    <div className="si">
      <div className="si-head">
        <strong>{tr("sys.info.title")}</strong>
        {info && (
          <span className="si-time">
            {tr("sys.info.probedAt").replace("{t}", fmtUptime(Math.floor((Date.now() - info.probedAtMs) / 1000)))}
          </span>
        )}
        <span className="si-acts">
          <button
            type="button"
            className="icon-btn"
            title={tr("sys.info.refresh")}
            disabled={loading}
            onClick={() => void load(true)}
          >
            <IconRefresh />
          </button>
          <button
            type="button"
            className="ghost-link"
            title={tr("sys.info.copy")}
            disabled={!info}
            onClick={() => void copyAll()}
          >
            {tr("sys.info.copy")}
          </button>
        </span>
      </div>

      {error && <div className="form-error">{error}</div>}
      {!info && !error && <div className="ss-empty">{loading ? "…" : tr("sys.info.empty")}</div>}

      {info && (
        <div className="si-body">
          {row(tr("sys.info.hostname"), info.hostname)}
          {row(tr("sys.info.ip"), info.ip)}
          {row(tr("sys.info.os"), info.os)}
          {row(tr("sys.info.kernel"), info.kernel)}
          {row(tr("sys.info.arch"), info.arch)}
          {row(tr("sys.info.distro"), info.distro)}
          {row(tr("sys.info.shell"), info.shell)}
          {row(tr("sys.info.cwd"), info.cwd, info.cwd)}
          {row(tr("sys.info.uptime"), fmtUptime(info.uptimeSec))}
          {row(
            tr("sys.info.cpu"),
            info.cpuModel
              ? `${info.cpuModel} · ${info.cpuCores ?? "?"} cores`
              : info.cpuCores
                ? `${info.cpuCores} cores`
                : undefined,
            info.cpuModel,
          )}
          {row(tr("sys.info.mem"), fmtKbPair(info.memTotalKb && info.memAvailKb ? info.memTotalKb - info.memAvailKb : undefined, info.memTotalKb))}
          {row(tr("sys.info.disk"), fmtKbPair(info.diskUsedKb, info.diskTotalKb))}
        </div>
      )}
    </div>
  );
}
