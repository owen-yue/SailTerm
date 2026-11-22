import { RAIL_TAB_HINT, RAIL_TAB_LABEL } from "../../types";
import { kindHasTerminal, railTabsFor } from "../../lib/capabilities";
import {
  aiStore,
  AI_QUEUE_KEYS,
  awaitingTracks,
  sessionStore,
  settingsActions,
  settingsStore,
  useStore,
  useStorePick,
} from "../../stores";
import { useI18n } from "../../lib/i18n";
import AgentPanel from "../ai/AgentPanel";
import AiDrawer from "../ai/AiDrawer";

interface Props {
  sessionId: string;
}

/**
 * AI 面板内容体（停靠面板 id = "ai"）：顶部横向子页签 `Agent | 聊天` 切换两种视图。
 *
 * Agent 与聊天本是同一个 AI 工作区的两面（数据源同为 aiStore），因此在右侧轨上
 * 合并为单一「AI」入口，进入面板后再横向切子页签；子页签选择仍落设置项
 * `ai.rail_tab`（重启记忆）。原「Agent 页签下悬浮聊天抽屉」路径已删除——
 * 想看完整对话直接切「聊天」子页签，不再有两套输入口。
 * 命令的执行通道由本组页签直接决定（旧的独立设置 `ai.exec_mode` 已删除）：
 * 停在「Agent」时 AI 命令恒打进会话 PTY（像手打一样回显执行）；停在「聊天」时恒静默
 * exec、终端视同纯 shell（后端按 rail 强制，见 tools.rs 逐轮快照）。
 *
 * **Agent 页签只在能兑现 Agent 语义的会话上出现**（能力表 canAgentRail）：Agent 档的语义是
 * 「AI 直接在终端里输出并执行」，没有终端（FTP/FTPS）没有它可言，有终端但无 exec 拿不到
 * 执行工具的（Telnet/串口/Mosh）与本地 Windows 同理不注册。判据取自能力表，
 * 不写死 `kind === "ssh"`：将来新增协议也只要改表。
 */
export default function SessionRail({ sessionId }: Props) {
  const tr = useI18n();
  const { rail } = useStore(settingsStore);
  const { tabs } = useStore(sessionStore);
  // 只选两字段：流式输出逐 token 写 turns，侧轨页签不该跟着重渲染（面板内的卡自便）。
  const { approvals, questions } = useStorePick(aiStore, AI_QUEUE_KEYS);
  const kind = tabs.find((t) => t.id === sessionId)?.kind ?? null;
  // 页签集合取自能力表注册（railTabsFor）：本地 Windows、Telnet/串口/mosh、FTP 都只留「聊天」。
  const railTabs = railTabsFor(kind);
  const canAgent = railTabs.includes("agent");
  // 页签栏：终端类会话恒画（聊天唯一形态只是少一枚 Agent、仍留「聊天」）；无终端会话单视图不画栏。
  const showTabs = kind !== null && kindHasTerminal(kind);
  // 页签集合被过滤时不能沿用落库的 `rail`：用户在 SSH 上选的 Agent 档不能带到本地 Windows/Telnet/FTP 来。
  const chat = !canAgent || rail === "chat";
  // 高亮跟实际渲染的视图（chat）而非裸 `rail`：Agent 被过滤时画的是聊天，页签不能还亮着 Agent。
  // 本会话「等你回执」的卡在哪一轨：反问 / 审批卡只渲在对应轨的子页签里，切错页签就看不见，
  // 所以两个页签各自明灭（不只闪当前那个）。
  const awaiting = awaitingTracks(approvals, questions, sessionId);

  return (
    <aside className="session-rail">
      {showTabs && (
        <div className="rail-tabs">
          {railTabs.map((id) => (
            <button
              key={id}
              type="button"
              className={`rail-tab${awaiting.has(id) ? " awaiting" : ""}`}
              data-on={(id === "chat" ? chat : rail === id) ? "true" : undefined}
              title={
                tr(RAIL_TAB_HINT[id]) +
                (awaiting.has(id) ? ` · ${tr("ai.awaitTip")}` : "")
              }
              onClick={() => {
                if (id !== rail) void settingsActions.update({ rail: id });
              }}
            >
              {tr(RAIL_TAB_LABEL[id])}
            </button>
          ))}
        </div>
      )}
      {chat ? (
        <AiDrawer open embedded sessionId={sessionId} />
      ) : (
        <AgentPanel sessionId={sessionId} />
      )}
    </aside>
  );
}
