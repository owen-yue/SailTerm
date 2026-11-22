// 文件浏览与管理：远端走 SFTP，本地走文件系统。
//
// 远端命令**复用已认证的 SSH 连接**开子系统通道，全程不再索取凭证。
// 本地命令的 path 只能来自用户在本机 UI 的选择/输入，不得把远端或 AI 产出的路径直接喂进来。
import type { UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { FileText, Listing } from "../../types";
import { call } from "./client";

/** 远端（SFTP）。`sessionId` 必须是处于连接态的会话，断线后需先重连。 */
export const file = {
  /** 用本机编辑器打开远程文件，改完自动写回（返回临时路径）。 */
  /** 写回前 diff 预览（- 旧远端 / + 新本地）。 */
  editPreview: (sessionId: string | null, path: string, local: string) =>
    call<{ diff: string[]; remoteBytes: number; localBytes: number }>("file_edit_preview", {
      sessionId,
      path,
      local,
    }),
  editExternal: (sessionId: string | null, path: string, editor?: string | null) =>
    call<string>("file_edit_external", {
      sessionId,
      path,
      editor: editor ?? null,
    }),
  /** 下载远端非文本文件到临时副本并用系统默认程序打开（图片/PDF/压缩包等，不回写）。 */
  openRemote: (sessionId: string, path: string) =>
    call<string>("file_open_remote", { sessionId, path }),
  /** 用户确认后，把外部编辑器的临时文件写回服务器（配合 `file-edit-dirty` 事件）。 */
  commit: (sessionId: string | null, path: string, local: string) =>
    call<void>("file_edit_commit", { sessionId, path, local }),
  /** 远端起始目录（SFTP 子系统默认落在用户家目录）。 */
  cwd: (sessionId: string) => call<string>("file_cwd", { sessionId }),
  /** 列目录。后端自带 12s 内部上限 + 通道自愈；前端兜底收紧到 25s（默认 60s 太久，
   *  目录栏会盯着「正在读取目录…」一分钟才见错误），后端正常路径远小于此，不误伤。 */
  list: (sessionId: string, path: string, refresh = false) =>
    call<Listing>("file_list", { sessionId, path, refresh: refresh ? true : null }, { timeoutMs: 25_000 }),
  mkdir: (sessionId: string, path: string) => call<void>("file_mkdir", { sessionId, path }),
  remove: (sessionId: string, path: string, isDir: boolean) =>
    call<void>("file_remove", { sessionId, path, isDir }),
  rename: (sessionId: string, from: string, to: string) =>
    call<void>("file_rename", { sessionId, from, to }),
  /** `mode` 是八进制字符串（如 `"644"`、`"0755"`），不是数字。 */
  chmod: (sessionId: string, path: string, mode: string) =>
    call<void>("file_chmod", { sessionId, path, mode }),
  /** 读取文本文件（编辑器）；二进制/过大由后端拒绝。*/
  read: (sessionId: string, path: string) => call<FileText>("file_read", { sessionId, path }),
  /** 覆盖写回远程文件。*/
  write: (sessionId: string, path: string, content: string) =>
    call<void>("file_write", { sessionId, path, content }),
};

/** 本地文件系统。 */
export const local = {
  /** `path` 省略时用用户主目录。前端兜底 25s：本地列目录走 spawn_blocking 通常毫秒级，
   *  只在慢盘/网络挂载上可能久挂；宁可报「操作超时」让用户重试，也不让栏位永久转圈。 */
  list: (path?: string | null) =>
    call<Listing>("local_list", { path: path ?? null }, { timeoutMs: 25_000 }),
  /** 递归创建（等价 `mkdir -p`）。 */
  mkdir: (path: string) => call<void>("local_mkdir", { path }),
  remove: (path: string, isDir: boolean) => call<void>("local_remove", { path, isDir }),
  rename: (from: string, to: string) => call<void>("local_rename", { from, to }),
  read: (path: string) => call<FileText>("local_read", { path }),
  write: (path: string, content: string) => call<void>("local_write", { path, content }),
  /** 另存为导出：把整段文本写到用户选定路径（解除编辑器 512KB 上限，供终端导出等）。 */
  saveText: (path: string, content: string) => call<void>("local_save_text", { path, content }),
  /** 在系统文件管理器中打开该本地路径（目录直接打开、文件定位选中）；走后端不受 opener 前端 scope 限制。 */
  reveal: (path: string) => call<void>("reveal_local_path", { path }),
  /** 用系统默认程序打开本地文件/目录（图片→看图器、PDF→阅读器…）。 */
  open: (path: string) => call<void>("open_local_path", { path }),
};

/** ZMODEM rz/sz：远端需安装 lrzsz。*/
export const zmodem = {
  probe: (sessionId: string) => call<boolean>("zmodem_probe", { sessionId }),
  download: (sessionId: string, remotePath: string, localDir: string) =>
    call<{ files: string[] }>("zmodem_download", { sessionId, remotePath, localDir }),
  upload: (sessionId: string, localPath: string, remoteDir: string) =>
    call<void>("zmodem_upload", { sessionId, localPath, remoteDir }),
  /** 终端内 rz/sz：用户在原生文件框里选好后通知后端接管传输。 */
  start: (sessionId: string, localPath: string) =>
    call<void>("zmodem_terminal_start", { sessionId, localPath }),
  /** 用户取消选择：让远端 rz/sz 放弃（发 CAN 恢复终端）。 */
  cancel: (sessionId: string) => call<void>("zmodem_terminal_cancel", { sessionId }),
  /** 弹非模态系统文件框（不设父窗口，主窗口不禁用）：返回选中路径，取消为 null。
   * `title` 为前端按界面语言本地化的对话框标题（走 i18n 字典），缺省时后端回落英文。 */
  pick: (direction: "upload" | "download", title?: string) =>
    call<string | null>("zmodem_pick", { direction, title: title ?? null }),
};

/**
 * 订阅「从操作系统拖文件进窗口」：回调给出本地绝对路径与落点（CSS 像素）。
 *
 * 与 HTML5 拖拽**并存**：面板内部的行间拖拽走 DOM 事件，外部文件走这里。
 * 落点用于命中测试（判断用户把文件丢在了哪一栏）；后端给的是物理像素，
 * 需除以 `devicePixelRatio` 才能喂给 `document.elementFromPoint`。
 */
export function onOsFileDrop(
  cb: (paths: string[], at: { x: number; y: number }) => void,
): Promise<UnlistenFn> {
  return getCurrentWebview().onDragDropEvent((e) => {
    const p = e.payload;
    if (p.type !== "drop") return;
    const ratio = window.devicePixelRatio || 1;
    cb(p.paths, { x: p.position.x / ratio, y: p.position.y / ratio });
  });
}
