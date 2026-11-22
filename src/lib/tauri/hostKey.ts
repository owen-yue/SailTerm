// 主机密钥（TOFU）：确认请求事件 + 裁决 + 已信任列表。
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { HostKeyPrompt, HostKeyRecord } from "../../types";
import { call } from "./client";

/** 首次连接的主机密钥确认请求事件名。 */
export const EVENT_HOST_KEY = "host-key-unknown";

export const hostKeys = {
  /** 应答确认请求；返回 false 表示请求已失效（超时/重复应答）。 */
  respond: (requestId: string, accept: boolean) =>
    call<boolean>("host_key_respond", { requestId, accept }),
  list: () => call<HostKeyRecord[]>("host_key_list"),
  /** 移除某主机已记录的全部密钥（服务器重装后用），返回删除条数。 */
  forget: (host: string, port: number) => call<number>("host_key_forget", { host, port }),

  onPrompt: (cb: (p: HostKeyPrompt) => void): Promise<UnlistenFn> =>
    listen<HostKeyPrompt>(EVENT_HOST_KEY, (e) => cb(e.payload)),
};
