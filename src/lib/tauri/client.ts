// IPC 边界基座：invoke 收口 + 事件订阅收口 + 错误规范化 + base64 解码（`lib/tauri`）。
// 前端唯一出口：组件与 store 都不直接 import @tauri-apps/api。
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn as TauriUnlistenFn } from "@tauri-apps/api/event";

/** 退订函数：调用后取消对应事件监听。收编在此，业务层不见 Tauri 类型。 */
export type UnlistenFn = TauriUnlistenFn;

// 底层原语透传：仅供单测 vi.mocked 打桩取模块 identity（setup.ts mock 的就是这两个包，
// 经这里重导出仍是同一引用）。业务代码请继续用 call/on，勿从这里的出口直接 invoke/listen。
export { invoke, listen };

/**
 * 后端 `AppError` 的序列化形态：`{ code, message, key?, detail? }`。
 * - `code`：稳定错误码，供逻辑分支（如 `E_HOST_KEY`），不译；
 * - `key`：`err.*` 本地化归类键，据此查词典渲染当前语言的类别文案；
 * - `detail`：不可翻译的动态细节（原始技术串 / 端点自报文本），拼在归类之后；
 * - `message`：中文兜底整串（词典未命中 / 旧版时直接展示）。
 */
export interface AppErrorBody {
  code: string;
  message: string;
  key?: string;
  detail?: string | null;
}

/**
 * IPC 兜底超时（毫秒）：后端命令「永不 settle」（russh await 无超时 / keyring 停顿 /
 * 共享 DB 锁被卡死任务长期占用）时，前端 `busy`/`loading` 会永久挂住、弹窗卡死无法退出。
 * 这里给所有调用套一道墙钟兜底：到点即抛错，让调用方的 `finally` 复位忙态、UI 恢复可操作。
 * 值取得足够宽（60s），只在「真的卡死」时兜底，几乎不会误伤正常慢操作；
 * 本就长耗时的命令族走 `NO_TIMEOUT` 豁免（它们自带阶段事件 / 人工裁决超时 / 取消通道）。
 */
const IPC_TIMEOUT_MS = 60_000;

/** 豁免兜底超时的命令：长连接 / AI 编排 / 传输 / 网络工具 / 大文件 / 密钥生成等本就耗时或有界。 */
const NO_TIMEOUT: (string | RegExp)[] = [
  "ai_send_message",
  "ai_provider_test",
  "ai_provider_test_draft",
  "ai_diagnose_connect",
  "ai_probe_attachment",
  "session_connect",
  "session_connect_direct",
  "session_connect_local",
  "session_reconnect",
  /^transfer_(upload|download)/,
  /^zmodem_/,
  /^net_/,
  /^record_(start|stop)/,
  "monitor_sample",
  "system_info",
  // 只豁免真正长耗时的文件命令（大文件读写/编辑）；rename/remove/mkdir/chmod 这类
  // 秒级短命令不豁免：后端 tokio 定时器被运行时冻结拖死时，它们会永不 settle，
  // 弹窗永久卡死（实锤过：改名成功但弹窗挂死）。60s 墙钟兜底是最后的逃生口。
  /^file_(read|write|edit_preview|edit_commit|open_remote)/,
  /^ssh_key(gen)?_generate/,
  "profile_export_file",
  "profile_import_file",
  "theme_import_file",
  "data_dir_move",
];

function exemptFromTimeout(cmd: string): boolean {
  return NO_TIMEOUT.some((p) => (typeof p === "string" ? cmd.startsWith(p) : p.test(cmd)));
}

/** 超时错误体：与后端 AppError 同构，`errMsg` 能正常归类展示。 */
function timeoutBody(cmd: string): AppErrorBody {
  return {
    code: "E_IPC_TIMEOUT",
    key: "err.ipcTimeout",
    message: `操作超时未响应：${cmd}`,
    detail: null,
  };
}

/** 调用一个后端命令。默认带 60s 兜底超时（长耗时命令自动豁免），可显式覆盖。 */
export function call<T>(
  cmd: string,
  args?: Record<string, unknown>,
  opts?: { timeoutMs?: number },
): Promise<T> {
  const ms = opts?.timeoutMs ?? (exemptFromTimeout(cmd) ? 0 : IPC_TIMEOUT_MS);
  const p = invoke<T>(cmd, args);
  if (!ms) return p;
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(timeoutBody(cmd)), ms);
    p.then(
      (v) => {
        clearTimeout(timer);
        resolve(v);
      },
      (e) => {
        clearTimeout(timer);
        reject(e);
      },
    );
  });
}

/** 订阅后端推送事件（`call()` 的事件侧对等收口）：回调直接收 payload，返回退订函数。 */
export function on<T>(event: string, cb: (payload: T) => void): Promise<UnlistenFn> {
  return listen<T>(event, (e) => cb(e.payload));
}

/** 取错误码（无法识别时为 `E_UNKNOWN`），供差异化处理（如 `E_HOST_KEY`）。 */
export function errCode(e: unknown): string {
  return isErrBody(e) ? e.code : "E_UNKNOWN";
}

/** 可直接展示给用户的错误文案。 */
export function errMsg(e: unknown): string {
  if (isErrBody(e)) return errTranslate(e.key ?? e.code, e.message, e.detail ?? undefined);
  if (e instanceof Error) return e.message;
  return String(e);
}

/**
 * 错误本地化钩子：由 i18n 在启动时注入。`key` 是后端 `err.*` 归类键（命中则用当前语言渲染
 * 类别文案，并按 `{d}` 占位或「 · 」拼上不可翻译的 `detail`）；未命中词典时回落 `message`（中文兜底）。
 * 采用依赖注入而非 client 直接 import i18n，是为避开 `client ↔ i18n` 的模块循环引用。
 */
let errTranslate: (key: string, message: string, detail?: string) => string = (
  _key,
  message,
) => message;
export function setErrTranslator(
  fn: (key: string, message: string, detail?: string) => string,
): void {
  errTranslate = fn;
}

function isErrBody(e: unknown): e is AppErrorBody {
  return !!e && typeof e === "object" && "code" in e && "message" in e;
}

/** base64 → Uint8Array（WebView 无 Buffer，用 atob 手动解码）。 */
export function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
