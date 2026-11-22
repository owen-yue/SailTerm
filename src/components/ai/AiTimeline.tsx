import { useEffect, useState, type ReactNode } from "react";
import { IconCheck, IconClose, IconFile, IconFolder, IconGear, IconRefresh, IconTerminalSel } from "../common/icons";
import { showImage } from "../file/ImageLightbox";
import type { ChatImage, ChatItem } from "../../stores";
import { degradedDetail } from "../../lib/chatFile";
import type { AiStep } from "../../types";
import { useI18n } from "../../lib/i18n";
import { ai as aiApi } from "../../lib/tauri";
import { RISK, TOOL_LABEL } from "./aiLabels";
import Markdown from "./Markdown";

/**
 * 把用户消息开头的 `📎 …文件：/path` 引用行拆成 chip（气泡内展示），其余为正文。
 *  与 AiDrawer 发送协议对齐：每条引用独占一行、`📎 ` 前缀稳定，冒号（半/全角）后为路径。 */
function splitUserRefs(text: string): { refs: { path: string; name: string; isDir: boolean }[]; body: string } {
  const lines = text.split("\n");
  const refs: { path: string; name: string; isDir: boolean }[] = [];
  let i = 0;
  while (i < lines.length && (lines[i].startsWith("📎 ") || lines[i].startsWith("📁 "))) {
    const isDir = lines[i].startsWith("📁 ");
    const rest = lines[i].slice(3); // 📎/📁 各占 2 个 UTF-16 单元 + 1 空格
    const idx = rest.search(/[:：]/);
    const path = (idx >= 0 ? rest.slice(idx + 1) : rest).trim();
    const name = path.split(/[\\/]/).filter(Boolean).pop() || path;
    refs.push({ path, name, isDir });
    i++;
  }
  return { refs, body: lines.slice(i).join("\n") };
}

/**
 * 从正文里抽出「终端选区」块（`🖥 ` 标记行 + 紧随的 ```text 围栏，与 AiDrawer 发送协议对齐），
 * 气泡内还原成 chip，其余保持正文。全局扫而非只看头部：问句在前、选区在后也是合法写法。
 */
function splitUserTerm(text: string): { terms: string[]; body: string } {
  const terms: string[] = [];
  const re = new RegExp("🖥\\n```text\\n([\\s\\S]*?)\\n```\\n?");
  const body = text.replace(new RegExp(re.source, "g"), (_m, b: string) => {
    terms.push(b);
    return "";
  });
  return { terms, body };
}

/**
 * 推理链步骤：Agent 面板与聊天抽屉共用的唯一实现，保证两处「思考过程」样式一致
 * （此前两处各写一套：聊天带 ◦ 图标 + 边框 <pre>，Agent 是紧凑折叠行，观感不统一）。
 * 默认折叠，标题行内联耗时（` · Ns`，旧数据无计时则不占位），点开回看当时在想什么。
 * 样式挂在 .agent-reasoning（全局选择器），聊天抽屉内同样生效。
 */
export function ReasoningStep({ text, durationMs }: { text: string; durationMs: number }) {
  const t = useI18n();
  const dur = durationMs > 0 ? ` · ${Math.max(1, Math.round(durationMs / 1000))}s` : "";
  return (
    <details className="agent-reasoning">
      <summary className="agent-reasoning-label">{t("ag.reasoningLabel")}{dur}</summary>
      <div className="agent-reasoning-text">{text}</div>
    </details>
  );
}

/** token 数紧凑显示：≥1000 折成 `1.2k`（一位小数，整千不带小数）；用量弹窗共用。 */
export function fmtTok(n: number): string {
  return n >= 1000 ? `${(n / 1000).toFixed(1).replace(/\.0$/, "")}k` : String(n);
}

/**
 * 已拉过的历史缩略图缓存（`attachmentId -> dataUrl`）：滚动时间线、来回切对话都会重复渲染
 * 同一张图，而每张都是一趟 IPC + 一次 base64 解码。只缓存成功结果（取不到就下次再试）。
 */
const thumbCache = new Map<string, string>();

/**
 * 用户气泡里的一张图。
 * 本轮刚贴的直接用本地 `dataUrl`；从归档恢复的只有 id → 按需拉字节（清单已随消息回传，
 * 字节只在本屏真要看时取）。图本体被手工删了也不报错：退化成一行文件名。
 *
 * 缩略图刻意做小（看不清内容），点开走 [`showImage`] 看原图：侧栏宽不到 400px，
 * 一张大图会把整条问答挤出视口，而回看历史时多数时候只需要「发过这张图」这个确认。
 */
