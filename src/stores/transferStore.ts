// 传输队列 store：任务列表 + 事件驱动的进度合并 + 冲突裁决队列。
//
// 提交命令立即返回，真正的搬运在后端 worker 里跑，所以列表的状态**完全由事件推进**：
// `transfer-progress` 只改字节数与速度，`transfer-state` 才改状态。任务行里只有路径与进度，
// 不含任何凭证。
import { transfers as api } from "../lib/tauri";
import { errCode, errMsg } from "../lib/tauri/client";
import { t as tr } from "../lib/i18n";
import type {
  Direction,
  OnConflict,
  SubmitOutcome,
  TaskRow,
  TaskStatus,
  TransferProgressEvent,
  TransferStateEvent,
  TransferPhaseEvent,
} from "../types";
import { createStore, isDetachedWindow } from "./store";

/** 队列里的一行：后端任务行 + 运行期速度。 */
export interface Task extends TaskRow {
  /** 字节/秒；任务不在运行时为 0。 */
  speed: number;
}

/** 一条待用户裁决的目标冲突（`ask` 策略下后端不入队，改把冲突抛回来）。 */
export interface Conflict {
  sessionId: string;
  direction: Direction;
  /** 上传=本地绝对路径；下载=远端绝对路径。 */
  src: string;
  /** 上传=远端目录；下载=本地目录。 */
  dstDir: string;
  /** 源文件大小，弹窗里与已存在的大小对比展示；未知时为 0。 */
  srcBytes: number;
  existingPath: string;
  existingBytes: number;
}

/** 提交一次传输所需的最小信息（尚无冲突详情）。 */
export type SubmitRequest = Omit<Conflict, "existingPath" | "existingBytes">;

interface TransferStoreState {
  /** 最新在前，与后端 `list_tasks` 的排序一致。 */
  tasks: Task[];
  conflicts: Conflict[];
  loading: boolean;
  error: string;
  /** 乐观占位计数：批量提交的外壳进入即 +1、退出 -1；>0 时底部传输条先出现，
   *  避免串行提交首个任务要等一次远端 stat 往返才建行、慢链路下数秒无反馈。 */
  preparing: number;
}

export const transferStore = createStore<TransferStoreState>({
  tasks: [],
  conflicts: [],
  loading: false,
  error: "",
  preparing: 0,
});

/** 刷新序号：晚发出的请求生效，早返回的旧响应直接丢弃（并发 refresh 防抖）。 */
let lastRefresh = 0;

/**
 * done/状态事件早于 submit 回执建行时（小文件 worker 在 <200ms 进度窗口内跑完）暂存于此，
 * 待 `upsert` 建行时回放；否则 `update` 因行不存在丢弃事件，任务永远停在 pending 0%。
 */
const earlyStates = new Map<string, TransferStateEvent>();

