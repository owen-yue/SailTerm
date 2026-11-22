import { useEffect, useMemo, useRef, useState } from "react";
import ProviderFormDialog from "../settings/ProviderFormDialog";
import EditableSelect from "../common/EditableSelect";
import { IconCheck, IconClose, IconRefresh } from "../common/icons";
import { aiChat, aiStore, defaultProviderId, providerActions, providerStore, settingsStore, terminalAiStore, turnKey, useStore } from "../../stores";
import { withTriggerHint } from "../../lib/agentMode";
import { useElapsedSec, formatElapsed, thinkingKeyForPhase } from "../../lib/useElapsed";
import { useI18n, tReason } from "../../lib/i18n";
import { sessions as sshApi } from "../../lib/tauri";
import type { ChatItem } from "../../stores";
import type { AiApprovalPrompt, AiProvider } from "../../types";
import { StepGroup, groupTimeline, ReasoningStep } from "./AiTimeline";
import ContextRing from "./ContextRing";
import Markdown from "./Markdown";
import QuestionBar from "./QuestionBar";
import { RISK } from "./aiLabels";

interface Props {
  sessionId: string;
}

/**
 * Agent 右侧面板：与终端**并排、不重叠**。
 * - 展示当前连接（sessionId）的完整时间线：问题/思考步骤/命令/回复，全程累积，
 *   切换标签互不干扰，关闭标签（连接结束）才清空。
 * - 命令「填入」写进终端不回车；「执行」才回车
 */
