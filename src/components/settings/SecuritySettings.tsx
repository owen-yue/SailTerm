import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import {
  lockActions,
  lockStore,
  policyActions,
  policyStore,
  settingsActions,
  settingsStore,
  toast,
  uiPrefs,
  uiPrefsStore,
  useStore,
} from "../../stores";
import { app as appApi, errMsg, ftpsCerts, hostKeys } from "../../lib/tauri";
import { formatTime } from "../../lib/format";
import { APPROVAL_MODE_LABEL, PROVIDER_LABEL, SANITIZE_LABEL, SCAN_LEVEL_LABEL } from "../../types";
import type {
  ApprovalMode,
  FtpsCertRecord,
  HostKeyRecord,
  PolicyRule,
  PolicyRuleInput,
  RuleAction,
  RuleKind,
  ScanLevel,
  SecurityStatus,
} from "../../types";
import { RULE_ACTION, RULE_KIND } from "../ai/aiLabels";
import Button from "../common/Button";
import DeleteButton from "../common/DeleteButton";
import Popconfirm from "../common/Popconfirm";
import Sheet from "../common/Sheet";
import FormDialog from "../dialog/FormDialog";
import { Row, Sec, Toggle } from "./settingsUi";
import Select from "../common/Select";
import { useI18n } from "../../lib/i18n";

/** 审批模式分段顺序（宽→严）。 */
const APPROVAL_MODES: ApprovalMode[] = ["auto_readonly", "all"];
/** 扫描层级分段顺序（松→严）。 */
const SCAN_LEVELS: ScanLevel[] = ["loose", "standard", "strict"];
/** 规则类别分段顺序。 */
const RULE_KINDS: RuleKind[] = ["readonly_whitelist", "write_list", "blacklist"];
/** 规则动作选项（含「按类别隐含」= null）。 */
const RULE_ACTIONS: (RuleAction | "")[] = ["", "allow", "approval", "block"];
/** 空闲自动锁屏档位（分钟，0=关闭）；后端允许 1-240 任意值，UI 只给常用档。 */
const LOCK_IDLE_OPTIONS = [0, 1, 5, 10, 30];

interface Props {
  onClose: () => void;
  /** 嵌入设置中心时不套模态壳。 */
  embedded?: boolean;
}

/**
 * AI 安全设置模态：出网闸门、审批模式、命令风险扫描层级，
 * 以及用户自定义策略规则（黑/白/写名单）的增删改。
 *
 * 安全红线：设置只影响下一轮 AI 编排；内置高危黑名单（如 `rm -rf /`）绝对优先，用户规则
 * 无法把它放行。本模态只读写非敏感偏好，绝不含任何凭证 / API Key。
 */
