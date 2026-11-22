import { useEffect, useState } from "react";
import type { UsageBucket, UsageByKey, UsageStats } from "../../types";
import { ai } from "../../lib/tauri";
import { errMsg } from "../../lib/tauri/client";
import { useI18n } from "../../lib/i18n";
import { fmtTok } from "./AiTimeline";

type TFn = (k: string) => string;

/** hover 浮层内容：由 Chart 上抛，渲染在弹窗顶层（fixed 定位），避免被滚动容器裁剪。 */
export type UsageTip = {
  x: number;
  y: number;
  head: string;
  rows: { model: string; tokens: number; color: string }[];
};

/**
 * 用量统计弹窗：AI 对话累计 token 的时间序列图表（按天/按月/按小时）+ 按模型分布。
 *
 * 口径：只统计 assistant 条的 `tokens` 列 = 本轮全部模型调用合计（含工具往返）。
 * 数据源 ai_usage_stats 只回「有量的桶」，缺口的天/月/时点由前端补齐成连续轴。
 * 时间柱按模型堆叠：每柱拆成多段，颜色↔模型由全局 byModel 顺序统一分配（图例与柱共用）。
 */
export default function UsageStatsDialog({ onClose }: { onClose: () => void }) {
  const t = useI18n();
  const [stats, setStats] = useState<UsageStats | null>(null);
  const [err, setErr] = useState("");
  const [tip, setTip] = useState<UsageTip | null>(null);

  useEffect(() => {
    void ai.usageStats().then(setStats).catch((e: unknown) => setErr(errMsg(e)));
  }, []);

  // 颜色↔模型映射：按全局 byModel（token 降序）顺序取色，图例与所有时间柱共用，保证一致。
  const colorOf = (model: string): string => {
    const idx = stats?.byModel.findIndex((m) => m.key === model) ?? -1;
    return idx < 0 ? "var(--txt-mute)" : PALETTE[idx % PALETTE.length];
  };

  return (
    <>
      <div
        className="modal-mask"
        onMouseDown={(e) => {
          if (e.target === e.currentTarget) onClose();
        }}
      >
        <div className="modal us" role="dialog" aria-label={t("us.title")}>
          <div className="modal-head">
            <h2>{t("us.title")}</h2>
            <button type="button" className="modal-x" title={t("common.close")} onClick={onClose}>
              ×
            </button>
          </div>
          <div className="us-body">
            {err && <div className="us-empty">{err}</div>}
            {!err && !stats && <div className="us-empty">{t("us.loading")}</div>}
            {stats && (
              <>
                <div className="us-cards">
                  <Card label={t("us.totalTokens")} value={fmtTok(stats.totalTokens)} big />
                  <Card label={t("us.totalTurns")} value={String(stats.totalTurns)} />
                  <Card label={t("us.models")} value={String(stats.byModel.length)} />
                  <Card
                    label={t("us.last30d")}
                    value={fmtTok(stats.byDay.reduce((s, d) => s + d.tokens, 0))}
                  />
                </div>
                {stats.byModel.length > 1 && <Legend rows={stats.byModel} colorOf={colorOf} />}
                <Chart
                  t={t}
                  title={t("us.byDay")}
                  rows={fillDays(stats.byDay)}
                  labelEvery={5}
                  xfmt={(k) => k.slice(5)}
                  colorOf={colorOf}
                  onTip={setTip}
                />
                <Chart
                  t={t}
                  title={t("us.byMonth")}
                  rows={fillMonths(stats.byMonth)}
                  labelEvery={1}
                  xfmt={(k) => k.slice(5)}
                  colorOf={colorOf}
                  onTip={setTip}
                />
                <Chart
                  t={t}
                  title={t("us.byHour")}
                  rows={fillHours(stats.byHour)}
                  labelEvery={3}
                  xfmt={(k) => String(Number(k))}
                  colorOf={colorOf}
                  onTip={setTip}
                />
                <HBars t={t} title={t("us.byModel")} rows={stats.byModel} colorOf={colorOf} />
              </>
            )}
          </div>
        </div>
      </div>
      {/* 浮层挂在弹窗外：.modal 的 overflow:hidden 会裁剪内部 fixed 子元素 */}
      {tip && <TipCard tip={tip} />}
    </>
  );
}

