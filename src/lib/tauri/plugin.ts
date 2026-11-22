// 插件系统：列表 / 安装卸载 / 隔离配置 / 命令包执行 / 输出钩子。
import type { PluginManifest, PluginSettingSpec } from "../../types";
import { call } from "./client";

export interface InstalledPlugin {
  manifest: PluginManifest;
  hasWasm: boolean;
  enabled: boolean;
  path: string;
}

export interface PluginSettingsView {
  values: Record<string, string>;
  specs: PluginSettingSpec[];
}

export interface PluginRunResult {
  command: string;
  stdout: string;
  stderr: string;
  exitCode: number | null;
}

export const plugins = {
  list: () => call<InstalledPlugin[]>("plugin_list"),
  reload: () => call<number>("plugin_reload"),
  install: (path: string) => call<InstalledPlugin>("plugin_install", { path }),
  /** 弹原生目录选择框，返回选中的插件目录（取消返回 null）。
   * `title` 为前端本地化的对话框标题（走 i18n 字典），缺省时后端回落英文。 */
  pickDir: (title?: string) => call<string | null>("plugin_pick_dir", { title: title ?? null }),
  uninstall: (pluginId: string) => call<boolean>("plugin_uninstall", { pluginId }),
  settings: (pluginId: string) => call<PluginSettingsView>("plugin_settings_get", { pluginId }),
  /** 读插件贡献的语料包 JSON（manifest.locales 声明的相对路径；后端限制在插件目录内）。 */
  locale: (pluginId: string, code: string) =>
    call<Record<string, string>>("plugin_locale_get", { pluginId, code }),
  setSetting: (pluginId: string, key: string, value: string) =>
    call<void>("plugin_setting_set", { pluginId, key, value }),
  run: (pluginId: string, commandId: string, sessionId: string, arg = "") =>
    call<PluginRunResult>("plugin_run_command", {
      args: { pluginId, commandId, sessionId, arg },
    }),
  onOutput: (sessionId: string, output: string) =>
    call<{ values: string[]; notices: string[] }>("plugin_on_output", {
      args: { sessionId, output },
    }),
};
