import { t as tr } from "../../lib/i18n";
import { fmtTok } from "./AiTimeline";
import type { TurnUsage } from "../../types";

/**
 * 上下文占用圆环（聊天 / Agent 两页共用，保证体验一致）：
 * 空心圆环进度 = 末轮实际发送量 / 窗口总量，常驻；hover 看短格式数字与百分比。
 * `usage` 为空（本轮尚无真值）时显示空心轨道，title 提示暂无。
 */
export default function ContextRing({ usage }: { usage: TurnUsage | null }) {
  const u = usage;
  const pct =
    u && u.contextWindow > 0 && u.contextUsed > 0
      ? Math.min(100, Math.round((u.contextUsed / u.contextWindow) * 100))
      : 0;
  const shown =
    u && u.contextWindow > 0 && u.contextUsed > 0
      ? Math.min(999, Math.round((u.contextUsed / u.contextWindow) * 100))
      : null;
  const R = 7;
  const C = 2 * Math.PI * R;
  return (
    <span
      className="ai-ctx-ring"
      data-hi={pct >= 90 ? 2 : pct >= 70 ? 1 : 0}
      title={
        shown !== null
          ? [
              // 数字折成 2.6k/200k 短格式（别显示那么长一串），不带 tok 字样。
              tr("aw.ctxTip")
                .replace("{used}", fmtTok(u!.contextUsed))
                .replace("{window}", fmtTok(u!.contextWindow))
                .replace("{pct}", String(shown)),
              u!.estimated ? tr("aw.ctxEst") : "",
            ]
              .filter(Boolean)
              .join(" · ")
          : tr("aw.ctxNone")
      }
    >
      <svg width="18" height="18" viewBox="0 0 18 18" aria-hidden="true">
        {/* 轨道：完整空心圈（底色） */}
        <circle
          cx="9"
          cy="9"
          r={R}
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          className="ai-ctx-track"
        />
        {/* 进度弧：从顶部起顺时针，长度按百分比 */}
        {pct > 0 && (
          <circle
            cx="9"
            cy="9"
            r={R}
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            className="ai-ctx-arc"
            strokeDasharray={C}
            strokeDashoffset={C * (1 - pct / 100)}
            transform="rotate(-90 9 9)"
          />
        )}
      </svg>
    </span>
  );
}