export default function AgentPanel({ sessionId }: Props) {
  const t = useI18n();
  const { timelines, turns, providerBy, approvals, error } = useStore(aiStore);
  // 终端 AI 档（Agent 轨专用：本面板只在 agent 轨挂载，故 rail/canAgentRail 恒成立）。
  // 开关开着时，终端画布会自带浮层 ApprovalBar/QuestionBar（离敲命令处最近、唯一裁决入口），
  // 面板就不重复渲染澄清卡（否则同会话两张、各自本地态不同步）；开关关上才回面板内作答。
  const { terminalAi: defaultAi } = useStore(settingsStore);
  const { bySession } = useStore(terminalAiStore);
  const terminalAiOn = (bySession[sessionId] ?? defaultAi) === "on";
  // 并发化：本面板只读 Agent 轨的在途槽（与同会话的聊天轨各自独立），与其他标签/轨互不串台。
  const tn = turns[turnKey(sessionId, "agent")];
  // 本轨末轮上下文用量（runTurn 收尾时记进 turns）：驱动底部工具栏的 ctx 圆环。
  const lastUsage = tn?.lastUsage ?? null;
  const streamingText = tn?.streamingText ?? "";
  // 推理链与当前相位（仅本轮在途时有值）：驱动分相位提示与推理实时块。
  const reasoningText = tn?.reasoningText ?? "";
  const phase = tn?.phase ?? "idle";
  const providerId = providerBy[sessionId] ?? null;
  const { items: providers } = useStore(providerStore);
  const [filled, setFilled] = useState<Set<string>>(new Set());
  const [confirmId, setConfirmId] = useState<string | null>(null);
  // 模型新增/编辑弹框（共用 ProviderFormDialog）：null=关；provider=null 新建、非空编辑该条。（行内编辑/底部添加分别置此）
  const [pv, setPv] = useState<{ provider: AiProvider | null } | null>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  // 推理块自带限高滚动：新 token 流进来时把它自己滚到底，否则文字在下方看不见。
  const reasoningRef = useRef<HTMLDivElement>(null);
  // 模型选择按会话独立（providerBy[sessionId]，与聊天页同一套 map）：切本会话的下拉只影响本会话
  // 下一轮（runTurn 取 providerBy[sessionId] ?? defaultProviderId）。保证直开 Agent 面板
  // （从未展开过聊天抽屉）时下拉也有数据。
  const noProvider = providers.length === 0;
  const effective = providerId ?? defaultProviderId(providers);
  useEffect(() => {
    void providerActions.refresh();
  }, []);

  // 本连接全程记录（仅 Agent 轨的 send/ai-step 按 sessionId 累积；聊天页发起的走 items 不进这里），
  // 非最后一轮也完整保留
  const items = timelines[sessionId] ?? [];
  // 连续步骤归成「执行过程」折叠块：默认收起，只占一行，否则问答会被十几条过程挤下屏
  const runs = useMemo(() => groupTimeline(items), [items]);
  // 只取 Agent 轨的待批准命令（与聊天轨隔离）；surface 已经 surfaceOf 归一为恒非空，严格比对即可。
  const pending = approvals.filter((a) => a.sessionId === sessionId && a.surface === "agent");
  const thinking = tn?.sending === true;
  // 思考占位走字：本轮已耗时每秒更新（模型静默思考段也能看出没卡住）
  const elapsed = useElapsedSec(tn?.startedAt, thinking);

  useEffect(() => {
    bodyRef.current?.scrollTo({ top: bodyRef.current.scrollHeight });
  }, [items.length, thinking, pending.length, reasoningText, streamingText]);

  // 推理链逐字增长：把限高滚动的推理块自身滚到底（外层面板滚到底不等于内部块滚到底）。
  useEffect(() => {
    const el = reasoningRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [reasoningText]);

  const fill = async (cmd: string) => {
    await sshApi.write(sessionId, cmd).catch(() => {});
    setFilled((s) => new Set(s).add(cmd));
  };

  const run = async (cmd: string, requestId?: string) => {
    const already = filled.has(cmd);
    await sshApi.write(sessionId, already ? "\r" : `${cmd}\r`).catch(() => {});
    setFilled((s) => {
      const n = new Set(s);
      n.delete(cmd);
      return n;
    });
    if (requestId) void aiChat.respondApproval(requestId, true);
    setConfirmId(null);
  };

  return (
    <>
    <aside className="agent-panel">
      <div className="agent-panel-body" ref={bodyRef}>
        {(tn?.error || error) && <div className="form-error">{tn?.error || error}</div>}
        {items.length === 0 && !thinking && pending.length === 0 && !(tn?.error || error) && (
          <div className="agent-empty">{withTriggerHint(t("ag.empty"))}</div>
        )}
        {runs.map((run, i) =>
          run.kind === "steps" ? (
            <StepGroup
              key={run.id}
              steps={run.steps}
              live={thinking && i === runs.length - 1}
              render={(it) => <AgentEntry key={it.id} item={it} sessionId={sessionId} />}
            />
          ) : (
            <AgentEntry key={run.id} item={run.item} sessionId={sessionId} />
          ),
        )}
        {thinking && reasoningText && (
          <div className="agent-reasoning agent-reasoning-live" data-open>
            <div className="agent-reasoning-label">
              {t("ag.reasoningLabel")} · {formatElapsed(elapsed)}
            </div>
            <div className="agent-reasoning-text" ref={reasoningRef}>{reasoningText}</div>
          </div>
        )}
        {thinking && streamingText && (
          <div className="agent-reply">
            <span className="agent-bullet">●</span>
            {/* 流式草稿也走 Markdown 子集：未闭合的半个围栏块按原文走（markdown.ts 流式口径） */}
            <div className="agent-reply-text"><Markdown text={streamingText} sessionId={sessionId} /></div>
          </div>
        )}
        {thinking && !streamingText && !reasoningText && (
          <div className="agent-reply">
            <span className="agent-bullet">●</span>
            {/* 分相位提示：推理中 / 生成命令中 / 思考中，均携已耗时走字 */}
            <div className="agent-reply-text dim">
              {t(`ag.${thinkingKeyForPhase(phase)}`).replace("{e}", formatElapsed(elapsed))}
            </div>
          </div>
        )}
        {pending.map((a) => (
          <CmdCard
            key={a.requestId}
            prompt={a}
            filled={filled.has(a.summary)}
            confirming={confirmId === a.requestId}
            onFill={() => void fill(a.summary)}
            onRun={() => {
              if (a.risk === "DANGEROUS" && confirmId !== a.requestId) {
                setConfirmId(a.requestId);
                return;
              }
              void run(a.summary, a.requestId);
            }}
            onAlways={() => void aiChat.respondApproval(a.requestId, true, undefined, true)}
            onReject={() => {
              void aiChat.respondApproval(a.requestId, false);
              setConfirmId(null);
            }}
            onEdit={() => aiChat.showDetail(a.requestId)}
          />
        ))}
        {/* 澄清反问：仅当终端未接管（AI 开关关）时在面板作答；开关开着时由终端浮层那一份承担，避免双框。*/}
        {!terminalAiOn && <QuestionBar sessionId={sessionId} inline surface="agent" />}
        {(tn?.error || error) && <div className="form-error">{tn?.error || error}</div>}
      </div>
      {/* 底部工具栏（与聊天页输入框下方那条对齐：模型下拉 + ctx 圆环在左）：
          Agent 轨没有输入框，但控件位置/观感与聊天页一致，两页签体验不再分叉。
          模型下拉用公共组件 EditableSelect（行悬停「编辑」+ 底部「添加模型」），不再单设齿轮按钮；
          故下拉常开（即便未配模型也要能点开走「添加模型」加首个）。 */}
      <div className="ai-input-bar agent-panel-foot">
        <EditableSelect
          className="ai-model-pick"
          value={effective ?? ""}
          title={noProvider ? t("aw.noProviderTip") : t("aw.pickModelTip")}
          placeholder={t("aw.noProvider")}
          options={providers.map((p) => ({ value: p.id, label: `${p.model} · ${p.name}` }))}
          onChange={(v) => aiChat.setProvider(sessionId, v || null)}
          addLabel={t("aw.addModels")}
          editTitle={t("common.edit")}
          onAdd={() => setPv({ provider: null })}
          onEdit={(id) => setPv({ provider: providers.find((p) => p.id === id) ?? null })}
        />
        <ContextRing usage={lastUsage} />
      </div>
    </aside>
    {pv && <ProviderFormDialog initial={pv.provider} onClose={() => setPv(null)} />}
    </>
  );
}


/** 面板里的一格：问答气泡或一条过程步骤（由 StepGroup 包成折叠块后调用）。 */
function AgentEntry({ item: it, sessionId }: { item: ChatItem; sessionId: string }) {
  const t = useI18n();
  if (it.kind === "user") {
    return (
      <div className="agent-user-line">
        <span className="agent-ps">Q</span> {it.text}
      </div>
    );
  }
  if (it.kind === "assistant") {
    return (
      <div className="agent-reply">
        <span className="agent-bullet">●</span>
        {/* 与聊天页时间线（AiTimeline）同款渲染：代码块去围栏加卡片，带复制/插入终端 */}
        <div className="agent-reply-text"><Markdown text={it.text} sessionId={sessionId} /></div>
      </div>
    );
  }
  const step = it.step;
  // 推理链：作为「执行过程」里的一步，默认折叠（长），标题行带耗时，点开回看。
  if (step.kind === "reasoning") {
    return <ReasoningStep text={step.text} durationMs={step.durationMs} />;
  }
  if (step.kind === "reflect") {
    return (
      <div className="agent-reflect">
        <span><IconRefresh /></span> {step.message}
      </div>
    );
  }
  if (step.kind === "plan") {
    return (
      <div className="agent-plan">
        <div className="agent-plan-head">{t("ag.plan").replace("{n}", String(step.steps.length))}</div>
        <ol className="agent-plan-list">
          {step.steps.map((s, i) => (
            <li key={i}>{s}</li>
          ))}
        </ol>
      </div>
    );
  }
  if (step.kind === "final") return null;
  return (
    <div
      className="agent-step"
      data-fail={step.kind === "toolResult" && !step.ok}
    >
      <span className="agent-step-ico">
        {step.kind === "toolResult" ? (step.ok ? <IconCheck /> : <IconClose />) : "›"}
      </span>
      <div className="agent-step-body">
        <div className="agent-step-title">
          {step.kind === "toolCall"
            ? t("ag.called").replace("{tool}", step.tool)
            : `${step.tool} ${step.ok ? t("ag.done") : t("ag.fail")}`}
        </div>
        {step.kind === "toolCall" && step.summary && (
          <div className="agent-step-sum">{step.summary}</div>
        )}
        {step.kind === "toolResult" && step.text && (
          <div className="agent-step-sum">{firstLines(step.text, 2)}</div>
        )}
      </div>
    </div>
  );
}

/** 结果摘要：只取前 n 行，避免右侧被输出刷满。 */
function firstLines(s: string, n: number): string {
  return s.trim().split("\n").slice(0, n).join(" · ").slice(0, 160);
}

function CmdCard({
  prompt,
  filled,
  confirming,
  onFill,
  onRun,
  onAlways,
  onReject,
  onEdit,
}: {
  prompt: AiApprovalPrompt;
  filled: boolean;
  confirming: boolean;
  onFill: () => void;
  onRun: () => void;
  onAlways: () => void;
  onReject: () => void;
  onEdit: () => void;
}) {
  const dangerous = prompt.risk === "DANGEROUS";
  const risk = RISK[prompt.risk] ?? RISK.UNKNOWN;
  const t = useI18n();
  return (
    <div className="agent-card" data-risk={prompt.risk}>
      <div className="agent-card-top">
        <span className="agent-card-q">
          {prompt.purpose
            ? t("ag.needAuth").replace("{p}", prompt.purpose)
            : dangerous
              ? t("ag.confirmDanger")
              : t("ag.confirmNormal")}
        </span>
        <span className="agent-card-btns">
          <button
            type="button"
            className="ag-always"
            title={t("ag.alwaysTip")}
            onClick={onAlways}
          >
            {t("ag.always")}
          </button>
          <button type="button" className="ag-exec" data-danger={dangerous} onClick={onRun}>
            {confirming ? t("ag.confirmRun") : filled ? t("ag.runArrow") : t("ag.run")}
          </button>
          <button type="button" className="ag-reject" onClick={onReject}>
            {t("ag.reject")}
          </button>
        </span>
      </div>
      {prompt.purpose && <div className="agent-card-purpose">{prompt.purpose}</div>}
      <pre className="agent-card-cmd">{prompt.summary}</pre>
      <div className="agent-card-foot">
        <span className={`risk-badge ${risk.cls}`}>{t(risk.label)}</span>
        {prompt.reason && (
          <span className="agent-card-why">
            {t("ag.whyNeed").replace(
              "{r}",
              tReason(prompt.reason.key, prompt.reason.args, prompt.reason.zh),
            )}
          </span>
        )}
        <button type="button" className="ghost-link" onClick={onFill} title={t("ag.fillTip")}>
          {filled ? t("ag.filled") : t("ag.fill")}
        </button>
        <button type="button" className="ghost-link" onClick={onEdit}>
          {t("ag.modify")}
        </button>
      </div>
    </div>
  );
}