function ChatThumb({ img }: { img: ChatImage }) {
  const t = useI18n();
  const cached = img.id ? thumbCache.get(img.id) ?? "" : "";
  const [src, setSrc] = useState(img.dataUrl ?? cached);
  useEffect(() => {
    if (src || !img.id) return;
    let live = true;
    void aiApi
      .getAttachment(img.id)
      .then((d) => {
        const url = `data:${d.mime};base64,${d.data}`;
        thumbCache.set(img.id as string, url);
        if (live) setSrc(url);
      })
      .catch(() => {}); // 拉不到就当没图（文件被删/路径迁移），不把一条旧问答渲染成错误
    return () => {
      live = false;
    };
  }, [img.id, src]);
  // 用 <button> 包而非给 <img> 挂 onClick：键盘（Tab + Enter）能打开，与项目里其余
  // 可点元素一致（见 ai-img-x / md-link）；尺寸与外观全在 CSS。
  if (src)
    return (
      <button
        type="button"
        className="ai-thumb-btn"
        title={img.name ? `${img.name} · ${t("aw.imgZoom")}` : t("aw.imgZoom")}
        onClick={() => showImage(src, img.name ?? "")}
      >
        <img className="ai-thumb" src={src} alt={img.name} />
      </button>
    );
  return <span className="ai-thumb ai-thumb-miss" title={img.name}>{img.name}</span>;
}

/**
 * AI 时间线渲染（抽屉与终端就地面板共用）。
 *
 * 一格 = 用户气泡 / 助手气泡 / 一个工具步骤卡片。步骤卡片复用 `.ai-step*` 与 `.risk-badge`
 * 既有样式，保证两处视觉一致；`final` 步骤与助手气泡重复，不渲染。
 *
 * 一轮问答之间可能夹十几条过程步骤（工具调用/输出/计划/反思），逐条铺开会把问答挤下屏，
 * 因此连续的步骤被归成一个「执行过程」折叠块（见 [`StepGroup`]）：默认收起，只占一行，
 * 想看当时怎么做的点开即可 —— 与主流 AI 工具对思考过程的处理一致。
 */
export function TimelineItem({
  item,
  sessionId,
}: {
  item: ChatItem;
  /** 关联会话：回复里的代码块据此提供「插入终端」动作（写入 PTY 但不回车，不自动执行）。 */
  sessionId?: string | null;
}) {
  const t = useI18n();
  if (item.kind === "user") {
    const { refs, body: rawBody } = splitUserRefs(item.text);
    const { terms, body } = splitUserTerm(rawBody);
    return (
      <div className="ai-msg user">
        <div className="ai-bubble">
          {(item.images?.length ?? 0) > 0 && (
            <span className="ai-bubble-imgs">
              {item.images?.map((im, i) => (
                <ChatThumb key={i} img={im} />
              ))}
            </span>
          )}
          {refs.length > 0 && (
            <span className="ai-bubble-refs">
              {refs.map((r, i) => (
                <span key={i} className="ai-ref" title={r.path}>
                  {r.isDir ? <IconFolder /> : <IconFile />}
                  <b>{r.name}</b>
                </span>
              ))}
            </span>
          )}
          {/* 终端选区：发送时是围栏全文，气泡里还原成小药丸（hover 预览前段），不把刷屏输出铺回去 */}
          {terms.length > 0 && (
            <span className="ai-bubble-refs">
              {terms.map((tx, i) => (
                <span key={i} className="ai-ref" title={`${tx.slice(0, 400)}${tx.length > 400 ? "…" : ""}`}>
                  <IconTerminalSel />
                  <b>{t("tx.askAiChip").replace("{n}", String(tx.split("\n").filter((l) => l.trim() !== "").length))}</b>
                </span>
              ))}
            </span>
          )}
          {/* 本轮随发的本地文档：只亮类型图标 + 文件名。「已抽取 N 字 / 在哪退化了」不直接展示，
              避免气泡冗长，完整路径与退化细节收进 tooltip。 */}
          {(item.files?.length ?? 0) > 0 && (
            <span className="ai-bubble-files">
              {item.files?.map((f, i) => {
                const detail = degradedDetail(f.degraded);
                return (
                  <span
                    key={i}
                    className="ai-file-chip"
                    title={[f.path || f.name, detail].filter(Boolean).join(" — ")}
                  >
                    <IconFile />
                    <b>{f.name}</b>
                  </span>
                );
              })}
            </span>
          )}
          {body && <span className="ai-bubble-text">{body}</span>}
        </div>
      </div>
    );
  }
  if (item.kind === "assistant") {
    return (
      <div className="ai-msg ai">
        {/* 回复按 Markdown 子集渲染（见 components/Markdown.tsx）：代码块自带「复制/插入终端」，
            故此处不再挂气泡级的单个插入钮——一条回复给多条命令时不该只能插第一条。 */}
        <div className="ai-bubble">
          <Markdown text={item.text} sessionId={sessionId} />
        </div>
        {/* 气泡脚注（本轮 token 合计）已去掉：逐条报数刷屏，
            上下文占用看输入栏旁的圆环即可；合计仍存于归档（hover 圆环看明细）。 */}
      </div>
    );
  }
  return <StepCard step={item.step} />;
}

