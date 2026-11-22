// SSH 隧道 store：转发规则 CRUD + 在已连接会话上开 / 关本地端口转发。
// 只处理地址端口，绝不含凭证；开启复用会话已认证连接，不二次索取密码 / 私钥。
import { tunnels as api } from "../lib/tauri";
import { t } from "../lib/i18n";
import { errMsg } from "../lib/tauri/client";
import { hasCap } from "../lib/capabilities";
import type { ActiveTunnel, TunnelConfig, TunnelInput } from "../types";
import { sessionStore } from "./sessionStore";
import { createStore } from "./store";

interface TunnelState {
  /** 已保存的转发规则配置。 */
  configs: TunnelConfig[];
  /** 正在运行的转发（活动隧道）。 */
  active: ActiveTunnel[];
  loading: boolean;
  error: string;
  /** 配置列表的连接筛选（null = 全部）。 */
  profileId: string | null;
}

export const tunnelStore = createStore<TunnelState>({
  configs: [],
  active: [],
  loading: false,
  error: "",
  profileId: null,
});

/** 按「名称 → id」稳定排序（与后端 list 一致，本地增改后维持有序）。 */
function sorted(items: TunnelConfig[]): TunnelConfig[] {
  return [...items].sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id));
}

/** 当前可用的已连接 SSH 会话 id：优先活动标签，否则退回任一已连接 SSH 会话（隧道只能依附 SSH，即能力表的 tunnel 档）。 */
function activeSshSession(): string | null {
  const { activeId, tabs } = sessionStore.get();
  const active = tabs.find((t) => t.id === activeId);
  if (active && hasCap(active.kind, "tunnel") && active.state === "connected") return active.id;
  return tabs.find((t) => hasCap(t.kind, "tunnel") && t.state === "connected")?.id ?? null;
}

export const tunnelActions = {
  /** 拉取配置；显式传 profileId 则同时更新筛选，否则沿用当前筛选。 */
  async refresh(profileId?: string | null): Promise<void> {
    const pid = profileId === undefined ? tunnelStore.get().profileId : profileId;
    tunnelStore.set({ loading: true, error: "", profileId: pid });
    try {
      tunnelStore.set({ configs: await api.list(pid), loading: false });
    } catch (e) {
      tunnelStore.set({ loading: false, error: errMsg(e) });
    }
  },

  /** 拉取运行中的转发；`sessionId` 为空列全部。 */
  async refreshActive(sessionId?: string | null): Promise<void> {
    try {
      tunnelStore.set({ active: await api.active(sessionId ?? null), error: "" });
    } catch (e) {
      tunnelStore.set({ error: errMsg(e) });
    }
  },

  /** 新建或编辑配置；成功返回落库 DTO，失败返回 null（错误已入 store）。 */
  async save(input: TunnelInput): Promise<TunnelConfig | null> {
    try {
      const t = await api.save(input);
      tunnelStore.set((prev) => ({
        configs: sorted(
          prev.configs.some((x) => x.id === t.id)
            ? prev.configs.map((x) => (x.id === t.id ? t : x))
            : [...prev.configs, t],
        ),
        error: "",
      }));
      return t;
    } catch (e) {
      tunnelStore.set({ error: errMsg(e) });
      return null;
    }
  },

  async remove(id: string): Promise<void> {
    try {
      await api.remove(id);
      tunnelStore.set((prev) => ({ configs: prev.configs.filter((x) => x.id !== id) }));
    } catch (e) {
      tunnelStore.set({ error: errMsg(e) });
    }
  },

  /**
   * 在某条会话上开启一条配置的转发（-L / -R / -D）。默认取当前活动的已连接 SSH 会话；
   * 无可用会话时回 false 并置错误提示（隧道必须依附一条已认证 SSH 连接）。
   */
  async open(tunnelId: string, sessionId?: string | null): Promise<boolean> {
    const sid = sessionId ?? activeSshSession();
    if (!sid) {
      tunnelStore.set({ error: t("tm.errNeedConnected") });
      return false;
    }
    try {
      const at = await api.open(sid, tunnelId);
      tunnelStore.set((prev) => ({
        active: [...prev.active.filter((x) => x.runId !== at.runId), at],
        error: "",
      }));
      return true;
    } catch (e) {
      tunnelStore.set({ error: errMsg(e) });
      return false;
    }
  },

  /** 关闭一条运行中的转发（按 runId）。 */
  async close(runId: string): Promise<void> {
    try {
      await api.close(runId);
      tunnelStore.set((prev) => ({ active: prev.active.filter((x) => x.runId !== runId) }));
    } catch (e) {
      tunnelStore.set({ error: errMsg(e) });
    }
  },

  clearError(): void {
    tunnelStore.set({ error: "" });
  },
};