export default function SecuritySettings({ onClose, embedded = false }: Props) {
  const t = useI18n();
  const {
    approvalMode,
    networkEnabled,
    scanLevel,
    allowPlaintextPassword,
    error: setErr,
  } = useStore(settingsStore);
  const { items: rules, error: ruleErr } = useStore(policyStore);
  const { devMode } = useStore(uiPrefsStore);

  // 规则编辑表单（内联展开，新建/编辑复用同一份）
  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<PolicyRule | null>(null);
  const [kind, setKind] = useState<RuleKind>("blacklist");
  const [pattern, setPattern] = useState("");
  const [action, setAction] = useState<RuleAction | "">("");
  const [note, setNote] = useState("");
  const [localErr, setLocalErr] = useState("");
  /** 待删除的规则 + 气泡锚点（取删除按钮位置）：与侧栏删连接同口径的就地确认。 */
  const [pendingDel, setPendingDel] = useState<{ id: string; pattern: string; x: number; y: number } | null>(
    null,
  );

  useEffect(() => {
    void settingsActions.refresh();
    void policyActions.refresh();
  }, []);

  /** 展开「新建规则」表单，回到缺省值。 */
  function openNew() {
    setEditing(null);
    setKind("blacklist");
    setPattern("");
    setAction("");
    setNote("");
    setLocalErr("");
    setFormOpen(true);
  }

  /** 载入一条既有规则到表单编辑。 */
  function openEdit(r: PolicyRule) {
    setEditing(r);
    setKind(r.kind);
    setPattern(r.pattern);
    setAction(r.action ?? "");
    setNote(r.note ?? "");
    setLocalErr("");
    setFormOpen(true);
  }

  async function saveRule() {
    if (!pattern.trim()) {
      setLocalErr(t("ss.errPattern"));
      return;
    }
    setLocalErr("");
    const input: PolicyRuleInput = {
      ...(editing ? { id: editing.id } : {}),
      kind,
      pattern: pattern.trim(),
      action: action === "" ? null : action,
      note: note.trim() || null,
      enabled: editing?.enabled ?? true,
      sortOrder: editing?.sortOrder ?? 0,
    };
    const saved = await policyActions.save(input);
    if (saved) {
      setFormOpen(false);
      setEditing(null);
    }
  }

  const err = localErr || setErr || ruleErr;

  return (
    <>
    <Sheet title={t("ss.title")} onClose={onClose} embedded={embedded}>
        <div className="sec-set-body">
          <AiDataFlow />
          <Sec title={t("ss.dataEgress")}>
            <Row title={t("ss.egressTitle")}>
              <Toggle
                checked={networkEnabled}
                onChange={(on) => void settingsActions.update({ networkEnabled: on })}
              />
            </Row>
            <Row
              title={t("ss.pwTitle")}
            >
              <Toggle
                checked={allowPlaintextPassword}
                onChange={(on) => void settingsActions.update({ allowPlaintextPassword: on })}
              />
            </Row>
          </Sec>

          <Sec title={t("ss.cmdControl")}>
            <Row title={t("ss.approvalTitle")}>
              <Select
                className="field"
                value={approvalMode}
                options={APPROVAL_MODES.map((m) => ({ value: m, label: t(APPROVAL_MODE_LABEL[m]) }))}
                onChange={(v) => void settingsActions.update({ approvalMode: v as ApprovalMode })}
              />
            </Row>
            <Row title={t("ss.scanTitle")}>
              <Select
                className="field"
                value={scanLevel}
                options={SCAN_LEVELS.map((l) => ({ value: l, label: t(SCAN_LEVEL_LABEL[l]) }))}
                onChange={(v) => void settingsActions.update({ scanLevel: v as ScanLevel })}
              />
            </Row>
          </Sec>

          <Sec
            title={t("ss.rulesTitle")}
            extra={
              !formOpen && (
                <button type="button" className="link" onClick={openNew}>
                  ＋ {t("common.new")}
                </button>
              )
            }
          >
            <Row title={t("ss.rulesHint")} />

            {rules.length === 0 && !formOpen && <div className="ss-empty">{t("ss.noRules")}</div>}

            <div className="ss-rules">
              {rules.map((r) => {
                const k = RULE_KIND[r.kind];
                return (
                  <div key={r.id} className="ss-rule" data-off={!r.enabled}>
                    <div className="ss-rule-main" onClick={() => openEdit(r)} title={t("ss.clickEdit")}>
                      <span className="ss-rule-kind">{k ? t(k.label) : r.kind}</span>
                      <code className="ss-rule-pat">{r.pattern}</code>
                      {r.action && (
                        <span className="ss-rule-act">{t(RULE_ACTION[r.action] ?? r.action)}</span>
                      )}
                      {r.note && <span className="ss-rule-note">{r.note}</span>}
                    </div>
                    <div className="ss-rule-side">
                      <Toggle
                        title={r.enabled ? t("ss.onTip") : t("ss.offTip")}
                        checked={r.enabled}
                        onChange={() => void policyActions.toggleEnabled(r)}
                      />
                      <DeleteButton
                        onConfirm={(x, y) => setPendingDel({ id: r.id, pattern: r.pattern, x, y })}
                      />
                    </div>
                  </div>
                );
              })}
            </div>

            {formOpen && (
              <form
                className="ss-form"
                onSubmit={(e) => {
                  e.preventDefault();
                  void saveRule();
                }}
              >
                <div className="fld">
                  <span>{t("ss.category")}</span>
                  <div className="seg">
                    {RULE_KINDS.map((kd) => (
                      <button key={kd} type="button" data-on={kind === kd} onClick={() => setKind(kd)}>
                        {t(RULE_KIND[kd]?.label ?? kd)}
                      </button>
                    ))}
                  </div>
                </div>
                <div className="form-hint">{t(RULE_KIND[kind]?.hint ?? "")}</div>

                <label>
                  {t("ss.patternLabel")}
                  <input
                    value={pattern}
                    onChange={(e) => setPattern(e.target.value)}
                    placeholder={kind === "blacklist" ? "rm -rf" : "systemctl restart"}
                    spellCheck={false}
                  />
                </label>

                <label>
                  {t("ss.actionLabel")}
                  <Select
                    className="field"
                    value={action}
                    options={RULE_ACTIONS.map((a) => ({
                      value: a,
                      label: a === "" ? t("ss.byKindImplicit") : t(RULE_ACTION[a]),
                    }))}
                    onChange={(v) => setAction(v as RuleAction | "")}
                  />
                </label>

                <label>
                  {t("ss.noteLabel")}
                  <input
                    value={note}
                    onChange={(e) => setNote(e.target.value)}
                    placeholder={t("ss.notePh")}
                  />
                </label>

                <div className="form-actions">
                  <button type="button" className="ghost" onClick={() => setFormOpen(false)}>
                    {t("common.cancel")}
                  </button>
                  <button type="submit">{editing ? t("ss.saveEdit") : t("ss.addRule")}</button>
                </div>
              </form>
            )}
          </Sec>

          <TrustedIdentities />

          <ScreenLock />

          <Sec title={t("ss.advanced")}>
            <Row title={t("general.dev.enable")}>
              <Toggle
                checked={devMode}
                onChange={(on) => {
                  uiPrefs.set({ devMode: on });
                  // 关闭时立即收起可能已打开的 devtools；开启不自动弹出（由用户按 F12）
                  if (!on) void appApi.setDevtools(false).catch(() => {});
                }}
              />
            </Row>
          </Sec>
        </div>

        {err && <div className="form-error">{err}</div>}

        <div className="form-actions sec-set-foot">
          <button type="button" className="ghost" onClick={() => void settingsActions.reset()}>
            {t("ss.resetDefaults")}
          </button>
          {!embedded && (
            <button type="button" onClick={onClose}>
              {t("ss.finish")}
            </button>
          )}
        </div>
    </Sheet>

    {pendingDel && (
      <Popconfirm
        x={pendingDel.x}
        y={pendingDel.y}
        title={t("ss.delRuleMsg").replace("{n}", pendingDel.pattern)}
        description={t("ss.delRuleWarn")}
        onConfirm={() => {
          const id = pendingDel.id;
          setPendingDel(null);
          void policyActions.remove(id);
        }}
        onClose={() => setPendingDel(null)}
      />
    )}
    </>
  );
}

