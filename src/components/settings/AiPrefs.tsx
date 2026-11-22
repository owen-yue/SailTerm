import { useEffect, useState } from "react";
import { settingsActions, settingsStore, useStore } from "../../stores";
import { REPLY_VERBOSITY_LABEL, type ReplyVerbosity } from "../../types";
import { useI18n } from "../../lib/i18n";
import Select from "../common/Select";
import { Row, Sec } from "./settingsUi";

/**
 * AI 分类里本模块自有的设置：回复详略 + 单轮最大工具往返数。
 * （Provider 配置见 ProviderSettings，提示词模板见 PromptTemplatePanel——都是复用既有管理器；
 * 内置工具/智能增强这类只读能力陈述已归到「关于」页，设置只留可配置项。）
 */
export default function AiPrefs() {
  return (
    <>
      <ReplyStyle />
      <Rounds />
    </>
  );
}

/**
 * 回复详略（运维只关注重点，AI 在终端刷一大片没人想看）：
 * 一个开关同时管两端——提示词里 AI 说多少（后端 `ReplyVerbosity::directive`）与终端里打几行
 * （`REPLY_TERM_LINES`）。右侧栏与历史不受影响，始终全文。
 */
function ReplyStyle() {
  const t = useI18n();
  const { replyVerbosity } = useStore(settingsStore);
  return (
    <Sec title={t("sc.replySec")}>
      <Row
        title={t("sc.replyTitle")}
      >
        <Select
          className="field"
          value={replyVerbosity}
          options={(Object.keys(REPLY_VERBOSITY_LABEL) as ReplyVerbosity[]).map((v) => ({
            value: v,
            label: t(REPLY_VERBOSITY_LABEL[v]),
          }))}
          onChange={(v) => void settingsActions.update({ replyVerbosity: v as ReplyVerbosity })}
        />
      </Row>
    </Sec>
  );
}

/**
 * 单轮最大工具往返数（Agent 一次任务的工具调用步数上限）：本地字符串态便于输入，
 * 失焦/回车才提交；后端钳制（8–200）后回弹非法值。原挂在「通用」页，属 AI 行为旋钮，归位到 AI 分类。
 */
function Rounds() {
  const t = useI18n();
  const { maxToolRounds } = useStore(settingsStore);
  const [roundsStr, setRoundsStr] = useState(String(maxToolRounds));
  useEffect(() => setRoundsStr(String(maxToolRounds)), [maxToolRounds]);
  const commitRounds = () => {
    const n = Number.parseInt(roundsStr, 10);
    if (!Number.isNaN(n) && n !== maxToolRounds) {
      void settingsActions.update({ maxToolRounds: n });
    } else {
      setRoundsStr(String(maxToolRounds)); // 非法或未变：回弹当前值
    }
  };
  return (
    <Sec title={t("sc.roundsSec")}>
      <Row title={t("ss.roundsTitle")}>
        <input
          className="ss-num"
          type="number"
          min={8}
          max={200}
          step={1}
          value={roundsStr}
          onChange={(e) => setRoundsStr(e.target.value)}
          onBlur={commitRounds}
          onKeyDown={(e) => {
            if (e.key === "Enter") (e.target as HTMLInputElement).blur();
          }}
        />
      </Row>
    </Sec>
  );
}
