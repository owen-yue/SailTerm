// 历史命令面板：展示后端 command_history 的脱敏命令流水。
// 数据通路早已存在（终端回车上报 history.record），本面板只做展示 + 筛选 + 单击插入 + 单条删除。
// 安全红线：命令只插入到当前会话提示符、不自动执行（与群发面板「不勾选执行」同款语义）；
// 文本入库前已由后端脱敏，前端拿不到未脱敏内容。
// 单实例跟随聚焦会话（停靠面板架构）：范围默认「本连接」（profile 维度，跨会话保留），
// 可切「本会话 / 本连接 / 全部」——后两者在前端对已取数据过滤，不另开 IPC 口径。
import { useCallback, useEffect, useMemo, useState } from "react";
import ConfirmDialog from "../dialog/ConfirmDialog";
import ContextMenu, { type MenuItem } from "../common/ContextMenu";
import Select from "../common/Select";
import { formatFull } from "../../lib/format";
import { useI18n } from "../../lib/i18n";
import { errMsg, history, sessions as sshApi } from "../../lib/tauri";
import { sessionStore, useStore } from "../../stores";
import { sessionLabel, type HistoryEntry } from "../../types";
import { IconRefresh, IconTrash } from "../common/icons";

/** 面板范围：本会话 / 本连接（profile，缺省）/ 全部连接。 */
type HistScope = "session" | "profile" | "all";

/** 单次取回上限：profile 桶后端封顶 1000，取满即全量，其余过滤在前端做。 */
const FETCH_LIMIT = 500;

