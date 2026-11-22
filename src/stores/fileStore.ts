// 文件浏览器 store：双栏「本地 | 远端」各自的目录与条目。
//
// 两栏共用同一份 `Pane` 形状，组件渲染逻辑不分叉。**远端栏按会话分片**
// （`remotes[sessionId]`）：文件视图现在是会话的主区内容，分屏下两个会话各自
// 浏览自己的目录、互不串台；本地栏则保持全局一份——它表示"我这台机器上的位置"，
// 与哪个会话无关，分片反而会让切标签时本地栏乱跳。
// 会话断开/关闭时由 App 调 `drop()` 丢掉该分片，避免用户对着已失效的数据操作。
// 这里只存路径与元数据，**不含任何凭证**。
import { file as remoteApi, local as localApi } from "../lib/tauri";
import { errMsg } from "../lib/tauri/client";
import type { FileEntry } from "../types";
import { createStore } from "./store";
import { t } from "../lib/i18n";
import { transferActions } from "./transferStore";

/** 面板的一侧。 */
export type Side = "local" | "remote";

/**
 * 一栏的定位。所有读写 action 都以 Lane 为准定位到自己那一栏，不再依赖"当前活动会话"这种隐式全局。
 */
export interface Lane {
  side: Side;
  /**
   * 本栏所在视图的会话。远端栏用它做**分片键**（决定读写哪个 Pane）；
   * 本地栏的状态虽全局共享，但上传目标与 zmodem 对端仍要落在"这一屏"的会话上，故同样带上。
   */
  session?: string;
}

/** 一栏的状态：当前目录 + 条目 + 选中项 + 加载/错误。 */
export interface Pane {
  /** 当前目录绝对路径；空串表示尚未加载过。 */
  path: string;
  /** 上级目录；null 表示已在根（禁用「返回上级」）。 */
  parent: string | null;
  entries: FileEntry[];
  loading: boolean;
  error: string;
  /**
   * 一次性成功/进展文案（如目录已排队）。与 `error` 分道：旧写法把「已排队 N 个」
   * 也塞进 error 槽，用户看到的是一条红色报错却什么事都没出错。
   */
  notice: string;
  /** 选中的条目路径（单选：右键菜单、上传/下载都以它为准）。 */
  selected: string;
  /** 多选集合（Ctrl/Shift）。 */
  selectedPaths: string[];
}

interface FileStoreState {
  local: Pane;
  /** sessionId → 该会话正在浏览的远端目录。 */
  remotes: Record<string, Pane>;
}

const emptyPane = (): Pane => ({
  path: "",
  parent: null,
  entries: [],
  loading: false,
  error: "",
  notice: "",
  selected: "",
  selectedPaths: [],
});

export const fileStore = createStore<FileStoreState>({
  local: emptyPane(),
  remotes: {},
});

/** 取一栏当前状态：本地栏全局一份（忽略 session），远端栏按 session 取分片；分片未建立时返回空栏。 */
export function paneOf(s: FileStoreState, lane: Lane): Pane {
  return lane.side === "local" ? s.local : (s.remotes[lane.session ?? ""] ?? emptyPane());
}

