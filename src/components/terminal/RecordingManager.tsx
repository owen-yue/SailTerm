import { useCallback, useEffect, useMemo, useState } from "react";
import { IconFolderOpen, IconRecord, IconRefresh } from "../common/icons";
import { formatBytes, formatTime } from "../../lib/format";
import { recordActions, recordStore, useStore } from "../../stores";
import { useI18n } from "../../lib/i18n";
import Sheet from "../common/Sheet";
import RecBody from "../common/RecBody";
import DeleteButton from "../common/DeleteButton";
import Popconfirm from "../common/Popconfirm";
import Splitter from "../common/Splitter";

interface Props {
  onClose: () => void;
  /** 嵌入独立工具窗口 / 设置中心时不套模态壳。 */
  embedded?: boolean;
}

// 两栏宽度：左列表可拖，拖完记进 localStorage（与侧栏 / AI 栏同一口径，主窗口与工具窗口同源共享）。
// 下限 400px 与 CSS .rec-lib-list 的 min-width 一致：再窄行内定宽列就会裁切。
const LS_LIST_W = "sailterm.recLib.listW";
const DEFAULT_LIST_W = 520;
const clampListW = (w: number) => Math.min(900, Math.max(400, w));

/**
 * 🎥 录制文件库（全局）：左列表 + 右回放两栏，跨目录、跨服务器聚合所有录制文件。
 *
 * 录制起停已移至边栏「录制」面板（跟随当前会话），本工具窗口只做跨会话、跨服务器的“事后回放”。
 * 顶部搜索按主机 / 文件名过滤；每条标明来源服务器（遗留/外部文件无主机则显示占位）。
 */
export default function RecordingManager({ onClose, embedded = false }: Props) {
  const tr = useI18n();
  const { items, error, playback } = useStore(recordStore);
  const [query, setQuery] = useState("");
  // 删除确认：与全项目行删除同一套路——DeleteButton 回传锚点坐标，就近弹 Popconfirm 气泡。
  const [confirmDel, setConfirmDel] = useState<{ name: string; dir: string; x: number; y: number } | null>(
    null,
  );
  // 左列表宽度（px）：回放区吃剩下的宽度，长行靠它自己横向滚动。
  const [listW, setListW] = useState(() =>
    clampListW(Number(localStorage.getItem(LS_LIST_W)) || DEFAULT_LIST_W),
  );
  // 落盘走 effect：updater 必须是纯函数（StrictMode 会双调用），副作用不写在里面。
  useEffect(() => {
    localStorage.setItem(LS_LIST_W, String(listW));
  }, [listW]);
  const resizeList = useCallback((d: number) => {
    setListW((w) => clampListW(w + d));
  }, []);

  useEffect(() => {
    void recordActions.refresh();
  }, []);

  // 搜索：命中主机名或文件名（大小写不敏感）。
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return items;
    return items.filter(
      (r) => r.name.toLowerCase().includes(q) || (r.host ?? "").toLowerCase().includes(q),
    );
  }, [items, query]);

  const err = error;

  return (
    <>
    <Sheet title={<><IconRecord /> {tr("topbar.record.title")}</>} onClose={onClose} embedded={embedded}>
      <div className="rec-lib">
        {/* 搜索 + 计数 + 刷新 */}
        <div className="rec-lib-toolbar">
          <input
            className="rec-lib-search"
            type="text"
            placeholder={tr("rec.search")}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          <span className="rec-lib-count">{filtered.length} / {items.length}</span>
          <button
            type="button"
            className="icon-btn"
            title={tr("common.refresh")}
            aria-label={tr("common.refresh")}
            onClick={() => void recordActions.refresh()}
          >
            <IconRefresh />
          </button>
        </div>

        <div className="rec-lib-body">
          {/* 左：文件列表（整行点选回放） */}
          <div className="rec-lib-list" style={{ width: listW }}>
            {items.length === 0 ? (
              <div className="ss-empty">{tr("rec.noFiles")}</div>
            ) : filtered.length === 0 ? (
              <div className="ss-empty">{tr("rec.noMatch")}</div>
            ) : (
              filtered.map((r) => {
                const sel = playback?.name === r.name && playback?.dir === r.dir;
                return (
                  <div
                    key={`${r.dir}/${r.name}`}
                    className={`rec-lib-item${sel ? " active" : ""}`}
                    onClick={() => void recordActions.play(r.name, r.dir)}
                  >
                    <span className={`rec-lib-host${r.host ? "" : " dim"}`} title={r.host ?? undefined}>
                      {r.host || tr("rec.unknownHost")}
                    </span>
                    <span className="rec-lib-name" title={r.name}>
                      {r.name}
                      {r.active && <span className="rp-live">{tr("rp.live")}</span>}
                    </span>
                    <span className="rec-lib-size">{formatBytes(r.size)}</span>
                    <span className="rec-lib-time">{formatTime(r.modifiedMs)}</span>
                    <span className="rec-lib-acts">
                      <button
                        type="button"
                        className="icon-btn"
                        title={tr("rp.revealDir")}
                        aria-label={tr("rp.revealDir")}
                        onClick={(e) => {
                          e.stopPropagation();
                          void recordActions.openDir(r.dir);
                        }}
                      >
                        <IconFolderOpen />
                      </button>
                      <DeleteButton
                        onConfirm={(x, y) => setConfirmDel({ name: r.name, dir: r.dir, x, y })}
                      />
                    </span>
                  </div>
                );
              })
            )}
          </div>

          {/* 两栏之间的拖拽条：透明热区 + 1px 可见线（与侧栏 / AI 栏同一组件） */}
          <Splitter axis="x" title={tr("rec.resize")} onDelta={resizeList} />

          {/* 右：回放内容 */}
          <div className="rec-lib-detail">
            {playback ? (
              <>
                <div className="rec-lib-play-head">
                  <span className="rec-lib-play-name" title={playback.name}>{playback.name}</span>
                  <button type="button" className="link" onClick={() => recordActions.closePlayback()}>
                    {tr("common.close")}
                  </button>
                </div>
                <RecBody text={playback.text} empty={tr("rec.empty2")} className="rp-play-body" />
              </>
            ) : (
              <div className="ss-empty rec-lib-detail-empty">{tr("rp.selectHint")}</div>
            )}
          </div>
        </div>
      </div>

      {err && <div className="form-error">{err}</div>}
    </Sheet>

    {/* 气泡作为 Sheet 的兄弟节点渲染（与隧道 / 密钥库同一约定）：Portal 到 body 才能跳出
        模态壳的层叠上下文，也不让嵌套弹层的点击冒泡回宿主。 */}
    {confirmDel && (
      <Popconfirm
        x={confirmDel.x}
        y={confirmDel.y}
        title={tr("rec.confirmDel").replace("{n}", confirmDel.name)}
        onConfirm={() => {
          const del = confirmDel;
          setConfirmDel(null);
          void recordActions.remove(del.name, del.dir);
        }}
        onClose={() => setConfirmDel(null)}
      />
    )}
    </>
  );
}
