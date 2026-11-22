// 片段新建/编辑表单（共享行为层）：快捷栏新建/编辑弹框（SnippetFormDialog）嵌同一份，
// 字段、校验、自动命名、报错回显全项目只此一套（片段库设置页已撤，入口统一到快捷栏）。
// 名称留空自动取命令首行前 12 字；编辑态带 id 保存，备注随表单可改。
// 归属池（全局/组/本连接）由入口决定，不在表单里填；旧「分组」标签字段已撤（编辑时原值保留）。
import { useState } from "react";
import { snippetActions, snippetStore } from "../../stores";
import type { Snippet, SnippetInput } from "../../types";
import { useI18n } from "../../lib/i18n";
import Button from "../common/Button";

interface Props {
  /** 编辑目标；null = 新建。改动 editing 需由调用方换 key 重挂载，避免残留旧值。 */
  editing: Snippet | null;
  /** 保存成功回调（落库已由本组件完成）；父级据此关表单 / 把新片段勾进池。 */
  onSave: (s: Snippet) => void;
  onCancel: () => void;
}

/** 名称留空时的自动命名：命令首行前 12 字（多行取首行、超长省略号）。
 *  按码点截断：slice 会把 emoji 等代理对劈成半个乱码字符。 */
function autoName(content: string): string {
  const trimmed = content.trim();
  return (
    Array.from(trimmed.split("\n", 1)[0]).slice(0, 12).join("") +
    (trimmed.length > 12 ? "…" : "")
  );
}

export default function SnippetEditor({ editing, onSave, onCancel }: Props) {
  const t = useI18n();
  const [fName, setFName] = useState(editing?.name ?? "");
  const [fContent, setFContent] = useState(editing?.content ?? "");
  const [fDesc, setFDesc] = useState(editing?.description ?? "");
  const [localErr, setLocalErr] = useState("");
  // 保存锁：submit 是异步的，双击会重复建条/重复保存，加锁串行化。
  const [submitting, setSubmitting] = useState(false);

  async function submit() {
    if (submitting) return;
    if (!fContent.trim()) {
      setLocalErr(t("sn.errContent"));
      return;
    }
    // 后端强校验 name 非空（commands/snippet.rs）；表单不强迫用户起名：
    // 留空时自动取命令首行作默认名，后期可再编辑改名。
    const name = fName.trim() || autoName(fContent);
    setLocalErr("");
    setSubmitting(true);
    try {
      const input: SnippetInput = {
        ...(editing ? { id: editing.id } : {}),
        name,
        // 分组标签不再让用户填：新建不带标签，编辑保留原值（列表里的小标签只展示历史数据）。
        groupName: editing?.groupName ?? null,
        content: fContent,
        description: fDesc.trim() || null,
      };
      const saved = await snippetActions.save(input);
      if (!saved) {
        // save 已把后端错（如「含明文凭证」）写进 snippetStore.error，接个本地回显保证就地可见
        setLocalErr(snippetStore.get().error || t("sn.errContent"));
        return;
      }
      onSave(saved);
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
      <label>
        {t("common.name")}
        <input
          value={fName}
          onChange={(e) => setFName(e.target.value)}
          placeholder={t("qbar.newNamePh")}
          spellCheck={false}
        />
      </label>
      <label>
        {t("sn.contentLabel")}
        <textarea
          value={fContent}
          onChange={(e) => setFContent(e.target.value)}
          placeholder={"sudo systemctl restart {{svc}}\ntail -n 100 /var/log/{{svc}}.log"}
          rows={3}
          autoFocus={!editing}
          spellCheck={false}
        />
      </label>
      <label>
        {t("sn.descLabel")}
        <input
          value={fDesc}
          onChange={(e) => setFDesc(e.target.value)}
          placeholder={t("sn.descPh")}
          spellCheck={false}
        />
      </label>
      {localErr && <div className="qbar-new-err">{localErr}</div>}
      <div className="form-actions">
        <Button variant="ghost" size="sm" onClick={onCancel} disabled={submitting}>
          {t("common.cancel")}
        </Button>
        <Button type="submit" variant="primary" size="sm" loading={submitting}>
          {t("common.save")}
        </Button>
      </div>
    </form>
  );
}
