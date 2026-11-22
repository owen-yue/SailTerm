import { useState } from "react";
import { aiChat, aiStore, useStore } from "../../stores";
import { useI18n } from "../../lib/i18n";
import { useBarFold } from "../../lib/useBarFold";
import Chevron from "../common/Chevron";

/** 保留标记：末位「其他（自行填写）」选项，非模型给的 label，用它把自填文本并入作答。 */
const OTHER = "\u0000__other__";

/** 一道子题的作答草稿（勾选 + 自定义文本）。 */
interface SubDraft {
  picked: Set<string>;
  custom: string;
}

/** 一张卡（含 1–4 题）的 UI 态：当前题号 + 各题草稿。 */
interface CardState {
  cur: number;
  per: Record<number, SubDraft>;
}

const emptySub = (): SubDraft => ({ picked: new Set(), custom: "" });
const emptyCard = (): CardState => ({ cur: 0, per: {} });

/** 选项字母标号：A/B/C/…（「其他」接在模型给的选项之后）。 */
const letter = (i: number) => String.fromCharCode(65 + i);

/**
 * 澄清反问条：AI 遇到无法自行确定的关键岔路时抛出的问题卡（复用审批条 `agent-float` 槽位与挂载点）。
 * - 一次可含 1–4 题（后端 questions 数组）：多题时头部给 ‹ n/N › 切换器逐题作答，未提交前可来回改；
 * - 每题选项按 A/B/C/… 编号，末位固定「其他（自行填写）」——选中它才可填输入框（自填与选项互斥/多选下可并存）；
 * - 底部一行左「推荐」右「提交」：推荐把**所有题**都勾上各自 recommended 项并定位到最后一题（仍可改、不提交），提交一次交整卡；
 * - 提交校验：任一题既无勾选又无自填文本 → 不提交，静默自动定位到第一道未答题（不显多余提示）；
 * - 不提供「你替我定」一键代答出口：关键岔路必须逐题拍板。
 *
 * 与审批条同槽但**天然互斥**：编排循环串行执行工具、全局同时只跑一轮，故任意时刻最多一个 pending
 * 中断（要么审批要么反问），两张卡不会同时出现、不撞车。
 */
