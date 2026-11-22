// 快捷命令栏（QuickBar）：终端底部的 chip 条，单击即发。
// 数据源：片段库 snippetStore（全局）× quickBarStore（每 profile 上栏关联）——两表交叉得出可视 chip。
// 执行语义：sessions.write(sid, text + "\n")——**写入即执行**（区别于设置页片段库的"插入不执行"）。
// 变量处理：含 {{var}} 的片段点开时，就地展开填值条（inline），提交即 render+write，取消即收起。
// 新建/编辑走独立弹框（SnippetFormDialog 包共享 SnippetEditor），填值面板、删除气泡为共享行为层
// （SnippetFillForm/useSnippetDelete），与设置页片段库同一套字段与校验，仅提交语义由本侧决定（写入即执行）。
// 无变量、无片段库依赖前端渲染成本：一期不做虚拟滚动，chip > 20 建议靠折叠。
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  connectionStore,
  connPoolKeyOf,
  groupKeyOf,
  profileKeyOf,
  QUICKBAR_GLOBAL_KEY,
  quickBarActions,
  quickBarStore,
  snippetActions,
  snippetStore,
  toast,
  useStore,
} from "../../stores";
import type { SessionDto, Snippet } from "../../types";
import { sessions as sshApi, snippets as snippetApi } from "../../lib/tauri";
import { errMsg } from "../../lib/tauri/client";
import { useI18n } from "../../lib/i18n";
import { hasQuickBar } from "../../lib/capabilities";
import { jumpToFaq } from "../../lib/faqJump";
import { useEscClose, useMaskDismiss } from "../../lib/useOverlay";
import { useSnippetDelete } from "../../lib/useSnippetDelete";
import Chevron from "../common/Chevron";
import QuickPickList from "./QuickPickList";
import SnippetFormDialog from "./SnippetFormDialog";
import SnippetFillForm from "./SnippetFillForm";

// 面板头问号的直达目标：faq.zh.md / faq.en.md 里 [11]「快捷命令栏怎么用」的编号
const FAQ_NO_QUICKBAR = 11;
// 稳定空数组引用：避免 ?? [] 每帧新造数组导致下游 useMemo/useEffect 依赖抖动重算。
const EMPTY_IDS: string[] = [];

interface Props {
  sessionId: string;
  tab: SessionDto;
}

/** 从片段库列表里按 id 顺序取；已删除的 id 自动跳过（列表只算可见 chip，清理另外走 useEffect）。 */
function resolveChips(ids: string[], items: Snippet[]): Snippet[] {
  const byId = new Map(items.map((s) => [s.id, s]));
  const out: Snippet[] = [];
  for (const id of ids) {
    const s = byId.get(id);
    if (s) out.push(s);
  }
  return out;
}

