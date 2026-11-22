// 预加载 / 失败页：「点击连接先进入预加载，成功原地切终端，失败原地报错并给下一步」。
//
// 承载在占位标签（tab.phase = "pending" | "failed"）的主区里，替代终端 / FTP 文件栏。
// 进度线与日志来自后端 `session-stage` 事件（按 connectId 归集，见 sessionStore.applyStage）。
// 「让 AI 分析」走会话外的单轮诊断（ai.diagnoseConnect）——失败的连接没有活动会话，
// 用不了聊天页那条按会话编排的通路，故结果直接内联渲染在本卡片里。
import { useState } from "react";
import { useI18n } from "../../lib/i18n";
import { connectUi } from "../../lib/connectUi";
import { ai } from "../../lib/tauri";
import { errMsg } from "../../lib/tauri/client";
import { copyText } from "../../lib/clipboard";
import { sessionLabel } from "../../types";
import { sessionActions, toast, type Tab } from "../../stores";
import Button from "../common/Button";

/** 粗粒度阶段名 → i18n 键；未知阶段名回退只显示 detail（后端可无约束扩展阶段）。 */
const STAGE_KEYS: Record<string, string> = {
  resolve: "conn.stage.resolve",
  tcp: "conn.stage.tcp",
  kex: "conn.stage.kex",
  tls: "conn.stage.tls",
  auth: "conn.stage.auth",
  login: "conn.stage.login",
  shell: "conn.stage.shell",
  ready: "conn.stage.ready",
  failed: "conn.stage.failed",
};

/** 阶段在进度条上的先后次序：把「当前走到哪一步」映射成圆点在轨道上的位置百分比。 */
const STAGE_ORDER = ["resolve", "tcp", "kex", "tls", "auth", "login", "shell", "ready"];

/** 内联 AI 诊断的状态机：idle=未发起，loading=请求中，done=有结果，error=失败可重试。 */
type AiState = "idle" | "loading" | "done" | "error";