export const transferActions = {
  /** 拉取当前会话的任务并与本地行**合并**（见 `mergeList`）：整体替换会让进行中任务的进度条归零。
   * 只按会话取：旧会话的历史行不露出（重传它们会报「会话不存在或已关闭」）。 */
  async refresh(sessionId?: string | null): Promise<void> {
    const reqId = ++lastRefresh;
    transferStore.set({ loading: true, error: "" });
    try {
      const rows = await api.list(sessionId);
      if (reqId !== lastRefresh) return; // 已有更新的刷新请求，丢弃本次结果
      mergeList(rows, sessionId);
    } catch (e) {
      if (reqId !== lastRefresh) return;
      transferStore.set({ loading: false, error: errMsg(e) });
    }
  },

  /** 上传本地文件。默认策略 `ask`：撞上同名先弹窗问，不闷头覆盖。 */
  upload(sessionId: string, localPath: string, remoteDir: string, srcBytes = 0): Promise<boolean> {
    return submit({ sessionId, direction: "upload", src: localPath, dstDir: remoteDir, srcBytes }, "ask");
  },

  /** 下载远端文件到本地目录。 */
  download(sessionId: string, remotePath: string, localDir: string, srcBytes = 0): Promise<boolean> {
    return submit({ sessionId, direction: "download", src: remotePath, dstDir: localDir, srcBytes }, "ask");
  },

  /**
   * 递归上传目录。已存在的目标文件直接覆盖：整夹传输必须完整落地，
   * `skip` 会让第二次上传同一目录时全部跳过（表现为“只传上去一个/没反应”）。
   */
  async uploadDir(sessionId: string, localPath: string, remoteDir: string): Promise<number> {
    const n = await api.uploadDir(sessionId, localPath, remoteDir, "overwrite");
    await this.refresh(sessionId);
    return n;
  },

  /** 递归下载目录，策略同 `uploadDir`：目标已存在一律覆盖，保证整夹落地。 */
  async downloadDir(sessionId: string, remotePath: string, localDir: string): Promise<number> {
    const n = await api.downloadDir(sessionId, remotePath, localDir, "overwrite");
    await this.refresh(sessionId);
    return n;
  },

  /** 冲突弹窗里用户选了策略：先出队（防重复点击），再带策略原样重提交。 */
  async resolve(conflict: Conflict, choice: OnConflict): Promise<boolean> {
    dismiss(conflict);
    return submit(conflict, choice);
  },

  /** 批量裁决：把同一策略套到队列里全部冲突（含当前这条），对应弹框的「应用到其余冲突」勾选。
   *  点击瞬间快照队列逐个重提交；处理期间新到的冲突进下一轮弹窗，默认策略仍是 ask。 */
  async resolveAll(choice: OnConflict): Promise<void> {
    const queue = transferStore.get().conflicts.slice();
    for (const c of queue) {
      await this.resolve(c, choice);
    }
  },

  /** 中止：放弃队列里全部待裁决冲突（这些文件不再传）。
   *  已在传 / 已排队的任务不受影响——那是传输面板「全部取消」的职责。
   *  取代旧的逐条 `abandon`：它与「跳过」终点完全相同（后端 Ask 不挂起任务，不重提交就是不传），
   *  留着只是多一颗冗余按钮。 */
  abortAll(): void {
    transferStore.set({ conflicts: [] });
  },

  /** 取消当前范围内全部未完成任务。 */
  async cancelAll(sessionId?: string | null): Promise<void> {
    const { tasks } = transferStore.get();
    const live = tasks.filter(
      (t) => !isFinal(t.status) && (!sessionId || t.sessionId === sessionId),
    );
    for (const t of live) {
      try {
        upsert(await api.cancel(t.id));
      } catch (e) {
        // 幽灵行（后端已无此任务，如 zmodem 本地行/被删的 bundle 行）：摘掉本地视图，
        // 否则它既不在 DB、取消又必然报错，用户拿它没办法。按稳定错误码判定，
        // 不匹配中文文案（文案本地化/改写后这条唯一出路会静默失效）。
        if (errCode(e) === "E_TRANSFER_NOT_FOUND") this.drop(t.id);
      }
    }
  },

  async cancel(taskId: string): Promise<void> {
    try {
      upsert(await api.cancel(taskId));
    } catch (e) {
      // 同上：后端无此行时直接把本地幽灵行摘掉，给手动清理留一条出路
      if (errCode(e) === "E_TRANSFER_NOT_FOUND") this.drop(taskId);
      else transferStore.set({ error: errMsg(e) });
    }
  },

  async retry(taskId: string): Promise<void> {
    try {
      // 重传回执行是 pending：唯一允许把终态行重置回非终态的路径
      upsert(await api.retry(taskId), true);
    } catch (e) {
      transferStore.set({ error: errMsg(e) });
    }
  },

  /** 清掉当前会话已结束的行；进行中的后端会保留，这里同步摘掉本地视图。 */
  async clear(sessionId?: string | null): Promise<void> {
    try {
      await api.clear(sessionId);
      const { tasks } = transferStore.get();
      // 后端口径一致：只删「已结束」且（未指定会话或属于该会话）的行，其余一律保留
      const dropped = (t: Task) => isFinal(t.status) && (!sessionId || t.sessionId === sessionId);
      transferStore.set({ tasks: tasks.filter((t) => !dropped(t)) });
    } catch (e) {
      transferStore.set({ error: errMsg(e) });
    }
  },

  /** 处理 `transfer-progress`：只合并字节数与速度。 */
  applyProgress(evt: TransferProgressEvent): void {
    lastEventAt = Date.now();
    update(evt.taskId, (t) => ({
      ...t,
      doneBytes: evt.transferred,
      totalBytes: evt.total || t.totalBytes,
      speed: evt.speed,
    }));
  },

  /** 处理 `transfer-phase`：只改打包阶段（compress/transfer/extract），不动状态。 */
  applyPhase(evt: TransferPhaseEvent): void {
    lastEventAt = Date.now();
    update(evt.taskId, (t) => ({ ...t, phase: evt.phase }));
  },

  /** 处理 `transfer-state`：状态跃迁（含会话断开时后端代为取消）。 */
  applyState(evt: TransferStateEvent): void {
    lastEventAt = Date.now();
    const { tasks } = transferStore.get();
    if (!tasks.some((t) => t.id === evt.taskId)) {
      // 事件早于建行（done 抢在 submit 回执 upsert 之前）：先缓冲，upsert 落地时回放，
      // 不直接丢弃——否则这一行会永远停在 pending 0%（传完还挂着进行中）。
      const prev = earlyStates.get(evt.taskId);
      if (!prev || !isFinal(prev.status)) earlyStates.set(evt.taskId, evt);
      return;
    }
    update(evt.taskId, (t) => ({
      ...t,
      status: evt.status,
      doneBytes: evt.transferred,
      totalBytes: evt.total || t.totalBytes,
      error: evt.error,
      speed: 0,
    }));
  },

  /** 传输提交外壳进出时增减乐观占位计数（delta 为 +1/-1）。 */
  setPreparing(delta: number): void {
    transferStore.set((s) => ({ preparing: Math.max(0, s.preparing + delta) }));
  },

  /**
   * 用户发起一批新传输前调用：若该会话当前没有在传的行，清掉上一批遗留的「冻结起点」。
   *
   * 为什么需要：`aggregateProgress` 只在被调用时才会走 idle 分支去 `delete(batchStarts)`，而
   * `TransferStrip` 在 `live===0 && preparing===0` 时**提前 return null**、根本不调它 → 一批传完、
   * 传输条隐藏后，冻结起点跨批泄漏。于是重复下载同一个 800M 文件时，上一批已 done 的 800M 行
   * 被重新计入这一批的分子分母，进度条显示 1.6G。此处按提交时机（非渲染时机）确定性清理。
   * 仍有在传行则不动——那是同一批的延续（如冲突重提交），清了反而会把同批已完成文件踢出。
   */
  resetBatchIfIdle(sessionId: string): void {
    const { tasks } = transferStore.get();
    if (!tasks.some((t) => t.sessionId === sessionId && !isFinal(t.status))) {
      batchStarts.delete(sessionId);
    }
  },

  /** 终端内 rz/sz 的本地任务行（不经后端注册表）：start 时插入，`zmodem-done` 收尾。 */
  addLocal(row: TaskRow): void {
    transferStore.set({ tasks: [toTask(row), ...transferStore.get().tasks] });
  },

  /** 按会话更新终端内 ZMODEM 的本地行（`zmodem-file` 补文件名/大小）。 */
  updateLocal(sessionId: string, fn: (t: Task) => Task): void {
    const { tasks } = transferStore.get();
    const i = tasks.findIndex((t) => t.id.startsWith(`zm-${sessionId}-`));
    if (i < 0) return;
    const next = tasks.slice();
    next[i] = fn(tasks[i]);
    transferStore.set({ tasks: next });
  },

  /** 终端内 ZMODEM 本地行收尾（done/failed/canceled）。 */
  settleLocal(sessionId: string, status: TaskStatus, error: string | null): void {
    this.updateLocal(sessionId, (t) => ({
      ...t,
      status,
      error,
      speed: 0,
      updatedAt: Date.now(),
    }));
  },

  /** 从本地视图摘掉一行（打包行失败回退时后端已删行、不会广播终态，不显式摘就会当 stray 长期挂着 running）。 */
  drop(taskId: string): void {
    const { tasks } = transferStore.get();
    transferStore.set({ tasks: tasks.filter((t) => t.id !== taskId) });
  },

  clearError(): void {
    transferStore.set({ error: "" });
  },
};

