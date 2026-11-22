// 快捷命令栏（QuickBar）关联层：
//   1) 不动 Snippet 表——只记录「哪些 snippetId 进了哪个池 / 顺序」；
//   2) 三个分类池：`global`（所有会话可见）· `group:<groupId>`（组内会话可见）·
//      profileId / `session:<id>`（本连接快捷栏实际内容 = 池里勾选进来的 + 本连接私有新建的）；
//   3) 折叠态按 profile 记忆，localStorage 持久化，无后端。
// 安全红线：本 store 只存 id 引用与顺序，绝不缓存 snippet.content（避免明文模板/变量值残留）。
import { createStore, isDetachedWindow } from "./store";
import type { SessionDto } from "../types";

/** 关联表：profileKey -> 有序 snippetId 列表（列表内顺序即 chip 显示顺序）。 */
export type QuickBarMap = Record<string, string[]>;

const LS_KEY = "sailterm.quickBar";

/** 全局池固定键：里面的命令在所有会话的 ＋ 面板可见。 */
export const QUICKBAR_GLOBAL_KEY = "global";

interface QuickBarState {
  byProfile: QuickBarMap;
  /** 折叠偏好按 profile 记忆：true/false = 用户显式选择；undefined = 无偏好，
      由 QuickBar 按栏是否为空派生（空栏默认收起、非空默认展开）。 */
  collapsed: Record<string, boolean>;
}

/** 独立窗口不共享持久化，起手空表；主窗口从 localStorage 恢复。 */
function loadInitial(): QuickBarState {
  if (isDetachedWindow()) return { byProfile: {}, collapsed: {} };
  try {
    const raw = localStorage.getItem(LS_KEY);
    if (!raw) return { byProfile: {}, collapsed: {} };
    const parsed = JSON.parse(raw) as Partial<QuickBarState>;
    return {
      byProfile: parsed.byProfile && typeof parsed.byProfile === "object" ? parsed.byProfile : {},
      collapsed: parsed.collapsed && typeof parsed.collapsed === "object" ? parsed.collapsed : {},
    };
  } catch {
    return { byProfile: {}, collapsed: {} };
  }
}

export const quickBarStore = createStore<QuickBarState>(loadInitial());

function persist(): void {
  if (isDetachedWindow()) return;
  try {
    localStorage.setItem(LS_KEY, JSON.stringify(quickBarStore.get()));
  } catch {
    // 存储写满 / disabled 忽略，不影响 UI
  }
}

/**
 * 会话 → profileKey：有档案走 profileId；否则以 `session:<id>` 兜底。
 * 兜底键只在会话有效期内可见（会话关闭也不主动清理，localStorage 里会随时间累积陈旧键，
 * 但成本极低——每项只是一个 id 数组；不做 LRU 是因为一期不想引入淘汰复杂度）。
 */
export function profileKeyOf(tab: SessionDto | null | undefined): string {
  if (!tab) return "";
  return tab.profileId ?? `session:${tab.id}`;
}

/** 分组 → 组默认键：无组返回空串（无继承源）。 */
export function groupKeyOf(groupId: string | null | undefined): string {
  return groupId ? `group:${groupId}` : "";
}

/**
 * 本连接私有池键：与「栏实际内容」(profileKey) 区分开——
 *   池 `conn:<profileKey>` = 归属本连接的所有命令（管理列表）；
 *   栏  `profileKey`      = 池里勾选上栏的部分（底部 chip）。
 * 这样取消勾选只是把命令移出底部栏，命令仍留在本连接池里可再勾回来（修复“一点就消失”）。
 */
export function connPoolKeyOf(profileKey: string): string {
  return profileKey ? `conn:${profileKey}` : "";
}

/**
 * 写回某 key 的有序列表：空列表直接删键。读取侧一律 `byProfile[key] ?? []`，空数组无语义；
 * 留着只会在 localStorage 里累积陈旧空键（`session:<id>` 兜底键尤多）。
 */
