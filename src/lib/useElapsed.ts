// 「思考中」已耗时计时：轮次在途期间每秒走字，让「模型长时间无输出」看得见心跳（卡住 vs 在思考）。
// 纯前端定时器，不依赖后端事件——ai-step/ai-stream 在模型静默思考段本就没有任何回包。
import { useEffect, useState } from "react";

/**
 * 返回距 `startedAt` 的已耗秒数：`running` 为 true 期间每秒自更新，停止后返回最终值。
 * 三个「思考中」指示（终端思考条 / Agent 面板 / 聊天抽屉）共用，口径一致。
 */
export function useElapsedSec(startedAt: number | undefined, running: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!running || !startedAt) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [running, startedAt]);
  if (!running || !startedAt) return 0;
  return Math.max(0, Math.floor((now - startedAt) / 1000));
}

/** 已耗秒数 → 走字文案（60 秒内 `42s`，之后 `4m12s`）。 */
export function formatElapsed(sec: number): string {
  if (sec < 60) return `${sec}s`;
  return `${Math.floor(sec / 60)}m${String(sec % 60).padStart(2, "0")}s`;
}

/**
 * 「思考中」提示按相位取 i18n 键后缀（各命名空间 ag/aw/tx 都备齐 thinking /
 * thinkingReasoning / thinkingTool / thinkingAnswering 四条）。相位来自 `ai-stream` 的 kind。
 */
export function thinkingKeyForPhase(phase: string): string {
  switch (phase) {
    case "reasoning":
      return "thinkingReasoning";
    case "tool":
      return "thinkingTool";
    case "text":
      return "thinkingAnswering";
    default:
      return "thinking";
  }
}
