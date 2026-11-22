import { useState } from "react";
import { useI18n } from "../../lib/i18n";
import { useEscClose, useMaskDismiss } from "../../lib/useOverlay";

interface Props {
  /** 目标文件当前权限位（八进制数值，如 0o644）。 */
  mode: number;
  /** 作用于的路径（仅提示用）。 */
  path: string;
  /** 提交：回传规范化后的八进制字符串（如 `"644"`、`"4755"`），与后端 `file_chmod` 契约一致。 */
  onSubmit: (octal: string) => void | Promise<void>;
  onClose: () => void;
}

/** 特殊位掩码（setuid|setgid|sticky），编辑符号串时原样保留。 */
const SPECIAL_MASK = 0o7000;

/** rwx 九位，按「用户/群组/其他 × 读/写/执行」顺序，与 9 字符符号串一一对应。 */
const RWX: { bit: number; ch: string }[] = [
  { bit: 0o400, ch: "r" },
  { bit: 0o200, ch: "w" },
  { bit: 0o100, ch: "x" },
  { bit: 0o040, ch: "r" },
  { bit: 0o020, ch: "w" },
  { bit: 0o010, ch: "x" },
  { bit: 0o004, ch: "r" },
  { bit: 0o002, ch: "w" },
  { bit: 0o001, ch: "x" },
];

/** 数值 → 9 字符 rwx 符号串（不含特殊位，对齐图2 的 Permissions 框）。 */
function symOf(m: number): string {
  return RWX.map(({ bit, ch }) => (m & bit ? ch : "-")).join("");
}

/** 数值 → 八进制串：≤0o777 补足 3 位，含特殊位则 4 位。 */
function octOf(m: number): string {
  return m > 0o777 ? m.toString(8) : m.toString(8).padStart(3, "0");
}

/** 9 字符 rwx 串 → rwx 位数值；含非法字符返回 NaN（特殊位由调用方另行保留）。 */
function rwxFromSym(s: string): number {
  let m = 0;
  for (let i = 0; i < 9; i++) {
    const c = s[i].toLowerCase();
    if (c === RWX[i].ch) m |= RWX[i].bit;
    else if (c !== "-") return NaN;
  }
  return m;
}

/**
 * 权限编辑器（替代原先只让填八进制的裸输入框）。
 *
 * 三种视图——3×3 勾选矩阵 / 八进制 / 符号串——以数值 `m` 为唯一真源双向联动；
 * 特殊位（setuid/setgid/sticky）折进「特殊位」抽屉，默认收起但状态始终正确。
 * 仅用于 SFTP 远程文件（POSIX 权限模型），见 `FilePane.tsx` 的 `isRemote && !ftp` 门槛。
 */
export default function ChmodDialog({ mode, path, onSubmit, onClose }: Props) {
  const t = useI18n();
  const [m, setM] = useState(mode & 0o7777);
  const [octal, setOctal] = useState(octOf(mode & 0o7777));
  const [sym, setSym] = useState(symOf(mode & 0o7777));
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);
  // Esc 挂 window 级弹层栈：旧写法把 Escape 绑在不可聚焦的 .chmod-body 上——
  // 它由右键菜单项触发，菜单卸载后焦点掉回 <body>，该 div 的 onKeyDown 永远接不到
  //（同一个坑 ConfirmDialog.tsx 的注释里已记过一次）。
  useEscClose(onClose);

  // 以数值为准回写三视图（勾选 / 特殊位走这里）
  const apply = (next: number) => {
    const v = next & 0o7777;
    setM(v);
    setOctal(octOf(v));
    setSym(symOf(v));
    setErr("");
  };
  const toggle = (bit: number) => apply(m & bit ? m & ~bit : m | bit);

  // 直接编辑文本框：各自解析成功后只更新「另一视图」，避免打断当前输入
  const onOctal = (v: string) => {
    setOctal(v);
    setErr("");
    if (/^(0o)?[0-7]{3,4}$/.test(v)) {
      const next = parseInt(v.replace(/^0o/, ""), 8) & 0o7777;
      setM(next);
      setSym(symOf(next));
    }
  };
  const onSym = (v: string) => {
    setSym(v);
    setErr("");
    if (/^[-rwx]{9}$/i.test(v)) {
      const next = (m & SPECIAL_MASK) | rwxFromSym(v);
      setM(next);
      setOctal(octOf(next));
    }
  };

  const submit = async () => {
    if (!/^[0-7]{3,4}$/.test(octal)) {
      setErr(t("fp.chmodErr"));
      return;
    }
    setBusy(true);
    try {
      await onSubmit(octOf(m));
      onClose();
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const rows = [
    { key: "fp.permUser", bits: [0o400, 0o200, 0o100] },
    { key: "fp.permGroup", bits: [0o040, 0o020, 0o010] },
    { key: "fp.permOther", bits: [0o004, 0o002, 0o001] },
  ];
  const cols = ["fp.permRead", "fp.permWrite", "fp.permExec"];
  const specials = [
    { key: "fp.permSetuid", bit: 0o4000 },
    { key: "fp.permSetgid", bit: 0o2000 },
    { key: "fp.permSticky", bit: 0o1000 },
  ];

  return (
    <div className="modal-mask" {...useMaskDismiss(onClose)}>
      <div className="modal chmod">
        <div className="modal-head">
          <h2>{t("fp.chmodTitle")}</h2>
          <button type="button" className="modal-x" title={t("common.close")} onClick={onClose}>
            ×
          </button>
        </div>

        <div
          className="chmod-body"
          onKeyDown={(e) => {
            // Enter 提交靠八进制输入框 autoFocus 保证焦点在窗内；Esc 已改走弹层栈。
            if (e.key === "Enter") void submit();
          }}
        >
          <table className="chmod-grid">
            <thead>
              <tr>
                <th />
                {cols.map((c) => (
                  <th key={c}>{t(c)}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {rows.map((r) => (
                <tr key={r.key}>
                  <th scope="row">{t(r.key)}</th>
                  {r.bits.map((b) => (
                    <td key={b}>
                      <input type="checkbox" checked={(m & b) !== 0} onChange={() => toggle(b)} />
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>

          <div className="chmod-fields">
            <label>
              <span>{t("fp.permOctal")}</span>
              <input value={octal} spellCheck={false} autoFocus onChange={(e) => onOctal(e.target.value)} />
            </label>
            <label>
              <span>{t("fp.permSymbolic")}</span>
              <input
                className="chmod-sym"
                value={sym}
                spellCheck={false}
                maxLength={9}
                onChange={(e) => onSym(e.target.value)}
              />
            </label>
          </div>

          <details className="chmod-adv">
            <summary>{t("fp.permSpecial")}</summary>
            <div className="chmod-special">
              {specials.map((s) => (
                <label key={s.bit} className="chmod-check">
                  <input type="checkbox" checked={(m & s.bit) !== 0} onChange={() => toggle(s.bit)} />
                  <span>{t(s.key)}</span>
                </label>
              ))}
            </div>
          </details>

          <div className="p-hint">{t("fp.chmodHint").replace("{n}", path)}</div>
          {err && <div className="form-error">{err}</div>}
        </div>

        <div className="form-actions">
          <button type="button" className="ghost" onClick={onClose}>
            {t("common.cancel")}
          </button>
          <button type="button" disabled={busy} onClick={() => void submit()}>
            {t("common.apply")}
          </button>
        </div>
      </div>
    </div>
  );
}
