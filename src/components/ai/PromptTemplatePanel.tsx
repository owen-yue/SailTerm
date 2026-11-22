import { useEffect, useRef, useState } from "react";
import { ai } from "../../lib/tauri";
import { errMsg } from "../../lib/tauri/client";
import { toast, uiPrefsStore, useStore } from "../../stores";
import { useI18n, getUiLangName } from "../../lib/i18n";
import type { PromptPreview } from "../../types";
import { Row, Sec } from "../settings/settingsUi";
import Button from "../common/Button";
import HoverPopover from "../common/HoverPopover";
import TokenTextarea, { type TokenDef, type TokenTextareaHandle } from "./TokenTextarea";

/** 模板宏（占位符）参考表：`k` 是 dev 写进模板的字面量，`key` 是说明的 i18n 键。 */
const MACROS: { k: string; key: string }[] = [
  { k: "{{role}}", key: "pt.mRole" },
  { k: "{{style}}", key: "pt.mStyle" },
  { k: "{{capability}}", key: "pt.mCapability" },
  { k: "{{credential}}", key: "pt.mCredential" },
  { k: "{{chat_mode}}", key: "pt.mChatMode" },
  { k: "{{language}}", key: "pt.mLanguage" },
  { k: "{{env}}", key: "pt.mEnv" },
  { k: "{{memory}}", key: "pt.mMemory" },
  { k: "{{context}}", key: "pt.mContext" },
];

/** 去掉 `{{ }}` 得到补全标识（TokenTextarea 的 key 只存裸名）。 */
const macroKey = (k: string) => k.slice(2, -2);

/**
 * 系统提示词模板编辑面板（仅开发者模式可见）。
 *
 * 分层兜底：① 内置默认模板编译进二进制、dev 改不到，库里只存覆盖版（空即回落）；
 * ② 打开设置页/本面板都不依赖 AI 能否运行，「恢复默认」即删覆盖；③ 运行期只体检+告警、
 * 绝不自动回退；④ 末尾【安全底线】由后端代码永远追加、不进模板、删不掉。
 * 因此这里编辑的是「立场与话术」，真正的执行安全仍由策略引擎/审批/脱敏兜底。
 */
export default function PromptTemplatePanel() {
  const t = useI18n();
  const { devMode } = useStore(uiPrefsStore);
  const editorRef = useRef<TokenTextareaHandle>(null);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [customized, setCustomized] = useState(false);
  const [defaultTemplate, setDefaultTemplate] = useState("");
  const [draft, setDraft] = useState("");
  const [preview, setPreview] = useState<PromptPreview | null>(null);
  const [error, setError] = useState("");

  // devMode 关闭时本面板整体不渲染、也不发任何 IPC（门禁 + 懒加载）。
  useEffect(() => {
    if (!devMode) return;
    setLoading(true);
    setError("");
    void (async () => {
      try {
        const v = await ai.promptGet();
        setCustomized(v.customized);
        setDefaultTemplate(v.defaultTemplate);
        // 未自定义时编辑框留空、以默认模板作占位提示；避免一进来就制造「等于默认」的覆盖。
        setDraft(v.customized ? v.template : "");
      } catch (e) {
        setError(errMsg(e));
      } finally {
        setLoading(false);
      }
    })();
  }, [devMode]);

  if (!devMode) return null;

  const run = async (fn: () => Promise<void>) => {
    setBusy(true);
    setError("");
    try {
      await fn();
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setBusy(false);
    }
  };

  const onSave = () =>
    run(async () => {
      const v = await ai.promptSet(draft);
      setCustomized(v.customized);
      setDraft(v.customized ? v.template : "");
      toast.success(t("pt.saved"));
    });

  const onReset = () =>
    run(async () => {
      const v = await ai.promptReset();
      setCustomized(v.customized);
      setDraft(v.template);
      setPreview(null);
      toast.show(t("pt.resetDone"));
    });

  const onPreview = () =>
    run(async () => {
      setPreview(await ai.promptPreview(draft, getUiLangName()));
    });

  // 候选令牌：key 用裸名（role/style/...），desc 取当前语言的宏说明。
  const tokens: TokenDef[] = MACROS.map((m) => ({ key: macroKey(m.k), desc: t(m.key) }));

  return (
    <>
      <Sec title={t("pt.title")}>
        <Row title={t("pt.rowTitle")} desc={t("pt.rowDesc")} wide>
          <span className="pt-badge" data-on={customized ? "true" : "false"}>
            {customized ? t("pt.customized") : t("pt.usingDefault")}
          </span>
        </Row>
      </Sec>

      <Sec title={t("pt.editorSec")} extra={
        <>
          {/* 悬停提示：用 portal+fixed 浮层，脱离设置面板的 overflow 裁切 */}
          <HoverPopover
            className="pt-macro-help"
            align="start"
            width={400}
            content={
              <div className="pt-macros-card">
                <div className="pt-macros-head">{t("pt.macroSec")}</div>
                <table className="pt-macros">
                  <tbody>
                    {MACROS.map((m) => (
                      <tr key={m.k}>
                        <td className="pt-macro-key">
                          <button
                            type="button"
                            className="pt-macro-chip"
                            title={t("pt.macroInsert")}
                            onClick={() => editorRef.current?.insertToken(macroKey(m.k))}
                          >
                            {m.k}
                          </button>
                        </td>
                        <td className="pt-macro-desc">{t(m.key)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            }
          >
            <span className="pt-macro-toggle-q">?</span>
            {t("pt.macroBtn")}
          </HoverPopover>
          <Button variant="ghost" size="sm" disabled={busy} onClick={() => setDraft(defaultTemplate)}>
            {t("pt.loadDefault")}
          </Button>
        </>
      }>
        <TokenTextarea
          ref={editorRef}
          className="pt-editor"
          value={draft}
          tokens={tokens}
          spellCheck={false}
          disabled={loading || busy}
          placeholder={loading ? t("pt.loading") : defaultTemplate}
          onChange={setDraft}
        />
        <p className="pt-safety">{t("pt.editorHint")}</p>
        <p className="pt-safety">{t("pt.scopeNote")}</p>
        <div className="pt-actions">
          <Button variant="ghost" disabled={busy || loading} onClick={() => void onPreview()}>
            {t("pt.preview")}
          </Button>
          <Button variant="ghost" disabled={busy || loading} onClick={() => void onReset()}>
            {t("pt.restoreDefault")}
          </Button>
          <Button disabled={busy || loading} onClick={() => void onSave()}>
            {t("pt.save")}
          </Button>
        </div>
        <p className="pt-safety">{t("pt.safetyNote")}</p>
        {error && <div className="form-error">{error}</div>}
      </Sec>

      {preview && (
        <Sec title={t("pt.previewSec")}>
          {preview.warning ? (
            <div className="pt-preview-warn">{preview.warning}</div>
          ) : (
            <div className="pt-preview-ok">{t("pt.previewOk")}</div>
          )}
          <pre className="pt-preview">{preview.rendered}</pre>
        </Sec>
      )}
    </>
  );
}
