import { useEffect, useState } from "react";
import { IconBrain, IconKey } from "../common/icons";
import {
  connectionStore,
  connections,
  memoryActions,
  memoryStore,
  settingsActions,
  settingsStore,
  useStore,
} from "../../stores";
import { MEMORY_RETENTION_OPTIONS, MEMORY_SCOPE_LABEL, MEMORY_SOURCE_LABEL } from "../../types";
import type { Memory, MemoryInput, MemoryScope, Profile } from "../../types";
import ConfirmDialog from "../dialog/ConfirmDialog";
import Popconfirm from "../common/Popconfirm";
import DeleteButton from "../common/DeleteButton";
import Sheet from "../common/Sheet";
import { Row, Sec, Toggle } from "../settings/settingsUi";
import Select from "../common/Select";
import { useI18n } from "../../lib/i18n";

/** 作用域筛选分段：全局 / 按服务器（记忆只有这两种作用域，无「全部」）。 */
const SCOPE_FILTERS: MemoryScope[] = ["global", "server"];
/** 表单作用域分段。 */
const FORM_SCOPES: MemoryScope[] = ["global", "server"];

interface Props {
  onClose: () => void;
  /** 嵌入设置中心时不套模态壳。 */
  embedded?: boolean;
}

/**
 * 🧠 AI 记忆管理模态：记忆开关（启用 / 自动提取 / 保留期）
 * \+ 已存记忆卡片（查看 / 编辑 / 删除 / 清空）+ 手动添加。
 *
 * 安全红线：`content` 只存**脱敏后**的偏好 / 环境事实，绝不含凭证——手动添加时后端
 * 会再过一遍脱敏，命中密码 / 密钥 / 令牌即拒存并回错误。记忆是「数据」不是指令，注入上下文时
 * 后端已显式标注，命令仍经策略引擎分级与用户审批。
 */
