// AI Provider 配置：CRUD + 连通性自检（顺带探测图片能力）。
//
// 契约纪律：API Key 只作为一次性入参（`ProviderInput.apiKey`）直送保险库，
// 响应体里只有 `hasKey` 布尔标记；前端组件提交后应立即清空本地 Key 字段。
import type { AiProvider, ProviderInput, ProviderTestResult } from "../../types";
import { call } from "./client";

export const providers = {
  /** 全部 provider（回填 hasKey，不含 Key 内容）。 */
  list: () => call<AiProvider[]>("ai_provider_list"),
  get: (id: string) => call<AiProvider>("ai_provider_get", { id }),

  /** 新建或编辑；`apiKey` 留空即保持原样（编辑不重填不动保险库）。返回落库后的 DTO。 */
  save: (input: ProviderInput) => call<AiProvider>("ai_provider_save", { input }),

  /** 删除并清理保险库里属于它的 API Key。 */
  remove: (id: string) => call<void>("ai_provider_delete", { id }),

  /** 设为默认（`ai_send_message` 未显式指定 provider 时用它）。 */
  setDefault: (id: string) => call<void>("ai_provider_set_default", { id }),

  /** 连通性自检：发一条极简 completion，再顺带探测图片能力（失败则抛错）。
   *  不进对话；探测结论会写回该 provider 的 `supportsVision`（发送带图时以此为准）。 */
  test: (id: string) => call<ProviderTestResult>("ai_provider_test", { id }),

  /** 用表单草稿直连自检（不必先保存）：新建模型填完即可测；编辑态 Key 留空回落已存 Key。
   *  同样顺带探测图片能力，但**不落库**（新建未保存态）：结果随表单存进 `ProviderInput`，保存时写入。 */
  testDraft: (input: ProviderInput) =>
    call<ProviderTestResult>("ai_provider_test_draft", { input }),
};
