// 连接档案与分组 store。凭证只经过 `save` 的入参直送后端，store 不留副本。
import { profiles as api } from "../lib/tauri";
import { errMsg } from "../lib/tauri/client";
import type { BackupInfo, ExportReport, Group, ImportReport, Profile, ProfileInput } from "../types";
import { createStore } from "./store";

interface ConnectionState {
  profiles: Profile[];
  groups: Group[];
  loading: boolean;
  error: string;
}

export const connectionStore = createStore<ConnectionState>({
  profiles: [],
  groups: [],
  loading: false,
  error: "",
});

export const connections = {
  /** 拉取档案与分组（启动时与各次写操作后调用）。 */
  async refresh(): Promise<void> {
    connectionStore.set({ loading: true, error: "" });
    try {
      const [profiles, groups] = await Promise.all([api.list(), api.groups()]);
      connectionStore.set({ profiles, groups, loading: false });
    } catch (e) {
      connectionStore.set({ loading: false, error: errMsg(e) });
    }
  },

  /** 保存（新建/编辑）连接；成功返回落库档案，失败返回 null。 */
  async save(input: ProfileInput): Promise<Profile | null> {
    try {
      const saved = await api.save(input);
      await connections.refresh();
      return saved;
    } catch (e) {
      connectionStore.set({ error: errMsg(e) });
      return null;
    }
  },

  /** 删除连接（后端同时清理保险库里的凭证条目）。 */
  async remove(id: string): Promise<boolean> {
    try {
      await api.remove(id);
      await connections.refresh();
      return true;
    } catch (e) {
      connectionStore.set({ error: errMsg(e) });
      return false;
    }
  },

  /** 局部改档案（重命名 / 换分组等）；不传凭证字段，保险库原样保留。 */
  async patch(id: string, patch: Partial<ProfileInput>): Promise<boolean> {
    const p = connectionStore.get().profiles.find((x) => x.id === id);
    if (!p) return false;
    return (
      (await connections.save({
        id,
        name: p.name,
        groupId: p.groupId,
        privateKeyId: p.privateKeyId,
        kind: p.kind,
        host: p.host,
        port: p.port,
        username: p.username,
        authType: p.authType,
        privateKeyPath: p.privateKeyPath,
        jumpProfileId: p.jumpProfileId,
        startupDir: p.startupDir,
        startupCommand: p.startupCommand,
        notes: p.notes,
        agentForward: p.agentForward,
        x11Forward: p.x11Forward,
        ftpActiveMode: p.ftpActiveMode,
        keepaliveSecs: p.keepaliveSecs,
        sortOrder: p.sortOrder,
        colorTag: p.colorTag,
        ...patch,
      })) !== null
    );
  },

  /** 复制连接：后端在可信区内连同保险库凭证一并复制到新档案，复制后可直接连接、无需重填密码。 */
  async duplicate(id: string): Promise<boolean> {
    try {
      await api.duplicate(id);
      await connections.refresh();
      return true;
    } catch (e) {
      connectionStore.set({ error: errMsg(e) });
      return false;
    }
  },

  async createGroup(name: string): Promise<boolean> {
    try {
      await api.createGroup(name);
      await connections.refresh();
      return true;
    } catch (e) {
      connectionStore.set({ error: errMsg(e) });
      return false;
    }
  },

  async renameGroup(id: string, name: string): Promise<boolean> {
    try {
      await api.renameGroup(id, name);
      await connections.refresh();
      return true;
    } catch (e) {
      connectionStore.set({ error: errMsg(e) });
      return false;
    }
  },

  async deleteGroup(id: string): Promise<boolean> {
    try {
      await api.deleteGroup(id);
      await connections.refresh();
      return true;
    } catch (e) {
      connectionStore.set({ error: errMsg(e) });
      return false;
    }
  },

  /**
   * 拖拽重排分组：先按新 id 顺序本地乐观重排（拖拽手感即时），再落库。
   * `orderedIds` 是全部真实分组（不含“未分组”）的期望顺序。
   */
  async reorderGroups(orderedIds: string[]): Promise<void> {
    const prev = connectionStore.get().groups;
    const rank = new Map(orderedIds.map((id, i) => [id, i]));
    connectionStore.set({
      groups: [...prev].sort(
        (a, b) => (rank.get(a.id) ?? 0) - (rank.get(b.id) ?? 0),
      ),
    });
    try {
      await api.reorderGroups(orderedIds);
    } catch (e) {
      connectionStore.set({ groups: prev, error: errMsg(e) });
    }
  },

  /**
   * 拖拽重排连接：`orderedIds` 是全部连接（跨分组拼接）的期望顺序，本地按此重排后落库。
   * 分组内展示仍靠 profiles 的全局顺序过滤，故全局重排即可驱动组内排序。
   */
  async reorderProfiles(orderedIds: string[]): Promise<void> {
    const prev = connectionStore.get().profiles;
    const rank = new Map(orderedIds.map((id, i) => [id, i]));
    connectionStore.set({
      profiles: [...prev].sort(
        (a, b) => (rank.get(a.id) ?? 0) - (rank.get(b.id) ?? 0),
      ),
    });
    try {
      await api.reorder(orderedIds);
    } catch (e) {
      connectionStore.set({ profiles: prev, error: errMsg(e) });
    }
  },

  /** 导出连接备份到 path；opts.includeCredentials=true 时生成主密码加密的完整备份。
   *  成功返回摘要，失败置错误返回 null。 */
  async exportToFile(
    path: string,
    groupId: string | null = null,
    opts?: { masterPassword?: string; includeCredentials?: boolean },
  ): Promise<ExportReport | null> {
    try {
      return await api.exportFile(path, groupId, opts);
    } catch (e) {
      connectionStore.set({ error: errMsg(e) });
      return null;
    }
  },

  /** 预检备份文件（是否含加密凭证）；失败返回 null。 */
  async inspectBackupFile(path: string): Promise<BackupInfo | null> {
    try {
      return await api.inspectBackup(path);
    } catch (e) {
      connectionStore.set({ error: errMsg(e) });
      return null;
    }
  },

  /** 从 path 导入连接备份（含密钥的备份需 masterPassword）；成功后刷新列表，返回摘要或 null。 */
  async importFromFile(
    path: string,
    skipDuplicates = true,
    masterPassword?: string,
  ): Promise<ImportReport | null> {
    try {
      const report = await api.importFile(path, skipDuplicates, masterPassword);
      await connections.refresh();
      return report;
    } catch (e) {
      connectionStore.set({ error: errMsg(e) });
      return null;
    }
  },

  clearError(): void {
    connectionStore.set({ error: "" });
  },
};
