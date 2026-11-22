// 会话显示名的唯一入口：重名会话在这里带上 `#n`。
//
// 为什么必须收口成一处：后端 `ConnectSpec::label()` 就是 `user@host`，同一台服务器开两条会话时
// 两行字**完全一样**。发送工具那一列先加了 `#n`、页签上却没有，用户看到的就成了「两个名字对不上，
// 到底是不是同一条」——所以序号只能有一处生成、处处相同。
//
// 口径（改动前务必读）：
// - 基数 = **全量真实会话**，按 tabs 数组次序（= 会话建立序）排 `#1..#n`。调用方一律不许先 filter
//   再编号：那样同一条会话在页签上是 #2、在发送列表里就成了 #1，正好把要解决的问题又造出来。
// - 占位标签（`phase` 非空：预加载中 / 失败卡）不入基数。它只存在于主窗口，后端 `session_list`
//   不返回，算进去会让两个窗口对同一条真实会话给出不同序号；占位标签自己显裸名（还不成会话，
//   没有可对应的对象）。
// - 覆盖面按「认窗口」收口：页签、分屏 pane 头、弹出窗口标题栏、发送工具的目标列表与结果行。
//   单会话面板的标题（文件 / 传输 / 监控 / 服务 / 历史…）仍显裸 label：它们跟着聚焦页签走，
//   不与页签并排点名，加下标只会让面板标题与后端 label 的真实值之间多一层翻译。
import { useMemo } from "react";
import { useI18n } from "./i18n";
import { disambiguateLabels, sessionLabel, type SessionKind } from "../types";
import { sessionStore } from "../stores/sessionStore";
import { useStore } from "../stores/store";

/** 可参与编号的会话：后端 DTO 字段 + 占位标签的 `phase`。 */
type Named = { id: string; kind: SessionKind; label: string; phase?: string };

/** 真实会话 → 带 `#n` 的显示名（占位标签已剔出基数）。 */
function namesOf(tabs: Named[], t: (key: string) => string): Map<string, string> {
  return disambiguateLabels(tabs.filter((x) => !x.phase), t);
}

/** 组件用：本窗口会话表的显示名函数（会话表 / 语言变化即重算）。 */
export function useSessionName(): (tab: Named) => string {
  const tr = useI18n();
  const { tabs } = useStore(sessionStore);
  return useMemo(() => {
    const names = namesOf(tabs, tr);
    return (tab) => names.get(tab.id) ?? sessionLabel(tab, tr);
  }, [tabs, tr]);
}

/** 非组件上下文（store 驱动、原生窗口标题）用：同口径，调用方把当前会话表传进来。 */
export function sessionNameOf(tabs: Named[], t: (key: string) => string, tab: Named): string {
  return namesOf(tabs, t).get(tab.id) ?? sessionLabel(tab, t);
}