/**
 * AI 数据流向只读状态区：原顶栏 🛡 安全灯弹窗的内容，因属非实时信息收进设置中心「安全」页。
 * 挂载时拉一次 securityStatus；只读展示，经不含凭证。
 */
function AiDataFlow() {
  const t = useI18n();
  // 出网 / 审批模式 / 扫描层级是本页可即时改动的设置：直接读实时 store，而非一次性快照。
  // 否则拨开关后状态卡会停在挂载时拉到的旧值；且 settingsActions.update 是乐观写（先翻本地
  // 再 await 落库），靠「值变化→重拉 securityStatus」会和落库竞态读到旧值。其余派生字段
  // （provider / 脱敏级 / 凭证后端 / 会话计数）仍取后端权威快照。
  const { networkEnabled, approvalMode, scanLevel } = useStore(settingsStore);
  const [status, setStatus] = useState<SecurityStatus | null>(null);
  useEffect(() => {
    appApi
      .securityStatus()
      .then(setStatus)
      .catch(() => {});
  }, []);
  if (!status) return null;
  return (
    <Sec title={t("sec.statusTitle")}>
      <Row title={t("sec.model")}>
        {status.provider ? (
          <>
            {status.provider.model} · {t(PROVIDER_LABEL[status.provider.kind])}{" "}
            <Tag tone={status.provider.isLocal ? "ok" : "info"}>
              {status.provider.isLocal ? t("sec.local") : t("sec.cloud")}
            </Tag>
          </>
        ) : (
          <Tag>{t("sec.notConfigured")}</Tag>
        )}
      </Row>
      <Row title={t("sec.egress")}>
        {networkEnabled ? (
          <>
            <Tag tone="ok">{t("sec.on")}</Tag> {t("sec.egressOnNote")}
          </>
        ) : (
          <>
            <Tag>{t("sec.off")}</Tag> {t("sec.egressOffNote")}
          </>
        )}
      </Row>
      <Row title={t("sec.sanitize")}>
        <>
          <Tag tone="ok">{t("sec.on")}</Tag>{" "}
          {t("sec.sanitizeNote").replace("{l}", t(SANITIZE_LABEL[status.sanitizeLevel]))}
        </>
      </Row>
      <Row title={t("sec.credential")}>
        <>
          {status.vaultIsolated ? <Tag tone="ok">{t("sec.isolated")}</Tag> : <Tag tone="warn">{t("sec.degraded")}</Tag>}{" "}
          {status.vaultBackend === "os-keyring" ? t("sec.osKeyring") : t("sec.encFile")} · {t("sec.neverContext")}
        </>
      </Row>
      <Row title={t("sec.approval")}>
        {t("sec.approvalNote")
          .replace("{a}", t(APPROVAL_MODE_LABEL[approvalMode]))
          .replace("{s}", t(SCAN_LEVEL_LABEL[scanLevel]))}
      </Row>
      <Row title={t("sec.thisSession")}>
        {t("sec.sessionNote")
          .replace("{c}", String(status.sessionCount))
          .replace("{a}", String(status.auditTotal))}
        {status.auditBlocked > 0 && <b className="sec-hit"> · {status.auditBlocked}</b>}
      </Row>
    </Sec>
  );
}