/** 是否已到终态（不会再有进度）。 */
export function isFinal(status: TaskStatus): boolean {
  return status === "done" || status === "failed" || status === "canceled";
}

/** 进行中的任务数：底部面板标签上的角标。 */
export function activeCount(tasks: Task[]): number {
  return tasks.reduce((n, t) => (isFinal(t.status) ? n : n + 1), 0);
}

/**
 * 冻结的「批次起点」表（按 scope key）：一旦某范围出现活动任务，就把批次起点钉死在
 * 当时最早那条未完成任务的 createdAt，直到该范围全部落定才清除。
 *
 * 为何必须冻结而非每次现算：目录上传是「串行 stat 建行 + 并发搬运」，靠前的文件常常在
 * 靠后的文件还没建行之前就已传完。若批次起点随批次推进取「当前最早未完成任务的 createdAt」，
 * 它会不断向前漂移、越过那些早已完成文件的时刻，把它们从分子分母同时踢出——聚合条于是长期
 * 贴 0%、看着完全不动。钉死后同批已完成文件始终留在窗口里。
 */
const batchStarts = new Map<string, number>();

/**
 * 聚合「当前批次」传输进度（供传输条与传输面板共用，保证口径一致）。
 * - 批次起点冻结于本批首次出现活动任务时（见 `batchStarts`），done 行按 createdAt 划界：
 *   同批文件 createdAt 恒 >= 冻结起点故全部计入，上一批未清理的历史 done 行自然剔除；
 * - 分母剔除 failed/canceled（已终结且不会传完，留在分母会让条子永远卡在 <100%）；
 * - 分子 = done 按全量计 + pending/running 按已传字节计。条子随批次推进单调上升至 100%。
 * `scopeKey`：调用方传的都是单一会话（可再按方向）的子集，缺省以会话 id 作冻结键；
 * 子集口径不同（如仅 upload）的调用方须传独立 key，避免与全会话锚点互相覆盖。
 */
