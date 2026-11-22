import AiDrawer from "./AiDrawer";
import Sheet from "../common/Sheet";
import { APP_CHAT_SESSION } from "../../types";
import { useI18n } from "../../lib/i18n";

interface Props {
  /** 关闭弹框：聊天视图卸载，草稿/在途轮次由 aiStore 与模块级草稿槽保活，再开即续上。 */
  onClose: () => void;
}

/**
 * 「应用级问答」弹框：未连接任何会话也能用 AI（纯聊天形态）。
 *
 * 复用 AiDrawer 全套交互（历史/附件/模型下拉/澄清卡），sessionId 传保留 id：
 * 后端给它钉死空能力集（只剩问答/出网/附件/知识库，无任何执行通路），
 * 前端各门控按 APP_CHAT_SESSION 放行（见 AiDrawer 的 appChat 分支）。
 * 独立模态不占首页/停靠区：首页只留品牌，dock 的开合仍按会话标签记忆（见 types.ts DockPanelId 注释）。
 * 外壳走 Sheet（Esc/外点关闭 + 标题栏全局拖拽）；模型下拉 portal 到 body，不被模态裁剪。
 */
export default function AppChatDialog({ onClose }: Props) {
  const t = useI18n();
  return (
    <Sheet title={t("appChat.title")} onClose={onClose} variant="app-chat">
      <div className="app-chat-body">
        <AiDrawer open embedded sessionId={APP_CHAT_SESSION} />
      </div>
    </Sheet>
  );
}