export const files = {
  /**
   * 确保某会话的远端栏可用：首次进入该会话的文件视图时调用。
   * 已有分片就保留它当前浏览的目录（切走再切回来不该回到根），只在从未列成功过时补拉一次。
   */
  async open(sessionId: string): Promise<void> {
    const lane: Lane = { side: "remote", session: sessionId };
    const exist = fileStore.get().remotes[sessionId];
    if (exist) {
      if (!exist.path && !exist.loading) await files.load(lane);
      return;
    }
    fileStore.set((prev) => ({ remotes: { ...prev.remotes, [sessionId]: emptyPane() } }));
    await files.load(lane);
  },

  /** 会话断开/关闭：整片丢弃，重连后从默认目录重新开始。 */
  drop(sessionId: string): void {
    const { remotes } = fileStore.get();
    if (!(sessionId in remotes)) return;
    const next = { ...remotes };
    delete next[sessionId];
    fileStore.set({ remotes: next });
  },

  /** 列目录；`path` 省略表示刷新当前目录（首次进入时后端会回落到主目录）。
   *  `refresh=true` 旁路后端目录缓存（PR-3）：用于传输完成回调与手动刷新按钮，
   *  写操作后的自动重列不用带（后端已主动 invalidate）。 */
  async load(lane: Lane, path?: string | null, refresh = false): Promise<void> {
    patch(lane, { loading: true, error: "", notice: "" });
    try {
      const listing =
        lane.side === "local"
          ? await localApi.list(resolve(lane, path))
          : await remoteApi.list(sessionOf(lane), resolve(lane, path) ?? ".", refresh);
      patch(lane, {
        path: listing.path,
        parent: listing.parent,
        entries: listing.entries,
        loading: false,
        selected: "",
        selectedPaths: [],
      });
    } catch (e) {
      patch(lane, { loading: false });
      flashError(lane, errMsg(e));
    }
  },

  /** 返回上级；已在根则什么也不做。 */
  async up(lane: Lane): Promise<void> {
    const { parent, path } = paneOf(fileStore.get(), lane);
    if (parent) {
      await files.load(lane, parent);
      return;
    }
    // 盘符/根没有 parent 时：本地回「此电脑」，远端回 `/`
    if (lane.side === "local") await files.load(lane, "此电脑");
    else if (path && path !== "/") await files.load(lane, "/");
  },

  /** 跳到根：本地「此电脑」，远端 `/`。 */
  async root(lane: Lane): Promise<void> {
    await files.load(lane, lane.side === "local" ? "此电脑" : "/");
  },

  /** 单击选中；`mode` 支持多选（ctrl 切换 / shift 区间）。 */
  select(lane: Lane, path: string, mode: "replace" | "toggle" | "range" = "replace"): void {
    const pane = paneOf(fileStore.get(), lane);
    if (mode === "replace") {
      patch(lane, { selected: path, selectedPaths: [path] });
      return;
    }
    if (mode === "toggle") {
      const set = new Set(pane.selectedPaths);
      if (set.has(path)) set.delete(path);
      else set.add(path);
      patch(lane, { selected: path, selectedPaths: [...set] });
      return;
    }
    // range：以 selected 为锚，entries 顺序切片
    const paths = pane.entries.map((e) => e.path);
    const anchor = pane.selected || path;
    const a = paths.indexOf(anchor);
    const b = paths.indexOf(path);
    if (a < 0 || b < 0) {
      patch(lane, { selected: path, selectedPaths: [path] });
      return;
    }
    const [lo, hi] = a <= b ? [a, b] : [b, a];
    const slice = paths.slice(lo, hi + 1);
    patch(lane, { selected: path, selectedPaths: slice });
  },

  /** 取消选中（点空白处）。 */
  unselect(lane: Lane): void {
    patch(lane, { selected: "", selectedPaths: [] });
  },

  /** 框选：按矩形命中的行（文档顺序）整体设选中；空命中即清空选择。 */
  selectBox(lane: Lane, paths: string[]): void {
    if (paths.length === 0) {
      patch(lane, { selected: "", selectedPaths: [] });
      return;
    }
    patch(lane, { selected: paths[paths.length - 1], selectedPaths: paths });
  },

  /** 新建目录。本地递归创建，远端要求父目录已存在（与后端一致）。 */
  async mkdir(lane: Lane, path: string): Promise<boolean> {
    return mutate(lane, () =>
      lane.side === "local" ? localApi.mkdir(path) : remoteApi.mkdir(sessionOf(lane), path),
    );
  },

  async remove(lane: Lane, entry: FileEntry): Promise<boolean> {
    return mutate(lane, () =>
      lane.side === "local"
        ? localApi.remove(entry.path, entry.isDir)
        : remoteApi.remove(sessionOf(lane), entry.path, entry.isDir),
    );
  },

  /** 批量删除当前多选项：逐个删完后只重列一次目录（避免每项各触发一次 load）。 */
  async removeSelected(lane: Lane): Promise<boolean> {
    const pane = paneOf(fileStore.get(), lane);
    const entries = pane.entries.filter((e) => pane.selectedPaths.includes(e.path));
    if (entries.length === 0) return false;
    return mutate(lane, async () => {
      for (const e of entries) {
        if (lane.side === "local") await localApi.remove(e.path, e.isDir);
        else await remoteApi.remove(sessionOf(lane), e.path, e.isDir);
      }
    });
  },

  /** 重命名：`toPath` 需为完整目标路径（用 `childPath` 拼）。 */
  async rename(lane: Lane, from: string, toPath: string): Promise<boolean> {
    return mutate(lane, () =>
      lane.side === "local"
        ? localApi.rename(from, toPath)
        : remoteApi.rename(sessionOf(lane), from, toPath),
    );
  },

  /** 改权限（仅远端有意义）。`mode` 是八进制字符串，如 `"644"`。 */
  async chmod(lane: Lane, path: string, mode: string): Promise<boolean> {
    return mutate(lane, () => remoteApi.chmod(sessionOf(lane), path, mode));
  },

  clearError(lane: Lane): void {
    flashError(lane, "");
  },
};

