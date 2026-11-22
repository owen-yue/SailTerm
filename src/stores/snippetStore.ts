// 命令片段 store：查看 / 增改 / 删除 / 插入到终端。
// 安全红线：content 绝不存明文凭证；命中密码/密钥/令牌后端拒存，提示改用 {{变量}} 占位。
// 插入 = 把（有变量则先渲染后的）命令模板写入当前活动会话的 PTY，**不自动执行**，由用户回车确认。
import { sessions as ssh, snippets as api } from "../lib/tauri";
import { t } from "../lib/i18n";
import { errMsg } from "../lib/tauri/client";
import type { Snippet, SnippetInput } from "../types";
import { sessionStore } from "./sessionStore";
import { createStore } from "./store";

interface SnippetState {
  items: Snippet[];
  loading: boolean;
  error: string;
  /** 当前分组筛选（null = 全部）。 */
  group: string | null;
}

export const snippetStore = createStore<SnippetState>({
  items: [],
  loading: false,
  error: "",
  group: null,
});

/** 按「分组名 → 片段名 → id」稳定排序（与后端 list 一致，本地增改后维持有序）。 */
function sorted(items: Snippet[]): Snippet[] {
  return [...items].sort(
    (a, b) =>
      a.groupName.localeCompare(b.groupName) ||
      a.name.localeCompare(b.name) ||
      a.id.localeCompare(b.id),
  );
}

export const snippetActions = {
  /** 拉取片段；显式传 group 则同时更新筛选态，否则沿用当前筛选。 */
  async refresh(group?: string | null): Promise<void> {
    const g = group === undefined ? snippetStore.get().group : group;
    snippetStore.set({ loading: true, error: "", group: g });
    try {
      snippetStore.set({ items: await api.list(g), loading: false });
    } catch (e) {
      snippetStore.set({ loading: false, error: errMsg(e) });
    }
  },

  /** 新建或编辑；成功返回落库 DTO，失败返回 null（错误已入 store，如「含凭证」被后端拒）。 */
  async save(input: SnippetInput): Promise<Snippet | null> {
    try {
      const s = await api.save(input);
      snippetStore.set((prev) => ({
        items: sorted(
          prev.items.some((x) => x.id === s.id)
            ? prev.items.map((x) => (x.id === s.id ? s : x))
            : [...prev.items, s],
        ),
        error: "",
      }));
      return s;
    } catch (e) {
      snippetStore.set({ error: errMsg(e) });
      return null;
    }
  },

  async remove(id: string): Promise<void> {
    try {
      await api.remove(id);
      snippetStore.set((prev) => ({ items: prev.items.filter((x) => x.id !== id) }));
    } catch (e) {
      snippetStore.set({ error: errMsg(e) });
    }
  },

  /**
   * 把片段插入当前活动会话的终端：有变量先用值渲染，再写入 PTY（不自动换行 / 执行，用户回车确认）。
   * 无活动连接时回 false 并置错误提示。
   */
  async insert(snippet: Snippet, values: Record<string, string>): Promise<boolean> {
    const { activeId, tabs } = sessionStore.get();
    const tab = tabs.find((t) => t.id === activeId);
    if (!activeId || !tab || tab.state !== "connected") {
      snippetStore.set({ error: t("sn.errNoSession") });
      return false;
    }
    try {
      const text =
        snippet.variables.length > 0 ? await api.render(snippet.content, values) : snippet.content;
      await ssh.write(activeId, text);
      snippetStore.set({ error: "" });
      return true;
    } catch (e) {
      snippetStore.set({ error: errMsg(e) });
      return false;
    }
  },

  clearError(): void {
    snippetStore.set({ error: "" });
  },
};