/**
 * 已信任的 SSH 主机密钥与 FTPS 证书：只读列表 + 移除。
 *
 * 为何必须存在：两者都是 TOFU 指纹固定且**指纹变更一律拒连**（fail-closed）。服务器正常轮换
 * 证书/重装系统后，若没有这个入口，用户就再也连不上去了 —— 弹窗里的提示也只指向这里。
 * 展示的全是公钥材料（指纹/主体/有效期），不含任何凭证。
 */
function TrustedIdentities() {
  const t = useI18n();
  const [keys, setKeys] = useState<HostKeyRecord[]>([]);
  const [certs, setCerts] = useState<FtpsCertRecord[]>([]);
  /** 待移除的信任记录 + 气泡锚点：删信任会直接影响下次连接的指纹核验，必须就地确认。 */
  const [pendingDel, setPendingDel] = useState<
    { kind: "key" | "cert"; host: string; port: number; x: number; y: number } | null
  >(null);

  const reload = () => {
    hostKeys
      .list()
      .then(setKeys)
      .catch(() => {});
    ftpsCerts
      .list()
      .then(setCerts)
      .catch(() => {});
  };
  useEffect(reload, []);

  /** 移除后重拉：失败就维持原状，下一次进入本页会再试。 */
  function remove(kind: "key" | "cert", host: string, port: number) {
    const done = () => reload();
    if (kind === "key") void hostKeys.forget(host, port).then(done, done);
    else void ftpsCerts.forget(host, port).then(done, done);
  }

  const rows: {
    host: string;
    port: number;
    label: string;
    tag: string;
    fp: string;
    note: string;
    kind: "key" | "cert";
  }[] = [
    ...keys.map((k) => ({
      kind: "key" as const,
      host: k.host,
      port: k.port,
      label: t("ss.trustSsh"),
      tag: k.keyType,
      fp: k.fingerprint,
      note: formatTime(k.createdAt),
    })),
    ...certs.map((c) => ({
      kind: "cert" as const,
      host: c.host,
      port: c.port,
      label: t("ss.trustFtps"),
      tag: c.subject,
      fp: c.fingerprint,
      note: c.validity,
    })),
  ];

  return (
    <>
    <Sec title={t("ss.trustTitle")}>
      <Row title={t("ss.trustHint")} />
      {rows.length === 0 && <div className="ss-empty">{t("ss.trustNone")}</div>}
      <div className="ss-rules">
        {rows.map((r) => (
          <div key={`${r.kind}:${r.host}:${r.port}`} className="ss-rule">
            <div className="ss-rule-main">
              <span className="ss-rule-kind">{r.label}</span>
              <code className="ss-rule-pat">
                {r.host}:{r.port}
              </code>
              {r.tag && <span className="ss-rule-act">{r.tag}</span>}
              <span className="ss-rule-note">{r.fp}</span>
              <span className="ss-rule-note">{r.note}</span>
            </div>
            <div className="ss-rule-side">
              <DeleteButton
                onConfirm={(x, y) =>
                  setPendingDel({ kind: r.kind, host: r.host, port: r.port, x, y })
                }
              />
            </div>
          </div>
        ))}
      </div>
    </Sec>

    {pendingDel && (
      <Popconfirm
        x={pendingDel.x}
        y={pendingDel.y}
        title={t("ss.delTrustMsg").replace("{n}", `${pendingDel.host}:${pendingDel.port}`)}
        description={t("ss.delTrustWarn")}
        onConfirm={() => {
          const { kind, host, port } = pendingDel;
          setPendingDel(null);
          remove(kind, host, port);
        }}
        onClose={() => setPendingDel(null)}
      />
    )}
    </>
  );
}

