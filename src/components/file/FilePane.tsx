import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import ConfirmDialog from "../dialog/ConfirmDialog";
import Popconfirm from "../common/Popconfirm";
import {
  IconArrowDown,
  IconArrowUp,
  IconClose,
  IconFile,
  IconFolder,
  IconHome,
  IconLock,
  IconPlus,
  IconRefresh,
  IconTarget,
  IconZap,
} from "../common/icons";
import { file as fileApi, local as localApi, plugins as pluginApi, sessions as sessionApi } from "../../lib/tauri";
import { errMsg } from "../../lib/tauri/client";
import ContextMenu, { type MenuItem } from "../common/ContextMenu";
import FileEditor from "./FileEditor";
import ChmodDialog from "./ChmodDialog";
import { copyText } from "../../lib/clipboard";
import { useMaskDismiss, useEscClose } from "../../lib/useOverlay";
import { writePluginOutput } from "../../lib/termAgent";
import { zmodem } from "../../lib/tauri";
import {
  beginDrag,
  canAccept,
  endDrag,
  isDragSource,
  readDragEntry,
  setDragEntry,
} from "../../lib/drag";
import { formatBytes, formatTime } from "../../lib/format";
import { isSensitive } from "../../lib/sensitive";
import {
  aiChat,
  childPath,
  files,
  fileStore,
  paneOf,
  sessionStore,
  settingsActions,
  transferSelected,
  useStore,
  type Lane,
} from "../../stores";
import { hasCap, type Capability } from "../../lib/capabilities";
import type { FileEntry } from "../../types";
import { useI18n } from "../../lib/i18n";

interface Props {
  /** 本栏定位：本地栏全局共享，远端栏按会话分片（带 session）。 */
  lane: Lane;
  /** 栏标题文字，如「本地」「web-01」。 */
  title: string;
  /** 栏标题前的扁平线稿图标（本地=笔记本、远端=显示器/地球）。 */
  icon?: ReactNode;
  /**
   * 远端栏绑定的是 FTP/FTPS 会话（本地栏恒为 false）。
   * FTP 没有统一的改权限命令（SITE CHMOD 只是各家私有扩展），「权限…」入口需隐藏。
   */
  ftp?: boolean;
  /**
   * 会话当前是否已连接（只对远端栏有意义，缺省视为已连接）。
   * 断线后远端栏必须整体禁用：会话 id 还在，但后端已经没有连接，
   * 此时任何刷新/新建/拖放都只会换来一条「连接已断开」或误导性的本地 IO 报错。
   */
  connected?: boolean;
}

/** 输入弹窗（改权限）；重命名与新建目录都已改为行内编辑，不再走弹窗。 */
type Prompt = { kind: "chmod"; entry: FileEntry } | null;

/** 右键菜单状态；`entry` 为 null 表示点在空白处。 */
type Menu = { x: number; y: number; entry: FileEntry | null } | null;

/** 明确的二进制/媒体/文档容器扩展名：这些「打开」走系统默认程序，不进文本编辑器。
 *  未知扩展名默认当文本（后端读文件时检出 NUL 字节会优雅拒绝），避免误把日志/配置挡在编辑器外。 */
const BINARY_EXT = new Set([
  "jpg", "jpeg", "png", "gif", "bmp", "webp", "ico", "tif", "tiff", "heic", "heif", "avif", "svgz", "psd", "ai",
  "mp4", "m4v", "mkv", "avi", "mov", "wmv", "flv", "webm", "mp3", "wav", "flac", "aac", "ogg", "m4a", "opus", "mid", "midi",
  "zip", "rar", "7z", "gz", "tgz", "bz2", "xz", "zst", "tar", "iso", "cab", "jar", "war", "apk", "deb", "rpm", "msi", "exe", "dll", "so", "dylib", "bin", "dat", "sys", "ocx",
  "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "pdf", "epub", "mobi", "dwg", "dxf", "skp", "stl", "step", "stp",
  "db", "sqlite", "sqlite3", "class", "pyc", "woff", "woff2", "ttf", "otf", "eot", "pdb", "ilk", "obj", "lib", "a",
]);
function isTextLike(name: string): boolean {
  const dot = name.lastIndexOf(".");
  if (dot < 0) return true; // 无扩展名（README / Makefile / 日志片段等）默认当文本
  return !BINARY_EXT.has(name.slice(dot + 1).toLowerCase());
}

/**
 * 文件浏览器的一栏。本地与远端**共用同一个组件**：
 * 后端两侧返回同构的 `Listing`，差异只体现在动作方向（本地栏=上传，远端栏=下载）
 * 与「远端栏需要已连接会话」这一前置条件上。
 */
