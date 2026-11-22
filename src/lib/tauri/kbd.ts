// SSH keyboard-interactive 认证：服务端提示事件 + 作答回传。
// 安全红线：答案只经 IPC 进入后端（入向凭证，同表单密码），前端 store 不留、事件不回显。
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { KbdPrompt } from "../../types";
import { call } from "./client";

/** 待作答的认证提示请求事件名。 */
export const EVENT_SSH_KBD = "ssh-kbd-request";

export const sshKbd = {
  /** 回传本轮答案（按提示顺序）；`null` = 用户取消（后端短路失败，不向服务端提交）。
   * 返回 false 表示请求已失效（超时/重复应答）。 */
  respond: (requestId: string, answers: string[] | null) =>
    call<boolean>("ssh_kbd_respond", { requestId, answers }),

  onPrompt: (cb: (p: KbdPrompt) => void): Promise<UnlistenFn> =>
    listen<KbdPrompt>(EVENT_SSH_KBD, (e) => cb(e.payload)),
};
