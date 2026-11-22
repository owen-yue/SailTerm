// 变量填值面板（共享行为层）：含 {{变量}} 的片段在执行前就地填值（快捷栏 chip / 分组池入口共用）。
// 面板只管状态与提交锁，提交语义由调用方 onSubmit 决定，标题文案（qbar.fill）也由各调用方自持。
import { useState } from "react";
import type { Snippet } from "../../types";
import { useI18n } from "../../lib/i18n";
import Button from "../common/Button";

interface Props {
  snippet: Snippet;
  /** 标题区文案（已替换 {n}）。 */
  title: string;
  /** 提交按钮文案。 */
  okLabel: string;
  /** 确认提交：values 为用户填好的变量值。返回 false 表示失败、保留面板。 */
  onSubmit: (values: Record<string, string>) => void | Promise<void>;
  onCancel: () => void;
}

export default function SnippetFillForm({ snippet, title, okLabel, onSubmit, onCancel }: Props) {
  const t = useI18n();
  const [values, setValues] = useState<Record<string, string>>(() => {
    const init: Record<string, string> = {};
    for (const v of snippet.variables) init[v] = "";
    return init;
  });
  // 提交锁：写入是异步的，双击会让同一条命令重复写进终端（破坏性命令尤其危险）；
  // 面板由调用方在提交成功后卸载，失败保留时值不丢。
  const [submitting, setSubmitting] = useState(false);

  async function submit() {
    if (submitting) return;
    setSubmitting(true);
    try {
      await onSubmit({ ...values });
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <form
      className="ss-form"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <div className="snip-fill-head">{title}</div>
      <div className="snip-content">{snippet.content}</div>
      {snippet.variables.map((v, i) => (
        <label key={v}>
          {`{{${v}}}`}
          <input
            value={values[v] ?? ""}
            onChange={(e) => setValues((prev) => ({ ...prev, [v]: e.target.value }))}
            autoFocus={i === 0}
            spellCheck={false}
          />
        </label>
      ))}
      <div className="form-actions">
        <Button variant="ghost" size="sm" onClick={onCancel} disabled={submitting}>
          {t("common.cancel")}
        </Button>
        <Button type="submit" size="sm" loading={submitting}>
          {okLabel}
        </Button>
      </div>
    </form>
  );
}
