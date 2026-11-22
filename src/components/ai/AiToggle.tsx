import { settingsStore, terminalAiActions, terminalAiStore, useStore } from "../../stores";
import { withTriggerHint } from "../../lib/agentMode";
import { useI18n } from "../../lib/i18n";
import type { TerminalAi } from "../../types";

/**
 * 终端上的 AI 开关：小滑动开关，两端文字 `shell`（左=关，纯 shell）/ `agent`（右=开，
 * 终端既能敲命令也能直接输对话），当前档那侧文字高亮。整颗开关可点，点哪侧即切到哪档。
 * 放在每条终端顶部的细条右端（Terminal.tsx 的 .term-strip）——不叠在画布上，永不遮挡输出。
 *
 * 每条终端各自独立（`terminalAiStore.bySession[sessionId]`）：在这个终端切 shell 不影响
 * 别的终端。切换时顺带把全局 terminalAi 更新为这次的选择，作为「新开终端的默认档」——
 * 但已开的其他终端各自快照过、不会被反向改动。跟右侧栏页签（`rail`：Agent/聊天）互不牵动。
 * 备用屏（vim/less 等全屏程序）下由 Terminal.tsx 连细条一起收起，不压程序画面。
 */
export default function AiToggle({ sessionId }: { sessionId: string }) {
  const t = useI18n();
  const { bySession } = useStore(terminalAiStore);
  const { terminalAi: defaultAi } = useStore(settingsStore);
  // 本终端有效档：自己的快照优先，未快照（刷新后）回落全局默认档
  const mode: TerminalAi = bySession[sessionId] ?? defaultAi;
  const on = mode === "on";
  return (
    <button
      type="button"
      className="term-ai-switch"
      role="switch"
      aria-checked={on}
      aria-label={t("atg.label")}
      title={
        on
          ? withTriggerHint(t("atg.on"))
          : t("atg.off")
      }
      onClick={() => terminalAiActions.set(sessionId, on ? "off" : "on")}
    >
      <span className="term-ai-word" data-on={!on}>
        shell
      </span>
      <span className="term-ai-knob" aria-hidden="true">
        <i />
      </span>
      <span className="term-ai-word" data-on={on}>
        agent
      </span>
    </button>
  );
}
