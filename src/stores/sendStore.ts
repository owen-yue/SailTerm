// 发送 store：脚本条目与发送参数（全局一份，持久化）+ 在途发送的驱动。
//
// 宿主是独立工具窗口（`tools/views/SendToolView`），不是主窗口的停靠面板：
// 脚本一份、目标多台，内容不属任何一条连接；而停靠区的开合按会话标签记忆，
// 全局工具塞进去就得为它破例——所以让它住在自己的窗口里，dock 的规矩一条不改。
//
// 驱动放 store 层而不是组件里：面板失焦 / 列表刷新都不该打断在途脚本；
// 但窗口关闭即 JS 上下文销毁，在途发送随之停止（已确认的产品口径：直接停、不打扰）。
//
// 目标只有一个口径：勾选集合。原先的「所有会话 / 勾选」两档下拉已删——「所有会话」
// 与列表头的「全选」是同一件事做两次，两个入口只会让用户怀疑它们是不是同一个开关。
// 也没有「当前会话」档：发送器住在独立窗口，活动标签是主窗口的 UI 态，跨窗口去追它
// 既不可靠又危险（你以为发到手边那个，其实发到了另一个）。
//
// 安全红线：条目只写入用户自己的会话 PTY，不经 AI、不过策略引擎、不落盘（与手打同级）；
// 本 store 不含任何凭证，草稿就是用户自己写的命令文本。
import { sessions as api } from "../lib/tauri";
import { errMsg } from "../lib/tauri/client";
import { t } from "../lib/i18n";
import { kindHasTerminal } from "../lib/capabilities";
import {
  clampNum,
  MAX_COUNT,
  MIN_INTERVAL,
  parseEntries,
  type SendFormat,
  type SendSplit,
} from "../lib/sendScript";
import { sessionNameOf } from "../lib/sessionName";
import { createStore } from "./store";
import { sessionActions, sessionStore, type Tab } from "./sessionStore";

/** 计数档位里 `0` 的语义：无限重复（直到用户按停止）。 */
export const INFINITE = 0;

/** 一个发送目标的运行期状态。 */
export interface SendTargetState {
  id: string;
  /** 会话显示名快照（启动时取）：会话中途关闭后仍要能认出这一行是谁。 */
  label: string;
  /** 已成功发出的条目数。 */
  sent: number;
  /** 最后一条错误（空串 = 一路顺利）。 */
  error: string;
  /** 该目标是否已退出本轮（发完 / 会话没了 / 出错放弃）。 */
  done: boolean;
}

export interface SendState {
  // —— 发送参数与草稿（持久化，全局一份）——
  format: SendFormat;
  split: SendSplit;
  /** 条目末尾补 `\r`（发送后执行）；按字符档忽略此项。 */
  appendEnter: boolean;
  /** 重复轮数；`INFINITE`(0) = 无限。 */
  count: number;
  /** 条目间隔（秒）。 */
  interval: number;
  /**
   * 勾选的目标会话 id（空 = 不发）：目标只有这一个口径，「所有会话」靠列表头的全选按钮表达；
   * 存量配置里的旧 `target` 字段读回时直接丢弃（不猜意图：勾选集合自己已经说明白）。
   */
  picked: string[];
  text: string;

  // —— 运行态（不持久化）——
  running: boolean;
  /** 当前轮次（1 起；无限档下一直往上加）。 */
  round: number;
  /** 本轮已发出的条目数（跨目标取已完成条目数，用于「12/40」这类进度串）。 */
  entry: number;
  /** 本轮条目总数（解析结果长度）。 */
  total: number;
  targets: SendTargetState[];
  /** 启动即失败的错误文案（解析非法 / 没有目标）；运行中不置。 */
  error: string;
  /** 收尾拉的各会话终端近况（末几行），供结果区展示。 */
  tails: Record<string, string>;
}

const LS_KEY = "sailterm.send";

/** 收尾拉近况前的等待（毫秒）：给最后一条命令留出吐出输出的时间。 */
const TAIL_DELAY_MS = 400;
/** 近况每会话取的字节上限（与结果区展示量相称）。 */
const TAIL_BYTES = 2048;

interface SavedConfig {
  format: SendFormat;
  split: SendSplit;
  appendEnter: boolean;
  count: number;
  interval: number;
  picked: string[];
  text: string;
}

/** 读回存量配置：逐字段校验 + 夹区间，脏值一律回落默认（不整份丢弃，用户草稿最贵）。 */
function loadConfig(): SavedConfig {
  const base: SavedConfig = {
    format: "text",
    split: "line",
    appendEnter: true,
    count: 1,
    interval: 1,
    picked: [],
    text: "",
  };
  // 草稿是全局一份的共享资产：工具窗口与主窗口同一份 localStorage，这里刻意不避开
  // （独立窗口不回读的那条规矩适用于「按会话走」的 UI 态，不适用于全局工具的数据）。
  try {
    const v = JSON.parse(localStorage.getItem(LS_KEY) ?? "") as Partial<SavedConfig>;
    return {
      format: v.format === "hex" ? "hex" : "text",
      split: v.split === "pipe" || v.split === "char" ? v.split : "line",
      appendEnter: v.appendEnter !== false,
      count: clampNum(Number(v.count), INFINITE, MAX_COUNT, base.count),
      interval: clampNum(Number(v.interval), MIN_INTERVAL, 3600, base.interval),
      // 旧版这里有 `target` 档位（含更早的 `current`），现已整体废弃：只认勾选集合。
      picked: Array.isArray(v.picked) ? v.picked.filter((x) => typeof x === "string") : [],
      text: typeof v.text === "string" ? v.text : "",
    };
  } catch {
    return base;
  }
}

