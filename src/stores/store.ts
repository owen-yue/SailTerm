// 极简外部 store（useSyncExternalStore）：零依赖，够 用；
// 状态复杂化后可平滑替换为 Zustand（`stores/`），组件侧调用方式不变。
// 安全红线：任何 store 都**不保存凭证字段**。
import { useCallback, useRef, useSyncExternalStore } from "react";

export interface Store<T extends object> {
  get(): T;
  /** 合并补丁；传函数则基于上一状态计算补丁。 */
  set(patch: Partial<T> | ((prev: T) => Partial<T>)): void;
  subscribe(fn: () => void): () => void;
}

export function createStore<T extends object>(initial: T): Store<T> {
  let state = initial;
  const listeners = new Set<() => void>();

  return {
    get: () => state,
    set: (patch) => {
      const next = typeof patch === "function" ? patch(state) : patch;
      state = { ...state, ...next };
      listeners.forEach((fn) => fn());
    },
    subscribe: (fn) => {
      listeners.add(fn);
      return () => {
        listeners.delete(fn);
      };
    },
  };
}

/** React 绑定：状态对象身份变化才触发重渲染。 */
export function useStore<T extends object>(store: Store<T>): T {
  return useSyncExternalStore(store.subscribe, store.get, store.get);
}

/**
 * React 绑定（字段选取版）：只订阅关心的几个字段，所选字段引用都没变就不重渲染。
 *
 * 整个 store 每次 `set` 都会换 state 对象身份（`{...state, ...patch}`），AI 流式输出逐 token
 * 写 `turns` 时，只关心 `approvals/questions` 的布局组件若用 [`useStore`] 会跟着逐 token
 * 重渲染。快照按字段引用缓存：同一轮 store 变更里所选引用未变则返回同一个对象，
 * 满足 `useSyncExternalStore` 对 getSnapshot 幂等性的要求。
 */
export function useStorePick<T extends object, K extends keyof T>(
  store: Store<T>,
  keys: readonly K[],
): Pick<T, K> {
  const cache = useRef<{ src: T; out: Pick<T, K> } | null>(null);
  const get = useCallback(() => {
    const s = store.get();
    const c = cache.current;
    if (c && keys.every((k) => c.src[k] === s[k])) return c.out;
    const out = {} as Pick<T, K>;
    for (const k of keys) out[k] = s[k];
    cache.current = { src: s, out };
    return out;
    // keys 约定为模块级常量数组；join 成原始值做依赖，避免引用比较失真
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [keys.join(",")]);
  return useSyncExternalStore(store.subscribe, get, get);
}

/**
 * 本 WebView 是否为独立窗口（会话弹出 `?pop=` / 工具窗口 `?tool=`）。
 *
 * 每个 WebView 本就是独立 JS 上下文，布局态（下栏/边栏展开收起）天然每窗口一份；
 * 唯一的跨窗口串味通道是「初始化时读同一份 localStorage」。故独立窗口按此判定：
 * 用固定默认值起手、不回写共享键，与主窗口彻底互不串味（见 panelStore / collapseStore）。
 */
export function isDetachedWindow(): boolean {
  const p = new URLSearchParams(window.location.search);
  return p.has("pop") || p.has("tool");
}