/** 语义小标签：ok=绿、warn=黄、info=蓝，缺省=中性灰（沿用安全灯样式 .sec-tag）。 */
function Tag({ children, tone }: { children: ReactNode; tone?: "ok" | "warn" | "info" }) {
  return (
    <span className="sec-tag" data-tone={tone}>
      {children}
    </span>
  );
}

/**
 * 屏幕锁定设置：解锁 PIN 的建立/修改/移除、空闲自动锁屏档位。
 * 手动锁屏入口在顶栏按钮与 Ctrl+Alt+L，这里不放「立即锁屏」（避免双处入口漂移）。
 *
 * 安全红线：PIN 明文只作命令入参直送后端（PBKDF2 哈希进保险库，见 commands::lock），
 * 本组件与 store 都不留存；锁态单源在后端，这里只镜像快照。定位是防「临时窥屏与
 * 误操作」的威慑层，不是加密边界——文案不得给出「军事级安全」之类过度承诺。
 */
function ScreenLock() {
  const t = useI18n();
  const { pinSet, idleMinutes } = useStore(lockStore);
  /** 弹层表单：set=首次建立 / change=改 / clear=关闭并移除（须验旧）。 */
  const [form, setForm] = useState<null | "set" | "change" | "clear">(null);
  const [oldPin, setOldPin] = useState("");
  const [pin, setPin] = useState("");
  const [pin2, setPin2] = useState("");
  const [question, setQuestion] = useState("");
  const [answer, setAnswer] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");

  useEffect(() => {
    void lockActions.refresh();
  }, []);

  function openForm(mode: NonNullable<typeof form>) {
    setOldPin("");
    setPin("");
    setPin2("");
    setQuestion("");
    setAnswer("");
    setErr("");
    setForm(mode);
  }

  async function submit() {
    if (form === "clear") {
      setBusy(true);
      setErr("");
      try {
        await lockActions.clearPin(oldPin);
        setForm(null);
      } catch (e) {
        setErr(errMsg(e));
      } finally {
        setBusy(false);
      }
      return;
    }
    if (!/^\d{4,6}$/.test(pin)) {
      setErr(t("lock.pinErrLen"));
      return;
    }
    if (pin !== pin2) {
      setErr(t("lock.pinErrMatch"));
      return;
    }
    if (form === "change" && !oldPin) {
      setErr(t("lock.pinErrOld"));
      return;
    }
    // 首次建立 PIN 必须同时设密保问答（与后端 err.lock.needSecq 同口径，前端先拦一道）。
    if (form === "set" && !question.trim()) {
      setErr(t("lock.secqErrQuestion"));
      return;
    }
    if (form === "set" && !answer.trim()) {
      setErr(t("lock.secqErrAnswer"));
      return;
    }
    setBusy(true);
    setErr("");
    try {
      await lockActions.setPin(
        pin,
        form === "change" ? oldPin : null,
        form === "set" ? question.trim() : null,
        form === "set" ? answer : null,
      );
      setForm(null);
    } catch (e) {
      setErr(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  /** 只收数字、限 6 位（后端 validate_pin 另有 fail-safe 兜底）。 */
  const digits = (v: string) => v.replace(/\D/g, "").slice(0, 6);

  return (
    <>
      <Sec title={t("lock.title")}>
        <Row
          title={pinSet ? t("lock.pinOn") : t("lock.pinOff")}
          desc={pinSet ? t("lock.pinOnDesc") : t("lock.pinOffDesc")}
        >
          {pinSet ? (
            <>
              <Button size="sm" variant="outline" onClick={() => openForm("change")}>
                {t("lock.changePin")}
              </Button>
              <Button size="sm" variant="link" danger onClick={() => openForm("clear")}>
                {t("lock.disable")}
              </Button>
            </>
          ) : (
            <Button size="sm" onClick={() => openForm("set")}>
              {t("lock.setPin")}
            </Button>
          )}
        </Row>
        <Row title={t("lock.idle")}>
          <Select
            className="field"
            value={String(idleMinutes)}
            disabled={!pinSet}
            title={pinSet ? undefined : t("lock.noPinHint")}
            options={LOCK_IDLE_OPTIONS.map((m) => ({
              value: String(m),
              label: m === 0 ? t("lock.off") : t("lock.mins").replace("{m}", String(m)),
            }))}
            onChange={(v) =>
              void lockActions.setConfig(Number(v)).catch((e) => toast.error(errMsg(e)))
            }
          />
        </Row>
      </Sec>

      {form && (
        <FormDialog
          title={t(form === "clear" ? "lock.clearTitle" : form === "change" ? "lock.changePin" : "lock.setPin")}
          onClose={() => setForm(null)}
          onSubmit={() => void submit()}
          submitting={busy}
          okText={t("common.save")}
        >
          <p className="form-hint">{t(form === "clear" ? "lock.clearWarn" : "lock.pinDesc")}</p>
          {form !== "set" && (
            <label>
              {t("lock.oldPinLabel")}
              <input
                type="password"
                inputMode="numeric"
                autoComplete="off"
                maxLength={6}
                value={oldPin}
                onChange={(e) => setOldPin(digits(e.target.value))}
              />
            </label>
          )}
          {form !== "clear" && (
            <>
              <label>
                {t("lock.pinLabel")}
                <input
                  type="password"
                  inputMode="numeric"
                  autoComplete="off"
                  maxLength={6}
                  value={pin}
                  onChange={(e) => setPin(digits(e.target.value))}
                />
              </label>
              <label>
                {t("lock.confirmLabel")}
                <input
                  type="password"
                  inputMode="numeric"
                  autoComplete="off"
                  maxLength={6}
                  value={pin2}
                  onChange={(e) => setPin2(digits(e.target.value))}
                />
              </label>
            </>
          )}
          {form === "set" && (
            <>
              <p className="form-hint">{t("lock.secqDesc")}</p>
              <label>
                {t("lock.secqQuestionLabel")}
                <input
                  type="text"
                  autoComplete="off"
                  maxLength={100}
                  value={question}
                  onChange={(e) => setQuestion(e.target.value)}
                  placeholder={t("lock.secqQuestionPh")}
                />
              </label>
              <label>
                {t("lock.secqAnswerLabel")}
                <input
                  type="text"
                  autoComplete="off"
                  maxLength={100}
                  value={answer}
                  onChange={(e) => setAnswer(e.target.value)}
                  placeholder={t("lock.secqAnswerPh")}
                />
              </label>
            </>
          )}
          {err && <div className="form-error">{err}</div>}
        </FormDialog>
      )}
    </>
  );
}
