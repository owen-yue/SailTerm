import { useEffect, useRef, useState } from "react";
import { IconEdit, IconLaptop, IconMonitor } from "../common/icons";
import ConfirmDialog from "../dialog/ConfirmDialog";
import { file as fileApi, local as localApi } from "../../lib/tauri";
import { errMsg } from "../../lib/tauri/client";
import { useI18n } from "../../lib/i18n";
import { kbdLabel } from "../../lib/kbd";
import { useEscClose, useMaskDismiss } from "../../lib/useOverlay";
import type { FileEntry } from "../../types";
import Button from "../common/Button";

interface Props {
  /** 编辑本地文件还是远程文件。 */
  side: "local" | "remote";
  /** 远程编辑时必填。 */
  sessionId?: string | null;
  entry: FileEntry;
  onClose: () => void;
  /** 保存成功后通知刷新列表。 */
  onSaved?: () => void;
}

/**
 * 📝 文件编辑器：打开远程/本地文本文件，编辑后覆盖写回。
 *
 * 安全红线：内容只在本机内存与目标文件之间往返，**不经 AI、不落额外副本**；
 * 写回走既有 SFTP/本地 IO，不二次索取凭证。二进制与超大文件由后端拒绝。
 */
export default function FileEditor({ side, sessionId, entry, onClose, onSaved }: Props) {
  const tr = useI18n();
  const [text, setText] = useState("");
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  /** 远端文件写回前的确认弹窗（保存先问一声，不静默改服务器文件）。 */
  const [confirming, setConfirming] = useState(false);
  const taRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    let cancelled = false;
    setBusy(true);
    setError("");
    const loader =
      side === "remote" && sessionId
        ? fileApi.read(sessionId, entry.path)
        : localApi.read(entry.path);
    loader
      .then((t) => {
        if (cancelled) return;
        setText(t.content);
        setDirty(false);
      })
      .catch((e) => {
        if (!cancelled) setError(errMsg(e));
      })
      .finally(() => {
        if (!cancelled) setBusy(false);
      });
    return () => {
      cancelled = true;
    };
  }, [side, sessionId, entry.path]);

  /** 保存入口：远端文件先弹确认再写回；本地文件直接保存。 */
  const requestSave = () => {
    if (busy || !dirty) return;
    if (side === "remote") {
      setConfirming(true);
      return;
    }
    void save();
  };

  const save = async () => {
    setBusy(true);
    setError("");
    try {
      if (side === "remote" && sessionId) await fileApi.write(sessionId, entry.path, text);
      else await localApi.write(entry.path, text);
      setDirty(false);
      setNotice(tr("fe.saved"));
      onSaved?.();
      window.clearTimeout(noticeTimer.current);
      noticeTimer.current = window.setTimeout(() => setNotice(""), 1600);
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setBusy(false);
    }
  };

  const [askClose, setAskClose] = useState(false);
  const close = () => {
    if (dirty) {
      setAskClose(true);
      return;
    }
    onClose();
  };
  // 保存成功提示的计时器必须复用并随卸载清理：旧写法每次 save 新建一个 setTimeout，
  // 连续保存时前一个定时器会提前抹掉后一条提示（甚至卸后 setState）。
  const noticeTimer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(noticeTimer.current), []);

  // Esc/遮罩都走 close（脏数据会先弹“放弃修改”确认，不会静默丢内容）；
  // 遮罩旧写法用 click + target 判定，从 textarea 拖选文本后松手在遮罩上仍会误关。
  const maskProps = useMaskDismiss(close);
  useEscClose(close);

  return (
    <div className="modal-mask" {...maskProps}>
      <div className="modal fe" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <h2>
            <IconEdit /> {entry.name}
            {dirty ? " *" : ""}
          </h2>
          {notice && <span className="fp-flash">{notice}</span>}
          <button className="modal-x" onClick={close} aria-label={tr("common.close")}>
            ×
          </button>
        </div>
        <div className="fe-path" title={entry.path}>
          {side === "remote" ? <IconMonitor /> : <IconLaptop />}
          {entry.path}
        </div>
        <textarea
          ref={taRef}
          className="fe-ta"
          value={text}
          spellCheck={false}
          disabled={busy && !text}
          onChange={(e) => {
            setText(e.target.value);
            setDirty(true);
          }}
          onKeyDown={(e) => {
            if ((e.ctrlKey || e.metaKey) && e.key === "s") {
              e.preventDefault();
              requestSave();
            }
          }}
        />
        {askClose && (
        <ConfirmDialog
          title={tr("fe.discardTitle")}
          message={tr("fe.discardMsg")}
          okText={tr("fe.discardOk")}
          onOk={() => {
            setAskClose(false);
            onClose();
          }}
          onClose={() => setAskClose(false)}
        />
      )}
      {error && <div className="form-error">{error}</div>}
        <div className="form-actions sec-set-foot">
          <span className="form-hint">
            {kbdLabel(
              tr("fe.hint")
                .replace("{lines}", String(text.split("\n").length))
                .replace("{chars}", String(text.length)),
            )}
          </span>
          {/* 两按钮包成一组：sec-set-foot 的 space-between 是为「左 hint + 右按钮」两孩子设计的，
              直接放三个会把「关闭」甩到中间悬着。 */}
          <div className="fe-foot-btns">
            {/* 共享 <Button>：`.fe-foot-btns` 在 CSS 里没有 `button` 规则，旧写法（裸 button + 死类名
               `.ghost`）会退回浏览器默认灰框，与全应用按钮观感割裂。 */}
            <Button variant="ghost" onClick={close}>
              {tr("common.close")}
            </Button>
            <Button variant="primary" disabled={busy || !dirty} onClick={requestSave}>
              {busy ? tr("fe.saving") : tr("common.save")}
            </Button>
          </div>
        </div>
      </div>

      {confirming && (
        <ConfirmDialog
          title={tr("fp.syncTitle")}
          message={entry.path}
          warning={tr("fe.writeWarn")}
          okText={tr("fe.writeBack")}
          onOk={() => {
            setConfirming(false);
            void save();
          }}
          onClose={() => setConfirming(false)}
        />
      )}
    </div>
  );
}
