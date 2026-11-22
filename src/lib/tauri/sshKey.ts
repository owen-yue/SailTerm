// SSH 密钥管理 IPC：导入既有私钥、列密钥（只回非敏感元数据 + 公钥）、导出私钥到文件、删除。
// 安全红线：私钥材料只进后端保险库，绝不经 IPC 回前端——list 只回元数据 + 公钥；
// 导出私钥走后端 std::fs 写用户指定文件，不返回内容；db 表无任何私钥列（见 db/ssh_key.rs）。
import type { KeyGenAlgorithm, KeyInput, KeygenInput, KeygenPreview, SshKey } from "../../types";
import { call } from "./client";

export const sshKeys = {
  /** 列全部密钥（只回非敏感元数据 + 公钥；私钥材料不出保险库）。 */
  list: () => call<SshKey[]>("ssh_key_list"),

  /** 导入一把既有私钥（粘贴内容或文件路径二选一，可带口令）；返回落库的非敏感元数据。 */
  importKey: (input: KeyInput) => call<SshKey>("ssh_key_import", { input }),

  /** 本地生成密钥（Ed25519 / ECDSA / RSA）；私钥只进保险库，不回显。 */
  generate: (input: { name: string; comment?: string; algorithm?: KeyGenAlgorithm }) =>
    call<SshKey>("ssh_key_generate", { input }),

  /** 导出私钥到用户指定文件（后端 std::fs 写盘，不经 IPC 返回内容）。*/
  exportFile: (id: string, path: string) => call<boolean>("ssh_key_export_file", { id, path }),

  /** 删除一把密钥（连带清理保险库中的私钥 + 口令）。 */
  remove: (id: string) => call<boolean>("ssh_key_delete", { id }),

  /** 生成器：生成密钥对（不入库），返回公钥 + 指纹 + 取回私钥的 token；私钥只留后端缓冲。 */
  keygenGenerate: (input: KeygenInput) => call<KeygenPreview>("ssh_keygen_generate", { input }),

  /** 生成器：把缓存的私钥写到用户指定文件（后端写盘，不回内容）。 */
  keygenSavePrivate: (token: string, path: string) =>
    call<boolean>("ssh_keygen_save_private", { token, path }),

  /** 生成器：把缓存的公钥（OpenSSH 单行）写到用户指定文件。 */
  keygenSavePublic: (token: string, path: string) =>
    call<boolean>("ssh_keygen_save_public", { token, path }),
};