function persist(s: SendState): void {
  const cfg: SavedConfig = {
    format: s.format,
    split: s.split,
    appendEnter: s.appendEnter,
    count: s.count,
    interval: s.interval,
    picked: s.picked,
    text: s.text,
  };
  localStorage.setItem(LS_KEY, JSON.stringify(cfg));
}

const saved = loadConfig();

export const sendStore = createStore<SendState>({
  ...saved,
  running: false,
  round: 0,
  entry: 0,
  total: 0,
  targets: [],
  error: "",
  tails: {},
});

/** 可发送的会话：有 PTY 终端且已连接（FTP 系没有可按键的地方）。面板列候选目标同用此谓词。 */
export const canSend = (tab: Tab) => kindHasTerminal(tab.kind) && tab.state === "connected";

/** 按勾选集合解析出发送对象（不认识的 id 自然被过滤掉）；名字按当前语言与页签同口径快照一次。 */
function resolveTargets(s: SendState): SendTargetState[] {
  const tabs = sessionStore.get().tabs;
  const chosen = tabs.filter((x) => canSend(x) && s.picked.includes(x.id));
  // 带 #n：结果行要说「root@x #2 已发 3 条」，用户才能对上刚才勾选的是哪一页（与页签、列表同一口径）。
  return chosen.map((x) => ({ id: x.id, label: sessionNameOf(tabs, t, x), sent: 0, error: "", done: false }));
}

/**
 * 运行代次：`start` / `stop` 都递增它，在途循环在每个 await 之后比对
 * 「我还是不是当前那一代」，不是就收手——这样停止不必等 sleep 跑完，
 * 也不会有上一轮的尾巴写进新一轮的进度（与 AI 侧 turnSeq 作废同一套路）。
 */
let seq = 0;
/** 叫醒在途 sleep：停止要立即生效，不能干等剩下的间隔。 */
let wake: (() => void) | null = null;

function sleep(ms: number): Promise<void> {
  return new Promise((res) => {
    const done = () => {
      clearTimeout(timer);
      wake = null;
      res();
    };
    const timer = setTimeout(done, ms);
    wake = done;
  });
}

/** 收尾：给每个已发过的目标拉一段终端近况（失败静默，结果区少一段而已）。 */
async function pullTails(mySeq: number, targets: SendTargetState[]): Promise<void> {
  const alive = targets.filter((x) => x.sent > 0);
  if (alive.length === 0) return;
  await sleep(TAIL_DELAY_MS);
  if (mySeq !== seq) return; // 已被新一轮取代：别把旧近况盖进去
  const entries = await Promise.all(
    alive.map(async (x): Promise<[string, string]> => {
      try {
        return [x.id, await api.snapshot(x.id, TAIL_BYTES)];
      } catch {
        return [x.id, ""];
      }
    }),
  );
  if (mySeq !== seq) return;
  sendStore.set({ tails: Object.fromEntries(entries) });
}

/** 把某一代的进度落到 store（引用不变则整段跳过，省掉高频小帧的重渲染）。 */
function patchProgress(mySeq: number, entry: number, targets: SendTargetState[]): void {
  if (mySeq !== seq) return;
  sendStore.set({ entry, targets: targets.map((x) => ({ ...x })) });
}

/** 发送主循环：轮次 × 条目两层，条目内多目标并发写（与群发同口径，一个慢会话不拖住整波）。 */
async function runLoop(mySeq: number, entries: Uint8Array[], targets: SendTargetState[]): Promise<void> {
  // 参数在启动那一刻定死：跑到一半改间隔/轮次不该影响在途这一代（要变先停止再重跑）。
  const gapMs = Math.round(sendStore.get().interval * 1000);
  const rounds = sendStore.get().count;
  // 目标是否还在：会话被关掉 / 断线后不再往里灌（失败一次即放弃该目标，其余照发）。
  const isAlive = (id: string) =>
    sessionStore.get().tabs.some((x) => x.id === id && canSend(x));

  for (let round = 1; rounds === INFINITE || round <= rounds; round++) {
    if (mySeq !== seq) return;
    sendStore.set({ round });
    for (let i = 0; i < entries.length; i++) {
      const live = targets.filter((x) => !x.done);
      if (live.length === 0) break; // 所有目标都退出了本轮：没有收件人，继续跑没意义
      await Promise.all(
        live.map(async (x) => {
          if (!isAlive(x.id)) {
            x.done = true;
            x.error = t("sd.goneSession");
            return;
          }
          try {
            await api.writeBytes(x.id, entries[i]);
            x.sent++;
          } catch (e) {
            // 会话已没了（NoSuchSession）与写失败都只放弃这一个目标，不牵连其它机器。
            x.error = errMsg(e);
            x.done = true;
          }
        }),
      );
      if (mySeq !== seq) return; // 已被停止 / 新一轮取代：不再写进度
      patchProgress(mySeq, i + 1, targets);
      if (i < entries.length - 1 || rounds === INFINITE || round < rounds) await sleep(gapMs);
    }
    if (mySeq !== seq) return;
  }
  if (mySeq !== seq) return;
  sendStore.set({ running: false, targets: targets.map((x) => ({ ...x })) });
  await pullTails(mySeq, targets);
}

