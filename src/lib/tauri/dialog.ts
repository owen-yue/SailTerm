// 原生系统文件 / 目录选择框的统一前端出口（走 tauri_plugin_dialog）。
// 凡「选本机文件/目录 / 另存为」都调这里，替代手输路径；取消返回 null。
import { call } from "./client";

export interface SaveFileOpts {
  title?: string;
  /** 默认文件名（含扩展名），如 "sailterm-connections.json"。 */
  defaultName?: string;
  /** 扩展名过滤名称，如 "JSON"。与 extensions 同时给出才生效。 */
  filterName?: string;
  /** 过滤扩展名（不含点），如 ["json"]。 */
  extensions?: string[];
}

export const dialog = {
  /** 选单个已存在文件（打开口）。 */
  pickFile: (title?: string) => call<string | null>("dialog_pick_file", { title: title ?? null }),
  /** 选目录。 */
  pickFolder: (title?: string) => call<string | null>("dialog_pick_folder", { title: title ?? null }),
  /** 另存为（可带默认文件名与扩展名过滤）。 */
  saveFile: (opts: SaveFileOpts = {}) =>
    call<string | null>("dialog_save_file", {
      title: opts.title ?? null,
      defaultName: opts.defaultName ?? null,
      filterName: opts.filterName ?? null,
      extensions: opts.extensions ?? null,
    }),
};