/** 在给定目录下拼出子项完整路径：远端固定 `/`，本地跟随该目录已有的分隔符。 */
export function childPath(dir: string, name: string, side: Side): string {
  if (side === "remote") {
    return dir === "/" ? `/${name}` : `${dir.replace(/\/+$/, "")}/${name}`;
  }
  const sep = dir.includes("/") && !dir.includes("\\") ? "/" : "\\";
  return `${dir.replace(/[\\/]+$/, "")}${sep}${name}`;
}

/**
 * 批量提交的统一外壳：所有提交入口（OS 拖入 `uploadPaths`、右键/GO/跨栏拖/双击 `transferSelected`）
 * 都经此，杜绝「一处加了反馈、另一处漏了」的体验漂移。固定做三件事：
 *  ①进入即 `setPreparing(1)`：串行提交的首个任务要等一次远端 stat 往返才落地，慢链路下会数秒
 *    无反馈，占位让底部传输条秒现身，真任务行随后无缝接管；
 *  ②逐个 await 串行提交（不并发）：并发提交会让多个 stat 同时砸同一 SFTP 通道，实测会「传一部分就卡住」；
 *  ③末尾统一 `refresh` 把整夹入队的任务捞进视图；退出（含异常）必撤占位。
 * 每个 thunk 返回真值表示该次提交成功；假值计入整体失败但不中断后续。
 */
async function submitBatch(session: string, submits: Array<() => Promise<unknown>>): Promise<boolean> {
  // 先按「该会话是否已空闲」清掉上一批遗留的冻结起点，再进占位：否则重复下载同一文件时，
  // 上一批已 done 的行会被算进这一批（进度条大小翻倍，见 transferStore.resetBatchIfIdle）。
  transferActions.resetBatchIfIdle(session);
  transferActions.setPreparing(1);
  let ok = true;
  try {
    for (const run of submits) {
      if (!(await run())) ok = false;
    }
    await transferActions.refresh(session);
  } finally {
    transferActions.setPreparing(-1);
  }
  return ok;
}

/** 批量传输：入口项若在多选里，传整组；否则只传它。右键上传/下载、GO 按钮、跨栏拖、双击文件都走这里。 */
export async function transferSelected(lane: Lane, entry: FileEntry): Promise<boolean> {
  const pane = paneOf(fileStore.get(), lane);
  const paths = pane.selectedPaths;
  const list =
    paths.length > 1 && paths.includes(entry.path)
      ? pane.entries.filter((e) => paths.includes(e.path))
      : [entry];
  return submitBatch(sessionOf(lane), list.map((e) => () => transferEntry(lane, e)));
}