export function aggregateProgress(
  tasks: Task[],
  scopeKey?: string,
): { done: number; total: number; pct: number } {
  const key = scopeKey ?? tasks[0]?.sessionId ?? "";
  let minLive = Infinity;
  for (const t of tasks) {
    if (!isFinal(t.status) && t.createdAt < minLive) minLive = t.createdAt;
  }
  let start: number;
  if (Number.isFinite(minLive)) {
    // 首次观测到活动即钉死，之后不随批次推进漂移（取已存值优先）。
    start = batchStarts.get(key) ?? minLive;
    batchStarts.set(key, start);
  } else if (transferStore.get().preparing > 0) {
    // 串行提交外壳（submitBatch）还在逐个建行：靠前的已传完、靠后的还没建，这一刻「零活动行」
    // 是批内空窗、不是批次结束。必须保持冻结起点不动——否则下一个文件建行时会把起点漂到更晚的
    // createdAt，把先前已传完的文件（其 createdAt < 新起点）从分子分母同时踢出，进度条就从满跌回空，
    // 反复抖动（传很多文件时整体进度条一会儿满一会儿空）。此处不 delete、沿用已钉起点。
    start = batchStarts.get(key) ?? 0;
  } else {
    // 外壳已退出且无活动行 = 整批确实落定：解除冻结，下一批重新钉；无活动任务时退化窗口（调用方此时一般已隐藏进度条）。
    batchStarts.delete(key);
    start = 0;
  }
  let done = 0;
  let total = 0;
  for (const t of tasks) {
    if (t.status === "failed" || t.status === "canceled") continue;
    const inBatch = !isFinal(t.status) || t.createdAt >= start;
    if (!inBatch) continue;
    total += t.totalBytes || 0;
    done += t.status === "done" ? t.totalBytes || 0 : t.doneBytes || 0;
  }
  return { done, total, pct: total > 0 ? Math.floor((done / total) * 100) : 100 };
}

async function submit(req: SubmitRequest, onConflict: OnConflict): Promise<boolean> {
  try {
    const outcome =
      req.direction === "upload"
        ? await api.upload(req.sessionId, req.src, req.dstDir, onConflict)
        : await api.download(req.sessionId, req.src, req.dstDir, onConflict);
    return settle(outcome, req);
  } catch (e) {
    transferStore.set({ error: errMsg(e) });
    return false;
  }
}

function settle(outcome: SubmitOutcome, req: SubmitRequest): boolean {
  switch (outcome.kind) {
    case "queued":
      upsert(outcome.task);
      return true;
    case "skipped":
      // 用户此前已选「跳过」，静默即可
      return true;
    case "conflict":
      push({ ...req, existingPath: outcome.existingPath, existingBytes: outcome.existingBytes });
      return false;
  }
}

function toTask(row: TaskRow): Task {
  return { ...row, speed: 0 };
}

