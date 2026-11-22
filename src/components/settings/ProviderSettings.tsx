import { useEffect, useState } from "react";
import { providerActions, providerStore, useStore } from "../../stores";
import { PROVIDER_LABEL } from "../../types";
import type { AiProvider } from "../../types";
import Sheet from "../common/Sheet";
import Popconfirm from "../common/Popconfirm";
import DeleteButton from "../common/DeleteButton";
import { IconEdit } from "../common/icons";
import { Toggle } from "./settingsUi";
import ProviderFormDialog from "./ProviderFormDialog";
import { useI18n } from "../../lib/i18n";

interface Props {
  onClose: () => void;
  /** 嵌入设置中心时不套模态壳。 */
  embedded?: boolean;
}

/**
 * AI 模型（Provider）管理列表：左列既有配置（编辑 / 删除 / 设默认 / 新建）。
 *
 * 新建与编辑的表单本体已抽成独立可复用组件 `ProviderFormDialog`——本列表只负责「何时弹表单、
 * 弹哪一条」，弹窗叠在列表之上；聊天 / Agent 的模型下拉则直接复用同一表单组件而不带这层列表壳。
 */
export default function ProviderSettings({ onClose, embedded = false }: Props) {
  const t = useI18n();
  const { items } = useStore(providerStore);
  // 打开的表单：null=关闭；provider=null 为新建、非空为编辑该条。
  const [form, setForm] = useState<{ provider: AiProvider | null } | null>(null);
  /** 待删除的 Provider + 气泡锚点（删模型会连带清除保险库里的 API Key）。 */
  const [pendingDel, setPendingDel] = useState<{ p: AiProvider; x: number; y: number } | null>(null);

  useEffect(() => {
    void providerActions.refresh();
  }, []);

  async function remove(p: AiProvider) {
    await providerActions.remove(p.id);
    if (form?.provider?.id === p.id) setForm(null);
  }

  return (
    <>
      <Sheet title={t("pv.title")} onClose={onClose} embedded={embedded} variant="pv">
        <div className="pv-body">
          <section className="ss-sec">
            <div className="ss-sec-head">
              {t("pv.modelConfig")}
              <span className="pv-head-desc">{t("pv.headDesc")}</span>
              <button type="button" className="link" onClick={() => setForm({ provider: null })}>
                ＋ {t("common.new")}
              </button>
            </div>

            <div className="pv-cards">
              {items.length === 0 && (
                <div className="pv-empty">
                  <svg
                    className="pv-empty-ico"
                    width="30"
                    height="30"
                    viewBox="0 0 24 24"
                    fill="none"
                    aria-hidden
                  >
                    <rect
                      x="3.5"
                      y="5.5"
                      width="17"
                      height="13"
                      rx="2.5"
                      stroke="currentColor"
                      strokeWidth="1.4"
                    />
                    <path d="M7 10.5h6M7 13.5h4" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
                  </svg>
                  <div className="pv-empty-title">{t("pv.emptyTitle")}</div>
                  <div className="pv-empty-desc">{t("pv.emptyDesc")}</div>
                </div>
              )}
              {items.map((p) => (
                <div key={p.id} className="pv-card" data-on={form?.provider?.id === p.id}>
                  <div
                    className="pv-card-main"
                    title={t("pv.clickEdit")}
                    onClick={() => setForm({ provider: p })}
                  >
                    <div className="pv-card-top">
                      <span className="pv-name">{p.name}</span>
                      <span className="pv-badge">{t(PROVIDER_LABEL[p.kind])}</span>
                      {p.isDefault && <span className="pv-badge pv-badge-def">{t("pv.default")}</span>}
                      <span
                        className={`pv-badge ${p.supportsVision === true ? "pv-badge-vis" : ""}`}
                        title={t("pv.visionBadgeTip")}
                      >
                        {p.supportsVision === true
                          ? t("pv.badgeVision")
                          : p.supportsVision === false
                            ? t("pv.badgeTextOnly")
                            : t("pv.badgeVisionUnknown")}
                      </span>
                    </div>
                    <div className="pv-sub">
                      {p.model} · {p.hasKey ? t("pv.keySaved") : t("pv.keyUnset")}
                    </div>
                  </div>
                  <div className="pv-card-act">
                    <button
                      type="button"
                      className="icon-btn"
                      title={t("common.edit")}
                      onClick={() => setForm({ provider: p })}
                    >
                      <IconEdit />
                    </button>
                    <DeleteButton onConfirm={(x, y) => setPendingDel({ p, x, y })} />
                    <Toggle
                      title={p.isDefault ? t("pv.isDefault") : t("pv.setDefault")}
                      checked={p.isDefault}
                      onChange={(on) => {
                        if (on) void providerActions.setDefault(p.id);
                      }}
                    />
                  </div>
                </div>
              ))}
            </div>
          </section>
        </div>
      </Sheet>

      {form && <ProviderFormDialog initial={form.provider} onClose={() => setForm(null)} />}

      {pendingDel && (
        <Popconfirm
          x={pendingDel.x}
          y={pendingDel.y}
          title={t("pv.delMsg").replace("{n}", pendingDel.p.name)}
          description={t("pv.delWarn")}
          onConfirm={() => {
            const p = pendingDel.p;
            setPendingDel(null);
            void remove(p);
          }}
          onClose={() => setPendingDel(null)}
        />
      )}
    </>
  );
}
