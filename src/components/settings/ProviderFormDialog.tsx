import { useState } from "react";
import { providerActions, providerStore, TESTING_DRAFT, useStore } from "../../stores";
import { PROVIDER_DEFAULT_CONTEXT, PROVIDER_LABEL, PROVIDER_NEEDS_KEY } from "../../types";
import type { AiProvider, ProviderInput, ProviderKind, SanitizeLevel } from "../../types";
import Button from "../common/Button";
import FormDialog from "../dialog/FormDialog";
import Select from "../common/Select";
import PasswordField from "../common/PasswordField";
import { useI18n } from "../../lib/i18n";

/** 服务商选择项（顺序即展示顺序）。 */
const KINDS: ProviderKind[] = ["openai", "claude", "ollama", "openai-compatible"];

/** 脱敏层级选项；云端服务会被后端强制抬到 ≥ 严格。*/
const LEVELS: { v: SanitizeLevel; label: string }[] = [
  { v: "strict", label: "pv.levelStrict" },
  { v: "standard", label: "pv.levelStandard" },
  { v: "loose", label: "pv.levelLoose" },
  { v: "custom", label: "pv.levelCustom" },
];

/** 图片能力三态：跟随探测（`null`）/ 强制支持（`true`）/ 强制不支持（`false`）。 */
const VISION_OPTS: { v: string; label: string }[] = [
  { v: "auto", label: "pv.visionFollow" },
  { v: "yes", label: "pv.visionForceYes" },
  { v: "no", label: "pv.visionForceNo" },
];

const visionToMode = (v: boolean | null): string => (v == null ? "auto" : v ? "yes" : "no");
const modeToVision = (m: string): boolean | null => (m === "yes" ? true : m === "no" ? false : null);

interface Props {
  /** 传入既有配置 = 编辑态；`null`/缺省 = 新建态（表单回缺省值）。 */
  initial?: AiProvider | null;
  onClose: () => void;
}

/**
 * AI 模型（Provider）新建 / 编辑表单弹窗——**独立、可复用**的单一实现。
 *
 * 两个入口共用它：设置中心的 `ProviderSettings` 管理列表在其上弹出本弹窗做增改；
 * 聊天 / Agent 的模型下拉则直接只弹本弹窗（不带管理列表壳），避免「列表 + 表单」两层叠窗。
 *
 * 安全红线：API Key 只在提交瞬间存在于组件 state，保存后立即清空；编辑时**从不回填** Key，
 * 留空即保持保险库原样。表单初始值取自 `initial`（组件按需挂载，无跨会话残留）。
 */