/**
 * 把某任务缓冲到的「早到状态事件」套用到行上并摘除缓冲。
 *
 * 目录/多文件上传时后端自己建行（`submit` 回 `Skipped` 不建行），前端只靠 `refresh()`
 * 列到行。若某个小文件恰好在 refresh 往返期间传完：它的 `done` 事件先到、行还没建 →
 * 暂存进 `earlyStates`；而 `mergeList` 拿到的 DB 快照仍是 `running 0%`（close 先写库后广播，
 * 快照取在写库之前）。此时若不回放缓冲，这行会永远停在 running 0%（再没有后续事件）。
 * `upsert` 建行时已回放，这里补上 `mergeList` 这条路径，两处共用一套口径。
 */
function replayEarly(task: Task): Task {
  const early = earlyStates.get(task.id);
  if (!early) return task;
  earlyStates.delete(task.id);
  return {
    ...task,
    status: early.status,
    doneBytes: early.transferred,
    totalBytes: early.total || task.totalBytes,
    error: early.error,
    speed: 0,
  };
}

/**
 * 用后端列表合并本地任务行（refresh 专用）。
 *
 * 后端只把终态与状态跃迁落库，**进度事件不落库**：整体替换会让进行中任务
 * 的 doneBytes 打回 0、状态闪回 pending，表现就像“切个页签又从头下载”。
 * 因此范围内以 DB 行为底、合并本地更靠前的进度与状态；
 * 范围外会话的行，以及刚提交、列表快照还没来得及包含的非终态行，原样保留。
 */
function mergeList(rows: TaskRow[], sessionId?: string | null): void {
  const { tasks } = transferStore.get();
  const scoped = !!sessionId;
  const hit = (t: Task) => !scoped || t.sessionId === sessionId;
  const inScope = tasks.filter(hit);
  const others = scoped ? tasks.filter((t) => !hit(t)) : [];
  const listed = new Set(rows.map((r) => r.id));
  const merged = rows.map((r) => replayEarly(mergeRow(r, inScope.find((t) => t.id === r.id))));
  // refresh 与 submit 的竞态：列表快照早于新任务入队时，不能把进行中的行丢掉；
  // 终端内 ZMODEM 的本地行（zm- 前缀）不进后端 DB，终态也保留，等用户手动清理。
  // 非终态、后端列表里又没有、且创建超 60s：多半是终态事件撞上页面重载/监听缝隙丢了——
  // 判死落 failed，否则底部传输条会永久挂着「上传中 0%」的幽灵行（后端实际早已传完）。
  const now = Date.now();
  const strays = inScope
    .filter((t) => !listed.has(t.id) && (!isFinal(t.status) || t.id.startsWith("zm-")))
    .map((t) =>
      !isFinal(t.status) && !t.id.startsWith("zm-") && now - t.createdAt > 60_000
        ? { ...t, status: "failed" as const, error: tr("tp.staleLost"), speed: 0 }
        : t,
    );
  transferStore.set({ tasks: [...merged, ...strays, ...others], loading: false });
}

/** DB 行与本地行合并：DB 终态为准；本地已终态而 DB 快照落后（终态落库前拍的）时保留本地行——
 *  done 被打回 pending 后终态事件已消费过、永不再来，会永久卡「进行中 0%」（小文件竞态实测踩中）。
 *  未结束时进度取较大值，running 不被 pending 覆盖。 */
function mergeRow(db: TaskRow, live?: Task): Task {
  if (!live || isFinal(db.status)) return toTask(db);
  if (isFinal(live.status)) return live;
  const running = live.status === "running" || db.status === "running";
  return {
    ...db,
    doneBytes: Math.max(live.doneBytes, db.doneBytes),
    status: running ? "running" : "pending",
    speed: running ? live.speed : 0,
  };
}

/** 状态合并：终态行重传/重试时重置为命令回执的状态；其余情况只前进不后退。 */
function preferStatus(prev: TaskStatus, next: TaskStatus): TaskStatus {
  if (isFinal(prev)) return next;
  if (prev === "running" && next === "pending") return prev;
  return next;
}