export default function FilePane({ lane, title, icon, ftp = false, connected = true }: Props) {
  const t = useI18n();
  const s = useStore(fileStore);
  const pane = paneOf(s, lane);
  // 拖拽来源栏、路径分隔符、后端 side 参数等都只需「哪一侧」，与分片无关
  const side = lane.side;
  /** 远端栏所属会话；本地栏恒为 null。 */
  const session = lane.session;
  const [menu, setMenu] = useState<Menu>(null);
  const [pluginMenus, setPluginMenus] = useState<
    { pluginId: string; title: string; when: string; commandId: string; requires: string }[]
  >([]);
  const [prompt, setPrompt] = useState<Prompt>(null);
  /** 待删除项（单或批量）+ 气泡锚点（取右键菜单位置），驱动 Popconfirm。 */
  const [pending, setPending] = useState<{ entries: FileEntry[]; x: number; y: number } | null>(null);
  const [editor, setEditor] = useState<FileEntry | null>(null);
  /** 外部编辑器改了远端文件、等用户确认写回的请求（`file-edit-dirty` 事件）。 */
  const [syncAsk, setSyncAsk] = useState<{ path: string; local: string } | null>(null);
  const [editDiff, setEditDiff] = useState<{ path: string; local: string; lines: string[] } | null>(null);
  // 覆盖确认差分的 Esc：以前这个弹窗只能点 × 或点遮罩（Esc 接不到，与其他弹窗不一致）
  useEscClose(() => setEditDiff(null), editDiff !== null);
  const [over, setOver] = useState(false);
  const [flash, setFlash] = useState("");
  /** 上一条 flash 文案的消失计时器（复用一个，不叠开新 timer）。 */
  const flashTimer = useRef<number>(0);
  /** 行内「新建文件夹」：在列表顶部插入一个可编辑行（替代旧弹窗）。 */
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState("");
  const newInputRef = useRef<HTMLInputElement>(null);
  /** 行内改名：对已选中行的名字再点一下进入编辑（Explorer 手感）；
   *  350ms 延时用于和双击打开/下载区分开。 */
  const [renaming, setRenaming] = useState<string | null>(null);
  const [renameVal, setRenameVal] = useState("");
  /** 回车提交后关掉输入框、弹确认框，确认后才真正落库。 */
  const [renameAsk, setRenameAsk] = useState<{ entry: FileEntry; name: string } | null>(null);
  const renameRef = useRef<HTMLInputElement>(null);
  const renameTimer = useRef<number>(0);
  /** 列排序：点表头切换，目录恒置顶（同列再点翻转升/降，换列从升序起）。 */
  const [sortKey, setSortKey] = useState<"name" | "size" | "time" | null>(null);
  const [sortDir, setSortDir] = useState<"asc" | "desc">("asc");

  /** 鼠标框选（橡皮筋）：在列表空白处按下拖出矩形，命中行整体选中。
   *  行本身留给点选与跨栏拖拽（HTML5 draggable 做传输），故框选只在非行区域起手。 */
  const listRef = useRef<HTMLDivElement>(null);
  const [box, setBox] = useState<{ x0: number; y0: number; x1: number; y1: number } | null>(null);
  /** 框选拖动结束后，抑制紧随其后的容器 click（否则会 unselect 抹掉刚框中的选择）。 */
  const suppressClick = useRef(false);

  const isRemote = side === "remote";
  /** 远端栏在没有会话、或会话已断开时整体不可操作，本地栏不受影响。 */
  const offline = isRemote && (!session || !connected);
  /**
   * 递给后端的会话上下文：本地栏恒为 null（后端据此走本机文件路径，不碰任何连接）。
   * lane.session 对本地栏也有值（传输目标用），因此不能直接外传给文件/插件 API。
   */
  const remote = isRemote ? (session ?? null) : null;
  const selected = pane.entries.find((e) => e.path === pane.selected) ?? null;

  /**
   * 「跟随终端」只看能力表：需要 exec 通道（FTP 没有，本地栏不展远端行为）。
   * 查会话 kind 而不是加 prop 层层传：本栏已持有 session，再往外钻参只是把一件事说三遍。
   */
  const { tabs } = useStore(sessionStore);
  const kind = tabs.find((x) => x.id === session)?.kind ?? null;
  const canFollow = isRemote && kind !== null && hasCap(kind, "exec");

  /**
   * 把远端栏切到终端当前目录：后端在 exec 通道跑一次固定的 `pwd`。
   * 只做点击式快照、不做 cd 实时监听——后者要 shell 集成（OSC 7 / 注入 hook），
   * 本机与远端 shell 都不受我们控制（见 `commands::sessions::session_cwd`）。
   */
  const followTerminal = async () => {
    if (!session) return;
    try {
      const dir = await sessionApi.cwd(session);
      if (dir !== pane.path) void files.load(lane, dir);
      say(t("fp.followed").replace("{n}", dir));
    } catch (e) {
      // 后端已给出人话（非 Linux 无 pwd / 已断开），不再套一层前缀
      say(errMsg(e));
    }
  };

  const toggleSort = (k: "name" | "size" | "time") => {
    if (sortKey === k) setSortDir((d) => (d === "asc" ? "desc" : "asc"));
    else {
      setSortKey(k);
      setSortDir("asc");
    }
  };
  /** 视图层排序：不改 store 里的原始 entries，只影响渲染顺序；目录始终排在文件前。 */
  const rows = useMemo(() => {
    const list = pane.entries;
    if (!sortKey) return list;
    const dir = sortDir === "asc" ? 1 : -1;
    return [...list].sort((a, b) => {
      if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
      let c = 0;
      if (sortKey === "name")
        c = a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" });
      else if (sortKey === "size") c = a.size - b.size;
      else c = (a.modifiedMs ?? 0) - (b.modifiedMs ?? 0);
      if (c === 0) c = a.name.localeCompare(b.name, undefined, { numeric: true });
      return c * dir;
    });
  }, [pane.entries, sortKey, sortDir]);

  /** 矩形与某行相交即命中（任一边越过即选，贴合 Explorer「擦边即选」）；按文档顺序收集 path。 */
  const applyBox = (b: { x0: number; y0: number; x1: number; y1: number }) => {
    const container = listRef.current;
    if (!container) return;
    const left = Math.min(b.x0, b.x1);
    const right = Math.max(b.x0, b.x1);
    const top = Math.min(b.y0, b.y1);
    const bottom = Math.max(b.y0, b.y1);
    const picked: string[] = [];
    container.querySelectorAll<HTMLElement>(".fp-row[data-epath]").forEach((el) => {
      const r = el.getBoundingClientRect();
      if (r.right > left && r.left < right && r.bottom > top && r.top < bottom) {
        const p = el.dataset.epath;
        if (p) picked.push(p);
      }
    });
    files.selectBox(lane, picked);
  };

  /** 列表空白处按下：起框。点在行上则不拦截（行 onClick 做点选、draggable 做跨栏拖拽传输）。 */
  const onListMouseDown = (ev: React.MouseEvent) => {
    if (offline || ev.button !== 0) return;
    if ((ev.target as HTMLElement).closest(".fp-row")) return;
    ev.preventDefault(); // 阻止浏览器原生文字选中
    const x0 = ev.clientX;
    const y0 = ev.clientY;
    let moved = false;
    setBox({ x0, y0, x1: x0, y1: y0 });
    const move = (e: MouseEvent) => {
      moved = true;
      const nb = { x0, y0, x1: e.clientX, y1: e.clientY };
      setBox(nb);
      applyBox(nb);
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      setBox(null);
      if (moved) suppressClick.current = true;
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  const say = (msg: string) => {
    setFlash(msg);
    // 复用同一个计时器：旧写法每调一次开一个新 timer，连点几下时先后的 timer 互错互踩，
    // 文案会在不该的时候提前消失。
    window.clearTimeout(flashTimer.current);
    flashTimer.current = window.setTimeout(() => setFlash(""), 1600);
  };
  useEffect(() => () => window.clearTimeout(flashTimer.current), []);

  const copy = async (text: string) => say((await copyText(text)) ? t("fp.copied") : t("fp.copyFail"));

  /** 开启行内新建：进入编辑态并聚焦输入框（未连接 / 目录未加载时不可用）。 */
  const startCreate = () => {
    if (offline || !pane.path) return;
    setNewName("");
    setCreating(true);
  };

  /** 提交新建。`viaBlur` 区分回车与失焦：
   *  空名一律取消；非法名（含分隔符）回车保留编辑态提示、失焦则直接放弃。 */
  const submitCreate = (viaBlur: boolean) => {
    const name = newName.trim();
    if (!name) {
      setCreating(false);
      setNewName("");
      return;
    }
    if (/[\\/]/.test(name)) {
      if (viaBlur) {
        setCreating(false);
        setNewName("");
      } else {
        say(t("fp.singleName"));
      }
      return;
    }
    setCreating(false);
    setNewName("");
    void files.mkdir(lane, childPath(pane.path, name, side));
  };

  // 进入新建态后把焦点交给输入框（行由 creating 挂载，ref 此时已就位）
  useEffect(() => {
    if (creating) newInputRef.current?.focus();
  }, [creating]);

  // 进入改名态后聚焦并全选原名（同 Explorer 改名选中的习惯）；卸载时清掉延时器
  useEffect(() => {
    if (renaming) {
      renameRef.current?.focus();
      renameRef.current?.select();
    }
  }, [renaming]);
  useEffect(() => () => window.clearTimeout(renameTimer.current), []);

  /** 提交行内改名：空名/没改 → 静默退出；含路径分隔符 → 只提示不弹框；真改了 → 弹确认框。 */
  const commitRename = () => {
    const path = renaming;
    setRenaming(null);
    if (!path) return;
    const entry = pane.entries.find((e) => e.path === path);
    const name = renameVal.trim();
    if (!entry || !name || name === entry.name) return;
    if (/[\\/]/.test(name)) {
      say(t("fp.singleName"));
      return;
    }
    setRenameAsk({ entry, name });
  };

  /** 点名字文本：该行已选中 → 延时 350ms 后进改名态；未选中 → 这一击只做选中。
   *  带修饰键时不拦截，交回行 onClick 走多选切换/范围选。 */
  const clickName = (ev: React.MouseEvent, e: FileEntry) => {
    if (offline || ev.button !== 0 || ev.ctrlKey || ev.metaKey || ev.shiftKey) return;
    ev.stopPropagation();
    const isSel =
      pane.selected === e.path ||
      (pane.selectedPaths.length === 1 && pane.selectedPaths[0] === e.path);
    if (!isSel) {
      files.select(lane, e.path);
      return;
    }
    window.clearTimeout(renameTimer.current);
    renameTimer.current = window.setTimeout(() => {
      setRenameVal(e.name);
      setRenaming(e.path);
    }, 350);
  };

  const openMenu = (e: React.MouseEvent, entry: FileEntry | null) => {
    e.preventDefault();
    e.stopPropagation();
    // 已在多选里：右键不冲掉；不在：才单选该行
    if (entry && !pane.selectedPaths.includes(entry.path) && pane.selected !== entry.path) {
      files.select(lane, entry.path);
    } else if (!entry) {
      files.unselect(lane);
    }
    setMenu({ x: e.clientX, y: e.clientY, entry });
  };

  /**
   * 外部编辑器同步：远端文件变更后不静默写回，
   * 收到 `file-edit-dirty` 先弹确认；本地栏编辑直接落盘，不监听该事件。
   */
  useEffect(() => {
    void pluginApi
      .list()
      .then((list) => {
        const ms: {
          pluginId: string;
          title: string;
          when: string;
          commandId: string;
          requires: string;
        }[] = [];
        for (const pl of list) {
          // 菜单项所需能力取自其绑定的命令包（缺省 exec）：命令包统一走 exec 通道。
          const reqByCmd = new Map(
            (pl.manifest.commands ?? []).map((c) => [c.id, c.requires ?? "exec"]),
          );
          for (const m of pl.manifest.menus ?? []) {
            ms.push({
              pluginId: pl.manifest.id,
              title: m.title,
              when: m.when ?? "always",
              commandId: m.commandId,
              requires: reqByCmd.get(m.commandId) ?? "exec",
            });
          }
        }
        setPluginMenus(ms);
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (!isRemote) return;
    let un: (() => void) | undefined;
    let un2: (() => void) | undefined;
    void (async () => {
      const { on } = await import("../../lib/tauri/client");
      un = await on<{ path: string }>("file-edit-synced", (p) => {
        say(t("fp.synced").replace("{n}", p.path));
      });
      un2 = await on<{ path: string; local: string }>("file-edit-dirty", (p) => {
        // 弹窗已开时忽略后续变化：确认时读的是临时文件最新内容
        setSyncAsk((cur) => cur ?? { path: p.path, local: p.local });
      });
    })();
    return () => {
      un?.();
      un2?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isRemote]);

  const editExternal = async (entry: FileEntry) => {
    try {
      const tmp = await fileApi.editExternal(remote, entry.path, null);
      say(t("fp.openedExternal").replace("{n}", tmp));
    } catch (e) {
      say(String((e as Error)?.message ?? e));
    }
  };

  /** 智能「打开」：文本走外部编辑器闭环；图片/二进制走系统默认程序。 */
  const openEntry = async (entry: FileEntry) => {
    if (isTextLike(entry.name)) {
      await editExternal(entry);
      return;
    }
    try {
      if (isRemote && remote) {
        const tmp = await fileApi.openRemote(remote, entry.path);
        say(t("fp.openedDefault").replace("{n}", tmp));
      } else if (!isRemote) {
        await localApi.open(entry.path);
      }
    } catch (e) {
      say(String((e as Error)?.message ?? e));
    }
  };

  /** 多选批量「打开」（文件项）。 */
  const openEntryBatch = async (entry: FileEntry) => {
    const paths = pane.selectedPaths;
    const list =
      paths.length > 1 && paths.includes(entry.path)
        ? pane.entries.filter((e) => paths.includes(e.path) && !e.isDir)
        : [entry];
    for (const e of list) await openEntry(e);
  };

  /** 把文件/目录作为引用 chip 加入 AI 聊天编辑区并切到「聊天」页（发送时才展开成 📎 协议文本；远程路径在智能体模式可由 read_file/ls 自读）。 */
  const sendToAi = (entry: FileEntry) => {
    void settingsActions.update({ rail: "chat" });
    // 带上本文件栏所属会话：chip 只能落进同一会话的输入框，右栏 AI 停在别的连接时不串台
    aiChat.addRef({ sessionId: session ?? "", path: entry.path, name: entry.name, remote: isRemote, isDir: entry.isDir });
  };

  const menuItems = (entry: FileEntry | null, ax: number, ay: number): MenuItem[] => {
    if (!entry) {
      return [
        { label: t("common.refresh"), disabled: offline, onClick: () => void files.load(lane, null, true) },
        { label: t("fp.mkdir"), sep: true, disabled: offline || !pane.path, onClick: startCreate },
        { label: t("fp.copyCurPath"), disabled: !pane.path, onClick: () => void copy(pane.path) },
      ];
    }
    // 多选：菜单按「整个选择」统一构建，不随被右键那一项是文件还是目录而变（多选时菜单漂移）。
    // 顺序与单选同构：主要操作 → 传输 → 新建 → 删除。
    if (pane.selectedPaths.length > 1 && pane.selectedPaths.includes(entry.path)) {
      return [
        { label: t("fp.open"), onClick: () => void openEntryBatch(entry) },
        {
          label: isRemote ? t("fp.dlFile") : t("fp.ulFile"),
          sep: true,
          disabled: offline,
          onClick: () => void transferSelected(lane, entry),
        },
        { label: t("fp.mkdir"), sep: true, disabled: offline, onClick: startCreate },
        {
          label: t("common.delete"),
          danger: true,
          sep: true,
          disabled: offline,
          onClick: () =>
            setPending({
              entries: pane.entries.filter((e) => pane.selectedPaths.includes(e.path)),
              x: ax,
              y: ay,
            }),
        },
      ];
    }
    // 顺位按文件管理器通用习惯成组：主要操作（打开/编辑）→ 传输 → 外部工具 → 实用操作 → 删除。
    const items: MenuItem[] = [
      {
        label: t("fp.open"),
        disabled: entry.isDir || offline,
        onClick: () => void openEntryBatch(entry),
      },
      // 内置编辑器只对文本文件有意义（图片/二进制看不了），故按类型显隐。
      ...(isTextLike(entry.name) && !entry.isDir
        ? ([{ label: t("fp.builtinEditor"), disabled: offline, onClick: () => setEditor(entry) }] as MenuItem[])
        : []),
      isRemote
        ? {
            // 文件/文件夹统一叫「下载到本地」/「上传到远端」：动作才是重点，
            // 按对象类型区分（下载文件夹/上传文件夹）反而容易误解。
            label: t("fp.dlFile"),
            sep: true,
            disabled: false,
            onClick: () => void transferSelected(lane, entry),
          }
        : {
            label: t("fp.ulFile"),
            sep: true,
            disabled: offline,
            onClick: () => void transferSelected(lane, entry),
          },
    ];
    // 本地栏：在资源管理器中打开（目录直接打开、文件定位选中）。走后端原生方式，
    // 不受 opener 插件前端 scope 限制（本地栏可浏览任意用户目录）。
    if (!isRemote) {
      items.push({
        label: t("fp.revealInExplorer"),
        onClick: () => {
          void localApi.reveal(entry.path).catch((e) => say(String(e?.message ?? e)));
        },
      });
    }
    // Zmodem：仅支持 zmodem 能力的会话（SSH，需 PTY + sz/rz）；FTP/FTPS 无终端，不显示。
    // 路径对侧取对方栏当前目录。
    if (!entry.isDir && session && kind !== null && hasCap(kind, "zmodem")) {
      const other =
        side === "remote" ? s.local.path : paneOf(s, { side: "remote", session }).path;
      if (other) {
        items.push({
          label: isRemote ? t("fp.zmodemDl") : t("fp.zmodemUl"),
          sep: true,
          onClick: () => {
            const job = isRemote
              ? zmodem.download(session, entry.path, other)
              : zmodem.upload(session, entry.path, other);
            void job
              .then(() => say(t("fp.zmodemDone")))
              .catch((e) => say(String(e?.message ?? e)));
          },
        });
      }
    }
    for (const pm of pluginMenus) {
      const hit =
        pm.when === "always" ||
        (pm.when === "dir" && entry.isDir) ||
        (pm.when === "file" && !entry.isDir);
      if (!hit) continue;
      // 能力门控：命令包按其 requires 档位（缺省 exec）对照会话能力表。FTP/FTPS 无 exec、
      // 本地栏无关联会话（kind 为 null）时不显示，避免点了必然失败（与 zmodem 项同构）。
      if (pm.requires !== "none" && (kind === null || !hasCap(kind, pm.requires as Capability))) {
        continue;
      }
      items.push({
        label: <><IconZap /> {pm.title}</>,
        onClick: () => {
          void pluginApi
            .run(pm.pluginId, pm.commandId, remote ?? "", entry.path)
            .then((r) => {
              // 结果回显到会话终端（只读本地写，不经 PTY）；终端未挂载时降级为面板顶一行提示
              if (!writePluginOutput(remote ?? "", r.command, r.stdout, r.stderr, r.exitCode))
                say(t("fp.executed"));
            })
            .catch((e) => say(String(e?.message ?? e)));
        },
      });
    }
    items.push({ label: t("fp.copyPath"), sep: true, onClick: () => void copy(entry.path) });
    // 文件与目录都可发：只是发路径引用，目录由 AI 在智能体模式自行 ls/读取
    items.push({ label: t("fp.sendToAi"), onClick: () => sendToAi(entry) });
    items.push({
      label: t("common.rename"),
      sep: true,
      disabled: offline,
      // 与「点名字行内改名」同一套：直接进编辑态，不再弹框（保持两处统一）。
      onClick: () => {
        setRenameVal(entry.name);
        setRenaming(entry.path);
      },
    });
    if (isRemote && !ftp) {
      items.push({ label: t("fp.chmod"), onClick: () => setPrompt({ kind: "chmod", entry }) });
    }
    // 行上右键也提供「新建目录」（与空白处菜单同款行内新建），免得到处找落点
    items.push({ label: t("fp.mkdir"), disabled: offline || !pane.path, onClick: startCreate });
    items.push({
      label: t("common.delete"),
      danger: true,
      sep: true,
      disabled: offline,
      onClick: () => setPending({ entries: [entry], x: ax, y: ay }),
    });
    return items;
  };

  return (
    <section className="fp" data-side={side} data-session={session ?? ""}>
      <div className="fp-head">
        <span className="fp-title">
          {icon}
          {title}
        </span>
        {flash && <span className="fp-flash">{flash}</span>}
        <span className="fp-acts">
          <button
            className="icon-btn"
            title={t("fp.up")}
            disabled={pane.loading}
            onClick={() => void files.up(lane)}
          >
            <IconArrowUp />
          </button>
          <button
            className="icon-btn"
            title={side === "local" ? t("fp.thisPC") : t("fp.root")}
            disabled={pane.loading}
            onClick={() => void files.root(lane)}
          >
            <IconHome />
          </button>
          <button
            className="icon-btn"
            title={t("common.refresh")}
            disabled={offline || pane.loading}
            onClick={() => void files.load(lane, null, true)}
          >
            <IconRefresh />
          </button>
          {canFollow && (
            <button
              className="icon-btn"
              title={t("fp.followTerm")}
              disabled={offline || pane.loading}
              onClick={() => void followTerminal()}
            >
              <IconTarget />
            </button>
          )}
          <button
            className="icon-btn"
            title={t("fp.mkdir")}
            disabled={offline || !pane.path}
            onClick={startCreate}
          >
            <IconPlus />
          </button>
          <button
            className="fp-go"
            disabled={offline || (pane.selectedPaths.length === 0 && !selected)}
            title={isRemote ? t("fp.dlSelTitle") : t("fp.ulSelTitle")}
            onClick={() => {
              const list =
                pane.selectedPaths.length > 0
                  ? pane.entries.filter((e) => pane.selectedPaths.includes(e.path))
                  : selected
                    ? [selected]
                    : [];
              if (list[0]) void transferSelected(lane, list[0]);
            }}
          >
            {isRemote ? <IconArrowDown /> : <IconArrowUp />}
            {pane.selectedPaths.length > 1 ? ` (${pane.selectedPaths.length})` : ""}
          </button>
        </span>
      </div>

      <div className="fp-path">
        {/* key 绑路径：目录一变就重挂载，defaultValue 始终是真实路径，不必再写同步 effect */}
        <input
          key={pane.path}
          defaultValue={pane.path}
          spellCheck={false}
          disabled={offline}
          placeholder={offline ? t("fp.notConnected") : isRemote ? t("fp.remotePathPh") : t("fp.localPathPh")}
          onKeyDown={(e) => {
            if (e.key !== "Enter") return;
            const v = (e.target as HTMLInputElement).value.trim();
            if (v) void files.load(lane, v);
          }}
        />
      </div>

      <div className="fp-cols">
        <button
          type="button"
          className={`fp-sort${sortKey === "name" ? " on" : ""}`}
          onClick={() => toggleSort("name")}
        >
          {t("fp.colName")}
          <span className="fp-sort-ind">{sortKey === "name" ? (sortDir === "asc" ? "▲" : "▼") : "↕"}</span>
        </button>
        <button
          type="button"
          className={`fp-sort r${sortKey === "size" ? " on" : ""}`}
          onClick={() => toggleSort("size")}
        >
          {t("fp.colSize")}
          <span className="fp-sort-ind">{sortKey === "size" ? (sortDir === "asc" ? "▲" : "▼") : "↕"}</span>
        </button>
        <span>{t("fp.colPerm")}</span>
        <button
          type="button"
          className={`fp-sort${sortKey === "time" ? " on" : ""}`}
          onClick={() => toggleSort("time")}
        >
          {t("fp.colTime")}
          <span className="fp-sort-ind">{sortKey === "time" ? (sortDir === "asc" ? "▲" : "▼") : "↕"}</span>
        </button>
      </div>

      {/* 状态槽固定在列表上方：旧写法把 loading / 报错渲染在 rows 之后，目录一多就得
          把列表滚到最底才看得到「读不出这个目录」的原因——报错被它自己解释的内容埋掉了。 */}
      {!offline && (pane.loading || pane.error || pane.notice) && (
        <div
          className={`fp-status${pane.error ? " err" : ""}${!pane.error && pane.notice ? " ok" : ""}`}
          role={pane.error ? "alert" : "status"}
        >
          <span className="fp-status-txt">{pane.error || pane.notice || t("fp.readingDir")}</span>
          {pane.error && (
            <button
              type="button"
              className="icon-btn"
              title={t("common.close")}
              onClick={() => files.clearError(lane)}
            >
              <IconClose />
            </button>
          )}
        </div>
      )}

      <div
        ref={listRef}
        className="fp-list"
        data-over={over}
        onContextMenu={(e) => openMenu(e, null)}
        onMouseDown={onListMouseDown}
        onClick={() => {
          // 刚完成一次框选：吞掉紧随的 click，避免 unselect 抹掉框中的选择
          if (suppressClick.current) {
            suppressClick.current = false;
            return;
          }
          files.unselect(lane);
        }}
        onDragOver={(e) => {
          // 源栏自己不接受落点；未连接时远端栏也不接受
          if (offline || isDragSource(side) || !canAccept(e.dataTransfer)) return;
          e.preventDefault();
          e.dataTransfer.dropEffect = "copy";
          setOver(true);
        }}
        onDragLeave={(e) => {
          // 只有真的离开列表容器才收起高亮（在子行间移动会连带触发 dragleave）
          if (e.currentTarget.contains(e.relatedTarget as Node)) return;
          setOver(false);
        }}
        onDrop={(e) => {
          e.preventDefault();
          endDrag();
          setOver(false);
          const payload = readDragEntry(e.dataTransfer);
          // 同栏内拖动没有意义；跨栏才是一次传输
          if (!payload || payload.side === side || offline) return;
          /**
           * 会话以**来源栏**为准：远端条目属于它那一条连接，分屏下落到另一个会话的本地栏
           * 也不该换成落点的连接去下载；本地条目则传到本栏（远端）所在的会话。
           */
          const src = payload.side === "remote" ? payload.session : session;
          if (!src) return;
          void transferSelected({ side: payload.side, session: src }, payload.entry);
        }}
      >
        {pane.parent && (
          <div
            className="fp-row fp-up"
            title={t("fp.upDbl")}
            // 只认双击，与其他目录一致：原先单击也翻页，双击下去实际连跳两级，
            // 容易误操作（其他文件夹都要双击，唯独 .. 一点就回，别扭）
            onDoubleClick={(ev) => {
              ev.stopPropagation();
              void files.up(lane);
            }}
          >
            <span className="fp-nm">
              <i>
                <IconFolder />
              </i>
              ..
            </span>
            <span className="fp-sz r">—</span>
            <span className="fp-pm">—</span>
            <span className="fp-tm">—</span>
          </div>
        )}
        {creating && (
          <div className="fp-row fp-creating" onClick={(ev) => ev.stopPropagation()}>
            <span className="fp-nm">
              <i>
                <IconFolder />
              </i>
              <input
                ref={newInputRef}
                className="fp-new"
                value={newName}
                spellCheck={false}
                placeholder={t("fp.newFolderPh")}
                onChange={(ev) => setNewName(ev.target.value)}
                onKeyDown={(ev) => {
                  if (ev.key === "Enter") {
                    ev.preventDefault();
                    submitCreate(false);
                  } else if (ev.key === "Escape") {
                    ev.preventDefault();
                    setCreating(false);
                    setNewName("");
                  }
                }}
                onBlur={() => submitCreate(true)}
              />
            </span>
            <span className="fp-sz r">—</span>
            <span className="fp-pm">—</span>
            <span className="fp-tm">—</span>
          </div>
        )}
        {rows.map((e) => (
          <div
            key={e.path}
            data-epath={e.path}
            className={`fp-row${
              (pane.selectedPaths.includes(e.path) || pane.selected === e.path) ? " sel" : ""
            }${renaming === e.path ? " fp-renaming" : ""}`}
            draggable={!offline && renaming !== e.path}
            title={e.path}
            onDragStart={(ev) => {
              beginDrag(side);
              setDragEntry(ev.dataTransfer, { side, entry: e, session });
            }}
            onDragEnd={() => {
              endDrag();
              setOver(false);
            }}
            onClick={(ev) => {
              if (ev.button !== 0) return;
              ev.stopPropagation();
              const mode = ev.ctrlKey || ev.metaKey ? "toggle" : ev.shiftKey ? "range" : "replace";
              files.select(lane, e.path, mode);
            }}
            onDoubleClick={() => {
              // 双击：目录进入；本地文件用系统默认程序打开；远端文件下载。
              // 撤掉可能已挂上的改名延时，避免快双击误入编辑态
              window.clearTimeout(renameTimer.current);
              if (e.isDir) void files.load(lane, e.path);
              else if (!isRemote)
                void localApi
                  .open(e.path)
                  .catch((err) => say(String((err as Error)?.message ?? err)));
              else void transferSelected(lane, e);
            }}
            onContextMenu={(ev) => openMenu(ev, e)}
          >
            <span className="fp-nm">
              <i>{e.isDir ? <IconFolder /> : <IconFile />}</i>
              {renaming === e.path ? (
                <input
                  ref={renameRef}
                  className="fp-new"
                  value={renameVal}
                  spellCheck={false}
                  onChange={(ev) => setRenameVal(ev.target.value)}
                  onKeyDown={(ev) => {
                    ev.stopPropagation();
                    if (ev.key === "Enter") commitRename();
                    else if (ev.key === "Escape") setRenaming(null);
                  }}
                  onBlur={() => setRenaming(null)}
                />
              ) : (
                <span className="fp-nm-txt" title={t("fp.renameTip")} onClick={(ev) => clickName(ev, e)}>
                  {e.name}
                </span>
              )}
              {e.isSymlink && <em className="fp-lnk" title={t("fp.symlink")}>@</em>}
              {isSensitive(e.name, e.isDir) && (
                <em className="fp-lock" title={t("fp.sensitive")}>
                  <IconLock />
                </em>
              )}
            </span>
            <span className="fp-sz r">{e.isDir ? "—" : formatBytes(e.size)}</span>
            <span className="fp-pm">{e.perms ?? "—"}</span>
            <span className="fp-tm">{formatTime(e.modifiedMs)}</span>
          </div>
        ))}

        {!offline && !pane.loading && !pane.error && pane.entries.length === 0 && !creating && (
          <div className="fp-empty">{isRemote ? t("fp.emptyUpload") : t("fp.empty")}</div>
        )}
        {offline && (
          <div className="fp-empty">
            {t("fp.notConnected")}
          </div>
        )}
      </div>

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={menuItems(menu.entry, menu.x, menu.y)}
          onClose={() => setMenu(null)}
        />
      )}

      {pending && (
        <Popconfirm
          x={pending.x}
          y={pending.y}
          title={
            pending.entries.length === 1
              ? t("fp.delConfirmQ").replace("{n}", pending.entries[0].name)
              : t("fp.delConfirmMulti").replace("{n}", String(pending.entries.length))
          }
          description={
            pending.entries.some((e) => e.isDir)
              ? isRemote
                ? t("fp.delWarnRemoteDir")
                : t("fp.delWarnLocalDir")
              : t("fp.delWarnFile")
          }
          onConfirm={() => {
            const { entries } = pending;
            setPending(null);
            void (entries.length === 1 ? files.remove(lane, entries[0]) : files.removeSelected(lane));
          }}
          onClose={() => setPending(null)}
        />
      )}

      {renameAsk && (
        <ConfirmDialog
          title={t("common.rename")}
          message={t("fp.renameConfirm")
            .replace("{o}", renameAsk.entry.name)
            .replace("{n}", renameAsk.name)}
          okText={t("common.rename")}
          onOk={async () => {
            const { entry, name } = renameAsk;
            setRenameAsk(null);
            await files.rename(lane, entry.path, childPath(pane.path, name, side));
          }}
          onClose={() => setRenameAsk(null)}
        />
      )}

      {prompt?.kind === "chmod" && (
        <ChmodDialog
          mode={prompt.entry.mode ?? 0o644}
          path={prompt.entry.path}
          onSubmit={async (v) => {
            await files.chmod(lane, prompt.entry.path, v);
          }}
          onClose={() => setPrompt(null)}
        />
      )}

      {editor && (
        <FileEditor
          side={side === "remote" ? "remote" : "local"}
          sessionId={remote}
          entry={editor}
          onClose={() => setEditor(null)}
          onSaved={() => void files.load(lane)}
        />
      )}

      {syncAsk && (
        <ConfirmDialog
          title={t("fp.syncTitle")}
          message={t("fp.syncMsg").replace("{n}", syncAsk.path)}
          warning={t("fp.syncWarn")}
          okText={t("fp.syncOk")}
          onOk={async () => {
            const ask = syncAsk;
            setSyncAsk(null);
            try {
              const pv = await fileApi.editPreview(remote, ask.path, ask.local);
              if (pv.diff.length === 0) {
                await fileApi.commit(remote, ask.path, ask.local);
                void files.load(lane);
                return;
              }
              setEditDiff({ path: ask.path, local: ask.local, lines: pv.diff.slice(0, 400) });
            } catch (e) {
              say(errMsg(e));
            }
          }}
          onClose={() => setSyncAsk(null)}
        />
      )}

      {editDiff && (
        <div className="modal-mask" {...useMaskDismiss(() => setEditDiff(null))}>
          <div className="modal fe">
            <div className="modal-head">
              <h2>{t("fp.diffTitle").replace("{n}", editDiff.path)}</h2>
              <button className="modal-x" onClick={() => setEditDiff(null)}>
                ×
              </button>
            </div>
            <pre className="fe-ta" style={{ maxHeight: "50vh", overflow: "auto", fontSize: 12 }}>
              {editDiff.lines.map((l, i) => (
                <div
                  key={i}
                  style={{
                    color: l.startsWith("+") ? "var(--green)" : l.startsWith("-") ? "var(--red)" : "var(--txt-dim)",
                  }}
                >
                  {l}
                </div>
              ))}
            </pre>
            <div className="form-actions">
              <button type="button" className="ghost" onClick={() => setEditDiff(null)}>
                {t("common.cancel")}
              </button>
              <button
                type="button"
                onClick={async () => {
                  const d = editDiff;
                  setEditDiff(null);
                  try {
                    await fileApi.commit(remote, d.path, d.local);
                    void files.load(lane);
                  } catch (e) {
                    say(errMsg(e));
                  }
                }}
              >
                {t("fp.overwrite")}
              </button>
            </div>
          </div>
        </div>
      )}

      {box && (
        <div
          className="fp-marquee"
          style={{
            left: Math.min(box.x0, box.x1),
            top: Math.min(box.y0, box.y1),
            width: Math.abs(box.x1 - box.x0),
            height: Math.abs(box.y1 - box.y0),
          }}
        />
      )}
    </section>
  );
}
