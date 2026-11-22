import { useEffect, useState, type ReactNode } from "react";
import {
  connectionStore,
  connections,
  sessionActions,
  sessionStore,
  sshKeyActions,
  sshKeyStore,
  useStore,
} from "../../stores";
import { local, sessions as sessionsApi } from "../../lib/tauri";
import { DEFAULT_PORT, DEFAULT_SERIAL, KIND_LABEL, kindIsFtp } from "../../types";
import { kindMetaOf } from "../../lib/kindMeta";
import { useI18n } from "../../lib/i18n";
import Select from "../common/Select";
import { IconKey } from "../common/icons";
import DirPicker from "../common/DirPicker";
import PasswordField from "../common/PasswordField";
import { toolsActions } from "../../tools/store";
import type {
  AuthType,
  DirectConnect,
  Profile,
  ProfileInput,
  SerialParams,
  SessionKind,
} from "../../types";

/** 协议选择项（顺序即展示顺序）。*/
const KINDS: SessionKind[] = ["ssh", "telnet", "mosh", "serial", "ftp", "ftps"];

/** 编辑已保存凭证时密码/口令框的装饰性灰点占位符——真实值绝不回填，
 *  仅用于传达“此处已存、输入即覆盖”，比“留空保持不变”的文字提示观感更干净。*/
const PWD_MASK = "••••••••";

/** 提交动作：仅连接（不保存）/ 仅保存 / 保存并连接。*/
type SubmitMode = "connect" | "save" | "save-connect";

interface Props {
  /** 编辑既有连接时传入；新建为 null。*/
  profile: Profile | null;
  onDone: () => void;
}

/**
 * 连接表单：密码与私钥两类认证。
 * 安全红线：凭证字段只在提交瞬间存在，提交后立即从组件 state 清空，
 * 前端 store 里也从不保存凭证。
 */
