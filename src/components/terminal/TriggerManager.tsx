import { useEffect, useState } from "react";
import { IconEdit, IconZap } from "../common/icons";
import { appearanceStore, triggerActions, triggerStore, useStore } from "../../stores";
import { highlightColor, resolveTheme, TRIGGER_COLORS } from "../../lib/themes";
import type { TriggerAction, TriggerColor, TriggerInput, TriggerRule } from "../../types";
import Sheet from "../common/Sheet";
import Popconfirm from "../common/Popconfirm";
import DeleteButton from "../common/DeleteButton";
import { Toggle } from "../settings/settingsUi";
import { useI18n } from "../../lib/i18n";

interface Props {
  onClose: () => void;
  /** 嵌入设置中心时不套模态壳。 */
  embedded?: boolean;
}

const ACTIONS: { v: TriggerAction; label: string }[] = [
  { v: "alert", label: "trg.alert" },
  { v: "highlight", label: "trg.highlight" },
  { v: "colorize", label: "trg.colorize" },
];

/** 试编译正则；非法则回错误文案。 */
function checkPattern(src: string): string {
  try {
    new RegExp(src);
    return "";
  } catch (e) {
    return e instanceof Error ? e.message : "invalid regex";
  }
}

/**
 * ⚡ 输出触发器：对终端输出做正则匹配，触发 toast 告警 / 瞬时闪高亮 / 关键字着色。
 *
 * 着色（colorize）是客户端语义着色——服务端没发色的地方由我们按规则上色，
 * 渲在终端缓冲上的具体实现见 `lib/termHighlight.ts`。
 *
 * 安全红线：规则只存名称、正则源串与色键，**绝不含凭证**；匹配与动作都在本机前端完成，
 * 不落盘输出、不出网。
 */
