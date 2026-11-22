// 展示层格式化：文件列表与传输队列共用（纯函数，不碰 IPC）。
import { t } from "./i18n";

const UNITS = ["B", "KB", "MB", "GB", "TB"];

/** 字节数 → 人类可读（1024 进制，非整数量保留一位小数）。 */
export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "—";
  if (n === 0) return "0 B";
  let v = n;
  let i = 0;
  while (v >= 1024 && i < UNITS.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${i === 0 ? Math.round(v) : v.toFixed(1)} ${UNITS[i]}`;
}

/** 字节/秒 → 速度文案。 */
export function formatSpeed(n: number): string {
  return n > 0 ? `${formatBytes(n)}/s` : "—";
}

/** 毫秒时间戳 → `YYYY-MM-DD HH:mm`；拿不到修改时间（如某些本地文件系统）时给占位。 */
export function formatTime(ms: number | null): string {
  if (!ms || ms <= 0) return "—";
  const d = new Date(ms);
  const p = (v: number) => String(v).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** 毫秒时间戳 → `MM-DD HH:mm:ss`（审计等需秒级精度的紧凑列表用）。 */
export function formatStamp(ms: number | null): string {
  if (!ms || ms <= 0) return "—";
  const d = new Date(ms);
  const p = (v: number) => String(v).padStart(2, "0");
  return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

/** 毫秒时间戳 → `YYYY-MM-DD HH:mm:ss`（完整年月日时分秒；历史命令等跨天/跨年流水用）。 */
export function formatFull(ms: number | null): string {
  if (!ms || ms <= 0) return "—";
  const d = new Date(ms);
  const p = (v: number) => String(v).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

/** 完成百分比（0~100 整数）；总大小未知时按「有进度即 100%」处理。 */
export function percent(done: number, total: number): number {
  if (total <= 0) return done > 0 ? 100 : 0;
  return Math.min(100, Math.max(0, Math.round((done / total) * 100)));
}

/** 剩余时间估算；速度为 0（排队/刚起步）时无从估起，返回空串。 */
export function formatEta(done: number, total: number, speed: number): string {
  if (total <= 0 || speed <= 0 || done >= total) return "";
  const secs = Math.round((total - done) / speed);
  if (secs < 60) return t("fm.sec").replace("{n}", String(secs));
  if (secs < 3600)
    return t("fm.minSec").replace("{m}", String(Math.floor(secs / 60))).replace("{s}", String(secs % 60));
  return t("fm.hourMin").replace("{h}", String(Math.floor(secs / 3600))).replace("{m}", String(Math.floor((secs % 3600) / 60)));
}

/** 从完整路径取文件名：本地可能是 `\`，远端固定 `/`。 */
export function baseName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}