export default function ConnectProgress({ tab }: { tab: Tab }) {
  const tr = useI18n();
  // 阶段流水默认展开：连接中与失败页都要看得到过程信息（失败时可手动收起）。
  const [showLog, setShowLog] = useState(true);
  const [aiState, setAiState] = useState<AiState>("idle");
  const [aiText, setAiText] = useState("");
  const [aiErr, setAiErr] = useState("");
  const stages = tab.stages ?? [];
  const failed = tab.phase === "failed";
  // 阶段流水：连接中与失败页都默认展开（showLog 初值为 true）；失败页可通过开关手动收起。
  const feedOpen = failed ? showLog : true;
  // 进度线停在最后一个阶段；pending 下即当前正在走的阶段
  const current = stages[stages.length - 1];
  // 圆点位置：取「已到达阶段」里最靠右者映射到 0~92%（留白避免压到右端终端图标）。
  // 用历史最远而非最后一条——失败追加的 "failed" 不在标准次序里，若按最后一条算会把圆点
  // 拽回起点（进度刚进就因报错退回，体验差）；取最远阶段可保证圆点只进不退、失败即冻结。
  let maxStep = 0;
  for (const s of stages) {
    const p = STAGE_ORDER.indexOf(s.stage);
    if (p >= 0 && p + 1 > maxStep) maxStep = p + 1;
  }
  // 一条可识别阶段都没有时（如极早期就报错），按已到达条数兜底估算，别让圆点空转在最左。
  if (maxStep === 0) maxStep = Math.min(stages.filter((s) => s.stage !== "failed").length, STAGE_ORDER.length);
  let pct = Math.min((maxStep / STAGE_ORDER.length) * 100, 92);
  // 起始/失败都给一点位移下限，圆点别贴死在最左（也避免失败时从 5% 掉回 0%）。
  if (pct < 5) pct = 5;
  const stageLabel = (stage: string) => {
    const k = STAGE_KEYS[stage];
    return k ? tr(k) : stage;
  };
  const statusText = failed
    ? tr("conn.failed")
    : current
      ? stageLabel(current.stage)
      : tr("conn.connecting");

  const logText = (): string => {
    const lines = stages.map((s) => `[${stageLabel(s.stage)}] ${s.detail}`);
    if (failed && tab.failMessage) lines.push(`✕ ${tab.failMessage}`);
    return lines.join("\n");
  };

  const copyLogs = async () => {
    const ok = await copyText(logText());
    toast.show(ok ? tr("conn.copied") : tr("conn.copyFail"), ok ? "success" : "error");
  };

  // 「让 AI 分析」：把连接日志交给后端做会话外的单轮诊断，结果内联展示在本卡片。
  const runDiagnose = async () => {
    setAiState("loading");
    setAiErr("");
    const header = `${tab.kind.toUpperCase()} ${tab.host || ""}`.trim();
    try {
      const res = await ai.diagnoseConnect(`${header}\n${logText()}`);
      setAiText(res);
      setAiState("done");
    } catch (e) {
      setAiErr(errMsg(e));
      setAiState("error");
    }
  };

  const copyAi = async () => {
    const ok = await copyText(aiText);
    toast.show(ok ? tr("conn.copied") : tr("conn.copyFail"), ok ? "success" : "error");
  };

  return (
    <div className={`conn-prog${failed ? " is-failed" : ""}`} role="status" aria-live="polite">
      <div className="conn-prog-card">
        <div className="conn-prog-head">
          <div className="conn-prog-id">
            <span className="conn-prog-kind">{tab.kind.toUpperCase()}</span>
            <span className="conn-prog-meta">
              <span className="conn-prog-label">{sessionLabel(tab, tr) || tab.host || tr("conn.connecting")}</span>
              {tab.host && <span className="conn-prog-addr">{tab.host}</span>}
            </span>
          </div>
          {failed && stages.length > 0 && (
            <Button size="sm" variant="ghost" onClick={() => setShowLog((v) => !v)}>
              {showLog ? tr("conn.hideLogs") : tr("conn.showLogs")}
            </Button>
          )}
        </div>

        {/* 进度条：加载圆点随阶段沿轨道右移（见 CSS）；失败时停在当前进度并染红，错误信息落在其下方 */}
        <div className="conn-track">
          <span className="conn-rail">
            <span className="conn-rail-fill" style={{ width: `${pct}%` }} />
            <span className="conn-runner" style={{ left: `${pct}%` }} aria-hidden="true" />
          </span>
          <span className="conn-node conn-node-b" aria-hidden="true">
            ›_
          </span>
        </div>

        {/* 状态行：仅连接中展示（阶段文字）；失败时省略——下方红色错误框已说话，避免与「连接失败」重复 */}
        {!failed && (
          <div className="conn-status">
            <span className="conn-status-text">{statusText}</span>
          </div>
        )}

        {failed && tab.failMessage && <div className="conn-fail">{tab.failMessage}</div>}

        {failed && aiState !== "idle" && (
          <div className={`conn-ai is-${aiState}`}>
            {aiState === "loading" && (
              <div className="conn-ai-loading">
                <span className="conn-spinner" aria-hidden="true" />
                <span>{tr("conn.aiAnalyzing")}</span>
              </div>
            )}
            {aiState === "error" && (
              <div className="conn-ai-err">
                <span>{aiErr || tr("conn.aiFail")}</span>
                <Button size="sm" variant="link" onClick={() => void runDiagnose()}>
                  {tr("conn.aiRedo")}
                </Button>
              </div>
            )}
            {(aiState === "done" || aiState === "error") && aiText && (
              <div className="conn-ai-body">
                <div className="conn-ai-bar">
                  <span>{tr("conn.aiResult")}</span>
                  <Button size="sm" variant="link" onClick={() => void copyAi()}>
                    {tr("conn.copyResult")}
                  </Button>
                </div>
                <div className="conn-ai-text">{aiText}</div>
              </div>
            )}
          </div>
        )}

        {feedOpen && stages.length > 0 && (
          <div className="conn-log">
            <div className="conn-log-bar">
              <span>{tr("conn.connLog")}</span>
              <Button size="sm" variant="link" onClick={() => void copyLogs()}>
                {tr("conn.copyLogs")}
              </Button>
            </div>
            {stages.length === 0 && <div className="conn-log-empty">{tr("conn.noLog")}</div>}
            {stages.map((s, i) => (
              <div key={i} className={`conn-log-line is-${s.status}`}>
                <span className="conn-log-ico" aria-hidden="true">
                  {s.status === "fail" ? "✕" : s.status === "ok" ? "✓" : "◦"}
                </span>
                <span className="conn-log-txt">{s.detail || stageLabel(s.stage)}</span>
              </div>
            ))}
          </div>
        )}

        <div className="conn-actions">
          <Button variant="ghost" onClick={() => sessionActions.closePending(tab.id)}>
            {tr("conn.close")}
          </Button>
          {failed && tab.profileId && (
            <Button variant="outline" onClick={() => connectUi.editProfile(tab.profileId!)}>
              {tr("conn.editHost")}
            </Button>
          )}
          {failed && aiState !== "loading" && (
            <Button variant="outline" onClick={() => void runDiagnose()}>
              {aiText ? tr("conn.aiRedo") : tr("conn.analyze")}
            </Button>
          )}
          {failed && (
            <Button variant="primary" onClick={() => void sessionActions.retry(tab.id)}>
              {tr("conn.retry")}
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}