/** 顶层浮层卡片：fixed 锚到悬停柱上方（锚点=tip 底边中心），坐标按视口边界夹取，
 *  保证贴边/贴顶柱的 tip 也完整可见；尺寸用预估值，配合 CSS max-width 兜底。 */
function TipCard({ tip }: { tip: UsageTip }) {
  const W = Math.min(340, window.innerWidth - 16); // 预估宽度上限
  const H = 22 + tip.rows.length * 15; // 预估高度：头部 + 每模型一行
  const left = Math.min(Math.max(tip.x, 8 + W / 2), window.innerWidth - 8 - W / 2);
  const top = Math.min(Math.max(tip.y - 10, 8 + H), window.innerHeight - 8);
  return (
    <span className="us-tip" style={{ left: `${left}px`, top: `${top}px` }}>
      <span className="us-tip-h">{tip.head}</span>
      {tip.rows.map((r, i) => (
        <span className="us-tip-row" key={i}>
          <i style={{ background: r.color }} />
          <span className="us-tip-model">{r.model}</span> · {fmtTok(r.tokens)}
        </span>
      ))}
    </span>
  );
}

/** 堆叠柱配色：深色底上区分度高的调色板，按模型全局用量顺序取色。 */
const PALETTE = [
  "#4c9aff",
  "#3ecf8e",
  "#f5a623",
  "#ef5da8",
  "#9b8cff",
  "#2dd4bf",
  "#ff7a59",
  "#c0ca33",
  "#e0d15a",
  "#5cc8ff",
];

function Card({ label, value, big = false }: { label: string; value: string; big?: boolean }) {
  return (
    <div className="us-card">
      <div className="us-card-label">{label}</div>
      <div className={big ? "us-card-value big" : "us-card-value"}>{value}</div>
    </div>
  );
}

/** 颜色↔模型图例：仅当用过多个模型时显示，与时间柱、按模型条共用同一取色。 */
function Legend({ rows, colorOf }: { rows: UsageByKey[]; colorOf: (m: string) => string }) {
  return (
    <div className="us-legend">
      {rows.map((r) => (
        <span className="us-legend-item" key={r.key} title={r.key}>
          <i className="us-legend-swatch" style={{ background: colorOf(r.key) }} />
          <span className="us-legend-name">{r.key}</span>
        </span>
      ))}
    </div>
  );
}