export const sendActions = {
  /** 改配置：运行态字段不接受外部写入（只能由 start/stop 驱动）。 */
  set(patch: Partial<Pick<SendState, keyof SavedConfig>>): void {
    sendStore.set((prev) => {
      const next = { ...prev, ...patch };
      // 落盘要夹：输入框中间态（清空计数 / 负间隔）不该写进存量，重启后自己变合法值。
      const clamped = {
        ...patch,
        count: clampNum(Number(next.count), INFINITE, MAX_COUNT, prev.count),
        interval: clampNum(Number(next.interval), MIN_INTERVAL, 3600, prev.interval),
      };
      persist({ ...next, ...clamped });
      return clamped;
    });
  },

  /** 勾选目标（增删一个会话）。 */
  togglePicked(id: string): void {
    const { picked } = sendStore.get();
    const next = picked.includes(id) ? picked.filter((x) => x !== id) : [...picked, id];
    sendActions.set({ picked: next });
  },

  /**
   * 整组勾选（列表的分组表头）：on = 把这组会话全加进勾选，off = 把这组全剔掉。
   * 只动这一组，其它组已有的勾选不受影响——「按分组全选」的全部语义。
   */
  setGroupPicked(ids: string[], on: boolean): void {
    const set = new Set(sendStore.get().picked);
    for (const id of ids) {
      if (on) set.add(id);
      else set.delete(id);
    }
    sendActions.set({ picked: [...set] });
  },

  /**
   * 拉一次后端会话表（工具窗口由「进窗口 / 拿到焦点 / session-state 跃迁」三个时机自动调）。
   * 顺手把已不存在的会话从勾选里剔掉：不然它们会永远堆在存量里（解析时会被过滤掉，
   * 但勾选框该如实反映「现在有哪些」）。
   */
  async refresh(): Promise<void> {
    await sessionActions.sync();
    const alive = new Set(sessionStore.get().tabs.filter(canSend).map((x) => x.id));
    const { picked } = sendStore.get();
    const next = picked.filter((id) => alive.has(id));
    if (next.length !== picked.length) sendActions.set({ picked: next });
  },

  /**
   * 开始发送。解析失败 / 没有可用目标时只落错误、不启动（按钮旁就地显示，不弹 toast）。
   * 已在跑则忽略：重复 start 会开出第二代循环，两代同时往同一批会话灌字节。
   * 启动前先 sync 一次会话表：目标以按下那一刻的后端实况为准，不能拿窗口开时的快照。
   */
  async start(): Promise<void> {
    const s0 = sendStore.get();
    if (s0.running) return;
    const plan = parseEntries(s0.text, s0.format, s0.split, s0.appendEnter);
    // 没内容就不发：不报错也不弹任何东西（按钮本已置灰，真走到这儿只可能是刚清空内容的竞态）。
    if (plan.entries.length === 0) return;
    if (plan.error) {
      sendStore.set({
        error: plan.error.at === undefined ? t(plan.error.key) : `${t(plan.error.key)} (${plan.error.at + 1})`,
      });
      return;
    }
    try {
      await sessionActions.sync();
    } catch {
      // sync 失败不阻断：沿用窗口内现有快照——列表已显在用户眼前，看到什么就发什么，
      // 不该因为拉不到权威值就把一整轮脚本吞掉。
    }
    const s = sendStore.get();
    const targets = resolveTargets(s);
    if (targets.length === 0) {
      sendStore.set({ error: t("sd.errNoTarget") });
      return;
    }
    const mySeq = ++seq;
    sendStore.set({
      running: true,
      round: 0,
      entry: 0,
      total: plan.entries.length,
      targets,
      error: "",
      tails: {},
    });
    void runLoop(mySeq, plan.entries, targets);
  },

  /**
   * 停止：作废在途代次并立即叫醒 sleep。
   * 收尾的近况由这里拉（循环那边代次已失效，不会再动 store），让用户停在哪儿就看到哪儿。
   */
  stop(): void {
    if (!sendStore.get().running) return;
    const mySeq = ++seq;
    const targets = sendStore.get().targets.map((x) => ({ ...x, done: true }));
    sendStore.set({ running: false, targets });
    wake?.();
    void pullTails(mySeq, targets);
  },
};