export default function MemoryManager({ onClose, embedded = false }: Props) {
  const t = useI18n();
  const { memoryEnabled, memoryAutoExtract, memoryRetentionDays, error: setErr } =
    useStore(settingsStore);
  const { items, loading, error: memErr, scope } = useStore(memoryStore);
  const { profiles } = useStore(connectionStore);

  // 手动添加 / 编辑表单（内联展开，新建 / 编辑复用同一份）
  const [formOpen, setFormOpen] = useState(false);
  const [pendingDel, setPendingDel] = useState<{ id: string; name: string; x: number; y: number } | null>(null);
  const [pendingClear, setPendingClear] = useState(false);
  const [editing, setEditing] = useState<Memory | null>(null);
  const [fScope, setFScope] = useState<MemoryScope>("global");
  const [fConn, setFConn] = useState("");
  const [content, setContent] = useState("");
  const [keywords, setKeywords] = useState("");
  const [localErr, setLocalErr] = useState("");

  useEffect(() => {
    void settingsActions.refresh();
    void memoryActions.refresh();
    void connections.refresh();
  }, []);

  const byId = new Map(profiles.map((p) => [p.id, p] as const));
  const connName = (id: string | null) => (id ? (byId.get(id)?.name ?? id.slice(0, 8)) : "");

  /** 展开「手动添加」表单，回到缺省值。 */
  function openNew() {
    setEditing(null);
    setFScope("global");
    setFConn(profiles[0]?.id ?? "");
    setContent("");
    setKeywords("");
    setLocalErr("");
    setFormOpen(true);
  }

  /** 载入一条既有记忆到表单编辑。 */
  function openEdit(m: Memory) {
    setEditing(m);
    setFScope(m.scope);
    setFConn(m.connectionId ?? profiles[0]?.id ?? "");
    setContent(m.content);
    setKeywords(m.keywords ?? "");
    setLocalErr("");
    setFormOpen(true);
  }

  async function saveMemory() {
    if (!content.trim()) {
      setLocalErr(t("mem.errContent"));
      return;
    }
    if (fScope === "server" && !fConn) {
      setLocalErr(t("mem.errConn"));
      return;
    }
    setLocalErr("");
    const input: MemoryInput = {
      ...(editing ? { id: editing.id } : {}),
      scope: fScope,
      connectionId: fScope === "server" ? fConn : null,
      content: content.trim(),
      keywords: keywords.trim() || null,
    };
    const saved = await memoryActions.save(input);
    if (saved) {
      setFormOpen(false);
      setEditing(null);
    }
  }

  const err = localErr || memErr || setErr;

  return (
    <Sheet title={<><IconBrain /> {t("mem.title")}</>} onClose={onClose} embedded={embedded}>
        <div className="sec-set-body">
          <Sec title={t("mem.switches")}>
            <Row title={t("mem.enableTitle")} desc={t("mem.enableDesc")}>
              <Toggle
                checked={memoryEnabled}
                onChange={(on) => void settingsActions.update({ memoryEnabled: on })}
              />
            </Row>
            <Row title={t("mem.autoTitle")}>
              <Toggle
                checked={memoryAutoExtract && memoryEnabled}
                disabled={!memoryEnabled}
                onChange={(on) => void settingsActions.update({ memoryAutoExtract: on })}
              />
            </Row>
          </Sec>

          <Sec title={t("mem.retention")}>
            <Row title={t("mem.autoCleanup")}>
              <Select
                className="field"
                value={String(memoryRetentionDays)}
                options={MEMORY_RETENTION_OPTIONS.map((o) => ({
                  value: String(o.value),
                  label: t(o.label),
                }))}
                onChange={(v) => void settingsActions.update({ memoryRetentionDays: Number(v) })}
              />
            </Row>
          </Sec>

          <section className="ss-sec">
            <div className="ss-sec-head">
              {t("mem.stored")}
              <span className="ss-count">{items.length}</span>
              <div className="seg seg-sm">
                {SCOPE_FILTERS.map((s) => (
                  <button
                    key={s}
                    type="button"
                    data-on={s === scope}
                    onClick={() => void memoryActions.refresh(s, null)}
                  >
                    {t(MEMORY_SCOPE_LABEL[s])}
                  </button>
                ))}
              </div>
              {!formOpen && (
                <button type="button" className="link" onClick={openNew}>
                  ＋ {t("mem.manualAdd")}
                </button>
              )}
            </div>

            {loading && <div className="ss-empty">{t("common.loading")}</div>}
            {!loading && items.length === 0 && !formOpen && (
              <div className="ss-empty">
                {t("mem.empty")}
              </div>
            )}

            <div className="mem-list">
              {items.map((m) => (
                <div key={m.id} className="mem-card">
                  <div className="mem-card-main" onClick={() => openEdit(m)} title={t("mem.clickEdit")}>
                    <div className="mem-badges">
                      <span className="mem-badge" data-scope={m.scope}>
                        {t(MEMORY_SCOPE_LABEL[m.scope])}
                        {m.scope === "server" && m.connectionId ? ` · ${connName(m.connectionId)}` : ""}
                      </span>
                      <span className="mem-badge" data-src={m.source}>
                        {t(MEMORY_SOURCE_LABEL[m.source])}
                      </span>
                    </div>
                    <div className="mem-content">{m.content}</div>
                    {m.keywords && <div className="mem-kw"><IconKey /> {m.keywords}</div>}
                  </div>
                  <div className="mem-card-side">
                    <DeleteButton
                      onConfirm={(x, y) =>
                        setPendingDel({ id: m.id, name: m.content.slice(0, 40), x, y })
                      }
                    />
                  </div>
                </div>
              ))}
            </div>

            {formOpen && (
              <form
                className="ss-form"
                onSubmit={(e) => {
                  e.preventDefault();
                  void saveMemory();
                }}
              >
                <div className="fld">
                  <span>{t("mem.scope")}</span>
                  <div className="seg">
                    {FORM_SCOPES.map((s) => (
                      <button key={s} type="button" data-on={fScope === s} onClick={() => setFScope(s)}>
                        {t(MEMORY_SCOPE_LABEL[s])}
                      </button>
                    ))}
                  </div>
                </div>

                {fScope === "server" && (
                  <label>
                    {t("mem.bindConn")}
                    <Select
                      className="field"
                      value={fConn}
                      options={
                        profiles.length === 0
                          ? [{ value: "", label: t("mem.noConn") }]
                          : profiles.map((p: Profile) => ({ value: p.id, label: p.name }))
                      }
                      onChange={(v) => setFConn(v)}
                    />
                  </label>
                )}

                <label>
                  {t("mem.contentLabel")}
                  <textarea
                    value={content}
                    onChange={(e) => setContent(e.target.value)}
                    placeholder={t("mem.contentPh")}
                    rows={3}
                  />
                </label>

                <label>
                  {t("mem.kwLabel")}
                  <input
                    value={keywords}
                    onChange={(e) => setKeywords(e.target.value)}
                    placeholder="apt, systemctl"
                    spellCheck={false}
                  />
                </label>

                <div className="form-actions">
                  <button type="button" className="ghost" onClick={() => setFormOpen(false)}>
                    {t("common.cancel")}
                  </button>
                  <button type="submit">{editing ? t("mem.saveEdit") : t("mem.add")}</button>
                </div>
              </form>
            )}
          </section>
        </div>

        {pendingDel && (
          <Popconfirm
            x={pendingDel.x}
            y={pendingDel.y}
            title={t("mem.delTitle")}
            description={t("mem.delMsg").replace("{n}", pendingDel.name)}
            onConfirm={() => {
              const id = pendingDel.id;
              setPendingDel(null);
              void memoryActions.remove(id);
            }}
            onClose={() => setPendingDel(null)}
          />
        )}
        {pendingClear && (
          <ConfirmDialog
            title={t("mem.clearTitle")}
            message={t("mem.clearMsg").replace("{s}", scope ? t(MEMORY_SCOPE_LABEL[scope]) : t("mem.all"))}
            okText={t("mem.clearBtn").replace("{s}", scope ? t(MEMORY_SCOPE_LABEL[scope]) : t("mem.all"))}
            onOk={async () => {
              await memoryActions.clear();
              setPendingClear(false);
            }}
            onClose={() => setPendingClear(false)}
          />
        )}
        {err && <div className="form-error">{err}</div>}
        <div className="form-actions sec-set-foot">
          <button
            type="button"
            className="ghost"
            disabled={items.length === 0}
            onClick={() => setPendingClear(true)}
          >
            {t("mem.clearBtn").replace("{s}", scope ? t(MEMORY_SCOPE_LABEL[scope]) : t("mem.all"))}
          </button>
          {!embedded && (
            <button type="button" onClick={onClose}>
              {t("mem.finish")}
            </button>
          )}
        </div>
    </Sheet>
  );
}