function withIds(prev: QuickBarState, key: string, next: string[]): QuickBarMap {
  const byProfile = { ...prev.byProfile };
  if (next.length === 0) delete byProfile[key];
  else byProfile[key] = next;
  return byProfile;
}

export const quickBarActions = {
  /** 当前 profile 已上栏的 id 有序列表（只读，未定义即空）。 */
  listFor(key: string): string[] {
    return quickBarStore.get().byProfile[key] ?? [];
  },

  /** 追加一条到末尾（已存在则忽略）——一期不做拖拽排序，先按添加顺序显示。 */
  addSnippet(key: string, snippetId: string): void {
    if (!key || !snippetId) return;
    quickBarStore.set((prev) => {
      const cur = prev.byProfile[key] ?? [];
      if (cur.includes(snippetId)) return {};
      return { byProfile: { ...prev.byProfile, [key]: [...cur, snippetId] } };
    });
    persist();
  },

  /** 从快捷栏/池移除（不删片段本体）；移空即删键。 */
  removeSnippet(key: string, snippetId: string): void {
    quickBarStore.set((prev) => {
      const cur = prev.byProfile[key];
      if (!cur) return {};
      const next = cur.filter((id) => id !== snippetId);
      if (next.length === cur.length) return {};
      return { byProfile: withIds(prev, key, next) };
    });
    persist();
  },

  /** 批量追加（一键应用）：跳过已存在的 id，一次 set 一次持久化。 */
  addMany(key: string, snippetIds: string[]): void {
    if (!key || snippetIds.length === 0) return;
    quickBarStore.set((prev) => {
      const cur = prev.byProfile[key] ?? [];
      const add = snippetIds.filter((id) => !cur.includes(id));
      if (add.length === 0) return {};
      return { byProfile: { ...prev.byProfile, [key]: [...cur, ...add] } };
    });
    persist();
  },

  /** 批量移出（一键取消）：从该 key 移除给定 id 集合，一次 set 一次持久化；移空即删键。 */
  removeMany(key: string, snippetIds: string[]): void {
    if (!key || snippetIds.length === 0) return;
    quickBarStore.set((prev) => {
      const cur = prev.byProfile[key];
      if (!cur) return {};
      const drop = new Set(snippetIds);
      const next = cur.filter((id) => !drop.has(id));
      if (next.length === cur.length) return {};
      return { byProfile: withIds(prev, key, next) };
    });
    persist();
  },

  /** 从 i 移到 j（chip 拖拽排序用；一期不启用亦可保留）。 */
  reorder(key: string, from: number, to: number): void {
    quickBarStore.set((prev) => {
      const cur = prev.byProfile[key];
      if (!cur || from < 0 || to < 0 || from >= cur.length || to >= cur.length || from === to) return {};
      const arr = [...cur];
      const [x] = arr.splice(from, 1);
      arr.splice(to, 0, x);
      return { byProfile: { ...prev.byProfile, [key]: arr } };
    });
    persist();
  },

  /** 显式写入折叠态：目标值由组件按「有效折叠态」算好传入——
      空栏默认收起是派生态（表里 undefined），不能在此盲翻 !prev 值。 */
  setCollapsed(key: string, on: boolean): void {
    if (!key) return;
    quickBarStore.set((prev) => ({
      collapsed: { ...prev.collapsed, [key]: on },
    }));
    persist();
  },

  /** 清理会话结束后的兜底键（`session:<id>`），避免长期累积；有 profileId 的键保留。 */
  forgetSession(sessionId: string): void {
    const key = `session:${sessionId}`;
    quickBarStore.set((prev) => {
      const byProfile = { ...prev.byProfile };
      const collapsed = { ...prev.collapsed };
      let dirty = false;
      if (key in byProfile) {
        delete byProfile[key];
        dirty = true;
      }
      if (key in collapsed) {
        delete collapsed[key];
        dirty = true;
      }
      return dirty ? { byProfile, collapsed } : {};
    });
    persist();
  },
};