/** 新增（插到队首）或就地更新一行。`reset=true`（仅 retry 回执）允许把终态行重置回 pending 开新一轮。 */
function upsert(row: TaskRow, reset = false): void {
  const task = toTask(row);
  const { tasks } = transferStore.get();
  const i = tasks.findIndex((t) => t.id === task.id);
  if (i < 0) {
    // 建行前若已缓冲到状态事件（done 早于回行的竞态）：回放后再入列，避免闪现 pending 0%。
    const early = earlyStates.get(task.id);
    if (early) {
      earlyStates.delete(task.id);
      task.status = early.status;
      task.doneBytes = early.transferred;
      if (early.total) task.totalBytes = early.total;
      task.error = early.error;
      task.speed = 0;
    }
    transferStore.set({ tasks: [task, ...tasks] });
    return;
  }
  const prev = tasks[i];
  // 迟到的旧回执不得把终态行打回非终态：done 常抢在 submit 回执之前被消费（缓冲不会二次回放），
  // 行若被回执里的 pending 覆盖就永久卡「进行中 0%」。显式重传（reset）才允许重置。
  if (isFinal(prev.status) && !isFinal(task.status) && !reset) return;
  const status = preferStatus(prev.status, task.status);
  const next = tasks.slice();
  next[i] = {
    ...task,
    status,
    // 命令返回的行不带速度；非重传路径下保留本地已推进的进度与速度
    doneBytes: isFinal(prev.status) ? task.doneBytes : Math.max(task.doneBytes, prev.doneBytes),
    totalBytes: task.totalBytes || prev.totalBytes,
    speed: status === "running" ? prev.speed : 0,
  };
  transferStore.set({ tasks: next });
}

function update(taskId: string, fn: (t: Task) => Task): void {
  const { tasks } = transferStore.get();
  const i = tasks.findIndex((t) => t.id === taskId);
  if (i < 0) return; // 事件早于列表加载：refresh() 会把终态一并补齐
  const next = tasks.slice();
  next[i] = fn(tasks[i]);
  transferStore.set({ tasks: next });
}

function push(conflict: Conflict): void {
  const { conflicts } = transferStore.get();
  if (conflicts.some((c) => same(c, conflict))) return;
  transferStore.set({ conflicts: [...conflicts, conflict] });
}

function dismiss(conflict: Conflict): void {
  const { conflicts } = transferStore.get();
  transferStore.set({ conflicts: conflicts.filter((c) => !same(c, conflict)) });
}

function same(a: Conflict, b: Conflict): boolean {
  return a.direction === b.direction && a.src === b.src && a.dstDir === b.dstDir;
}

/** 最近一次传输事件时刻（正常传输中事件连续不断），供对账看门狗判「事件静默」。 */
let lastEventAt = Date.now();
/** 退避计数：自上次事件以来已经空转对账几轮（见下面看门狗的注释）。 */
let idleRounds = 0;
/** 上一轮退避基线：`lastEventAt` 与它不等就说明来了新事件，重新武装。 */
let armedAt = lastEventAt;

// 对账看门狗（彻底解决的最后一层）：存在非终态行、且事件静默超 30s 时，主动全量 refresh
// 一次（每 10s 检查）。后端 DB 是事实源：任何理论上可能的事件丢失/旧数据覆盖，即使没被
// 上面的不变量拦住，也会在 ≤60s 内自动收敛回真实状态，不依赖用户打开面板或手动刷新。
// zm- 前缀的终端 ZMODEM 行不在后端库里，排除在触发条件外，避免空转；window 标志位
// 防开发期 HMR 重复执行模块叠加多个定时器。
// 两个约束防「无界轮询」：① 只有主窗口跑——`refresh(null)` 是跨全部会话的 200 行查询，
// 弹出的每个 WebView 各挂一份就是 N 倍 IPC；② 同一静默期内最多对账 4 次后退避，直到
// 下一个事件重新武装——真卡死的行（后端 worker 已死）每 10s 拉一次全量表不会自愈，只会长跑。
if (!isDetachedWindow() && !(window as { __transferReconcile?: boolean }).__transferReconcile) {
  (window as { __transferReconcile?: boolean }).__transferReconcile = true;
  window.setInterval(() => {
    const { tasks } = transferStore.get();
    const live = tasks.filter((t) => !isFinal(t.status) && !t.id.startsWith("zm-"));
    if (live.length === 0 || Date.now() - lastEventAt < 30_000) return;
    if (lastEventAt !== armedAt) {
      armedAt = lastEventAt;
      idleRounds = 0;
    } else if (idleRounds >= 4) {
      return;
    }
    idleRounds++;
    void transferActions.refresh(null);
  }, 10_000);
}