/** 纵向堆叠柱状图：每柱按模型分色堆叠，带 Y 轴刻度 + 网格线，hover 弹总量与各模型明细。 */
function Chart({
  t,
  title,
  rows,
  labelEvery,
  xfmt,
  colorOf,
  onTip,
}: {
  t: TFn;
  title: string;
  rows: UsageBucket[];
  labelEvery: number;
  xfmt: (k: string) => string;
  colorOf: (m: string) => string;
  onTip: (tip: UsageTip | null) => void;
}) {
  const max = Math.max(1, ...rows.map((r) => r.tokens));
  const hasData = rows.some((r) => r.tokens > 0);
  return (
    <div className="us-sec us-chart">
      <div className="us-sec-title">{title}</div>
      {!hasData ? (
        <div className="us-empty">{t("us.empty")}</div>
      ) : (
        <div className="us-plot-row">
          <div className="us-yaxis">
            <span>{fmtTok(max)}</span>
            <span>{fmtTok(Math.round(max / 2))}</span>
            <span>0</span>
          </div>
          <div className="us-plot-col">
            <div className="us-plot">
              <div className="us-grid" aria-hidden>
                <i />
                <i />
                <i />
              </div>
              <div className="us-bars">
                {rows.map((b) => (
                  <div
                    className="us-bar-col"
                    key={b.key}
                    onPointerEnter={(e) => {
                      if (b.tokens <= 0) return;
                      const r = e.currentTarget.getBoundingClientRect();
                      onTip({
                        x: r.left + r.width / 2,
                        y: r.top,
                        head: `${xfmt(b.key)} · ${fmtTok(b.tokens)} tokens · ${b.count} ${t("us.turns")}`,
                        rows: b.segments.map((seg) => ({
                          model: seg.model,
                          tokens: seg.tokens,
                          color: colorOf(seg.model),
                        })),
                      });
                    }}
                    onPointerLeave={() => onTip(null)}
                  >
                    <div
                      className="us-bar"
                      style={{ height: `${(b.tokens / max) * 100}%` }}
                      data-empty={b.tokens <= 0 ? "1" : undefined}
                    >
                      {b.segments.map((seg, i) => (
                        <div
                          className="us-seg"
                          key={i}
                          style={{
                            height: `${(seg.tokens / b.tokens) * 100}%`,
                            background: colorOf(seg.model),
                          }}
                        />
                      ))}
                    </div>
                  </div>
                ))}
              </div>
            </div>
            <div className="us-xaxis">
              {rows.map((r, i) => (
                <span className="us-x-label" key={r.key}>
                  {i % labelEvery === 0 ? xfmt(r.key) : ""}
                </span>
              ))}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/** 横向条形图（按模型）：色块 + 名称 + 占比条 + token 值（与时间柱同色）。 */
function HBars({
  t,
  title,
  rows,
  colorOf,
}: {
  t: TFn;
  title: string;
  rows: UsageByKey[];
  colorOf: (m: string) => string;
}) {
  const max = Math.max(1, ...rows.map((r) => r.tokens));
  return (
    <div className="us-sec us-hbars">
      <div className="us-sec-title">{title}</div>
      {rows.length === 0 && <div className="us-empty">{t("us.empty")}</div>}
      {rows.map((r) => (
        <div
          className="us-hrow"
          key={r.key}
          title={`${r.key} · ${fmtTok(r.tokens)} tokens · ${r.count} ${t("us.turns")}`}
        >
          <i className="us-hrow-swatch" style={{ background: colorOf(r.key) }} />
          <span className="us-hrow-key">{r.key}</span>
          <div className="us-hrow-track">
            <div
              className="us-hrow-fill"
              style={{ width: `${(r.tokens / max) * 100}%`, background: colorOf(r.key) }}
            />
          </div>
          <span className="us-hrow-val">{fmtTok(r.tokens)}</span>
        </div>
      ))}
    </div>
  );
}

// —— 轴补齐：后端只回有量的桶，这里铺成连续序列（缺口显 0，堆叠段为空）——

const pad2 = (n: number) => String(n).padStart(2, "0");
const dayKey = (d: Date) => `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
const monthKey = (d: Date) => `${d.getFullYear()}-${pad2(d.getMonth() + 1)}`;

function reindex(rows: UsageBucket[], keys: string[]): UsageBucket[] {
  const map = new Map(rows.map((r) => [r.key, r]));
  return keys.map(
    (k) => map.get(k) ?? { key: k, tokens: 0, count: 0, segments: [] },
  );
}

function fillDays(rows: UsageBucket[]): UsageBucket[] {
  const now = new Date();
  const keys: string[] = [];
  for (let i = 29; i >= 0; i--) {
    const d = new Date(now);
    d.setDate(now.getDate() - i);
    keys.push(dayKey(d));
  }
  return reindex(rows, keys);
}

function fillMonths(rows: UsageBucket[]): UsageBucket[] {
  const now = new Date();
  const keys: string[] = [];
  for (let i = 11; i >= 0; i--) {
    keys.push(monthKey(new Date(now.getFullYear(), now.getMonth() - i, 1)));
  }
  return reindex(rows, keys);
}

function fillHours(rows: UsageBucket[]): UsageBucket[] {
  const keys: string[] = [];
  for (let h = 0; h < 24; h++) keys.push(pad2(h));
  return reindex(rows, keys);
}
