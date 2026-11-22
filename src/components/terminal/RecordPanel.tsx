// 会话录制面板（停靠区）：跟随当前聚焦会话，提供录制起停 + 本会话录制文件回放。
import { useEffect, useMemo, useState } from "react";
import { formatBytes, formatTime } from "../../lib/format";
import { recordActions, recordStore, connectionStore, useStore } from "../../stores";
import { useI18n } from "../../lib/i18n";
import RecBody from "../common/RecBody";
import ContextMenu from "../common/ContextMenu";
import ConfirmDialog from "../dialog/ConfirmDialog";
import { IconGear } from "../common/icons";
import type { Tab } from "../../stores";

interface Props {
  tab: Tab | null;
}

export default function RecordPanel({ tab }: Props) {
  const tr = useI18n();
  const { profiles } = useStore(connectionStore);
  const { items, recordingIds, playback } = useStore(recordStore);
  // 文件行右键菜单（打开所在目录 / 删除）；记住该文件自己的目录（全局库下面板内文件可能来自不同目录）。
  const [fileMenu, setFileMenu] = useState<{ x: number; y: number; name: string; dir: string } | null>(null);
  // 待删除录制文件：非空则弹确认框（不可逆动作先过一道）。
  const [pendingDel, setPendingDel] = useState<{ name: string; dir: string } | null>(null);

  const sessionId = tab?.state === "connected" ? tab.id : "";
  const isRecording = tab ? recordingIds.includes(tab.id) : false;

  // 当前会话绑定的档案（用于预填录制设置）。
  const profile = useMemo(
    () => (tab?.profileId ? profiles.find((x) => x.id === tab.profileId) ?? null : null),
    [tab?.profileId, profiles],
  );

  // 录制设置（齿轮弹层）：临时覆盖本会话的手动录制参数，不写回档案。
  // 有档案→预填档案值；无档案（本地/快速直连）→空/默认，仍可临时配。
  const [showSettings, setShowSettings] = useState(false);
  const [rDir, setRDir] = useState("");
  const [rPattern, setRPattern] = useState("");
  const [rOverwrite, setROverwrite] = useState(false);
  const [rTs, setRTs] = useState(false);
  // 切会话、或档案异步加载完身份变化时回填（不键控档案内容，避免编辑中被刷新冲掉）。
  useEffect(() => {
    setRDir(profile?.recordDir ?? "");
    setRPattern(profile?.recordFilePattern ?? "");
    setROverwrite(profile?.recordOverwrite ?? false);
    setRTs(profile?.recordTimestampPrefix ?? false);
    setShowSettings(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab?.id, profile?.id]);

  // 本连接的录制文件：按真实主机名过滤（起录时已登记到 record_index）。
  // 比旧的“文件名前缀”法更准：自定义命名模式、遗留 s… 命名不再错漏。
  const sessionFiles = useMemo(() => {
    if (!tab?.host) return [];
    return items.filter((r) => r.host === tab.host);
  }, [items, tab?.host]);

  useEffect(() => {
    if (tab) {
      void recordActions.refresh();
      void recordActions.syncStatus([tab.id]);
    }
  }, [tab?.id]);

  return (
    <div className="rp">
      {/* 录制控制 */}
      <div className="rp-head">
        <span className="fp-title">{tr("rp.title")}</span>
        <div className="rp-head-acts">
          <button
            type="button"
            className={`rp-toggle${isRecording ? " on" : ""}`}
            disabled={!sessionId}
            onClick={() =>
              sessionId &&
              void recordActions.toggle(sessionId, rDir.trim() || null, {
                filePattern: rPattern.trim() || null,
                overwrite: rOverwrite,
                timestampPrefix: rTs,
              })
            }
          >
            <span className="rp-dot" />
            {isRecording ? tr("rp.stop") : tr("rp.start")}
          </button>
          <button
            type="button"
            className={`rp-setbtn${showSettings ? " on" : ""}`}
            title={tr("rp.settings")}
            aria-label={tr("rp.settings")}
            onClick={() => setShowSettings((v) => !v)}
          >
            <IconGear />
          </button>
        </div>
      </div>

      {/* 录制设置弹层：仅影响本次手动录制，不改档案 */}
      {showSettings && (
        <div className="rp-settings">
          <label className="rp-set-row">
            <span className="rp-set-label">{tr("rp.setDir")}</span>
            <input
              className="rp-set-input"
              type="text"
              value={rDir}
              placeholder={tr("rp.setDirPh")}
              spellCheck={false}
              onChange={(e) => setRDir(e.target.value)}
            />
          </label>
          <label className="rp-set-row">
            <span className="rp-set-label">{tr("rp.setPattern")}</span>
            <input
              className="rp-set-input"
              type="text"
              value={rPattern}
              placeholder="%n-%Y-%m-%d_%H-%M-%S"
              spellCheck={false}
              onChange={(e) => setRPattern(e.target.value)}
            />
          </label>
          <label className="rp-set-check">
            <input type="checkbox" checked={rOverwrite} onChange={(e) => setROverwrite(e.target.checked)} />
            {tr("rp.setOverwrite")}
          </label>
          <label className="rp-set-check">
            <input type="checkbox" checked={rTs} onChange={(e) => setRTs(e.target.checked)} />
            {tr("rp.setTs")}
          </label>
          <div className="rp-set-note">{tr("rp.settingsNote")}</div>
        </div>
      )}

      {/* 左列表 / 右内容 */}
      <div className="rp-body">
        <div className="rp-list">
          {sessionFiles.length === 0 ? (
            <div className="ss-empty">{tr("rp.noFiles")}</div>
          ) : (
            sessionFiles.map((r) => (
              <button
                type="button"
                key={`${r.dir}/${r.name}`}
                className={`rp-file${playback?.name === r.name ? " active" : ""}`}
                title={r.name}
                onClick={() => void recordActions.play(r.name, r.dir)}
                onContextMenu={(e) => {
                  e.preventDefault();
                  setFileMenu({ x: e.clientX, y: e.clientY, name: r.name, dir: r.dir });
                }}
              >
                <span className="rp-name">
                  {r.name}
                  {r.active && <span className="rp-live">{tr("rp.live")}</span>}
                </span>
                <span className="rp-meta">{formatBytes(r.size)} · {formatTime(r.modifiedMs)}</span>
              </button>
            ))
          )}
        </div>

        <div className="rp-detail">
          {playback ? (
            <>
              <div className="rp-play-head">
                <span className="rp-play-name" title={playback.name}>{playback.name}</span>
                <button type="button" className="link" onClick={() => recordActions.closePlayback()}>
                  {tr("common.close")}
                </button>
              </div>
              <RecBody text={playback.text} empty={tr("rec.empty2")} className="rp-play-body" />
            </>
          ) : (
            <div className="ss-empty rp-detail-empty">{tr("rp.selectHint")}</div>
          )}
        </div>
      </div>

      {fileMenu && (
        <ContextMenu
          x={fileMenu.x}
          y={fileMenu.y}
          onClose={() => setFileMenu(null)}
          items={[
            {
              label: tr("rp.revealDir"),
              onClick: () => void recordActions.openDir(fileMenu.dir),
            },
            {
              label: tr("common.delete"),
              danger: true,
              sep: true,
              onClick: () => setPendingDel({ name: fileMenu.name, dir: fileMenu.dir }),
            },
          ]}
        />
      )}

      {pendingDel && (
        <ConfirmDialog
          title={tr("common.delete")}
          message={tr("rec.confirmDel").replace("{n}", pendingDel.name)}
          onOk={async () => {
            const del = pendingDel;
            setPendingDel(null);
            await recordActions.remove(del.name, del.dir);
          }}
          onClose={() => setPendingDel(null)}
        />
      )}
    </div>
  );
}
