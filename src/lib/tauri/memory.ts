// 长期记忆 CRUD：查看/手动添加/编辑/删除/清空。
// content 只存脱敏后的偏好/环境事实，绝不含凭证；召回注入时按本轮层级再脱敏。
import type { Memory, MemoryInput, MemoryScope } from "../../types";
import { call } from "./client";

export const memory = {
  /** 列记忆；`scope` 为空不限作用域，`connectionId` 非空只看该连接的 server 记忆。 */
  list: (scope?: MemoryScope | null, connectionId?: string | null, limit?: number) =>
    call<Memory[]>("memory_list", {
      scope: scope ?? null,
      connectionId: connectionId ?? null,
      limit: limit ?? null,
    }),

  /** 新建或编辑；返回落库后的记忆（内容已脱敏）。 */
  save: (input: MemoryInput) => call<Memory>("memory_save", { input }),

  /** 删除单条记忆，返回是否删到。 */
  remove: (id: string) => call<boolean>("memory_delete", { id }),

  /** 清空：`scope` 为空清全部，否则按作用域清（server 再按 connectionId 限定）。返回删除条数。 */
  clear: (scope?: MemoryScope | null, connectionId?: string | null) =>
    call<number>("memory_clear", {
      scope: scope ?? null,
      connectionId: connectionId ?? null,
    }),
};
