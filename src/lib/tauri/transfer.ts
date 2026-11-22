// 传输队列：提交 / 取消 / 重试 / 列表 / 清理，进度与终态走事件。
//
// 命令**立即返回**，分块搬运在后端 worker 协程里跑；因此 UI 的进度条完全由
// `transfer-progress`（200ms 节流）与 `transfer-state`（状态跃迁）两个事件驱动。
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  OnConflict,
  SubmitOutcome,
  TaskRow,
  TransferProgressEvent,
  TransferStateEvent,
  TransferPhaseEvent,
  BundleFallbackEvent,
} from "../../types";
import { call } from "./client";

/** 进度事件名（负载 `{ taskId, transferred, total, speed }`）。 */
export const EVENT_PROGRESS = "transfer-progress";
/** 状态事件名（负载 `{ taskId, status, transferred, total, error }`）。 */
export const EVENT_STATE = "transfer-state";
/** 打包阶段事件名（负载 `{ taskId, phase }`）。 */
export const EVENT_PHASE = "transfer-phase";
/** 打包失败回退事件名（负载 `{ taskId, reason }`）。 */
export const EVENT_FALLBACK = "bundle-fallback";

export const transfers = {
  /**
   * 上传本地文件到远端目录；`remoteDir` 省略时用远端当前目录。
   * 目标已存在且策略为 `ask` 时返回 `{ kind: "conflict" }`，任务不入队。
   */
  upload: (
    sessionId: string,
    localPath: string,
    remoteDir?: string | null,
    onConflict?: OnConflict | null,
  ) =>
    call<SubmitOutcome>("transfer_upload", {
      sessionId,
      localPath,
      remoteDir: remoteDir ?? null,
      onConflict: onConflict ?? null,
    }),

  /** 下载远端文件到本地目录；`localDir` 省略时用用户主目录。 */
  download: (
    sessionId: string,
    remotePath: string,
    localDir?: string | null,
    onConflict?: OnConflict | null,
  ) =>
    call<SubmitOutcome>("transfer_download", {
      sessionId,
      remotePath,
      localDir: localDir ?? null,
      onConflict: onConflict ?? null,
    }),

  /** 递归上传目录，返回入队文件数。 */
  uploadDir: (
    sessionId: string,
    localPath: string,
    remoteDir?: string | null,
    onConflict?: OnConflict | null,
  ) =>
    call<number>("transfer_upload_dir", {
      sessionId,
      localPath,
      remoteDir: remoteDir ?? null,
      onConflict: onConflict ?? null,
    }),
  /** 递归下载目录，返回入队文件数。 */
  downloadDir: (
    sessionId: string,
    remotePath: string,
    localDir?: string | null,
    onConflict?: OnConflict | null,
  ) =>
    call<number>("transfer_download_dir", {
      sessionId,
      remotePath,
      localDir: localDir ?? null,
      onConflict: onConflict ?? null,
    }),

  /** 请求取消：活动任务置标志位，返回取消后的任务行（可就地更新，不必等事件）。 */
  cancel: (taskId: string) => call<TaskRow>("transfer_cancel", { taskId }),
  /** 失败/取消的任务从已传字节续传；已完成的会重头再传一次。 */
  retry: (taskId: string) => call<TaskRow>("transfer_retry", { taskId }),
  /** 任务列表（最新在前）。为空则跨全部会话；面板日常只传当前会话 id。 */
  list: (sessionId?: string | null, profileId?: string | null, limit?: number | null) =>
    call<TaskRow[]>("transfer_list", {
      sessionId: sessionId ?? null,
      profileId: profileId ?? null,
      limit: limit ?? null,
    }),
  /** 清掉当前会话已结束的任务行，进行中的保留。返回删除条数。 */
  clear: (sessionId?: string | null) =>
    call<number>("transfer_clear", { sessionId: sessionId ?? null }),

  onProgress: (cb: (p: TransferProgressEvent) => void): Promise<UnlistenFn> =>
    listen<TransferProgressEvent>(EVENT_PROGRESS, (e) => cb(e.payload)),
  onState: (cb: (p: TransferStateEvent) => void): Promise<UnlistenFn> =>
    listen<TransferStateEvent>(EVENT_STATE, (e) => cb(e.payload)),
  /** 打包阶段跃迁（compress/transfer/extract）。 */
  onPhase: (cb: (p: TransferPhaseEvent) => void): Promise<UnlistenFn> =>
    listen<TransferPhaseEvent>(EVENT_PHASE, (e) => cb(e.payload)),
  /** 打包失败已回退逐文件（携原因）。 */
  onFallback: (cb: (p: BundleFallbackEvent) => void): Promise<UnlistenFn> =>
    listen<BundleFallbackEvent>(EVENT_FALLBACK, (e) => cb(e.payload)),
  /** 小文件打包传输总开关（目录上传命中阈值时先 tar 再传单包）。 */
  bundleGet: () => call<boolean>("transfer_bundle_get", {}),
  bundleSet: (enabled: boolean) =>
    call<boolean>("transfer_bundle_set", { enabled }),
};
