// 连接档案与分组。凭证只作为入参单向送入后端保险库，永不回读。
import type { BackupInfo, ExportReport, Group, ImportReport, Profile, ProfileInput } from "../../types";
import { call } from "./client";

export interface SshConfigEntry {
  name: string;
  host: string;
  port: number;
  username: string;
  identityFile?: string | null;
  proxyJump?: string | null;
}

export const profiles = {
  list: () => call<Profile[]>("profile_list"),
  /** 解析本机 ~/.ssh/config 为连接草稿。 */
  parseSshConfig: () => call<SshConfigEntry[]>("ssh_config_parse"),
  get: (id: string) => call<Profile>("profile_get", { id }),
  /** 新建或编辑；返回落库后的档案（凭证已转为布尔标记）。 */
  save: (input: ProfileInput) => call<Profile>("profile_save", { input }),
  remove: (id: string) => call<void>("profile_delete", { id }),
  /** 复制连接：后端在可信区内连同保险库凭证一并复制到新档案，复制后可直接连接。 */
  duplicate: (id: string) => call<Profile>("profile_duplicate", { id }),

  groups: () => call<Group[]>("group_list"),
  createGroup: (name: string, parentId: string | null = null) =>
    call<Group>("group_create", { name, parentId }),
  deleteGroup: (id: string) => call<void>("group_delete", { id }),
  /** 重命名分组（侧栏分组行右键→重命名）。 */
  renameGroup: (id: string, name: string) => call<void>("group_rename", { id, name }),

  /** 拖拽重排：按期望顺序传 id 列表，后端把 sort_order 写成下标。 */
  reorder: (ids: string[]) => call<void>("profile_reorder", { ids }),
  reorderGroups: (ids: string[]) => call<void>("group_reorder", { ids }),

  /** 导出连接备份到磁盘路径；groupId 为空导出全部。
   *  默认不含凭证；includeCredentials=true 时生成主密码加密的「完整备份」。 */
  exportFile: (
    path: string,
    groupId: string | null = null,
    opts?: { masterPassword?: string; includeCredentials?: boolean },
  ) =>
    call<ExportReport>("profile_export_file", {
      path,
      groupId,
      masterPassword: opts?.masterPassword ?? null,
      includeCredentials: opts?.includeCredentials ?? false,
    }),
  /** 从磁盘路径导入连接备份；skipDuplicates 默认跳过完全同键的连接；含密钥的备份需传 masterPassword。 */
  importFile: (path: string, skipDuplicates = true, masterPassword?: string) =>
    call<ImportReport>("profile_import_file", {
      path,
      skipDuplicates,
      masterPassword: masterPassword ?? null,
    }),
  /** 预检备份文件：是否含加密凭证 + 条数（导入前用来判定要不要弹主密码）。 */
  inspectBackup: (path: string) => call<BackupInfo>("profile_backup_inspect", { path }),
};
