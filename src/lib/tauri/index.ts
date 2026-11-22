// lib/tauri 统一出口：前端所有 IPC 都从这里走（「前端唯一 IPC 出口」）。
// client 只具名导出规范化 API：invoke/listen 裸原语在 client.ts 里另有导出，
// 但仅供单测直引 ../tauri/client 打桩，不走本 barrel（防业务层绕过 call/on 出口）。 
export { call, on, errCode, errMsg, setErrTranslator, b64ToBytes, type UnlistenFn, type AppErrorBody } from "./client";
export { ai, EVENT_AI_APPROVAL, EVENT_AI_STEP } from "./ai";
export { app } from "./app";
export type { MoveProgress } from "./app";
export { audit } from "./audit";
export { dialog } from "./dialog";
export { file, local, onOsFileDrop, zmodem } from "./file";
export { EVENT_FTPS_CERT, ftpsCerts } from "./ftpsCert";
export { history } from "./history";
export { EVENT_HOST_KEY, hostKeys } from "./hostKey";
export { EVENT_SSH_KBD, sshKbd } from "./kbd";
export { EVENT_LOCK_CHANGED, lock } from "./lock";
export { memory } from "./memory";
export { openExternal } from "./open";
export { policy } from "./policy";
export { proc } from "./process";
export { profiles } from "./profiles";
export { providers } from "./providers";
export { plugins } from "./plugin";
export { records } from "./record";
export type { RecordOverrides } from "./record";
export { EVENT_OUTPUT, EVENT_STATE, sessions } from "./sessions";
export { appearance, settings, terminal } from "./settings";
export { snippets } from "./snippet";
export { svc } from "./service";
export { sshKeys } from "./sshKey";
export { system } from "./system";
export {
  EVENT_PROGRESS as EVENT_TRANSFER_PROGRESS,
  EVENT_STATE as EVENT_TRANSFER_STATE,
  transfers,
} from "./transfer";
export { triggers } from "./trigger";
export { tunnels } from "./tunnel";
export { updater } from "./updater";
export type { UpdateInfo, Update } from "./updater";
export {
  createDetachedWindow,
  focusDetachedWindow,
  revealWindow,
  selfWindow,
  setWindowTheme,
} from "./window";
export type { DetachedWindowSpec, Rgba } from "./window";