export default function HistoryPanel() {
  const tr = useI18n();
  const { tabs, activeId } = useStore(sessionStore);
  const tab = tabs.find((x) => x.id === activeId) ?? null;
  const sessionId = tab?.id ?? "";
  // 重连会走 disconnected → connected；切标签 / 切范围都触发重取
  const loadKey = `${sessionId}:${tab?.state ?? ""}`;

  const [scope, setScope] = useState<HistScope>("profile");
  const [filter, setFilter] = useState("");
  const [items, setItems] = useState<HistoryEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [confirmClear, setConfirmClear] = useState(false);
  // 行右键菜单（共享 ContextMenu）：插入 / 删除都收在这里，行内不放按钮
  const [ctxMenu, setCtxMenu] = useState<{ x: number; y: number; entry: HistoryEntry } | null>(null);

  const load = useCallback(async () => {
    if (!sessionId) {
      setItems([]);
      return;
    }
    setLoading(true);
    try {
      // 「本连接」走 profile 桶；「本会话 / 全部」走全局桶再前端过滤——
      // 全局桶 limit 500 已覆盖单屏浏览需求，不做分页（超出部分靠刷新终端现场看）。
      const profileId = scope === "profile" ? tab?.profileId ?? "" : "";
      setItems(await history.list(profileId || null, FETCH_LIMIT));
      setError("");
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }, [sessionId, scope, tab?.profileId]);

  useEffect(() => {
    void load();
  }, [load, loadKey]);

  const rows = useMemo(() => {
    let list = items;
    if (scope === "session") list = list.filter((e) => e.sessionId === sessionId);
    const q = filter.trim().toLowerCase();
    if (q) list = list.filter((e) => e.command.toLowerCase().includes(q));
    return list;
  }, [items, scope, sessionId, filter]);

  /** 单击插入：命令原文写进聚焦会话的提示符，不带回车——执行与否交给用户。 */
  const insert = (cmd: string) => {
    if (!tab || tab.state !== "connected") return;
    void sshApi.write(tab.id, cmd).catch((e) => setError(errMsg(e)));
  };

  /** 来源会话友好名；会话已关闭（命不中标签页）返回 null——此时行内只给完整时间，不摆「已关闭」这类废话。 */
  const sessionLabelOf = (id: string | null): string | null => {
    const t2 = id ? tabs.find((x) => x.id === id) : null;
    return t2 ? sessionLabel(t2, tr) : null;
  };

  /** 单条删除：低危操作（命令可重新敲出），不弹确认；乐观移除，失败重取回滚。 */
  const removeOne = async (id: string) => {
    setItems((xs) => xs.filter((x) => x.id !== id));
    try {
      await history.remove(id);
      setError("");
    } catch (e) {
      setError(errMsg(e));
      await load();
    }
  };

  async function doClear() {
    // 清空口径跟随范围：本会话按其所属连接清（后端无 session 粒度），全部则整表清。
    const profileId = scope === "all" ? null : tab?.profileId ?? null;
    try {
      await history.clear(profileId);
      setError("");
      await load();
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setConfirmClear(false);
    }
  }

  return (
    <>
    <div className="ap">
      <div className="ap-head">
        <span className="fp-title">{tr("panel.history")}</span>
        <label className="ap-filter">
          {tr("hist.scope")}
          <Select
            className="mini"
            value={scope}
            options={(["session", "profile", "all"] as HistScope[]).map((s) => ({
              value: s,
              label: tr(`hist.scope.${s}`),
            }))}
            onChange={(v) => setScope(v as HistScope)}
          />
        </label>
        <input
          className="hist-filter"
          type="search"
          placeholder={tr("hist.filter")}
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <span className="ap-sum">{tr("hist.count").replace("{n}", String(rows.length))}</span>
        <span className="fp-acts">
          <button
            className="icon-btn"
            title={tr("common.refresh")}
            disabled={loading || !sessionId}
            onClick={() => void load()}
          >
            <IconRefresh />
          </button>
          <button
            className="icon-btn danger"
            title={scope === "all" ? tr("hist.clearAllTip") : tr("hist.clearConnTip")}
            disabled={rows.length === 0}
            onClick={() => setConfirmClear(true)}
          >
            <IconTrash />
          </button>
        </span>
      </div>
      {error && <div className="form-error">{error}</div>}
      {!tab && <div className="ss-empty">{tr("hist.empty")}</div>}
      {tab && (
        <div className="ap-list">
          {rows.length === 0 && <div className="ss-empty">{tr("hist.noMatch")}</div>}
          {rows.map((e) => {
            const src = e.source === "ai" ? "AI" : sessionLabelOf(e.sessionId);
            const stamp = formatFull(e.createdAt);
            const meta = src ? `${src} · ${stamp}` : stamp;
            return (
              <div
                key={e.id}
                className="hist-row"
                title={`${e.command}\n${meta}`}
                onContextMenu={(ev) => {
                  ev.preventDefault();
                  setCtxMenu({ x: ev.clientX, y: ev.clientY, entry: e });
                }}
              >
                <button
                  type="button"
                  className="hist-main"
                  disabled={tab.state !== "connected"}
                  onClick={() => insert(e.command)}
                >
                  <span className="hist-dot">●</span>
                  <code className="hist-cmd">{e.command}</code>
                  <span className="hist-meta">{meta}</span>
                </button>
              </div>
            );
          })}
        </div>
      )}
    </div>
    {ctxMenu && (
      <ContextMenu
        x={ctxMenu.x}
        y={ctxMenu.y}
        items={
          [
            {
              label: tr("hist.ctxInsert"),
              disabled: tab?.state !== "connected",
              onClick: () => insert(ctxMenu.entry.command),
            },
            { label: tr("hist.del"), danger: true, onClick: () => void removeOne(ctxMenu.entry.id) },
          ] as MenuItem[]
        }
        onClose={() => setCtxMenu(null)}
      />
    )}
    {confirmClear && (
      <ConfirmDialog
        title={tr("hist.clearTitle")}
        message={
          scope === "all"
            ? tr("hist.clearAllMsg")
            : tr("hist.clearConnMsg").replace("{n}", String(rows.length))
        }
        warning={tr("hist.clearWarn")}
        okText={tr("common.delete")}
        onOk={() => void doClear()}
        onClose={() => setConfirmClear(false)}
      />
    )}
    </>
  );
}
