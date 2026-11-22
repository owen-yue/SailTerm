// FTPS 服务端证书（TOFU）：裁决请求事件 + 应答 + 已信任列表。
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { FtpsCertPrompt, FtpsCertRecord } from "../../types";
import { call } from "./client";

/** FTPS 证书未通过系统信任链校验时的裁决请求事件名。 */
export const EVENT_FTPS_CERT = "ftps-cert-unknown";

export const ftpsCerts = {
  /** 应答裁决请求；返回 false 表示请求已失效（超时/重复应答）。 */
  respond: (requestId: string, accept: boolean) =>
    call<boolean>("ftps_cert_respond", { requestId, accept }),
  list: () => call<FtpsCertRecord[]>("ftps_cert_list"),
  /** 移除某 host:port 的证书记录（服务器换证书 / 怀疑中间人后用），返回删除条数。 */
  forget: (host: string, port: number) => call<number>("ftps_cert_forget", { host, port }),

  onPrompt: (cb: (p: FtpsCertPrompt) => void): Promise<UnlistenFn> =>
    listen<FtpsCertPrompt>(EVENT_FTPS_CERT, (e) => cb(e.payload)),
};
