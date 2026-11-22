// 命令片段 / 快捷指令 CRUD + 渲染：查看 / 新建 / 编辑 / 删除，插入时渲染变量。
// content 绝不存明文凭证；命中密码/密钥/令牌后端拒存，提示改用 {{变量}} 占位。
import type { Snippet, SnippetInput } from "../../types";
import { call } from "./client";

export const snippets = {
  /** 列片段；`group` 为空列全部，否则只列该分组。 */
  list: (group?: string | null) => call<Snippet[]>("snippet_list", { group: group ?? null }),

  /** 新建或编辑；返回落库后的片段（含解析出的 variables）。 */
  save: (input: SnippetInput) => call<Snippet>("snippet_save", { input }),

  /** 删除单条，返回是否删到。 */
  remove: (id: string) => call<boolean>("snippet_delete", { id }),

  /** 用变量值渲染命令模板（无状态纯变换），返回可直接写入终端的字符串。 */
  render: (content: string, values: Record<string, string>) =>
    call<string>("snippet_render", { content, values }),
};