export default function ConnectForm({ profile, onDone }: Props) {
  const tr = useI18n();
  const { groups, profiles, error: connErr } = useStore(connectionStore);
  const { items: keyItems } = useStore(sshKeyStore);
  const { error: sessErr } = useStore(sessionStore);

  const [kind, setKind] = useState<SessionKind>(profile?.kind ?? "ssh");
  const [name, setName] = useState(profile?.name ?? "");
  const [groupId, setGroupId] = useState(profile?.groupId ?? "");
  // 引用的密钥库私钥（ssh_key.id）；仅在「私钥认证」下出现，选择动作绝不反向改 authType。
  const [privateKeyId, setPrivateKeyId] = useState(profile?.privateKeyId ?? "");
  const [host, setHost] = useState(profile?.host ?? "");
  const [port, setPort] = useState(String(profile?.port ?? DEFAULT_PORT.ssh));
  const [username, setUsername] = useState(profile?.username ?? "");
  const [authType, setAuthType] = useState<AuthType>(() => {
    if (!profile) return "password";
    // DB 兼容：旧档案 keyboardInteractive 统一映射为 auto（UI 显示为「协商」）
    if (profile.authType === "keyboardInteractive") return "auto";
    return profile.authType;
  });
  const [password, setPassword] = useState("");
  // 磁盘私钥路径：UI 不再提供输入，仅携带档案既有值以免编辑时丢失（改选密钥库项时清空）。
  const [keyPath] = useState(profile?.privateKeyPath ?? "");
  const [passphrase, setPassphrase] = useState("");
  const [startupDir, setStartupDir] = useState(profile?.startupDir ?? "");
  const [clearCredential, setClearCredential] = useState(false);
  const [localError, setLocalError] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [savedProfile, setSavedProfile] = useState<Profile | null>(null);
  const [serial, setSerial] = useState<SerialParams>(profile?.serial ?? DEFAULT_SERIAL);
  const [portList, setPortList] = useState<string[]>([]);
  const [jumpProfileId, setJumpProfileId] = useState(profile?.jumpProfileId ?? "");
  const [startupCommand, setStartupCommand] = useState(profile?.startupCommand ?? "");
  const [notes, setNotes] = useState(profile?.notes ?? "");
  const [agentForward, setAgentForward] = useState(profile?.agentForward ?? false);
  const [x11Forward, setX11Forward] = useState(profile?.x11Forward ?? false);
  const [ftpActiveMode, setFtpActiveMode] = useState(profile?.ftpActiveMode ?? false);
  const [keepaliveSecs, setKeepaliveSecs] = useState(String(profile?.keepaliveSecs ?? ""));
  // —— 新区块字段（连接超时/压缩、日志录制、终端 per-session）——
  const [connectTimeoutSecs, setConnectTimeoutSecs] = useState(String(profile?.connectTimeoutSecs ?? ""));
  const [compression, setCompression] = useState(profile?.compression ?? false);
  const [autoRecord, setAutoRecord] = useState(profile?.autoRecord ?? false);
  const [recordDir, setRecordDir] = useState(profile?.recordDir ?? "");
  const [recordFilePattern, setRecordFilePattern] = useState(profile?.recordFilePattern ?? "");
  const [recordOverwrite, setRecordOverwrite] = useState(profile?.recordOverwrite ?? false);
  const [recordTimestampPrefix, setRecordTimestampPrefix] = useState(profile?.recordTimestampPrefix ?? false);
  // 加密落盘默认关（明文，随处可读）；想加密的敏感档案手动勾上（文件成密文，仅本程序可回放）。
  const [recordEncrypt, setRecordEncrypt] = useState(profile?.recordEncrypt ?? false);
  const [termCols, setTermCols] = useState(String(profile?.termCols ?? ""));
  const [termRows, setTermRows] = useState(String(profile?.termRows ?? ""));
  // "" = 跟随全局终端偏好（不覆盖）。
  const [bellStyle, setBellStyle] = useState<string>(profile?.bellStyle ?? "");

  const editing = !!profile;
  /** 协议差异全部从此元数据表投影（src/lib/kindMeta.ts）；新增协议只改表，不加判断。*/
  const meta = kindMetaOf(kind);
  const isFtp = kindIsFtp(kind);
  // 渐进式披露：可选区默认收起，降低新建时的视觉压力；
  // 编辑已有连接时，若某区含非默认值则自动展开，避免把用户已配的东西藏起来。
  const [openSecs, setOpenSecs] = useState<Record<string, boolean>>(() => {
    const p = profile;
    if (!p) return { conn: false, record: false, term: false, adv: false };
    return {
      conn: !!(p.startupDir || p.keepaliveSecs || p.connectTimeoutSecs || p.compression),
      record: !!(p.autoRecord || p.recordDir || p.recordFilePattern || p.recordOverwrite || p.recordTimestampPrefix || p.recordEncrypt === true),
      term: !!(p.termCols || p.termRows || p.bellStyle),
      adv: !!(p.jumpProfileId || p.startupCommand || p.agentForward || p.x11Forward || p.ftpActiveMode || p.notes),
    };
  });
  const toggleSec = (id: string) => setOpenSecs((s) => ({ ...s, [id]: !s[id] }));
  // 串口协议的串口参数是必需项（放在「连接选项」内）→ 该区对串口恒展开、不可折叠。
  const connOpen = !!openSecs.conn || meta.serialParams;
  // 新建态下首次「保存并连接」即使连接失败，档案也已落库；用 savedProfile 记住它，
  // 重试时复用同一 id 走「更新」而非再插一条——修复「连接失败反复点保存并连接 → 重复连接」。
  const base = profile ?? savedProfile;
  /** 按钮锁只看本表单自己的提交态：旧写法带上了全局 busy（任何一条会话在建连中就把这张
   *  无关的新建表单一起锁死），一条永不返回的手握会让保存/取消永久停在「处理中」。*/
  const busyNow = submitting;
  /** 用户名输入：从固定顶部移入认证页（SSH 必填；FTP/FTPS 留空即匿名；Telnet/Mosh 可选）。*/
  const usernameField = !meta.hostLabelIsDevice && (
    <label>
      {tr("cf.username")}
      <input
        value={username}
        onChange={(e) => setUsername(e.target.value)}
        placeholder={isFtp ? tr("cf.ftpAnonPh") : !meta.hasAuth ? tr("cf.optional") : undefined}
      />
    </label>
  );

  useEffect(() => {
    if (!meta.serialParams) return;
    void sessionsApi.serialPorts().then(setPortList).catch(() => setPortList([]));
  }, [meta.serialParams]);
  // 认证区密钥下拉需列密钥库：挂载时拉一次（只回非敏感元数据，私钥本体不出后端）。
  useEffect(() => {
    void sshKeyActions.refresh();
  }, []);
  const error = localError || sessErr || connErr;
  const storedKey = !!base?.hasPrivateKey || !!base?.privateKeyPath;

  /** 提交后清空所有凭证输入（含 DOM value）。*/
  const wipe = () => {
    setPassword("");
    setPassphrase("");
  };

  /** 切协议：端口跟着换默认值；切到表上不支持私钥/协商的协议时强制回密码并清掉已填材料。*/
  const pickKind = (next: SessionKind) => {
    setKind(next);
    setPort(String(DEFAULT_PORT[next]));
    const m = kindMetaOf(next);
    if (!m.authModes.includes(authType)) setAuthType(m.authModes[0] ?? "password");
    if (!m.authModes.includes("privateKey")) {
      setPassphrase("");
      setPrivateKeyId("");
    }
    if (m.hostLabelIsDevice) setUsername("");
  };

  const setSerialField = <K extends keyof SerialParams>(k: K, v: SerialParams[K]) =>
    setSerial((s) => ({ ...s, [k]: v }));

  /** 是否按私钥认证收集材料：表上不支持私钥的协议恒为 false（私钥是 SSH 的概念）。*/
  const useKey = meta.authModes.includes("privateKey") && authType === "privateKey";

  /**
   * 两条提交路径（仅连接 / 保存档案）共享的参数核心：协议清洗判断只依元数据表写**一遍**，
   * 原先 directParams / profileInput 各写一份的同义判断在此收敛，杜绝两路漂移。
   * 凭证三字段（password/keyPem/passphrase）不在此处——它们只在「仅连接」直传，
   * 保存路径由后端落保险库，两语义保持分离。
   */
  const coreInput = () => {
    return {
      kind,
      host: host.trim(),
      port: Number(port) || DEFAULT_PORT[kind],
      username: username.trim(),
      startupDir: startupDir.trim(),
      keepaliveSecs: Number(keepaliveSecs) || 0,
      serial: meta.serialParams ? { ...DEFAULT_SERIAL, ...serial } : null,
      // 连接超时：仅经网络协议（connectTimeout）收集；0/空 = 用默认。
      connectTimeoutSecs: meta.connectTimeout ? Number(connectTimeoutSecs) || 0 : 0,
      // 压缩优先：SSH 概念，表外协议恒关。
      compression: meta.sshExclusive ? compression : false,
      // 日志/录制：仅在有终端输出环的协议收集，表外恒关且不下发配置。
      autoRecord: meta.hasRecord ? autoRecord : false,
      recordDir: meta.hasRecord ? recordDir.trim() || null : null,
      recordFilePattern: meta.hasRecord ? recordFilePattern.trim() || null : null,
      recordOverwrite: meta.hasRecord ? recordOverwrite : false,
      recordTimestampPrefix: meta.hasRecord ? recordTimestampPrefix : false,
      recordEncrypt: meta.hasRecord ? recordEncrypt : false,
      // 终端初始行列：仅有 PTY 的协议收集；0 = 用实时尺寸。
      termCols: meta.hasTerminal ? Number(termCols) || 0 : 0,
      termRows: meta.hasTerminal ? Number(termRows) || 0 : 0,
    };
  };
  /** 免材料认证（无预认证协议 / 协商）：不收集凭证，连接时自动弹窗走交互认证。*/
  const authInteractive = authType === "auto" || authType === "keyboardInteractive";

  /** 仅连接（不落库）入参：凭证在此刻直传后端，提交后 wipe；表外字段一律不下发。*/
  const directParams = (): DirectConnect => {
    const c = coreInput();
    return {
      ...c,
      startupDir: c.startupDir || undefined,
      serial: c.serial ?? undefined,
      keepaliveSecs: c.keepaliveSecs || undefined,
      authType,
      password: authType === "password" ? password : undefined,
      privateKeyId: useKey && privateKeyId ? privateKeyId : undefined,
      privateKeyPath: useKey && !privateKeyId && keyPath.trim() ? keyPath.trim() : undefined,
      keyPassphrase: useKey && passphrase ? passphrase : undefined,
      startupCommand: meta.sshExclusive ? startupCommand.trim() || undefined : undefined,
      agentForward: meta.sshExclusive ? agentForward : undefined,
      x11Forward: meta.sshExclusive ? x11Forward : undefined,
      ftpActiveMode: meta.ftpExclusive ? ftpActiveMode : undefined,
    };
  };

  /** 保存档案入参：凭证只带非空值（空 = 不动保险库已存项）；表外字段强制回缺省值。*/
  const profileInput = (): ProfileInput => {
    const c = coreInput();
    return {
      ...c,
      ...(base ? { id: base.id, sortOrder: base.sortOrder, colorTag: base.colorTag } : {}),
      name: name.trim() || c.host,
      groupId: groupId || null,
      privateKeyId: privateKeyId || null,
      authType,
      startupDir: c.startupDir || null,
      serial: c.serial,
      startupCommand:
        meta.sshExclusive && startupCommand.trim() ? startupCommand.trim() : null,
      // 表外协议强制关：跳板/转发组仅 SSH，主动模式仅 FTP 家族。
      jumpProfileId: meta.sshExclusive ? jumpProfileId || null : null,
      agentForward: meta.sshExclusive ? agentForward : false,
      x11Forward: meta.sshExclusive ? x11Forward : false,
      ftpActiveMode: meta.ftpExclusive ? ftpActiveMode : false,
      notes: notes.trim() || null,
      privateKeyPath: useKey && !privateKeyId ? keyPath.trim() || null : null,
      // Bell 覆盖仅对 PTY 终端类协议有意义；空串 = 跟随全局（存 null）。
      bellStyle: meta.hasTerminal ? ((bellStyle || null) as ProfileInput["bellStyle"]) : null,
      // 凭证只带非空值：私钥走密钥库引用（privateKeyId）或库内既有引用，粘贴 PEM 已从表单移除。
      password: authType === "password" && password && !authInteractive ? password : undefined,
      keyPassphrase: useKey && passphrase ? passphrase : undefined,
      clearCredential: clearCredential ? true : undefined,
    };
  };

  /** 连接前的最低校验：能否凑齐认证材料。*/
  const validate = (mode: SubmitMode): string | null => {
    if (!host.trim()) return meta.hostLabelIsDevice ? tr("cf.errSerial") : tr("cf.errHost");
    if (meta.usernameRequired && !username.trim()) return tr("cf.errUser");
    if (mode === "save") return null;
    if (!meta.hasAuth) return null;
    if (authInteractive) return null;

    if (authType === "password") {
      if (!meta.passwordOptional) {
        const reusable = !!base && base.hasCredential && !clearCredential;
        if (!password && !(mode === "save-connect" && reusable)) return tr("cf.errPass");
      }
    } else if (!privateKeyId && !storedKey && !clearCredential) {
      return tr("cf.errKey");
    }
    return null;
  };

  async function submit(mode: SubmitMode) {
    if (submitting) return;
    const errMsg = validate(mode);
    if (errMsg) {
      setLocalError(errMsg);
      return;
    }
    setLocalError("");
    setSubmitting(true);
    try {
      if (mode === "connect" && !editing) {
        const id = await sessionActions.openDirect(directParams());
        wipe();
        if (id) onDone();
        return;
      }

      // 保存（新建/编辑）→ 用档案连接，这样已存凭证可直接复用。
      // 记住落库档案：新建连接失败时，重试会复用同一 id 更新，而不是再插一条重复连接。
      const saved = await connections.save(profileInput());
      wipe();
      if (!saved) return;
      setSavedProfile(saved);
      if (mode === "save") {
        onDone();
        return;
      }
      const id = await sessionActions.openProfile(saved.id);
      if (id) onDone();
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <form
      className="connect-form cp-form"
      onSubmit={(e) => {
        e.preventDefault();
        void submit("save-connect");
      }}
    >
      <div className="cp-scroll">
        {/* —— 基本信息 —— */}
        <section className="cp-sec">
          <h3>{tr("cf.secBasic")}</h3>
          <div className="fld">
            <span>{tr("cf.protocol")}</span>
            <div className="seg">
              {KINDS.map((k) => (
                <button key={k} type="button" data-on={kind === k} onClick={() => pickKind(k)}>
                  {tr(KIND_LABEL[k])}
                </button>
              ))}
            </div>
          </div>
          <div className="form-row">
            <label className="grow">
              {meta.hostLabelIsDevice ? tr("cf.serialDev") : tr("cf.host")}
              <input
                value={host}
                onChange={(e) => setHost(e.target.value)}
                placeholder={meta.hostLabelIsDevice ? "COM3" : undefined}
                list={meta.hostLabelIsDevice ? "serial-port-list" : undefined}
                autoFocus
                required
              />
              {meta.hostLabelIsDevice && (
                <datalist id="serial-port-list">
                  {portList.map((p) => (
                    <option key={p} value={p} />
                  ))}
                </datalist>
              )}
            </label>
            {!meta.hostLabelIsDevice && (
              <label className="w20">
                {tr("cf.port")}
                <input value={port} onChange={(e) => setPort(e.target.value)} placeholder="22" inputMode="numeric" />
              </label>
            )}
            {meta.hostLabelIsDevice && (
              <label className="w20">
                {tr("cf.baud")}
                <input
                  value={String(serial.baud ?? DEFAULT_SERIAL.baud)}
                  onChange={(e) => setSerialField("baud", Number(e.target.value) || 0)}
                  placeholder="115200"
                  inputMode="numeric"
                />
              </label>
            )}
          </div>
          <div className="form-row">
            <label className="grow">
              {tr("common.name")}
              {/* 名称为空时直接跟主机输入框同屏显示（用户要求）：后端与保存口径本来就是 `name.trim() || host`，
                  这里只是把那个既定回退提前画出来——存下的仍是空值，行为不变。
                  展示判据同样用 trim 后的值（与保存口径一致）：纯空格不算“有名字”，不能显成空白框。
                  一旦名称有了自己的值就不再跟；把名称清空会重新跟着主机走。
                  主机也为空时显提示文案（cf.nameAuto）。 */}
              <input
                value={name.trim() ? name : host}
                onChange={(e) => setName(e.target.value)}
                placeholder={tr("cf.nameAuto")}
              />
            </label>
            <label className="w140">
              {tr("cf.group")}
              <Select
                className="field"
                value={groupId}
                options={[{ value: "", label: "—" }, ...groups.map((g) => ({ value: g.id, label: g.name }))]}
                onChange={(v) => setGroupId(v)}
              />
            </label>
          </div>
        </section>

        {/* —— 认证 —— */}
        {meta.hasAuth && (
          <section className="cp-sec">
            <h3>{tr("cf.secAuth")}</h3>
            {usernameField}
            {meta.authModes.length > 1 && (
              <div className="fld">
                <span>{tr("cf.authType")}</span>
                <div className="seg">
                  {meta.authModes.map((m) => (
                    <button key={m} type="button" data-on={authType === m} onClick={() => setAuthType(m)}>
                      {m === "password" ? tr("cf.password") : m === "privateKey" ? tr("cf.privateKey") : tr("cf.auto")}
                    </button>
                  ))}
                </div>
              </div>
            )}
            {!authInteractive && (authType === "password" ? (
              <label>
                {tr("cf.password")}
                <PasswordField
                  value={password}
                  onChange={setPassword}
                  saved={editing && profile.hasCredential}
                  savedLabel={tr("cf.savedBadge")}
                  placeholder={editing && profile.hasCredential ? PWD_MASK : undefined}
                />
              </label>
            ) : (
              <>
                {/* 从密钥库选一把私钥；旁键跳到密钥库整页去导入/生成（同一个库、同一个 store，
                    表单只是 display:none 隐去，回来即原样接着填）。
                    按钮必须是 <label> 的兄弟而非子节点：WebKit 会把 label 的 :hover 连带转发给它
                    关联的控件，按钮装在 label 里时悬停按钮会让左侧下拉一起染上品牌色，看着像焦点跑错。*/}
                <div className="cf-keyrow">
                  <label>
                    {tr("cf.privateKey")}
                    <Select
                      className="field"
                      value={privateKeyId}
                      options={[
                        { value: "", label: tr("cf.keyStorePh") },
                        ...keyItems.map((k) => ({ value: k.id, label: k.name })),
                      ]}
                      onChange={(v) => setPrivateKeyId(v)}
                    />
                  </label>
                  <button
                    type="button"
                    className="icon-btn"
                    title={tr("cf.manageKeys")}
                    onClick={() => toolsActions.setLeftTab("keys")}
                  >
                    <IconKey />
                  </button>
                </div>

                <label>
                  {tr("cf.passphrase")}
                  <PasswordField
                    value={passphrase}
                    onChange={setPassphrase}
                    saved={editing && profile.hasCredential}
                    savedLabel={tr("cf.savedBadge")}
                    placeholder={editing && profile.hasCredential ? PWD_MASK : tr("cf.passphrasePh")}
                  />
                </label>
              </>
            ))}
            {editing && (profile.hasCredential || profile.hasPrivateKey) && (
              <label className="check">
                <input type="checkbox" checked={clearCredential} onChange={(e) => setClearCredential(e.target.checked)} />
                {tr("cf.clearCred")}
              </label>
            )}
          </section>
        )}

        {/* —— 连接选项（可选，默认收起；串口参数为必需项 → 该区对串口常开不可折）—— */}
        <OptSection
          title={tr("cf.secConn")}
          open={connOpen}
          collapsible={!meta.serialParams}
          onToggle={() => toggleSec("conn")}
        >
          {!meta.hasAuth && usernameField}
          {meta.serialParams && (
            <div className="form-row">
              <label>
                {tr("cf.dataBits")}
                <Select
                  className="field"
                  value={String(serial.dataBits ?? 8)}
                  options={[5, 6, 7, 8].map((n) => ({ value: String(n), label: String(n) }))}
                  onChange={(v) => setSerialField("dataBits", Number(v))}
                />
              </label>
              <label>
                {tr("cf.stopBits")}
                <Select
                  className="field"
                  value={String(serial.stopBits ?? 1)}
                  options={[
                    { value: "1", label: "1" },
                    { value: "2", label: "2" },
                  ]}
                  onChange={(v) => setSerialField("stopBits", Number(v))}
                />
              </label>
              <label>
                {tr("cf.parity")}
                <Select
                  className="field"
                  value={serial.parity ?? "none"}
                  options={[
                    { value: "none", label: tr("cf.none") },
                    { value: "even", label: tr("cf.parityEven") },
                    { value: "odd", label: tr("cf.parityOdd") },
                  ]}
                  onChange={(v) => setSerialField("parity", v as SerialParams["parity"])}
                />
              </label>
              <label>
                {tr("cf.flow")}
                <Select
                  className="field"
                  value={serial.flowControl ?? "none"}
                  options={[
                    { value: "none", label: tr("cf.none") },
                    { value: "hardware", label: tr("cf.flowHw") },
                    { value: "software", label: tr("cf.flowSw") },
                  ]}
                  onChange={(v) => setSerialField("flowControl", v as SerialParams["flowControl"])}
                />
              </label>
            </div>
          )}
          <label>
            {tr("cf.startupDir")}
            <input value={startupDir} onChange={(e) => setStartupDir(e.target.value)} placeholder="/var/log" spellCheck={false} />
          </label>
          {meta.keepalive && (
            <label>
              {tr("cf.keepalive")}
              <input
                type="number"
                min={0}
                value={keepaliveSecs}
                onChange={(e) => setKeepaliveSecs(e.target.value)}
                placeholder="30"
              />

            </label>
          )}
          {meta.connectTimeout && (
            <label>
              {tr("cf.connectTimeout")}
              <input
                type="number"
                min={0}
                value={connectTimeoutSecs}
                onChange={(e) => setConnectTimeoutSecs(e.target.value)}
                placeholder="20"
                inputMode="numeric"
              />

            </label>
          )}
          {meta.sshExclusive && (
            <label className="check">
              <input type="checkbox" checked={compression} onChange={(e) => setCompression(e.target.checked)} />
              {/* 开关行的标签统一包一层：裸文本在 flex 里是匿名子项，默认允许收缩，
                  中文可以逐字断行，窄面板下「X11 转发」会被压成两行。 */}
              <span className="cf-check-label">{tr("cf.compression")}</span>

            </label>
          )}

        </OptSection>

        {/* —— 日志录制 —— */}
        {meta.hasRecord && (
          <OptSection title={tr("cf.secRecord")} open={!!openSecs.record} onToggle={() => toggleSec("record")}>
            <label className="check">
              <input type="checkbox" checked={autoRecord} onChange={(e) => setAutoRecord(e.target.checked)} />
              <span className="cf-check-label">{tr("cf.autoRecord")}</span>

            </label>
            <label>
              {tr("cf.recordDir")}
              <DirPicker
                value={recordDir}
                onChange={setRecordDir}
                placeholder="recordings"
                title={tr("cf.recordDir")}
                browseLabel={tr("common.browse")}
                onOpen={recordDir ? () => void local.reveal(recordDir) : undefined}
                openLabel={tr("common.openDir")}
              />

            </label>
            <label>
              {tr("cf.recordPattern")}
              <input
                value={recordFilePattern}
                onChange={(e) => setRecordFilePattern(e.target.value)}
                placeholder="%n-%Y-%m-%d_%H-%M-%S"
                spellCheck={false}
              />

            </label>
            <label className="check">
              <input type="checkbox" checked={recordOverwrite} onChange={(e) => setRecordOverwrite(e.target.checked)} />
              <span className="cf-check-label">{tr("cf.recordOverwrite")}</span>

            </label>
            <label className="check">
              <input
                type="checkbox"
                checked={recordTimestampPrefix}
                onChange={(e) => setRecordTimestampPrefix(e.target.checked)}
              />
              <span className="cf-check-label">{tr("cf.recordTs")}</span>

            </label>
            {/* 加密落盘开关暂时隐藏：当前一律明文落盘（recordEncrypt 恒 false）。日后要放开只需删掉下方 `false &&`。 */}
            {false && (
              <label className="check">
                <input
                  type="checkbox"
                  checked={recordEncrypt}
                  onChange={(e) => setRecordEncrypt(e.target.checked)}
                />
                <span className="cf-check-label">{tr("cf.recordEncrypt")}</span>

              </label>
            )}
          </OptSection>
        )}

        {/* —— 终端 —— */}
        {meta.hasTerminal && (
          <OptSection title={tr("cf.secTerm")} open={!!openSecs.term} onToggle={() => toggleSec("term")}>
            <div className="form-row">
              <label className="grow">
                {tr("cf.termCols")}
                <input
                  type="number"
                  min={0}
                  value={termCols}
                  onChange={(e) => setTermCols(e.target.value)}
                  placeholder="80"
                  inputMode="numeric"
                />
              </label>
              <label className="grow">
                {tr("cf.termRows")}
                <input
                  type="number"
                  min={0}
                  value={termRows}
                  onChange={(e) => setTermRows(e.target.value)}
                  placeholder="24"
                  inputMode="numeric"
                />
              </label>
            </div>

            <label>
              {tr("cf.bell")}
              <Select
                className="field"
                value={bellStyle}
                options={[
                  { value: "", label: tr("cf.bellInherit") },
                  { value: "none", label: tr("term.bell.none") },
                  { value: "visual", label: tr("term.bell.visual") },
                  { value: "sound", label: tr("term.bell.sound") },
                ]}
                onChange={(v) => setBellStyle(v)}
              />

            </label>
          </OptSection>
        )}

        {/* —— 高级（可选，默认收起）—— */}
        <OptSection title={tr("cf.secAdv")} open={!!openSecs.adv} onToggle={() => toggleSec("adv")}>
          {meta.sshExclusive && (
            <label>
              {tr("cf.jump")}
              <Select
                className="field"
                value={jumpProfileId}
                options={[
                  { value: "", label: tr("cf.direct") },
                  ...profiles
                    .filter((p) => p.kind === "ssh" && p.id !== profile?.id)
                    .map((p) => ({ value: p.id, label: `${p.name} (${p.username}@${p.host})` })),
                ]}
                onChange={(v) => setJumpProfileId(v)}
              />

            </label>
          )}
          {meta.sshExclusive && (
            <label>
              {tr("cf.execCmd")}
              <input
                value={startupCommand}
                onChange={(e) => setStartupCommand(e.target.value)}
                placeholder="cd ~/project && ls"
                spellCheck={false}
              />

            </label>
          )}
          {meta.sshExclusive && (
            <label className="check">
              <input
                type="checkbox"
                checked={agentForward}
                onChange={(e) => setAgentForward(e.target.checked)}
              />
              <span className="cf-check-label">{tr("cf.agentFwd")}</span>

            </label>
          )}
          {meta.sshExclusive && (
            <label className="check">
              <input
                type="checkbox"
                checked={x11Forward}
                onChange={(e) => setX11Forward(e.target.checked)}
              />
              <span className="cf-check-label">{tr("cf.x11Fwd")}</span>

            </label>
          )}
          {isFtp && (
            <label className="check">
              <input
                type="checkbox"
                checked={ftpActiveMode}
                onChange={(e) => setFtpActiveMode(e.target.checked)}
              />
              <span className="cf-check-label">{tr("cf.ftpActive")}</span>

            </label>
          )}
          <label>
            {tr("cf.notes")}
            <textarea
              value={notes}
              onChange={(e) => setNotes(e.target.value)}
              rows={2}
            />
          </label>
        </OptSection>
      </div>

      {/* 底部：错误提示 + 操作按钮 */}
      <div className="cf-foot">
        {error && <div className="form-error">{error}</div>}
        <div className="form-actions">
          <button type="button" className="ghost" onClick={onDone} disabled={busyNow}>
            {tr("common.cancel")}
          </button>
          {!editing && (
            <button type="button" className="ghost" disabled={busyNow} onClick={() => void submit("connect")}>
              {tr("cf.connectOnly")}
            </button>
          )}
          <button type="button" disabled={busyNow} onClick={() => void submit("save")}>
            {tr("common.save")}
          </button>
          <button type="submit" disabled={busyNow}>
            {busyNow ? tr("cf.busy") : tr("cf.saveConnect")}
          </button>
        </div>
      </div>
    </form>
  );
}

/**
 * 渐进式披露的可选分区：默认收起，点标题展开。
 * collapsible=false 时（如串口的必需参数）退化为普通常开分区，不显折叠控件。
 */
function OptSection({
  title,
  open,
  collapsible = true,
  onToggle,
  children,
}: {
  title: string;
  open: boolean;
  collapsible?: boolean;
  onToggle: () => void;
  children: ReactNode;
}) {
  if (!collapsible) {
    return (
      <section className="cp-sec">
        <h3>{title}</h3>
        {children}
      </section>
    );
  }
  return (
    <section className="cp-sec cp-collapsible">
      <button type="button" className="cp-sec-head" onClick={onToggle} aria-expanded={open}>
        <span className="cp-caret" data-open={open} aria-hidden="true">
          ▸
        </span>
        <h3>{title}</h3>
      </button>
      {open && <div className="cp-sec-body">{children}</div>}
    </section>
  );
}