export async function transferEntry(lane: Lane, entry: FileEntry): Promise<boolean> {
  const s = fileStore.get();
  // 无会话时软失败（挂文案）而非抛异常：双击文件等入口未必都经过按钮的 disabled 判定
  if (!lane.session) {
    flashError(lane, t("fso.noSessionTransfer"));
    return false;
  }
  const session = lane.session;
  // 目标目录取「对侧栏的当前目录」：本地栏→远端分片，远端栏→本地栏
  const other: Lane = { side: lane.side === "local" ? "remote" : "local", session };
  const dst = paneOf(s, other).path;
  flashError(lane, "");
  try {
    if (entry.isDir) {
      const n =
        lane.side === "local"
          ? await transferActions.uploadDir(session, entry.path, dst)
          : await transferActions.downloadDir(session, entry.path, dst);
      flashNotice(lane, t("fso.queued").replace("{n}", String(n)));
      return n > 0;
    }
    return lane.side === "local"
      ? await transferActions.upload(session, entry.path, dst, entry.size)
      : await transferActions.download(session, entry.path, dst, entry.size);
  } catch (e) {
    flashError(lane, errMsg(e));
    return false;
  }
}

/**
 * 从操作系统拖进来的本地文件（只有路径，没有元数据）：一律上传到该会话的远端当前目录。
 * 提交外壳（占位 + 串行 + 末尾 refresh）统一走 `submitBatch`，与 `transferSelected` 同一条体验。
 */
export async function uploadPaths(sessionId: string, paths: string[]): Promise<void> {
  const dst = paneOf(fileStore.get(), { side: "remote", session: sessionId }).path;
  await submitBatch(sessionId, paths.map((p) => () => transferActions.upload(sessionId, p, dst)));
}

/** 写操作的统一收尾：成功即重列当前目录，失败把文案挂到该栏。 */
async function mutate(lane: Lane, fn: () => Promise<void>): Promise<boolean> {
  flashError(lane, "");
  try {
    await fn();
    await files.load(lane);
    return true;
  } catch (e) {
    flashError(lane, errMsg(e));
    return false;
  }
}

/** 就地改一栏的状态。 */
function patch(lane: Lane, p: Partial<Pane>): void {
  fileStore.set((prev) => {
    if (lane.side === "local") return { local: { ...prev.local, ...p } };
    const id = lane.session ?? "";
    return {
      remotes: { ...prev.remotes, [id]: { ...(prev.remotes[id] ?? emptyPane()), ...p } },
    };
  });
}

/** 每栏错误提示的自动消失计时器：非阻断的一次性文案几秒后自行清除，不用用户点 ×。 */
const errTimers = new Map<string, number>();
function timerKey(lane: Lane): string {
  return lane.side === "local" ? "local" : lane.session ?? "";
}
function flashError(lane: Lane, msg: string): void {
  patch(lane, { error: msg });
  const k = timerKey(lane);
  window.clearTimeout(errTimers.get(k));
  if (msg) errTimers.set(k, window.setTimeout(() => patch(lane, { error: "" }), 4000));
}

/** 成功文案的自动消失计时器（与错误各自一份，互不踩脚）。 */
const noticeTimers = new Map<string, number>();
function flashNotice(lane: Lane, msg: string): void {
  patch(lane, { notice: msg });
  const k = timerKey(lane);
  window.clearTimeout(noticeTimers.get(k));
  if (msg) noticeTimers.set(k, window.setTimeout(() => patch(lane, { notice: "" }), 4000));
}

/** 目标目录：显式传入优先，否则刷新当前目录；两者都没有则交给后端回落主目录。 */
function resolve(lane: Lane, path?: string | null): string | null {
  if (path) return path;
  return paneOf(fileStore.get(), lane).path || null;
}

/** 远端栏操作的硬前置：没有会话就没有连接可用。 */
function sessionOf(lane: Lane): string {
  if (!lane.session) throw new Error(t("fso.noSession"));
  return lane.session;
}