/** 时间线的一段：一条问答气泡，或一串连续的过程步骤（折叠成一块）。 */
export type TimelineRun =
  | { kind: "single"; id: string; item: ChatItem }
  | { kind: "steps"; id: string; steps: ChatItem[] };

/**
 * 把时间线切成「气泡 / 过程块」交替序列：连续的 step 归一组，其余逐条透出。
 * 归组只看相邻性，不看属于哪一轮 —— 回看历史时两条气泡之间的步骤天然就是一组。
 */
export function groupTimeline(items: ChatItem[]): TimelineRun[] {
  const runs: TimelineRun[] = [];
  for (const it of items) {
    const last = runs[runs.length - 1];
    if (it.kind === "step" && last && last.kind === "steps") {
      last.steps.push(it);
      continue;
    }
    if (it.kind === "step") runs.push({ kind: "steps", id: `g-${it.id}`, steps: [it] });
    else runs.push({ kind: "single", id: it.id, item: it });
  }
  return runs;
}

/**
 * 过程折叠块：一行标题（步数 + 状态）+ 可展开的明细。
 * `live` = 这一块正在生成（本轮在途且它是最后一段）：展开着让用户看到实时进展，
 * 轮次结束后自动收起；用户手动开合过则以手动为准（`manual`）。
 */
export function StepGroup({
  steps,
  live,
  render,
}: {
  steps: ChatItem[];
  live: boolean;
  render: (item: ChatItem) => ReactNode;
}) {
  const [manual, setManual] = useState<boolean | null>(null);
  const t = useI18n();
  const open = manual ?? live;
  return (
    <div className="ai-proc" data-open={open} data-live={live}>
      <button
        type="button"
        className="ai-proc-head"
        title={t("tl.expandTip")}
        onClick={() => setManual(!open)}
      >
        <span className="ai-proc-caret">{open ? "▾" : "▸"}</span>
        <span className="ai-proc-title">{t("tl.process").replace("{n}", String(steps.length))}</span>
        <span className="ai-proc-state">{live ? t("tl.running") : t("tl.done")}</span>
      </button>
      {open && <div className="ai-proc-body">{steps.map((s) => render(s))}</div>}
    </div>
  );
}

/** 工具步骤卡片：调用（含风险/裁决）/ 结果（含输出）/ 计划 / 反思 / 失败。 */
export function StepCard({ step }: { step: AiStep }) {
  const t = useI18n();
  if (step.kind === "toolCall") {
    const risk = RISK[step.risk] ?? RISK.UNKNOWN;
    return (
      <div className="ai-step call">
        <div className="ai-step-head">
          <span className="ai-step-ico"><IconGear /></span>
          <span className="ai-step-tool">{t(TOOL_LABEL[step.tool] ?? step.tool)}</span>
          <span className={`risk-badge ${risk.cls}`}>{t(risk.label)}</span>
          {step.decision === "NEEDS_APPROVAL" && <span className="ai-flag wait">{t("tl.wait")}</span>}
          {step.decision === "BLOCKED" && <span className="ai-flag block">{t("tl.blocked")}</span>}
        </div>
        {step.summary && <div className="ai-step-summary">{step.summary}</div>}
      </div>
    );
  }
  if (step.kind === "toolResult") {
    return (
      <div className="ai-step result" data-ok={step.ok}>
        <div className="ai-step-head">
          <span className="ai-step-ico">{step.ok ? <IconCheck /> : <IconClose />}</span>
          <span className="ai-step-tool">{t(TOOL_LABEL[step.tool] ?? step.tool)}</span>
          <span className="ai-step-state">{step.ok ? t("tl.ok") : t("tl.fail")}</span>
        </div>
        {step.text && <pre className="ai-step-out">{step.text}</pre>}
      </div>
    );
  }
  if (step.kind === "plan") {
    return (
      <div className="ai-step plan">
        <div className="ai-step-head">
          <span className="ai-step-ico">≡</span>
          <span className="ai-step-tool">{t("tl.plan")}</span>
          <span className="ai-step-state">{t("tl.stepsCount").replace("{n}", String(step.steps.length))}</span>
        </div>
        <ol className="ai-step-plan">
          {step.steps.map((s, i) => (
            <li key={i}>{s}</li>
          ))}
        </ol>
      </div>
    );
  }
  if (step.kind === "reflect") {
    return (
      <div className="ai-step reflect">
        <span className="ai-step-ico"><IconRefresh /></span>
        <span>{step.message}</span>
      </div>
    );
  }
  // 推理链：作为「执行过程」里的一步，与 Agent 面板共用 ReasoningStep，样式两处一致。
  if (step.kind === "reasoning") {
    return <ReasoningStep text={step.text} durationMs={step.durationMs} />;
  }
  return null;
}
