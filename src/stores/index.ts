// stores 统一出口：UI 状态、连接档案、会话标签、文件浏览、传输队列、AI 对话（均不含凭证）。
export {
  aiChat,
  aiStore,
  AI_QUEUE_KEYS,
  awaitingTracks,
  turnKey,
  type ChatFile,
  type ChatImage,
  type ChatItem,
} from "./aiStore";
export { appearanceActions, appearanceStore } from "./appearanceStore";
export { AUDIT_RANGES, auditActions, auditSessions, auditStore, visibleAudit } from "./auditStore";
export { collapseActions, collapseStore, type CollapseKey } from "./collapseStore";
export { dockActions, dockStore, dockZoneOf, type DockZone } from "./dockStore";
export {
  dockPaneActions,
  dockPaneStore,
  useDockSplit,
  SPLIT_DOCK_LEFT,
  SPLIT_DOCK_RIGHT,
} from "./dockPaneStore";
export { connections, connectionStore } from "./connectionStore";
export {
  childPath,
  files,
  fileStore,
  paneOf,
  transferEntry,
  transferSelected,
  uploadPaths,
  type Lane,
  type Pane,
  type Side,
} from "./fileStore";
export { ftpsCertDecisions, ftpsCertStore } from "./ftpsCertStore";
export { hostKeyDecisions, hostKeyStore } from "./hostKeyStore";
export { lockActions, lockStore } from "./lockStore";
export { kbdDecisions, kbdStore } from "./kbdStore";
export {
  layoutActions,
  layoutStore,
  MAX_PANES,
  nodeLeaves,
  type DropZone,
  type SplitDirection,
  type SplitNode,
} from "./layoutStore";
export { memoryActions, memoryStore } from "./memoryStore";
export {
  PANEL_DEFAULT,
  PANEL_MAX,
  PANEL_MIN,
  panel,
  panelStore,
  type PanelTab,
} from "./panelStore";
export { policyActions, policyStore } from "./policyStore";
export { defaultProviderId, providerActions, providerStore, TESTING_DRAFT } from "./providerStore";
export {
  profileKeyOf,
  groupKeyOf,
  connPoolKeyOf,
  QUICKBAR_GLOBAL_KEY,
  quickBarActions,
  quickBarStore,
  type QuickBarMap,
} from "./quickBarStore";
export { recordActions, recordStore } from "./recordStore";
export { restartActions, restartStore } from "./restartStore";
export {
  DEFAULT_SIZE,
  rememberSize,
  sessionActions,
  sessionStore,
  type ConnectIntent,
  type StageLine,
  type Tab,
} from "./sessionStore";
export { settingsActions, settingsStore } from "./settingsStore";
export { snippetActions, snippetStore } from "./snippetStore";
export {
  INFINITE,
  canSend,
  sendActions,
  sendStore,
  type SendState,
  type SendTargetState,
} from "./sendStore";
export { sshKeyActions, sshKeyStore } from "./sshKeyStore";
export { createStore, useStore, useStorePick, type Store } from "./store";
export { terminalActions, terminalStore } from "./terminalStore";
export {
  effectiveTerminalAi,
  terminalAiActions,
  terminalAiStore,
} from "./terminalAiStore";
export { termSplitStore, splitActions, MAIN_SPLIT_ENABLED } from "./termSplitStore";
export { toast, toastStore, setWinFocused, notifyOsBypass } from "./toastStore";
export type { ToastItem, ToastKind, ToastRecord } from "./toastStore";
export { triggerActions, triggerStore } from "./triggerStore";
export {
  MONITOR_INTERVALS,
  uiPrefs,
  UIPREFS_DEFAULT,
  uiPrefsStore,
  type UiPrefs,
} from "./uiPrefsStore";
export {
  activeCount,
  aggregateProgress,
  isFinal,
  transferActions,
  transferStore,
  type Conflict,
  type SubmitRequest,
  type Task,
} from "./transferStore";
export { tunnelActions, tunnelStore } from "./tunnelStore";