export default function QuickBar({ sessionId, tab }: Props) {
  const t = useI18n();
  const { items, loading } = useStore(snippetStore);
  const { byProfile, collapsed } = useStore(quickBarStore);
  const { profiles } = useStore(connectionStore);

  const key = profileKeyOf(tab);
  // 三个分类池：全局（任何会话可见）/ 组（组内会话可见）/ 本连接私有池（归属本连接的所有命令）。
  // 栏（底部 chip）= 三个池里被勾选进来的部分，对应 byProfile[key]；池与栏正交：
  // 取消勾选只把命令移出栏，命令仍留在原池里（修复旧版“本连接区一点就消失”的缺陷）。
  const groupId = tab.profileId ? profiles.find((p) => p.id === tab.profileId)?.groupId ?? null : null;
  const groupKey = groupKeyOf(groupId);
  const connKey = connPoolKeyOf(key);
  const connIds = key ? byProfile[key] ?? EMPTY_IDS : EMPTY_IDS;
  const globalIds = byProfile[QUICKBAR_GLOBAL_KEY] ?? EMPTY_IDS;
  const groupIds = groupKey ? byProfile[groupKey] ?? EMPTY_IDS : EMPTY_IDS;
  const connPoolIds = connKey ? byProfile[connKey] ?? EMPTY_IDS : EMPTY_IDS;
  const chips = useMemo(() => resolveChips(connIds, items), [connIds, items]);
  const globalItems = useMemo(() => resolveChips(globalIds, items), [globalIds, items]);
  const groupItems = useMemo(() => resolveChips(groupIds, items), [groupIds, items]);
  const connItems = useMemo(() => resolveChips(connPoolIds, items), [connPoolIds, items]);

  // items 是否已是「权威快照」。store 初值为 { items: [], loading: false }，而本组件的
  // 清理 effect 声明在拉取 effect 之前——挂载首帧会看到「items 为空且 loading 未置起」，
  // 若直接据此判定陈旧 id，会把用户整条快捷栏配置误删光。故用 ref 闩锁：
  // 只有确认拉到过数据（或正在拉取时归位）才放行清理；拉取失败保持不放行。
  const listReady = useRef(false);
  useEffect(() => {
    if (loading) listReady.current = false;
    else if (items.length > 0) listReady.current = true;
    // items 为空且未在加载：维持原值——“用户刚把片段删光”时仍为 true，清理才能生效。
  }, [loading, items]);

  // 关联表里指向已删片段的 id：进入新一帧后四个键一起异步清掉（避免在 render 中 setState）。
  // 以 listReady 而非 items.length 作门：否则用户把片段全删光后，陈旧 id 永远清不掉。
  useEffect(() => {
    if (!listReady.current) return;
    const have = new Set(items.map((s) => s.id));
    for (const k of [key, groupKey, connKey, QUICKBAR_GLOBAL_KEY]) {
      if (!k) continue;
      const stale = (byProfile[k] ?? EMPTY_IDS).filter((id) => !have.has(id));
      for (const id of stale) quickBarActions.removeSnippet(k, id);
    }
  }, [key, groupKey, connKey, byProfile, items]);

  // 迁移：栏上但不属于任何池的历史私有 chip → 归入本连接池，避免改造后从「本连接」区消失。
  // addSnippet 幂等，入池后 connPoolIds 刷新、下一帧无孤儿自然停。
  useEffect(() => {
    if (!connKey) return;
    const pooled = new Set([...globalIds, ...groupIds, ...connPoolIds]);
    for (const id of connIds) if (!pooled.has(id)) quickBarActions.addSnippet(connKey, id);
  }, [connKey, connIds, globalIds, groupIds, connPoolIds]);

  // 片段库常驻刷新策略：首次挂载且 items 为空时拉一次（各弹框打开也会拉、幂等）。
  useEffect(() => {
    if (snippetStore.get().items.length === 0 && !snippetStore.get().loading) {
      void snippetActions.refresh();
    }
  }, []);

  const [pickerOpen, setPickerOpen] = useState(false);
  // 变量填值面板：面板在共享 SnippetFillForm，本侧只记目标片段与提交语义（写入即执行）。
  const [filling, setFilling] = useState<Snippet | null>(null);
  // 删除气泡：与设置页片段库 / 组池弹窗共用 useSnippetDelete（删片段库本体，
  // 各池/栏的陈旧引用由上面 effect 清理）。
  const delSnippet = useSnippetDelete();

  // 新建/编辑：独立弹框盖在 ＋ 面板之上（列表不位移），null = 未开；每次开关都重挂载，无草稿残留。
  // scope = 归属分类（全局 / 组 / 本连接），由入口的 ＋ / 片段所在分类决定。
  const [form, setForm] = useState<{ editing: Snippet | null; scope: "global" | "group" | "conn" } | null>(null);

  // 有效折叠态：collapsed[key]  undefined = 无显式偏好→按栏是否为空派生
  // （一条命令都没有时默认收起，只留右下角悬浮小钮，把行高让回画布；首条命令上栏即自动展开）；
  // true/false = 用户显式选择，空栏时也尊重（手动展开过就保持展开）。
  const isCollapsed = key
    ? chips.length === 0
      ? collapsed[key] !== false
      : Boolean(collapsed[key])
    : false;

  // 关整个弹框（新建/编辑已拆成独立弹框，各有各的关闭出口，不连带）
  function closeDialog() {
    setPickerOpen(false);
  }
  // Esc 只关最顶层：表单开着时 SnippetFormDialog 后挂载、在栈顶，先关它；这里轮到 ＋ 面板
  useEscClose(closeDialog, pickerOpen);
  // 遮罩关闭属性集：必须在顶层无条件调用（内含 hook），再展开到弹框遮罩上
  const maskDismiss = useMaskDismiss(closeDialog);

  async function runText(snippet: Snippet, values: Record<string, string>): Promise<boolean> {
    if (tab.state !== "connected") {
      toast.error(t("sn.errNoSession"));
      return false;
    }
    try {
      const text =
        snippet.variables.length > 0
          ? await snippetApi.render(snippet.content, values)
          : snippet.content;
      // 一步到位：写入终端 + 补 \n 让 shell 直接执行。已含末尾换行则不重复补。
      await sshApi.write(sessionId, text.endsWith("\n") ? text : `${text}\n`);
      return true;
    } catch (e) {
      toast.error(errMsg(e));
      return false;
    }
  }

  function onChip(snippet: Snippet) {
    if (snippet.variables.length > 0) {
      setFilling(snippet);
      return;
    }
    void runText(snippet, {});
  }

  /** 池区行点击：勾上 / 取消本连接快捷栏。 */
  const toggleBar = useCallback(
    (snippet: Snippet, on: boolean) => {
      if (on) quickBarActions.removeSnippet(key, snippet.id);
      else quickBarActions.addSnippet(key, snippet.id);
    },
    [key],
  );

  /** 一键应用：把某个池的命令全部勾到本连接快捷栏（已在栏的跳过）。 */
  function applyAll(poolIds: string[]) {
    quickBarActions.addMany(key, poolIds);
  }

  /** 一键取消：把某个池已上栏的命令全部从本连接栏移出（仍留在池里）。 */
  function unapplyAll(poolIds: string[]) {
    quickBarActions.removeMany(key, poolIds);
  }

  /** 从分类旁的 ＋ 打开新建弹框：保存后落进该池。 */
  function openNew(scope: "global" | "group" | "conn") {
    setForm({ editing: null, scope });
  }

  /** 行尾 ✎ 打开编辑弹框：预填内容由 SnippetEditor 负责，归属按片段所在分类判。 */
  const openEdit = useCallback(
    (s: Snippet) => {
      setForm({
        editing: s,
        scope: globalIds.includes(s.id) ? "global" : groupIds.includes(s.id) ? "group" : "conn",
      });
    },
    [globalIds, groupIds],
  );

  /** 新建片段落进所在分类池，并顺带勾上本连接栏，让创建者立即看到效果。
   *  本连接池键与栏键不同（conn:<key> vs key），因此池里取消勾选后命令仍在。 */
  function adoptNew(scope: "global" | "group" | "conn", saved: Snippet) {
    const poolKey = scope === "global" ? QUICKBAR_GLOBAL_KEY : scope === "group" && groupKey ? groupKey : connKey;
    if (poolKey) quickBarActions.addSnippet(poolKey, saved.id);
    quickBarActions.addSnippet(key, saved.id);
  }

  async function submitFill(values: Record<string, string>) {
    if (!filling) return;
    await runText(filling, values);
    setFilling(null);
  }

  function openPicker() {
    setPickerOpen(true);
  }

  // 无 shell 语义的会话类型（ftp / ftps / serial）不渲染快捷栏。
  // 注意：必须排在所有 hook（useCallback / useEffect / useMemo）之后——提前 return 会让
  // hook 调用数量随渲染变化，触发 React「Rendered fewer hooks than expected」崩溃。
  if (!hasQuickBar(tab.kind)) return null;

  // 收起/展开是同一颗药丸的形变（CSS transition）：不再两套元素互换，
  // 锚点恒为 .term-wrap 右下（right:20 bottom:8），两态右缘/底缘对齐不跳位。
  // 行高由入流 .qbar-space 预留：展开 44px、收起 0；高度不加过渡——
  // 避免动画期间 xterm 随容器连续 refit，过渡感由药丸自身形变承担。
  return (
    <>
      <div className="qbar-space" style={{ height: isCollapsed ? 0 : 44 }} />
      <div
        className="qbar"
        data-collapsed={isCollapsed ? "true" : "false"}
        // 收起态整颗药丸可点展开（与原 mini 钮同语义）；内部按钮自行 stopPropagation
        onClick={isCollapsed ? () => quickBarActions.setCollapsed(key, false) : undefined}
      >
        <div className="qbar-head">
          <span className="qbar-label">{t("qbar.title")}</span>
          {/* 常驻挂载（空时 :empty 隐去）：收起形变靠 max-width/opacity 过渡，卸载就没有退场动画 */}
          <div className="qbar-chips">
            {chips.map((s) => (
              <button
                key={s.id}
                type="button"
                className="qbar-chip"
                onClick={() => onChip(s)}
              >
                {s.name}
                {s.variables.length > 0 && <span className="qbar-chip-var">｛{s.variables.length}｝</span>}
              </button>
            ))}
          </div>
          <button
            type="button"
            className="qbar-add"
            title={t("qbar.addTitle")}
            onClick={() => (pickerOpen ? setPickerOpen(false) : openPicker())}
          >
            {t("qbar.add")}
          </button>
          <button
            type="button"
            className="qbar-toggle"
            title={isCollapsed ? t("qbar.expand") : t("qbar.collapse")}
            onClick={(e) => {
              e.stopPropagation();
              if (!isCollapsed) setFilling(null);
              quickBarActions.setCollapsed(key, !isCollapsed);
            }}
          >
            {/* 箭头恒渲染 right，收起态由 CSS 旋转 180°——方向切换带旋转过渡 */}
            <span className="qbar-toggle-ico">
              <Chevron dir="right" />
            </span>
          </button>
        </div>

        {filling && !isCollapsed && (
          <SnippetFillForm
            snippet={filling}
            title={t("qbar.fill").replace("{n}", filling.name)}
            okLabel={t("sn.writeTerm")}
            onSubmit={submitFill}
            onCancel={() => setFilling(null)}
          />
        )}
      </div>

      {pickerOpen &&
        createPortal(
          <div className="modal-mask" {...maskDismiss}>
            <div className="modal form-dialog qbar-dialog" style={{ width: 520 }} onClick={(e) => e.stopPropagation()}>
              <div className="modal-head">
                <h2>{t("qbar.pick")}</h2>
                {/* 直达答疑：与顶栏问号同效，定位到「快捷命令栏」那一条 */}
                <button
                  type="button"
                  className="qbar-head-help"
                  title={t("qbar.faqTitle")}
                  onClick={() => jumpToFaq(FAQ_NO_QUICKBAR)}
                >
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                    <circle cx="12" cy="12" r="10" />
                    <path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3" />
                    <path d="M12 17h.01" />
                  </svg>
                </button>
                <button type="button" className="modal-x" onClick={closeDialog} aria-label={t("common.close")}>
                  ×
                </button>
              </div>
              <div className="qbar-picker-body">
              {items.length === 0 && <div className="qbar-picker-empty">{t("qbar.pickEmpty")}</div>}
              {/* 三个分类区必须常驻：它们不依赖 items.length，各区无内容时自带「暂无」占位。
                  旧写法把整块包在 items.length > 0 里，导致空片段库时连「＋」新建按钮都不渲染，
                  而空态提示又让用户「点分类旁的 ＋ 录入」——无的放矢，弹框里根本建不出第一条。 */}
                <>
                  {/* 三个分类区：全局/组是候选池（✓=已勾到本连接栏），本连接区=栏上私有命令；
                      区头右侧：一键应用（仅池）+ ＋（新建到本分类） */}
                  <div className="qbar-picker-sec">
                    <span>{t("qbar.secGlobal")}</span>
                    <span className="qbar-picker-sec-acts">
                      {globalItems.length > 0 && (
                        <button
                          type="button"
                          className="qbar-sec-btn"
                          onClick={() => applyAll(globalIds)}
                        >
                          {t("qbar.applyAll")}
                        </button>
                      )}
                      {globalItems.length > 0 && (
                        <button
                          type="button"
                          className="qbar-sec-btn"
                          onClick={() => unapplyAll(globalIds)}
                        >
                          {t("qbar.unapplyAll")}
                        </button>
                      )}
                      <button
                        type="button"
                        className="qbar-sec-add"
                        title={t("qbar.newInto").replace("{n}", t("qbar.secGlobal"))}
                        onClick={() => openNew("global")}
                      >
                        ＋
                      </button>
                    </span>
                  </div>
                  {globalItems.length === 0 ? (
                    <div className="qbar-picker-dim">{t("qbar.secEmpty")}</div>
                  ) : (
                    <QuickPickList
                      list={globalItems}
                      checkedIds={connIds}
                      onToggle={toggleBar}
                      onEdit={openEdit}
                      onRemove={(s, a) => delSnippet.confirm(s, a.x, a.y)}
                    />
                  )}
                  {groupKey !== "" && (
                    <>
                      <div className="qbar-picker-sec">
                        <span>{t("qbar.secGroup")}</span>
                        <span className="qbar-picker-sec-acts">
                          {groupItems.length > 0 && (
                            <button
                              type="button"
                              className="qbar-sec-btn"
                              onClick={() => applyAll(groupIds)}
                            >
                              {t("qbar.applyAll")}
                            </button>
                          )}
                          {groupItems.length > 0 && (
                            <button
                              type="button"
                              className="qbar-sec-btn"
                              onClick={() => unapplyAll(groupIds)}
                            >
                              {t("qbar.unapplyAll")}
                            </button>
                          )}
                          <button
                            type="button"
                            className="qbar-sec-add"
                            title={t("qbar.newInto").replace("{n}", t("qbar.secGroup"))}
                            onClick={() => openNew("group")}
                          >
                            ＋
                          </button>
                        </span>
                      </div>
                      {groupItems.length === 0 ? (
                        <div className="qbar-picker-dim">{t("qbar.secEmpty")}</div>
                      ) : (
                        <QuickPickList
                          list={groupItems}
                          checkedIds={connIds}
                          onToggle={toggleBar}
                          onEdit={openEdit}
                          onRemove={(s, a) => delSnippet.confirm(s, a.x, a.y)}
                        />
                      )}
                    </>
                  )}
                  <div className="qbar-picker-sec">
                    <span>{t("qbar.secConn")}</span>
                    <span className="qbar-picker-sec-acts">
                      <button
                        type="button"
                        className="qbar-sec-add"
                        title={t("qbar.newInto").replace("{n}", t("qbar.secConn"))}
                        onClick={() => openNew("conn")}
                      >
                        ＋
                      </button>
                    </span>
                  </div>
                  {connItems.length === 0 ? (
                    <div className="qbar-picker-dim">{t("qbar.connEmpty")}</div>
                  ) : (
                    <QuickPickList
                      list={connItems}
                      checkedIds={connIds}
                      onToggle={toggleBar}
                      onEdit={openEdit}
                      onRemove={(s, a) => delSnippet.confirm(s, a.x, a.y)}
                    />
                  )}
                </>
              </div>
            </div>
            {delSnippet.element}
          </div>,
          document.body,
        )}
      {/* 新建/编辑独立弹框：portal 排在 ＋ 面板之后，同 z-index 靠 DOM 序盖在其上 */}
      {form && (
        <SnippetFormDialog
          editing={form.editing}
          scopeName={t(
            form.scope === "global" ? "qbar.secGlobal" : form.scope === "group" ? "qbar.secGroup" : "qbar.secConn",
          )}
          onSave={(saved) => {
            if (!form.editing) adoptNew(form.scope, saved);
            setForm(null);
          }}
          onCancel={() => setForm(null)}
        />
      )}
    </>
  );
}
