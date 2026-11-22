// 会话录制：开始/停止、列表、回放、删除。落盘内容已由后端脱敏。
import type { RecordingInfo } from "../../types";
import { call } from "./client";

/** 手动录制的临时覆盖：仅对有值的字段下发，其余留 null 交后端回退档案/默认。 */
export interface RecordOverrides {
  filePattern?: string | null;
  overwrite?: boolean;
  timestampPrefix?: boolean;
}

export const records = {
  start: (sessionId: string, dir?: string | null, opts?: RecordOverrides) =>
    call<string>("record_start", {
      sessionId,
      dir: dir ?? null,
      filePattern: opts?.filePattern || null,
      overwrite: opts?.overwrite ?? null,
      timestampPrefix: opts?.timestampPrefix ?? null,
    }),
  stop: (sessionId: string) => call<string>("record_stop", { sessionId }),
  status: (sessionId: string) => call<boolean>("record_status", { sessionId }),
  /** 全局录制库：跨目录聚合（默认目录 + 各档案自定义目录 + 索引），无参。*/
  list: () => call<RecordingInfo[]>("record_list", {}),
  read: (name: string, dir?: string | null) => call<string>("record_read", { name, dir: dir ?? null }),
  delete: (name: string, dir?: string | null) => call<void>("record_delete", { name, dir: dir ?? null }),
  /** 在系统文件管理器中打开录制存放目录（dir 为空 = 默认 recordings/）。 */
  openDir: (dir?: string | null) => call<void>("record_open_dir", { dir: dir ?? null }),
};