export default function TriggerManager({ onClose, embedded = false }: Props) {
  const t = useI18n();
  const { items, error } = useStore(triggerStore);
  const { theme, customTheme } = useStore(appearanceStore);
  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<TriggerRule | null>(null);
  const [name, setName] = useState("");
  const [pattern, setPattern] = useState("");
  const [action, setAction] = useState<TriggerAction>("alert");
  const [color, setColor] = useState<TriggerColor>("red");
  const [localErr, setLocalErr] = useState("");
  /** 待删除的触发器 + 气泡锚点：与侧栏删连接同口径的就地确认。 */
  const [pendingDel, setPendingDel] = useState<{ id: string; name: string; x: number; y: number } | null>(null);

  /** 色键 → 当前主题下的实色（预览点与终端里渲出来的色一致）。 */
  const hex = (key: TriggerColor) => highlightColor(resolveTheme(theme, customTheme), key);

  useEffect(() => {
    void triggerActions.refresh();
  }, []);

  const openNew = () => {
    setEditing(null);
    setName("");
    setPattern("");
    setAction("alert");
    setColor("red");
    setLocalErr("");
    setFormOpen(true);
  };

  const openEdit = (r: TriggerRule) => {
    setEditing(r);
    setName(r.name);
    setPattern(r.pattern);
    setAction(r.action);
    setColor(r.color);
    setLocalErr("");
    setFormOpen(true);
  };

  async function save() {
    const n = name.trim();
    const p = pattern.trim();
    if (!n || !p) {
      setLocalErr(t("trg.errRequired"));
      return;
    }
    const perr = checkPattern(p);
    if (perr) {
      setLocalErr(t("trg.errRegex").replace("{m}", perr));
      return;
    }
    setLocalErr("");
    const input: TriggerInput = {
      ...(editing ? { id: editing.id } : {}),
      name: n,
      pattern: p,
      action,
      color,
    };
    if (await triggerActions.save(input)) {
      setFormOpen(false);
      setEditing(null);
    }
  }

  const err = localErr || error;

  return (
    <>
    <Sheet title={<><IconZap /> {t("trg.title")}</>} onClose={onClose} embedded={embedded}>
      <div className="sec-set-body">
        <section className="ss-sec">
          <div className="ss-sec-head">
            {t("trg.rules")}
            <span className="ss-count">{items.length}</span>
            {!formOpen && (
              <button type="button" className="link" onClick={openNew}>
                ＋ {t("trg.new")}
              </button>
            )}
          </div>
          {items.length === 0 && !formOpen && (
            <div className="ss-empty">{t("trg.empty")}</div>
          )}
          {items.map((r) => (
            <div key={r.id} className="trg-item">
              <span className="trg-name">{r.name}</span>
              <code className="trg-pat" title={r.pattern}>
                {r.pattern}
              </code>
              {r.action === "colorize" && (
                <span className="trg-dot" style={{ background: hex(r.color) }} title={t(`trg.color.${r.color}`)} />
              )}
              <span className="trg-act">{t(ACTIONS.find((a) => a.v === r.action)?.label ?? r.action)}</span>
              <Toggle
                checked={r.enabled}
                title={r.enabled ? t("trg.disable") : t("trg.enable")}
                onChange={(on) =>
                  void triggerActions.save({ id: r.id, name: r.name, pattern: r.pattern, action: r.action, enabled: on })
                }
              />
              <button
                type="button"
                className="icon-btn"
                title={t("common.edit")}
                aria-label={t("common.edit")}
                onClick={() => openEdit(r)}
              >
                <IconEdit />
              </button>
              <DeleteButton onConfirm={(x, y) => setPendingDel({ id: r.id, name: r.name, x, y })} />
            </div>
          ))}
          {formOpen && (
            <form
              className="ss-form"
              onSubmit={(e) => {
                e.preventDefault();
                void save();
              }}
            >
              <label>
                {t("trg.name")}
                <input value={name} onChange={(e) => setName(e.target.value)} placeholder={t("trg.namePh")} />
              </label>
              <label>
                {t("trg.pattern")}
                <input
                  value={pattern}
                  spellCheck={false}
                  onChange={(e) => setPattern(e.target.value)}
                  placeholder="error|fail|critical"
                />
              </label>
              <div className="seg">
                {ACTIONS.map((a) => (
                  <button
                    key={a.v}
                    type="button"
                    data-on={action === a.v}
                    onClick={() => setAction(a.v)}
                  >
                    {t(a.label)}
                  </button>
                ))}
              </div>
              {action === "colorize" && (
                <div className="trg-colors">
                  {TRIGGER_COLORS.map((c) => (
                    <button
                      key={c}
                      type="button"
                      className="trg-dot-btn"
                      data-on={color === c}
                      title={t(`trg.color.${c}`)}
                      onClick={() => setColor(c)}
                    >
                      <span className="trg-dot" style={{ background: hex(c) }}>
                        Aa
                      </span>
                    </button>
                  ))}
                </div>
              )}
              <div className="form-hint">{t("trg.hintLocal")}</div>
              <div className="form-actions">
                <button type="button" className="ghost" onClick={() => setFormOpen(false)}>
                  {t("common.cancel")}
                </button>
                <button type="submit">{editing ? t("trg.saveEdit") : t("trg.add")}</button>
              </div>
            </form>
          )}
        </section>
      </div>

      {err && <div className="form-error">{err}</div>}

      <div className="form-actions sec-set-foot">
        {!embedded && (
          <button type="button" onClick={onClose}>
            {t("trg.finish")}
          </button>
        )}
      </div>
    </Sheet>

    {pendingDel && (
      <Popconfirm
        x={pendingDel.x}
        y={pendingDel.y}
        title={t("trg.delMsg").replace("{n}", pendingDel.name)}
        onConfirm={() => {
          const id = pendingDel.id;
          setPendingDel(null);
          void triggerActions.remove(id);
        }}
        onClose={() => setPendingDel(null)}
      />
    )}
    </>
  );
}
