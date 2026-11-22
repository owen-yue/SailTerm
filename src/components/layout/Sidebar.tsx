import { useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { profiles as profilesApi, dialog } from "../../lib/tauri";
import { errMsg } from "../../lib/tauri/client";
import { connectionStore, connections, sessionActions, sessionStore, useStore } from "../../stores";
import { DEFAULT_PORT, KIND_LABEL } from "../../types";
import { useI18n } from "../../lib/i18n";
import type { ImportReport, Profile } from "../../types";
import ContextMenu, { type MenuItem } from "../common/ContextMenu";
import { IconClose } from "../common/icons";
import PromptDialog from "../dialog/PromptDialog";
import GroupQuickBarDialog from "../dialog/GroupQuickBarDialog";
import Popconfirm from "../common/Popconfirm";
import StatusDot, { statusLabel } from "../common/StatusDot";

interface Props {
  onNew: () => void;
  onEdit: (p: Profile) => void;
  /** 当前在编辑面板中展示的连接 id（用于高亮）。*/
  selectedId?: string | null;
}

/** 判定「按下即拖」还是「只是点击」的最小位移（px），与 lib/dragDockTab 一致。 */
const DRAG_THRESHOLD = 4;

/** 连接在侧栏的状态点：未连接 / 连接中 / 已连接 / 已断开。 */
type Status = "idle" | "connecting" | "connected" | "disconnected";

interface Section {
  id: string;
  name: string;
  items: Profile[];
}

/** 左侧连接栏：分组、搜索、状态点、编辑/删除入口。*/
export default function Sidebar({ onNew, onEdit, selectedId }: Props) {
  const t = useI18n();
  const { profiles, groups, loading } = useStore(connectionStore);
  const { tabs, activeId } = useStore(sessionStore);
  const [keyword, setKeyword] = useState("");
  const [collapsed, setCollapsed] = useState<string[]>([]);
  // 删连接改为气泡确认（Popconfirm）：在右键菜单位置就地弹小卡片确认，不再弹模态
  const [deleteProfile, setDeleteProfile] = useState<{ profile: Profile; x: number; y: number } | null>(null);
  // 「改分组」改为选分组模态（不再手动输入）
  const [groupPick, setGroupPick] = useState<Profile | null>(null);
  // 新建分组改为弹框（复用 PromptDialog）：不再在列表顶部内联输入，避免撑开列表布局
  const [addingGroup, setAddingGroup] = useState(false);
  // 分组行右键菜单：重命名 / 删除分组（与连接行交互对齐）
  const [groupMenu, setGroupMenu] = useState<{ id: string; name: string; x: number; y: number } | null>(null);
  // 删分组改为气泡确认（与删连接同一套 Popconfirm）：不再在分组头就地确认
  const [deleteGroup, setDeleteGroup] = useState<{ id: string; name: string; x: number; y: number } | null>(null);
  // 重命名分组弹框（复用 PromptDialog）
  const [renameGroup, setRenameGroup] = useState<{ id: string; name: string } | null>(null);
  // 分组默认快捷栏弹窗：组内未自定义的连接继承这份列表
  const [groupQBar, setGroupQBar] = useState<{ id: string; name: string } | null>(null);
  // 备份主密码弹框：exportFull=设置并二次确认（加密导出），import=解锁输入（解密导入）。
  const [pwd, setPwd] = useState<
    { kind: "exportFull"; path: string } | { kind: "import"; path: string } | null
  >(null);
  const [prompt, setPrompt] = useState<"rename" | "group" | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; profile: Profile } | null>(null);
  const [moreMenu, setMoreMenu] = useState<{ x: number; y: number } | null>(null);
  // 列表空白处右键菜单：新建连接 / 新建分组（连接行自身的右键已在 openMenu 里 stopPropagation）
  const [listMenu, setListMenu] = useState<{ x: number; y: number } | null>(null);
  const [menuTarget, setMenuTarget] = useState<Profile | null>(null);
  const [notice, setNotice] = useState<{ msg: string; err: boolean } | null>(null);
  /** 上一条提示的消失计时器：旧写法每次 flash 新开一个 timer，后一条会被前一条的时间提前抹掉。 */
  const flashTimer = useRef<number>(0);
  useEffect(() => () => window.clearTimeout(flashTimer.current), []);
  // 拖拽手动排序（分组 + 组内服务器）：drag 记当前拖起项（仅用于源行半透明反馈）。
  // 用 pointer 手写而非 HTML5 DnD——后者与原生文件拖放（dragDropEnabled）互斥，
  // 见 lib/dragDockTab 顶部说明。搜索态禁用（过滤视图里的顺序不代表真实顺序，拖了会误导）。
  const [drag, setDrag] = useState<{
    type: "group" | "profile";
    id: string;
    section?: string;
  } | null>(null);
  /** 正在建连的档案 id 集合：逐行给「连接中」脉冲反馈。用集合而非单值，
   *  才能允许一条卡住（慢/连不上）时仍点击其它服务器，只锁同一行的重复点击。 */
  const [connectingIds, setConnectingIds] = useState<Set<string>>(() => new Set());

  const flash = (msg: string, err = false) => {
    setNotice({ msg, err });
    window.clearTimeout(flashTimer.current);
    flashTimer.current = window.setTimeout(() => setNotice(null), 4000);
  };
  /** 从 ~/.ssh/config 批量导入（无凭证；同 host:port:user 跳过）。 */
  const importSshConfig = async () => {
    try {
      const entries = await profilesApi.parseSshConfig();
      if (entries.length === 0) {
        flash(t("sidebar.importNone"));
        return;
      }
      let n = 0;
      // 单条写入失败不中断整批：旧写法第一条报错就抛到外层 catch，剩下几十条白扫。
      let bad = 0;
      for (const e of entries) {
        const exists = profiles.some(
          (p) => p.host === e.host && p.port === e.port && p.username === e.username,
        );
        if (exists) continue;
        try {
          await connections.save({
            name: e.name,
            kind: "ssh",
            host: e.host,
            port: e.port,
            username: e.username,
            authType: "privateKey",
            privateKeyPath: e.identityFile ?? null,
            jumpProfileId: null,
          });
          n += 1;
        } catch {
          bad += 1;
        }
      }
      flash(
        t("sidebar.imported")
          .replace("{n}", String(n))
          .replace("{total}", String(entries.length)) +
          (bad ? t("sidebar.importFailed").replace("{n}", String(bad)) : ""),
        bad > 0,
      );
    } catch (err) {
      // errMsg：后端错误体不是 Error 实例，String(err) 只会给出一块 [object Object]
      flash(errMsg(err), true);
    }
  };
  const doExport = async (path: string) => {
    const r = await connections.exportToFile(path, null);
    if (r)
      flash(
        t("sidebar.exported").replace("{profiles}", String(r.profiles)).replace("{groups}", String(r.groups)) +
          (r.providers ? t("sidebar.exportedProviders").replace("{n}", String(r.providers)) : ""),
      );
  };
  /** 导入备份（master 可选）：直连 api，以便「主密码错误」等异常抛给调用方（弹框内联显示、不关闭）。 */
  const doImport = async (path: string, master?: string) => {
    const r = await profilesApi.importFile(path, true, master);
    await connections.refresh();
    flash(importSummary(r));
  };
  const importSummary = (r: ImportReport) =>
    t("sidebar.importResult")
      .replace("{created}", String(r.profilesCreated))
      .replace("{groups}", String(r.groupsCreated))
      .replace("{skipped}", String(r.profilesSkipped)) +
      (r.providersCreated
        ? t("sidebar.importProviders").replace("{n}", String(r.providersCreated))
        : "") +
      (r.credentialsApplied
        ? t("sidebar.importCreds").replace("{n}", String(r.credentialsApplied))
        : "") +
      (r.errors.length ? t("sidebar.importInvalid").replace("{n}", String(r.errors.length)) : "");

  /** 导出备份（不含凭证）：弹原生另存为框选目标文件。 */
  const pickExport = async () => {
    try {
      const path = await dialog.saveFile({
        title: t("sidebar.exportTitle"),
        defaultName: "sailterm-connections.sailbak",
        filterName: t("sidebar.backupFilter"),
        extensions: ["sailbak"],
      });
      if (path) await doExport(path);
    } catch (e) {
      flash(errMsg(e), true);
    }
  };
  /** 导出完整备份（含密钥）：先选目标文件，再弹框设置并确认主密码。 */
  const pickExportFull = async () => {
    try {
      const path = await dialog.saveFile({
        title: t("sidebar.exportFullTitle"),
        defaultName: "sailterm-backup-full.sailbak",
        filterName: t("sidebar.backupFilter"),
        extensions: ["sailbak"],
      });
      if (path) setPwd({ kind: "exportFull", path });
    } catch (e) {
      flash(errMsg(e), true);
    }
  };
  /** 导入备份：选文件后预检——含加密凭证则先弹主密码解锁，否则直接导入。 */
  const pickImport = async () => {
    try {
      const path = await dialog.pickFile(t("sidebar.importTitle"));
      if (!path) return;
      const info = await connections.inspectBackupFile(path);
      if (info?.hasCredentials) setPwd({ kind: "import", path });
      else await doImport(path);
    } catch (e) {
      flash(errMsg(e), true);
    }
  };
  /** 主密码弹框提交：exportFull 加密导出（已二次确认），import 用密码解密导入。 */
  const submitPwd = async (master: string) => {
    if (!pwd) return;
    if (pwd.kind === "exportFull") {
      const r = await profilesApi.exportFile(pwd.path, null, {
        masterPassword: master,
        includeCredentials: true,
      });
      flash(
        t("sidebar.exportedFull")
          .replace("{secrets}", String(r.secrets))
          .replace("{profiles}", String(r.profiles))
          .replace("{groups}", String(r.groups)) +
          (r.providers ? t("sidebar.exportedProviders").replace("{n}", String(r.providers)) : ""),
      );
    } else {
      await doImport(pwd.path, master);
    }
  };

  const kw = keyword.trim().toLowerCase();
  const visible = kw
    ? profiles.filter((p) =>
        [p.name, p.host, p.username, p.kind].some((v) => v.toLowerCase().includes(kw)),
      )
    : profiles;

  const sections: Section[] = [
    ...groups.map((g) => ({ id: g.id, name: g.name, items: visible.filter((p) => p.groupId === g.id) })),
    { id: "", name: t("common.ungrouped"), items: visible.filter((p) => !p.groupId) },
  ].filter((s) => !kw || s.items.length > 0);

  const statusOf = (profileId: string): Status => {
    const tab = tabs.find((t) => t.profileId === profileId);
    if (!tab) return "idle";
    // 与页签 TabBar.dotState 同口径：phase 比 state 更准——握手期间 tab 已建、state 仍
    // disconnected，只有 phase==="pending" 才代表「连接中」；漏判 phase 会把连接中显成红点。
    if (tab.phase === "pending") return "connecting";
    if (tab.phase === "failed") return "disconnected";
    return tab.state === "connected" ? "connected" : "disconnected";
  };

  const toggle = (id: string) =>
    setCollapsed((prev) => (prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id]));

  const openMenu = (e: React.MouseEvent, profile: Profile) => {
    e.preventDefault();
    e.stopPropagation();
    setMenuTarget(profile);
    setMenu({ x: e.clientX, y: e.clientY, profile });
  };

  /** 发起连接并就地给出「连接中」反馈：仅锁定**同一行**的重复点击，
   *  不影响其它服务器（一条卡住时仍可连别的）。失败时 sessionStore.error 经 App 顶部 toast 呈现。 */
  const runConnect = async (id: string, fn: () => Promise<string | null>) => {
    if (connectingIds.has(id)) return;
    setConnectingIds((prev) => new Set(prev).add(id));
    try {
      await fn();
    } finally {
      setConnectingIds((prev) => {
        if (!prev.has(id)) return prev;
        const next = new Set(prev);
        next.delete(id);
        return next;
      });
    }
  };

  // 右键菜单「连接」= 新标签连接（同一服务器可开多条）；单实例复用/失败重连走页面点击与「保存并连接」，
  // 故不再提供旧的 openProfile 菜单项（此前「连接」+「新建连接」两项语义重复且文案误导）。
  const menuItems = (p: Profile, ax: number, ay: number): MenuItem[] => [
    { label: t("sidebar.menu.connect"), onClick: () => void runConnect(p.id, () => sessionActions.openProfileNew(p.id)) },
    {
      label: t("common.rename"),
      sep: true,
      onClick: () => {
        setMenuTarget(p);
        setPrompt("rename");
      },
    },
    {
      label: t("sidebar.menu.changeGroup"),
      onClick: () => setGroupPick(p),
    },
    {
      label: t("common.copy"),
      onClick: () => void connections.duplicate(p.id).then(() => flash(t("sidebar.copied"))),
    },
    {
      label: t("common.edit"),
      sep: true,
      onClick: () => onEdit(p),
    },
    {
      label: t("common.delete"),
      danger: true,
      onClick: () => setDeleteProfile({ profile: p, x: ax, y: ay }),
    },
  ];

  /** 把当前连接移到指定分组（null = 未分组）。 */
  const moveToGroup = async (groupId: string | null) => {
    if (!groupPick) return;
    await connections.patch(groupPick.id, { groupId });
    setGroupPick(null);
  };

  /** 分组落点：把 srcId 移到 targetId 前/后，提交全量分组新顺序。 */
  const commitGroupDrop = (srcId: string, targetId: string, after: boolean) => {
    const ids = connectionStore
      .get()
      .groups.map((g) => g.id)
      .filter((id) => id !== srcId);
    let at = ids.indexOf(targetId);
    at = at < 0 ? ids.length : at + (after ? 1 : 0);
    ids.splice(at, 0, srcId);
    void connections.reorderGroups(ids);
  };

  /** 连接落点（限同组）：把 srcId 移到 targetId 前/后，提交全量连接新顺序。 */
  const commitProfileDrop = (srcId: string, targetId: string, after: boolean) => {
    const ids = connectionStore
      .get()
      .profiles.map((p) => p.id)
      .filter((id) => id !== srcId);
    let at = ids.indexOf(targetId);
    at = at < 0 ? ids.length : at + (after ? 1 : 0);
    ids.splice(at, 0, srcId);
    void connections.reorderProfiles(ids);
  };

  /**
   * 起一次 pointer 拖拽排序：按下行/组头 → 移动过阈值判定拖拽 → 跟随画插入线 + 幽灵胶囊
   * → 松手按落点提交重排。不用 HTML5 DnD 是因其与原生文件拖放互斥（见 lib/dragDockTab）。
   * 未在 pointerdown 上 preventDefault，以保证未移动时的 click / dblclick 照常触发。
   */
  const startReorder = (
    src: { type: "group" | "profile"; id: string; section: string },
    label: string,
    e: ReactPointerEvent<HTMLElement>,
  ) => {
    if (e.button !== 0 || kw) return; // 仅左键；搜索态禁用
    const startX = e.clientX;
    const startY = e.clientY;
    let moved = false;
    let caretEl: HTMLElement | null = null;
    let ghost: HTMLElement | null = null;
    let hit: { id: string; after: boolean } | null = null;

    const clearCaret = () => {
      caretEl?.remove();
      caretEl = null;
    };
    const makeGhost = () => {
      ghost = document.createElement("div");
      ghost.className = "dock-ghost";
      const txt = document.createElement("span");
      txt.textContent = label;
      ghost.appendChild(txt);
      document.body.appendChild(ghost);
    };
    const placeGhost = (x: number, y: number) => {
      if (ghost) ghost.style.transform = `translate(${x + 14}px, ${y + 16}px)`;
    };
    const showCaret = (t: { id: string; after: boolean }) => {
      const sel =
        src.type === "profile" ? `[data-conn-id="${t.id}"]` : `[data-group-id="${t.id}"]`;
      const row = document.querySelector<HTMLElement>(sel);
      if (!row) return;
      const r = row.getBoundingClientRect();
      if (!caretEl) {
        caretEl = document.createElement("div");
        caretEl.className = "dock-caret";
        document.body.appendChild(caretEl);
      }
      caretEl.style.left = `${r.left + 6}px`;
      caretEl.style.top = `${(t.after ? r.bottom : r.top) - 1}px`;
      caretEl.style.width = `${Math.max(0, r.width - 12)}px`;
      caretEl.style.height = "2px";
    };
    // 命中测试：连接仅同组内可落（跨组换分组是另一回事，走右键“改分组”）；组头排除“未分组”。
    const hitTest = (x: number, y: number) => {
      const point = document.elementFromPoint(x, y);
      if (src.type === "profile") {
        const row = point?.closest<HTMLElement>("[data-conn-id]");
        if (!row || row.dataset.connId === src.id || row.dataset.section !== src.section)
          return null;
        const r = row.getBoundingClientRect();
        return { id: row.dataset.connId!, after: y > r.top + r.height / 2 };
      }
      const head = point?.closest<HTMLElement>("[data-group-id]");
      const gid = head?.dataset.groupId;
      if (!head || !gid || gid === src.id) return null;
      const r = head.getBoundingClientRect();
      return { id: gid, after: y > r.top + r.height / 2 };
    };

    const onMove = (ev: PointerEvent) => {
      if (!moved) {
        if (Math.hypot(ev.clientX - startX, ev.clientY - startY) < DRAG_THRESHOLD) return;
        moved = true;
        document.getSelection()?.removeAllRanges();
        document.body.classList.add("slist-dragging");
        setDrag(src);
        makeGhost();
      }
      ev.preventDefault(); // 拖拽中阻止浏览器原生选文字
      placeGhost(ev.clientX, ev.clientY);
      hit = hitTest(ev.clientX, ev.clientY);
      if (hit) showCaret(hit);
      else clearCaret();
    };
    const teardown = () => {
      document.removeEventListener("pointermove", onMove, true);
      document.removeEventListener("pointerup", onUp, true);
      document.removeEventListener("pointercancel", onUp, true);
      document.body.classList.remove("slist-dragging");
      ghost?.remove();
      clearCaret();
      setDrag(null);
    };
    const onUp = () => {
      const wasMoved = moved;
      const t = hit;
      teardown();
      if (!wasMoved) return; // 只是点击：不吞事件，onClick/onDoubleClick 照常
      // 拖拽后浏览器会补发一次 click，捕获阶段吞掉，避免误触发切页/收起
      const swallow = (ce: MouseEvent) => {
        ce.stopPropagation();
        ce.preventDefault();
        window.removeEventListener("click", swallow, true);
      };
      window.addEventListener("click", swallow, true);
      if (!t) return;
      if (src.type === "group") commitGroupDrop(src.id, t.id, t.after);
      else commitProfileDrop(src.id, t.id, t.after);
    };

    document.addEventListener("pointermove", onMove, true);
    document.addEventListener("pointerup", onUp, true);
    document.addEventListener("pointercancel", onUp, true);
  };

  return (
    <aside className="sidebar">
      <div className="side-head">
        <span>{t("sidebar.header")}</span>
        <span className="side-count">{profiles.length}</span>
        {/* 新建分组（低频）已收进下方 ⋯ 菜单；头部只留 ⋯，避免与底部「＋ 连接」两个新建入口一上一下割裂 */}
        <button
          className="icon-btn"
          title={t("sidebar.more")}
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            setMoreMenu({ x: r.left, y: r.bottom + 4 });
          }}
        >
          ⋯
        </button>
      </div>

      <div className="side-search">
        <input
          value={keyword}
          onChange={(e) => setKeyword(e.target.value)}
          placeholder={t("common.search")}
          spellCheck={false}
        />
        {keyword && (
          <button
            type="button"
            className="icon-btn"
            title={t("sidebar.clear")}
            onClick={() => setKeyword("")}
          >
            <IconClose />
          </button>
        )}
      </div>

      <div
        className="side-list"
        onContextMenu={(e) => {
          e.preventDefault();
          setListMenu({ x: e.clientX, y: e.clientY });
        }}
      >
        {loading && profiles.length === 0 && <div className="side-empty">{t("common.loading")}</div>}
        {!loading && profiles.length === 0 && <div className="side-empty">{t("sidebar.empty")}</div>}
        {profiles.length > 0 && visible.length === 0 && <div className="side-empty">{t("common.noMatch")}</div>}

        {sections.map((s) => (
          <section key={s.id || "__ungrouped"} className="group">
            <div
              className={`group-head${
                drag?.type === "group" && drag.id === s.id ? " dragging" : ""
              }`}
              data-group-id={s.id}
              data-draggable={s.id && !kw ? "1" : undefined}
              onClick={() => toggle(s.id)}
              onPointerDown={(e) => {
                if (s.id) startReorder({ type: "group", id: s.id, section: s.id }, s.name, e);
              }}
              onContextMenu={(e) => {
                // 「未分组」表头没有可改名/删除的对象，不弹菜单，冒泡给列表空白菜单
                if (!s.id) return;
                e.preventDefault();
                e.stopPropagation();
                setGroupMenu({ id: s.id, name: s.name, x: e.clientX, y: e.clientY });
              }}
            >
              <span className="caret">{collapsed.includes(s.id) ? "▸" : "▾"}</span>
              <span className="gname">{s.name}</span>
            </div>

            {!collapsed.includes(s.id) &&
              s.items.map((p) => {
                const connecting = connectingIds.has(p.id);
                const status = connecting ? "connecting" : statusOf(p.id);
                const live = tabs.find((t) => t.profileId === p.id);
                return (
                  <div
                    key={p.id}
                    className={`conn${live && live.id === activeId ? " active" : ""}${
                      drag?.type === "profile" && drag.id === p.id ? " dragging" : ""
                    }${connecting ? " connecting" : ""}`}
                    data-conn-id={p.id}
                    data-section={s.id}
                    data-selected={selectedId === p.id ? "1" : undefined}
                    data-draggable={!kw ? "1" : undefined}
                    title={`${p.username}@${p.host}:${p.port} · ${statusLabel(t, status)}${p.notes ? "\n— " + p.notes : ""}`}
                    onDoubleClick={() => void runConnect(p.id, () => sessionActions.openProfileNew(p.id))}
                    onContextMenu={(e) => openMenu(e, p)}
                    onPointerDown={(e) =>
                      startReorder({ type: "profile", id: p.id, section: s.id }, p.name, e)
                    }
                  >
                    <StatusDot state={status} tooltip={false} />
                    <span className="meta">
                      <span className="nm">
                        {p.name}
                        {p.kind !== "ssh" && <em className="kind">{t(KIND_LABEL[p.kind])}</em>}
                      </span>
                      <span className="sub">
                        {p.username}@{p.host}
                        {/* 只在端口偏离该协议默认值时才显示，免得每行都拖个 :22 */}
                        {p.port !== DEFAULT_PORT[p.kind] ? `:${p.port}` : ""}
                      </span>
                    </span>
                  </div>
                );
              })}
          </section>
        ))}
      </div>

      {notice && (
        <div className="side-notice" data-err={notice.err || undefined} role={notice.err ? "alert" : "status"}>
          {notice.msg}
        </div>
      )}

      <button className="btn-new" onClick={onNew}>
        {t("sidebar.addBtn")}
      </button>

      {prompt && (
        <PromptDialog
          title={prompt === "rename" ? t("common.rename") : t("sidebar.newGroup")}
          label={prompt === "rename" ? t("common.name") : t("sidebar.groupName")}
          okText={prompt === "rename" ? t("common.rename") : undefined}
          initial={
            prompt === "rename"
              ? (menuTarget?.name ?? "")
              : (groups.find((g) => g.id === menuTarget?.groupId)?.name ?? "")
          }
          onSubmit={async (v) => {
            const tgt = menuTarget;
            if (!tgt) return;
            if (prompt === "rename") {
              await connections.patch(tgt.id, { name: v.trim() || tgt.name });
              return;
            }
            const name = v.trim();
            if (!name) {
              await connections.patch(tgt.id, { groupId: null });
              return;
            }
            let g = groups.find((x) => x.name === name);
            if (!g) {
              await connections.createGroup(name);
              g = connectionStore.get().groups.find((x) => x.name === name);
            }
            await connections.patch(tgt.id, { groupId: g?.id ?? null });
          }}
          onClose={() => setPrompt(null)}
        />
      )}

      {addingGroup && (
        <PromptDialog
          title={t("sidebar.newGroup")}
          label={t("sidebar.groupName")}
          onSubmit={async (v) => {
            await connections.createGroup(v);
          }}
          onClose={() => setAddingGroup(false)}
        />
      )}

      {deleteProfile && (
        <Popconfirm
          x={deleteProfile.x}
          y={deleteProfile.y}
          title={t("sidebar.deleteConnMsg")
            .replace("{name}", deleteProfile.profile.name)
            .replace("{who}", `${deleteProfile.profile.username}@${deleteProfile.profile.host}`)}
          description={t("sidebar.deleteConnWarn")}
          onConfirm={() => {
            const id = deleteProfile.profile.id;
            setDeleteProfile(null);
            void connections.remove(id);
          }}
          onClose={() => setDeleteProfile(null)}
        />
      )}

      {groupPick && (
        <div
          className="modal-mask"
          onMouseDown={(e) => {
            if (e.target === e.currentTarget) setGroupPick(null);
          }}
        >
          <div className="modal prompt">
            <div className="modal-head">
              <h2>{t("sidebar.moveToGroup")}</h2>
              <button
                type="button"
                className="modal-x"
                title={t("common.close")}
                onClick={() => setGroupPick(null)}
              >
                ×
              </button>
            </div>
            <div className="hk-body">
              <div className="gp-list">
                <button
                  type="button"
                  className="gp-item"
                  data-on={!groupPick.groupId}
                  onClick={() => void moveToGroup(null)}
                >
                  {t("common.ungrouped")}
                </button>
                {groups.map((g) => (
                  <button
                    key={g.id}
                    type="button"
                    className="gp-item"
                    data-on={g.id === groupPick.groupId}
                    onClick={() => void moveToGroup(g.id)}
                  >
                    {g.name}
                  </button>
                ))}
              </div>
            </div>
            <div className="form-actions">
              <button
                type="button"
                className="ghost"
                onClick={() => {
                  setMenuTarget(groupPick);
                  setGroupPick(null);
                  setPrompt("group");
                }}
              >
                {t("sidebar.newGroupEllipsis")}
              </button>
              <button type="button" onClick={() => setGroupPick(null)}>
                {t("common.close")}
              </button>
            </div>
          </div>
        </div>
      )}

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={menuItems(menu.profile, menu.x, menu.y)}
          onClose={() => setMenu(null)}
        />
      )}

      {listMenu && (
        <ContextMenu
          x={listMenu.x}
          y={listMenu.y}
          items={[
            { label: t("sidebar.newConn"), onClick: onNew },
            { label: t("sidebar.newGroup"), sep: true, onClick: () => setAddingGroup(true) },
          ]}
          onClose={() => setListMenu(null)}
        />
      )}

      {groupMenu && (
        <ContextMenu
          x={groupMenu.x}
          y={groupMenu.y}
          items={[
            {
              label: t("qbar.groupCmds"),
              onClick: () => setGroupQBar({ id: groupMenu.id, name: groupMenu.name }),
            },
            {
              label: t("common.rename"),
              onClick: () => setRenameGroup({ id: groupMenu.id, name: groupMenu.name }),
            },
            {
              label: t("sidebar.deleteGroup"),
              danger: true,
              sep: true,
              onClick: () => setDeleteGroup(groupMenu),
            },
          ]}
          onClose={() => setGroupMenu(null)}
        />
      )}

      {renameGroup && (
        <PromptDialog
          title={t("common.rename")}
          label={t("sidebar.groupName")}
          okText={t("common.rename")}
          initial={renameGroup.name}
          onSubmit={async (v) => {
            await connections.renameGroup(renameGroup.id, v);
          }}
          onClose={() => setRenameGroup(null)}
        />
      )}

      {groupQBar && (
        <GroupQuickBarDialog
          groupId={groupQBar.id}
          groupName={groupQBar.name}
          onClose={() => setGroupQBar(null)}
        />
      )}

      {deleteGroup && (
        <Popconfirm
          x={deleteGroup.x}
          y={deleteGroup.y}
          title={t("sidebar.deleteGroupMsg").replace("{name}", deleteGroup.name)}
          description={t("sidebar.deleteGroupWarn")}
          onConfirm={() => {
            const id = deleteGroup.id;
            setDeleteGroup(null);
            void connections.deleteGroup(id);
          }}
          onClose={() => setDeleteGroup(null)}
        />
      )}

      {moreMenu && (
        <ContextMenu
          x={moreMenu.x}
          y={moreMenu.y}
          items={[
            { label: t("sidebar.newGroup"), onClick: () => setAddingGroup(true) },
            { label: t("sidebar.importSshConfig"), sep: true, onClick: () => void importSshConfig() },
            { label: t("sidebar.importTitle"), onClick: () => void pickImport() },
            { label: t("sidebar.exportMenu"), sep: true, onClick: () => void pickExport() },
            { label: t("sidebar.exportFullMenu"), onClick: () => void pickExportFull() },
          ]}
          onClose={() => setMoreMenu(null)}
        />
      )}

      {pwd && (
        <PromptDialog
          title={pwd.kind === "exportFull" ? t("sidebar.pwdSetTitle") : t("sidebar.pwdUnlockTitle")}
          label={t("sidebar.pwdLabel")}
          confirmLabel={t("sidebar.pwdConfirmLabel")}
          confirm={pwd.kind === "exportFull"}
          secret
          hint={pwd.kind === "exportFull" ? t("sidebar.pwdSetHint") : t("sidebar.pwdUnlockHint")}
          onSubmit={submitPwd}
          onClose={() => setPwd(null)}
        />
      )}
    </aside>
  );
}
