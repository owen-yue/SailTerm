import { useEffect, useMemo, useState } from "react";
import { IconArrowDown, IconCheck, IconClipboard, IconKey, IconLock } from "../common/icons";
import { copyText } from "../../lib/clipboard";
import { sshKeyActions, sshKeyStore, useStore } from "../../stores";
import { dialog } from "../../lib/tauri";
import type { KeyGenAlgorithm, KeyInput, SshKey } from "../../types";
import Popconfirm from "../common/Popconfirm";
import DeleteButton from "../common/DeleteButton";
import DirPicker from "../common/DirPicker";
import Sheet from "../common/Sheet";
import Select from "../common/Select";
import Button from "../common/Button";
import { useI18n } from "../../lib/i18n";

/** 私钥来源：粘贴内容 or 磁盘文件路径。 */
type Source = "paste" | "file";
/** 弹窗模式：新建（导入 / 生成）或查看某把密钥详情；null 表示只在看网格。 */
type SheetState = { kind: "new" } | { kind: "detail"; key: SshKey } | null;

/** 默认导出文件名：把名称里不适合做文件名的字符（含空白）替换为下划线。 */
function defaultFileName(name: string): string {
  const base = name.replace(/[\\/:*?"<>|\s]+/g, "_").replace(/^_+|_+$/g, "");
  return `${base || "ssh_key"}.key`;
}

/**
 * 密钥库整页管理台：沿用本项目既有的卡片 + Sheet 弹窗语言。
 *
 * 主区是一整片密钥卡片网格；点卡片 / 「新建密钥」走共享 Sheet 弹窗看详情或录入，
 * 避免把窄表单丢在空荡的右栏里。页面按「分区」组织：当前只有「密钥库」分区，
 * 未来「生物认证」会作为与之明确区隔的并列分区加入，不与密钥条目混为一谈。
 *
 * 本页是密钥管理的唯一入口：连接表单认证区的私钥下拉与本页共用同一后端密钥库与 store——
 * 从表单点密钥键跳到本页导入 / 生成，回到会话区即可在下拉里选中新密钥。
 *
 * 安全红线：私钥材料只进后端保险库，**绝不经 IPC 回前端**——列表只回算法 / 指纹 / 公钥 / 注释；
 * 导出私钥走后端写用户指定文件（不返回内容）；口令只作入参直送保险库，前端不留存、不回显。
 */
export default function KeyVaultPage() {
  const t = useI18n();
  const { items, loading, error } = useStore(sshKeyStore);

  const [query, setQuery] = useState("");
  /** 弹窗状态：null=只看网格；new=录入；detail=查看某把密钥。 */
  const [sheet, setSheet] = useState<SheetState>(null);
  /** 刚复制过公钥的密钥 id（短暂显示「已复制」）。 */
  const [copied, setCopied] = useState("");

  // 新建表单（导入 / 生成二选一）
  const [tab, setTab] = useState<"import" | "generate">("import");
  const [fName, setFName] = useState("");
  const [source, setSource] = useState<Source>("paste");
  const [fPem, setFPem] = useState("");
  const [fPath, setFPath] = useState("");
  const [fPass, setFPass] = useState("");
  const [genName, setGenName] = useState("");
  const [genAlg, setGenAlg] = useState<KeyGenAlgorithm>("ed25519");
  const [localErr, setLocalErr] = useState("");
  const [busy, setBusy] = useState(false);

  // 删除确认（气泡锚点）——渲染在 Sheet 之外作兄弟节点，避免嵌套模态事件冒泡
  const [confirmDel, setConfirmDel] = useState<{ key: SshKey; x: number; y: number } | null>(null);

  useEffect(() => {
    void sshKeyActions.refresh();
  }, []);

  /** 搜索过滤（名 / 算法 / 指纹 / 注释，大小写不敏感）；顺序沿用 store 的「名 → id」稳定序。 */
  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return items;
    return items.filter(
      (k) =>
        k.name.toLowerCase().includes(q) ||
        k.algorithm.toLowerCase().includes(q) ||
        k.fingerprint.toLowerCase().includes(q) ||
        k.comment.toLowerCase().includes(q),
    );
  }, [items, query]);

  function openNew() {
    setFName("");
    setSource("paste");
    setFPem("");
    setFPath("");
    setFPass("");
    setGenName("");
    setGenAlg("ed25519");
    setTab("import");
    setLocalErr("");
    setSheet({ kind: "new" });
  }

  function openDetail(k: SshKey) {
    setLocalErr("");
    setSheet({ kind: "detail", key: k });
  }

  async function submitImport() {
    if (!fName.trim()) {
      setLocalErr(t("km.errName"));
      return;
    }
    const pem = fPem.trim();
    const path = fPath.trim();
    if (source === "paste" && !pem) {
      setLocalErr(t("km.errPem"));
      return;
    }
    if (source === "file" && !path) {
      setLocalErr(t("km.errPath"));
      return;
    }
    setLocalErr("");
    setBusy(true);
    const input: KeyInput = {
      name: fName.trim(),
      privateKeyPem: source === "paste" ? pem : null,
      privateKeyPath: source === "file" ? path : null,
      passphrase: fPass.trim() || null,
    };
    const saved = await sshKeyActions.importKey(input);
    setBusy(false);
    if (saved) {
      // 导入成功即清空私钥 / 口令输入框（前端不留存明文），关弹窗回网格看到新卡片。
      setFPem("");
      setFPath("");
      setFPass("");
      setSheet(null);
    }
  }

  async function submitGenerate() {
    if (!genName.trim()) {
      setLocalErr(t("km.errName"));
      return;
    }
    setLocalErr("");
    setBusy(true);
    const k = await sshKeyActions.generate({
      name: genName.trim(),
      comment: "sailterm",
      algorithm: genAlg,
    });
    setBusy(false);
    if (k) setSheet(null);
  }

  async function onCopyPublic(k: SshKey) {
    if (await copyText(k.publicKey)) {
      setCopied(k.id);
      window.setTimeout(() => setCopied((c) => (c === k.id ? "" : c)), 1500);
    }
  }

  /** 导出私钥：弹原生另存为框选目标文件（后端写盘，内容不回前端）。 */
  async function exportKey(k: SshKey) {
    try {
      const path = await dialog.saveFile({ title: t("km.exportTitle"), defaultName: defaultFileName(k.name) });
      if (!path) return;
      const ok = await sshKeyActions.exportFile(k.id, path);
      if (!ok) setLocalErr(sshKeyStore.get().error || t("km.exportFail"));
    } catch (e) {
      setLocalErr(String(e));
    }
  }

  const err = localErr || error;

  return (
    <div className="key-vault">
      <div className="kv-main">
        {/* 分区头：标题 + 计数 + 搜索 + 新建。未来「生物认证」分区与此并列。 */}
        <div className="kv-toolbar">
          <span className="kv-section">
            <IconKey /> {t("km.vault")}
            <span className="ss-count">{items.length}</span>
          </span>
          <input
            className="key-search kv-searchbox"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("km.searchPh")}
            spellCheck={false}
          />
          <Button size="sm" onClick={openNew}>
            ＋ {t("kv.new")}
          </Button>
        </div>

        {/* 卡片网格 */}
        <div className="kv-grid">
          {loading && <div className="ss-empty">{t("km.loading")}</div>}
          {!loading && visible.length === 0 && (
            <div className="ss-empty">{items.length === 0 ? t("km.none") : t("common.noMatch")}</div>
          )}
          {visible.map((k) => (
            <div
              key={k.id}
              role="button"
              tabIndex={0}
              className="kv-tile"
              onClick={() => openDetail(k)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  openDetail(k);
                }
              }}
            >
              <span className="kv-tile-ico">
                <IconKey />
              </span>
              <span className="kv-tile-main">
                <span className="kv-tile-name">{k.name}</span>
                <span className="kv-tile-sub">{k.algorithm}</span>
              </span>
              {k.encrypted && (
                <span className="key-badge">
                  <IconLock />
                </span>
              )}
            </div>
          ))}
        </div>
      </div>

      {sheet && (
        <Sheet
          title={
            sheet.kind === "new" ? (
              t("kv.new")
            ) : (
              <>
                <IconKey /> {sheet.key.name}
              </>
            )
          }
          onClose={() => setSheet(null)}
        >
          <div className="sec-set-body">
            {sheet.kind === "new" ? (
              <section className="ss-sec">
                <div className="seg seg-sm kv-tab">
                  <button type="button" data-on={tab === "import"} onClick={() => setTab("import")}>
                    {t("km.import")}
                  </button>
                  <button type="button" data-on={tab === "generate"} onClick={() => setTab("generate")}>
                    {t("km.generate")}
                  </button>
                </div>

                {tab === "import" ? (
                  <form
                    className="ss-form"
                    onSubmit={(e) => {
                      e.preventDefault();
                      void submitImport();
                    }}
                  >
                    <label>
                      {t("common.name")}
                      <input
                        value={fName}
                        onChange={(e) => setFName(e.target.value)}
                        placeholder={t("km.namePh")}
                        autoFocus
                        spellCheck={false}
                      />
                    </label>

                    <div className="seg seg-sm">
                      <button type="button" data-on={source === "paste"} onClick={() => setSource("paste")}>
                        {t("km.srcPaste")}
                      </button>
                      <button type="button" data-on={source === "file"} onClick={() => setSource("file")}>
                        {t("km.srcFile")}
                      </button>
                    </div>

                    {source === "paste" ? (
                      <label>
                        {t("km.pemLabel")}
                        <textarea
                          value={fPem}
                          onChange={(e) => setFPem(e.target.value)}
                          placeholder={"-----BEGIN OPENSSH PRIVATE KEY-----\n…\n-----END OPENSSH PRIVATE KEY-----"}
                          rows={6}
                          spellCheck={false}
                        />
                      </label>
                    ) : (
                      <label>
                        {t("km.pathLabel")}
                        <DirPicker
                          value={fPath}
                          onChange={setFPath}
                          placeholder={t("km.pathPh")}
                          title={t("km.pathLabel")}
                          mode="file"
                          browseLabel={t("common.browse")}
                        />
                      </label>
                    )}

                    <label>
                      {t("km.passLabel")}
                      <input
                        type="password"
                        value={fPass}
                        onChange={(e) => setFPass(e.target.value)}
                        placeholder={t("km.passPh")}
                        spellCheck={false}
                        autoComplete="off"
                      />
                    </label>

                    {err && <div className="form-error">{err}</div>}

                    <div className="form-actions">
                      <Button type="submit" loading={busy}>
                        {busy ? t("km.importing") : t("km.import")}
                      </Button>
                    </div>
                  </form>
                ) : (
                  <form
                    className="ss-form"
                    onSubmit={(e) => {
                      e.preventDefault();
                      void submitGenerate();
                    }}
                  >
                    <label>
                      {t("common.name")}
                      <input
                        value={genName}
                        onChange={(e) => setGenName(e.target.value)}
                        placeholder={t("km.namePh")}
                        autoFocus
                        spellCheck={false}
                      />
                    </label>
                    <label>
                      {t("km.algorithm")}
                      <Select
                        className="field"
                        value={genAlg}
                        options={[
                          { value: "ed25519", label: t("km.ed25519Rec") },
                          { value: "ecdsa", label: "ECDSA P-256" },
                          { value: "ecdsa-p384", label: "ECDSA P-384" },
                          { value: "ecdsa-p521", label: "ECDSA P-521" },
                          { value: "rsa", label: "RSA 2048" },
                          { value: "rsa-3072", label: "RSA 3072" },
                          { value: "rsa-4096", label: "RSA 4096" },
                        ]}
                        onChange={(v) => setGenAlg(v as KeyGenAlgorithm)}
                      />
                    </label>
                    <div className="form-hint">{t("km.genNote")}</div>
                    {err && <div className="form-error">{err}</div>}
                    <div className="form-actions">
                      <Button type="submit" loading={busy}>
                        {busy ? t("km.generating") : t("km.generateBtn")}
                      </Button>
                    </div>
                  </form>
                )}
              </section>
            ) : (
              <section className="ss-sec">
                <div className="kv-field">
                  <span className="kv-k">{t("km.algorithm")}</span>
                  <span className="kv-v">{sheet.key.algorithm}</span>
                </div>
                <div className="kv-field">
                  <span className="kv-k">{t("kv.fingerprint")}</span>
                  <span className="kv-v kv-mono" title={t("km.fpTitle")}>
                    {sheet.key.fingerprint}
                  </span>
                </div>
                {sheet.key.comment && (
                  <div className="kv-field">
                    <span className="kv-k">{t("kv.comment")}</span>
                    <span className="kv-v">{sheet.key.comment}</span>
                  </div>
                )}
                {sheet.key.encrypted && (
                  <div className="kv-field">
                    <span className="kv-k">{t("km.withPass")}</span>
                    <span className="kv-v">
                      <IconLock /> {t("km.withPass")}
                    </span>
                  </div>
                )}
                <div className="kv-field kv-field-block">
                  <span className="kv-k">{t("km.pubTitle")}</span>
                  <div className="key-pub">{sheet.key.publicKey}</div>
                </div>

                {err && <div className="form-error">{err}</div>}

                <div className="form-actions kv-acts">
                  <Button
                    variant="outline"
                    onClick={() => void onCopyPublic(sheet.key)}
                    title={t("km.copyTitle")}
                  >
                    {copied === sheet.key.id ? <IconCheck /> : <IconClipboard />} {t("km.copyPub")}
                  </Button>
                  <Button variant="outline" onClick={() => void exportKey(sheet.key)} title={t("km.export")}>
                    <IconArrowDown /> {t("km.export")}
                  </Button>
                  <DeleteButton
                    className="kv-del"
                    title={t("km.delTitle")}
                    onConfirm={(x, y) => setConfirmDel({ key: sheet.key, x, y })}
                  />
                </div>
              </section>
            )}
          </div>
        </Sheet>
      )}

      {confirmDel && (
        <Popconfirm
          x={confirmDel.x}
          y={confirmDel.y}
          title={t("km.delMsg").replace("{n}", confirmDel.key.name)}
          description={t("km.delWarn")}
          onConfirm={() => {
            const id = confirmDel.key.id;
            setConfirmDel(null);
            // 删的正是弹窗里查看的那把：关弹窗回网格。
            if (sheet && sheet.kind === "detail" && sheet.key.id === id) setSheet(null);
            void sshKeyActions.remove(id);
          }}
          onClose={() => setConfirmDel(null)}
        />
      )}
    </div>
  );
}
