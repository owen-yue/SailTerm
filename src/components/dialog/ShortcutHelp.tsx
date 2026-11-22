import { useI18n } from "../../lib/i18n";
import { kbdLabel } from "../../lib/kbd";
import { useEscClose } from "../../lib/useOverlay";
import { COMMANDS, EXTRA_SHORTCUTS, HELP_GROUP_ORDER, type ShortcutRow } from "../../lib/commands";

interface Props {
  open: boolean;
  onClose: () => void;
}

/**
 * 快捷键帮助浮层（Ctrl+/）：把所有键位按分组铺开，纯只读速查。
 * 数据 = 命令注册表里带 keys 的命令 + EXTRA_SHORTCUTS（终端/导航类只读键位），
 * 与命令面板共用 lib/commands，改一处两处同步。Esc / 点遮罩 / 右上 × 关闭。
 */
export default function ShortcutHelp({ open, onClose }: Props) {
  const tr = useI18n();

  // Esc 关窗走弹层栈（旧写法自挂 window capture 监听，嵌套弹层时与下层各写一遍的监听互撞）。
  useEscClose(onClose, open);

  if (!open) return null;

  // 命令（有键位的）+ 额外只读键位，合成统一行，再按 HELP_GROUP_ORDER 归并。
  const rows: ShortcutRow[] = [
    ...COMMANDS.filter((c) => c.keys).map((c) => ({ labelKey: c.labelKey, keys: c.keys!, groupKey: c.groupKey })),
    ...EXTRA_SHORTCUTS,
  ];
  const groups: { key: string; items: ShortcutRow[] }[] = HELP_GROUP_ORDER.map((key) => ({
    key,
    items: rows.filter((r) => r.groupKey === key),
  })).filter((g) => g.items.length > 0);

  return (
    <div className="cmdp-mask" onMouseDown={onClose}>
      <div
        className="cmdp help"
        role="dialog"
        aria-modal="true"
        aria-label={tr("cmd.help")}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="help-head">
          <h2>{tr("cmd.helpTitle")}</h2>
          <button type="button" className="help-x" onClick={onClose} aria-label={tr("common.close")} title={tr("common.close")}>
            ✕
          </button>
        </div>
        <div className="help-body">
          {groups.map((g) => (
            <div className="help-group" key={g.key}>
              <div className="cmdp-group-title">{tr(g.key)}</div>
              {g.items.map((r) => (
                <div className="help-row" key={r.labelKey + r.keys}>
                  <span className="cmdp-label">{tr(r.labelKey)}</span>
                  <kbd className="cmdp-keys">{kbdLabel(r.keys)}</kbd>
                </div>
              ))}
            </div>
          ))}
        </div>
        <div className="cmdp-foot">{kbdLabel(tr("cmd.helpHint"))}</div>
      </div>
    </div>
  );
}