export default function QuestionBar({
  sessionId,
  inline = false,
  surface,
}: {
  sessionId: string;
  inline?: boolean;
  /** 只渲染该「来源轨」的题（chat / agent）：不传则不按轨过滤。避免切页签时跨轨泄漏。 */
  surface?: "chat" | "agent";
}) {
  const { questions } = useStore(aiStore);
  const t = useI18n();
  // 按轨严格过滤：只渲染本轨的题。surface 已在 pushQuestion 经 surfaceOf 归一为恒非空（缺 track 时兑底猜单一轨），
  // 故此处直接比对即可——既不跨轨泄漏，也不会因滤空而永久挂死。`surface == null` 指调用方未指定轨（不过滤）。
  const list = questions.filter(
    (q) => q.sessionId === sessionId && (surface == null || q.surface === surface),
  );
  const [cards, setCards] = useState<Record<string, CardState>>({});
  // 提交被拦下的题号（连着卡号一起记）：旧写法只把卡片静默跳回那道未答题，什么也不说——
  // 用户只看到题面自己变了，以为是卡片抽风（多题卡里这一下根本无从归因）。
  // 带卡号才能在新卡到来时自然作废旧提示。
  const [miss, setMiss] = useState<{ id: string; k: number } | null>(null);
  const [foldPref, toggleFold] = useBarFold();
  // 只有挂在面板里的两处（inline）能收起：终端浮层那条是离敲命令最近的唯一裁决入口，
  // 折叠会弱化命令可见性，故保持常驻展开。
  const folded = inline && foldPref;
  if (!list.length) return null;
  // 编排串行：同会话同时至多一张卡，取队首渲染。
  const prompt = list[0];
  const card = cards[prompt.requestId] ?? emptyCard();
  const subs = prompt.questions;
  const multiQ = subs.length > 1;
  const i = Math.min(card.cur, subs.length - 1);
  const sub = subs[i];

  const subDraft = (k: number): SubDraft => card.per[k] ?? emptySub();
  // 收起态要一眼看出这题答没答、答的是哪个字母，否则收起等于把已勾的进度藏起来。
  const marksOf = (k: number): string => {
    const d = subDraft(k);
    const marks = subs[k].options
      .map((o, idx) => (d.picked.has(o.label) ? letter(idx) : null))
      .filter(Boolean) as string[];
    if (d.picked.has(OTHER)) marks.push(letter(subs[k].options.length));
    return marks.join("·");
  };
  const patchSub = (k: number, p: Partial<SubDraft>) =>
    setCards((m) => {
      const c = m[prompt.requestId] ?? emptyCard();
      const d = c.per[k] ?? emptySub();
      return { ...m, [prompt.requestId]: { ...c, per: { ...c.per, [k]: { ...d, ...p } } } };
    });
  const patchCard = (p: Partial<CardState>) =>
    setCards((m) => ({
      ...m,
      [prompt.requestId]: { ...emptyCard(), ...m[prompt.requestId], ...p },
    }));

  // 点选项只改勾选，不提交；单选互斥替换，多选 toggle（「其他」同理，自填框随其勾选启用）。
  // 单选选中真实选项后自动跳到下一题（末题停留、「其他」需自填故不跳、多选不跳）。
  const onOption = (k: number, isMulti: boolean, marker: string) => {
    if (miss && miss.k === k) setMiss(null);
    setCards((m) => {
      const c = m[prompt.requestId] ?? emptyCard();
      const d = c.per[k] ?? emptySub();
      const picked = new Set(d.picked);
      if (isMulti) {
        if (picked.has(marker)) picked.delete(marker);
        else picked.add(marker);
      } else {
        picked.clear();
        picked.add(marker);
      }
      const advance = !isMulti && marker !== OTHER && k < subs.length - 1;
      return {
        ...m,
        [prompt.requestId]: {
          ...c,
          cur: advance ? k + 1 : c.cur,
          per: { ...c.per, [k]: { ...d, picked } },
        },
      };
    });
  };

  // 本题作答文本：命中的选项 label + （勾了「其他」且非空时）自填文本，拼成一段。
  const subText = (k: number): string => {
    const s = subs[k];
    const d = subDraft(k);
    const labels = s.options.filter((o) => d.picked.has(o.label)).map((o) => o.label);
    const other = d.picked.has(OTHER) ? d.custom.trim() : "";
    return [...labels, other].filter(Boolean).join("；");
  };

  // 「推荐」：把**所有题**都勾上各自标 recommended 的项（多选下可全勾），仍可改、不直接提交。
  // 勾完定位到最后一题：与单选逐题自动跳转的落点一致，用户在此复核末题后直接提交。
  const anyRec = subs.some((s) => s.options.some((o) => o.recommended));
  const applyRecommend = () =>
    setCards((m) => {
      const c = m[prompt.requestId] ?? emptyCard();
      const per = { ...c.per };
      subs.forEach((s, k) => {
        const rec = s.options.filter((o) => o.recommended).map((o) => o.label);
        if (rec.length) per[k] = { ...(c.per[k] ?? emptySub()), picked: new Set(rec) };
      });
      return { ...m, [prompt.requestId]: { ...c, cur: subs.length - 1, per } };
    });

  // 提交整卡：任一空题 → 静默定位到第一道未答（不显提示）；否则拼答案回后端（单题只给答案，多题带题干）。
  const submit = () => {
    const texts = subs.map((_, k) => subText(k));
    const missingIdx = texts.findIndex((x) => !x);
    if (missingIdx >= 0) {
      setMiss({ id: prompt.requestId, k: missingIdx });
      patchCard({ cur: missingIdx });
      return;
    }
    setMiss(null);
    const answer = multiQ
      ? subs.map((s, k) => `【${s.question}】${texts[k]}`).join("\n")
      : texts[0];
    setCards((m) => {
      const { [prompt.requestId]: _, ...rest } = m;
      return rest;
    });
    void aiChat.respondQuestion(prompt.requestId, answer);
  };

  return (
    <div
      className={`agent-float qb${inline ? " inline" : ""}${folded ? " folded" : ""}`}
      // 这是一张需要作答的卡，不是播报区：`role=status` 会把里面的选项当成状态文本读
      role="group"
      aria-label={t("qb.title")}
    >
      <div className="qb-head">
        <span className="agent-float-tag">{t("qb.title")}</span>
        <span className="qb-q" title={sub.question}>
          {sub.question}
        </span>
        {sub.multi && <span className="qb-hint">{t("qb.pick")}</span>}
        {/* 行尾靠右的一按：展开态是多题切换器，收起态换成「答到哪」的一句话 + 展开按钮。
            选项区与操作行不在这里渲染，由 .qb.folded 整块隐掉（见 App.css）。 */}
        <span className="qb-tail">
          {multiQ && !folded && (
            <span className="qb-nav">
            <button
              type="button"
              className="qb-nav-btn"
              disabled={i === 0}
              title={t("qb.navPrev")}
              onClick={() => patchCard({ cur: i - 1 })}
            >
              ‹
            </button>
            <span className="qb-nav-count" aria-live="polite">
              {i + 1}/{subs.length}
            </span>
            <button
              type="button"
              className="qb-nav-btn"
              disabled={i >= subs.length - 1}
              title={t("qb.navNext")}
              onClick={() => patchCard({ cur: i + 1 })}
            >
              ›
            </button>
            </span>
          )}
          {folded && (
            <span className="qb-fold-state" data-todo={marksOf(i) ? undefined : "true"}>
              {multiQ ? `${i + 1}/${subs.length} · ` : ""}
              {marksOf(i) || t("qb.todo")}
            </span>
          )}
          {inline && (
            <button
              type="button"
              className="bar-fold"
              aria-expanded={!folded}
              title={t(folded ? "common.expandAnswer" : "common.collapse")}
              onClick={toggleFold}
            >
              <Chevron dir={folded ? "up" : "down"} />
            </button>
          )}
        </span>
      </div>

      <div className="qb-options" role={sub.multi ? "group" : "radiogroup"}>
        {sub.options.map((o, idx) => (
          <button
            key={o.label}
            type="button"
            className="qb-opt"
            // 选项改成单行后文字会被省略号截掉，全文挂到 title 悬停即读：有说明给全量，
            // 只有 label（AI 常不给 description）也要兜住，否则被截的那半句无处可读。
            title={o.description ? `${o.label} — ${o.description}` : o.label}
            role={sub.multi ? "checkbox" : "radio"}
            aria-checked={subDraft(i).picked.has(o.label)}
            data-on={subDraft(i).picked.has(o.label)}
            onClick={() => onOption(i, sub.multi, o.label)}
          >
            <span className="qb-opt-label">
              <span className="qb-letter">{letter(idx)}</span>
              {/* 标签套独立 span 才能吃到省略号（flex 容器里的裸文本节点不会截字） */}
              <span className="qb-opt-text">{o.label}</span>
              {o.recommended && <span className="qb-rec">{t("qb.recommended")}</span>}
            </span>
            {o.description && <span className="qb-opt-desc">{o.description}</span>}
          </button>
        ))}
        <button
          type="button"
          className="qb-opt qb-other"
          role={sub.multi ? "checkbox" : "radio"}
          aria-checked={subDraft(i).picked.has(OTHER)}
          data-on={subDraft(i).picked.has(OTHER)}
          onClick={() => onOption(i, sub.multi, OTHER)}
        >
          <span className="qb-opt-label">
            <span className="qb-letter">{letter(sub.options.length)}</span>
            {t("qb.other")}
          </span>
        </button>
        <input
          className="qb-custom"
          value={subDraft(i).custom}
          placeholder={t("qb.custom")}
          disabled={!subDraft(i).picked.has(OTHER)}
          onChange={(e) => patchSub(i, { custom: e.target.value })}
          onKeyDown={(e) => {
            // 回车=提交整卡（与「提交」同口径），不冒泡到终端/聊天输入框的回车处理。
            if (e.key === "Enter") {
              e.preventDefault();
              e.stopPropagation();
              submit();
            }
          }}
        />
      </div>

      {miss && miss.id === prompt.requestId && (
        <p className="qb-miss" role="alert">
          {t("qb.unanswered")
            .replace("{n}", String(miss.k + 1))
            .replace("{q}", subs[miss.k]?.question ?? "")}
        </p>
      )}

      <div className="qb-actions-row">
        <button
          type="button"
          className="qb-recommend"
          disabled={!anyRec}
          title={anyRec ? undefined : t("qb.recommendNone")}
          onClick={applyRecommend}
        >
          {t("qb.recommend")}
        </button>
        <button type="button" className="qb-submit" onClick={submit}>
          {t("qb.submit")}
        </button>
      </div>
    </div>
  );
}
