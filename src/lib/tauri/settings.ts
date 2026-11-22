// AI/安全设置：出网开关、审批模式、命令风险扫描层级。
// 只存非敏感偏好，绝不含凭证/API Key。出网关闭时仅本地模型（Ollama）可用。
import type {
  AiSettings,
  AiSettingsPatch,
  AppearancePatch,
  AppearanceSettings,
  TerminalSettings,
  TerminalSettingsPatch,
  ThemeScanResult,
} from "../../types";
import { call } from "./client";

export const settings = {
  /** 读取当前 AI/安全设置（缺省项回落默认值）。 */
  get: () => call<AiSettings>("settings_get"),

  /** 局部更新（只传要改的字段）；返回更新后的完整设置。 */
  set: (patch: AiSettingsPatch) => call<AiSettings>("settings_set", { patch }),

  /** 恢复默认：清除已存的 AI 设置键，返回默认设置。 */
  reset: () => call<AiSettings>("settings_reset"),
};

// 外观/主题设置：主题 id / 字号 / 字体 + 自定义 JSON 主题导入。
// 同样只存非敏感偏好（颜色/字体），绝不含凭证。
export const appearance = {
  /** 读取当前外观设置（缺省项回落默认值）。 */
  get: () => call<AppearanceSettings>("appearance_get"),

  /** 局部更新（主题/字号/字体）；返回更新后的完整设置，前端据此热切换 xterm。 */
  set: (patch: AppearancePatch) => call<AppearanceSettings>("appearance_set", { patch }),

  /** 恢复默认外观（不影响 AI/安全设置）。 */
  reset: () => call<AppearanceSettings>("appearance_reset"),

  /** 从 JSON 文件导入自定义主题（后端 std::fs 读取+校验，无 dialog/fs 插件），返回更新后的外观设置。 */
  importTheme: (path: string) => call<AppearanceSettings>("theme_import_file", { path }),

  /** 扫描数据目录 themes/*.json，返回可点选的自定义主题清单（校验失败的静默跳过）。 */
  scanThemes: () => call<ThemeScanResult>("theme_scan_dir"),
};

// 终端行为偏好：滚动缓冲 / 光标闪烁 / 本地 shell 路径。
export const terminal = {
  get: () => call<TerminalSettings>("terminal_get"),
  set: (patch: TerminalSettingsPatch) => call<TerminalSettings>("terminal_set", { patch }),
  reset: () => call<TerminalSettings>("terminal_reset"),
};
