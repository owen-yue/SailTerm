// SSH 密钥管理 store：列密钥 / 导入 / 导出私钥文件 / 删除。
// 安全红线：私钥材料只进后端保险库，绝不经 IPC 回前端；本 store 只持有非敏感元数据 + 公钥。
// 导出私钥走后端 std::fs 写用户指定文件（不回内容）；删除连带清理保险库条目。
import { sshKeys as api } from "../lib/tauri";
import { errMsg } from "../lib/tauri/client";
import type { KeyGenAlgorithm, KeyInput, SshKey } from "../types";
import { createStore } from "./store";

interface SshKeyState {
  items: SshKey[];
  loading: boolean;
  error: string;
}

export const sshKeyStore = createStore<SshKeyState>({
  items: [],
  loading: false,
  error: "",
});

/** 按「名称 → id」稳定排序（与后端 list 一致，本地增删后维持有序）。 */
function sorted(items: SshKey[]): SshKey[] {
  return [...items].sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id));
}

export const sshKeyActions = {
  /** 拉取全部密钥元数据。 */
  async refresh(): Promise<void> {
    sshKeyStore.set({ loading: true, error: "" });
    try {
      sshKeyStore.set({ items: await api.list(), loading: false });
    } catch (e) {
      sshKeyStore.set({ loading: false, error: errMsg(e) });
    }
  },

  /** 导入一把私钥；成功返回落库 DTO，失败返回 null（错误已入 store，如指纹重复被后端拒）。 */
  async importKey(input: KeyInput): Promise<SshKey | null> {
    try {
      const k = await api.importKey(input);
      sshKeyStore.set((prev) => ({
        items: sorted(
          prev.items.some((x) => x.id === k.id)
            ? prev.items.map((x) => (x.id === k.id ? k : x))
            : [...prev.items, k],
        ),
        error: "",
      }));
      return k;
    } catch (e) {
      sshKeyStore.set({ error: errMsg(e) });
      return null;
    }
  },

  /** 生成密钥（Ed25519 / ECDSA / RSA）；私钥只进保险库。 */
  async generate(input: {
    name: string;
    comment?: string;
    algorithm?: KeyGenAlgorithm;
  }): Promise<SshKey | null> {
    try {
      const k = await api.generate(input);
      sshKeyStore.set((prev) => ({
        items: sorted([k, ...prev.items.filter((x) => x.id !== k.id)]),
        error: "",
      }));
      return k;
    } catch (e) {
      sshKeyStore.set({ error: errMsg(e) });
      return null;
    }
  },

  /** 导出私钥到用户指定文件（后端写盘，不回内容）；成功返回 true。*/
  async exportFile(id: string, path: string): Promise<boolean> {
    try {
      const ok = await api.exportFile(id, path);
      sshKeyStore.set({ error: "" });
      return ok;
    } catch (e) {
      sshKeyStore.set({ error: errMsg(e) });
      return false;
    }
  },

  async remove(id: string): Promise<void> {
    try {
      await api.remove(id);
      sshKeyStore.set((prev) => ({ items: prev.items.filter((x) => x.id !== id) }));
    } catch (e) {
      sshKeyStore.set({ error: errMsg(e) });
    }
  },

  clearError(): void {
    sshKeyStore.set({ error: "" });
  },
};
