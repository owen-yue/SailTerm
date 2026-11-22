// 面板内部拖拽的数据约定（「拖拽到对侧即上传/下载」）。
//
// HTML5 `DataTransfer` 只能携带字符串，因此把「源栏 + 条目」序列化成 JSON 挂在自定义 MIME 上；
// 目标目录不在负载里——它由**落点那一栏的当前路径**决定，用户拖到哪就传到哪。
import type { Side } from "../stores/fileStore";
import type { FileEntry } from "../types";

/** 自定义 MIME：用来区分「我们拖的行」与普通文本拖拽。 */
export const DRAG_MIME = "application/x-sailterm-entry";

/** 拖拽负载。 */
export interface DragEntry {
  side: Side;
  /**
   * 来源栏所在视图的会话。文件视图按会话分片后，一个文档里可能并存多个远端栏（分屏），
   * 条目属于**它那一条连接**：落到另一个会话的本地栏时，不能改用落点的会话。
   */
  session?: string;
  entry: FileEntry;
}

export function setDragEntry(dt: DataTransfer, payload: DragEntry): void {
  dt.setData(DRAG_MIME, JSON.stringify(payload));
  dt.effectAllowed = "copy";
  // 部分 WebView 要求同时有 text/plain 才会真正启动拖拽，附一份路径兜底
  dt.setData("text/plain", payload.entry.path);
}

/** 读取拖拽负载；不是我们拖的行（例如用户拖了一段选中文本）则返回 null。 */
export function readDragEntry(dt: DataTransfer): DragEntry | null {
  const raw = dt.getData(DRAG_MIME);
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as DragEntry;
    return parsed?.entry?.path ? parsed : null;
  } catch {
    return null;
  }
}

/** dragover 阶段能否落下：此时读不到数据，只能看 types 里有没有我们的 MIME。 */
export function canAccept(dt: DataTransfer): boolean {
  return Array.from(dt.types).includes(DRAG_MIME);
}

/**
 * 正在拖的源栏。dragover 读不到负载内容，不记下源栏就会把**自己**也高亮成落点，
 * 拖回原栏松手又什么也不发生，白白误导用户。两栏同在一个文档里，模块变量就够。
 */
let dragSide: Side | null = null;

export function beginDrag(side: Side): void {
  dragSide = side;
}

/** `dragend` 时调：无论松手在面板内、面板外还是被 Esc 打断，都会触发。 */
export function endDrag(): void {
  dragSide = null;
}

export function isDragSource(side: Side): boolean {
  return dragSide === side;
}