export default function ProviderFormDialog({ initial = null, onClose }: Props) {
  const t = useI18n();
  const { error, testing } = useStore(providerStore);

  const isEdit = !!initial;
  // 编辑态载入既有值；API Key 一律留空（不回填）。新建态用缺省 + 按 kind 预填上下文窗口。
  const [kind, setKind] = useState<ProviderKind>(initial?.kind ?? "openai");
  const [name, setName] = useState(initial?.name ?? "");
  const [model, setModel] = useState(initial?.model ?? "");
  const [baseUrl, setBaseUrl] = useState(initial?.baseUrl ?? "");
  const [apiKey, setApiKey] = useState("");
  const [clearKey, setClearKey] = useState(false);
  const [sanitizeLevel, setSanitize] = useState<SanitizeLevel>(initial?.sanitizeLevel ?? "strict");
  /** 图片能力：`null` = 跟随探测（默认），`true`/`false` = 手动强制改判。 */
  const [supportsVision, setSupportsVision] = useState<boolean | null>(initial?.supportsVision ?? null);
  /** `supportsVision` 的值来源：follow = 跟随探测；cached = 探测/已存库的缓存值；
   *  manual = 用户显式改判。保存时 manual 优先，不被预检探测结果冲掉。 */
  const [visionSource, setVisionSource] = useState<"follow" | "cached" | "manual">(
    initial?.supportsVision == null ? "follow" : "cached",
  );
  const [maxTokens, setMaxTokens] = useState(
    String(initial?.maxContextTokens ?? PROVIDER_DEFAULT_CONTEXT[initial?.kind ?? "openai"]),
  );
  const [temperature, setTemp] = useState(initial?.temperature == null ? "" : String(initial.temperature));
  const [isDefault, setDefault] = useState(initial?.isDefault ?? false);
  const [localError, setLocalError] = useState("");
  const [testMsg, setTestMsg] = useState("");
  /** 保存→自动测试的在途标记：主按钮转圈、弹窗禁止退出，回执到手才关窗。 */
  const [saving, setSaving] = useState(false);

  const needsKey = PROVIDER_NEEDS_KEY[kind];
  const savedKey = !!initial?.hasKey;
  const err = localError || error;

  /** 切协议类型：新建态且窗口未被手改过（仍等于旧 kind 默认或空）时，跟随新 kind 预填主流默认。 */
  function pickKind(k: ProviderKind) {
    setKind(k);
    if (isEdit) return;
    setMaxTokens((prev) => {
      const untouched = prev.trim() === "" || Number(prev) === PROVIDER_DEFAULT_CONTEXT[kind];
      return untouched ? String(PROVIDER_DEFAULT_CONTEXT[k]) : prev;
    });
  }

  function validate(): string {
    if (!name.trim()) return t("pv.errName");
    if (!model.trim()) return t("pv.errModel");
    if (kind === "openai-compatible" && !baseUrl.trim()) return t("pv.errBaseUrl");
    if (needsKey && !savedKey && !apiKey.trim()) return t("pv.errApiKey");
    return "";
  }

  /** 连通自检的入参：用当前表单草稿直连（不必先保存）；id 仅供编辑态 Key 留空时回落保险库。 */
  function connInput(): ProviderInput {
    return {
      ...(initial ? { id: initial.id } : {}),
      name: name.trim(),
      kind,
      baseUrl: baseUrl.trim() || null,
      model: model.trim(),
      apiKey: apiKey.trim() || undefined,
    };
  }

  async function save() {
    const invalid = validate();
    if (invalid) {
      setLocalError(invalid);
      return;
    }
    setLocalError("");
    // 先测后存：连通失败就不落库，错误留在弹窗里供修正；不支持图片不算失败，
    // 探测结论已回填 supportsVision，随下面的 input 一起写入。
    setSaving(true);
    try {
      // probeDraft 返回本次探测结论（后端连通成功时必为 true/false）：
      // setSupportsVision 是异步的，新建时下方闭包里的 supportsVision 还是旧值 null，
      // 必须用返回值落库，否则探测结果被丢成「未探测」。
      // 优先级：手动改判（manual）> 本次探测 > 旧值/缓存。
      const probe = await probeDraft(connInput(), false);
      if (!probe.ok) return;
      const input: ProviderInput = {
        ...(initial ? { id: initial.id } : {}),
        name: name.trim(),
        kind,
        // base_url/supports_tools/temperature/extra 在后端是直接赋值：编辑时须回传，否则会被清空
        baseUrl: baseUrl.trim() || null,
        model: model.trim(),
        supportsTools: initial?.supportsTools ?? null,
        // 同上：后端对 supports_vision 也是直接赋值，不回传会把已探测结论清回「未探测」；
        // 手动改判优先，其次本次探测结论，都没有时回落表单缓存值
        supportsVision: visionSource === "manual" ? supportsVision : (probe.vision ?? supportsVision),
        sanitizeLevel,
        maxContextTokens: Number(maxTokens) || PROVIDER_DEFAULT_CONTEXT[kind],
        temperature: temperature.trim() === "" ? null : Number(temperature),
        isDefault,
        extra: initial?.extra ?? null,
        apiKey: apiKey.trim() || undefined,
        clearKey: clearKey || undefined,
      };
      const saved = await providerActions.save(input);
      // 保存成功即关闭弹窗（与片段/触发器等管理页一致）；列表已由 store 的 upsert 刷新。
      if (saved) onClose();
    } finally {
      setSaving(false);
    }
  }

  /** 跑一次草稿自检并回填表单/回执文案；返回 `{ ok, vision }`：
   *  - `ok: false` —— 连通失败（调用方以此不保存）；
   *  - `ok: true` —— 连通成功，`vision` 是本次探测结论（`null` = 未探出，不阻断保存）。
   *  `applyToForm`：探测结果是否写回下拉并清掉手动改判——「测试连通」显式重探传 true
   *  （保留“测试覆盖改判”语义）；保存前预检传 false，manual 改判不被冲掉。 */
  async function probeDraft(
    input: ProviderInput,
    applyToForm: boolean,
  ): Promise<{ ok: boolean; vision: boolean | null }> {
    const r = await providerActions.testDraft(input);
    if (!r) return { ok: false, vision: null };
    if (applyToForm && r.vision != null) {
      setSupportsVision(r.vision);
      setVisionSource("follow");
    }
    const base = t("pv.testOk").replace("{r}", r.reply);
    // 图片能力接在同一行：不支持时附端点报错摘要（已截断），连通失败的错误走上抛路径不在此处。
    const vision =
      r.vision === true
        ? t("pv.visionSupported")
        : r.vision === false
          ? `${t("pv.visionUnsupported")}${r.visionError ? `：${r.visionError}` : ""}`
          : t("pv.visionUnknown");
    // 保存预检：非 manual 时把探测结论缓存进表单（来源记为 cached，仍随下一次探测更新）
    if (!applyToForm && r.vision != null && visionSource !== "manual") {
      setSupportsVision(r.vision);
      setVisionSource("cached");
    }
    setTestMsg(`${base} · ${t("pv.visionProbe")}：${vision}`);
    // 后端连通成功时 vision 必为 true/false；`null` 按「未探出」处理，但不算失败（不阻断保存）
    return { ok: true, vision: r.vision ?? null };
  }

  async function test() {
    setLocalError("");
    setTestMsg("");
    // 只校验与连通相关的字段（name 不参与）：新建未保存时也能直接测。
    if (!model.trim()) {
      setLocalError(t("pv.errModel"));
      return;
    }
    if (kind === "openai-compatible" && !baseUrl.trim()) {
      setLocalError(t("pv.errBaseUrl"));
      return;
    }
    if (needsKey && !savedKey && !apiKey.trim()) {
      setLocalError(t("pv.errApiKey"));
      return;
    }
    void probeDraft(connInput(), true);
  }

  return (
    <FormDialog
      title={isEdit ? t("pv.editModel") : t("pv.newModel")}
      onClose={onClose}
      onSubmit={() => void save()}
      okText={isEdit ? t("pv.saveEdit") : t("pv.create")}
      loading={saving}
      extraActions={
        <Button variant="ghost" disabled={testing !== ""} onClick={() => void test()}>
          {testing === TESTING_DRAFT || testing === initial?.id ? t("pv.testing") : t("pv.test")}
        </Button>
      }
    >
      <div className="fld">
        <span>{t("pv.provider")}</span>
        <div className="seg">
          {KINDS.map((k) => (
            <button key={k} type="button" data-on={kind === k} onClick={() => pickKind(k)}>
              {t(PROVIDER_LABEL[k])}
            </button>
          ))}
        </div>
      </div>

      <label>
        {t("common.name")}
        <input value={name} onChange={(e) => setName(e.target.value)} />
      </label>

      <label>
        {t("pv.model")}
        <input value={model} onChange={(e) => setModel(e.target.value)} spellCheck={false} />
      </label>

      <label>
        {kind === "openai-compatible" ? t("pv.baseUrlRequired") : t("pv.baseUrlOptional")}
        <input
          value={baseUrl}
          onChange={(e) => setBaseUrl(e.target.value)}
          placeholder={kind === "ollama" ? "http://localhost:11434" : "https://api.example.com/v1"}
          spellCheck={false}
        />
      </label>

      {needsKey && (
        <label>
          API Key
          <PasswordField
            value={apiKey}
            onChange={setApiKey}
            placeholder={savedKey ? t("pv.keySavedPh") : undefined}
          />
        </label>
      )}

      {savedKey && (
        <label className="check">
          <input type="checkbox" checked={clearKey} onChange={(e) => setClearKey(e.target.checked)} />
          {t("pv.clearKey")}
        </label>
      )}

      <label>
        {t("pv.sanitizeLevel")}
        <Select
          className="field"
          value={sanitizeLevel}
          options={LEVELS.map((l) => ({ value: l.v, label: t(l.label) }))}
          onChange={(v) => setSanitize(v as SanitizeLevel)}
        />
      </label>
      {kind !== "ollama" && <div className="form-hint">{t("pv.cloudEnforce")}</div>}

      <label>
        {t("pv.vision")}
        <Select
          className="field"
          value={visionToMode(supportsVision)}
          options={VISION_OPTS.map((o) => ({ value: o.v, label: t(o.label) }))}
          onChange={(v) => {
            const next = modeToVision(v);
            setSupportsVision(next);
            setVisionSource(next == null ? "follow" : "manual");
          }}
        />
      </label>
      <div className="form-hint">{t("pv.visionHint")}</div>

      <div className="form-row">
        <label className="grow">
          {t("pv.maxTokens")}
          <input value={maxTokens} onChange={(e) => setMaxTokens(e.target.value)} inputMode="numeric" />
        </label>
        <label className="w20">
          {t("pv.temperature")}
          <input
            value={temperature}
            onChange={(e) => setTemp(e.target.value)}
            placeholder={t("pv.tempPh")}
            inputMode="decimal"
          />
        </label>
      </div>

      <label className="check">
        <input type="checkbox" checked={isDefault} onChange={(e) => setDefault(e.target.checked)} />
        {t("pv.setDefault")}
      </label>

      {!testMsg && <div className="form-hint">{t("pv.autoTestHint")}</div>}
      {testMsg && <div className="pv-test">{testMsg}</div>}
      {err && <div className="form-error">{err}</div>}
    </FormDialog>
  );
}
