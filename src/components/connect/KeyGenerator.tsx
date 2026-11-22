// SSH 密钥生成器（工具 keygen 的独立窗口本体）：选算法 →（可选）口令 → 生成 → 展示公钥 → 复制 / 存公钥 / 存私钥。
//
// 与「密钥库」(KeyVaultPage) 完全解耦：这里生成的密钥**不入 SailTerm 库、不进保险库**，只输出公钥文本
// 与写盘文件（纯生成器定位）。私钥材料只在后端进程内存短暂停留供写盘，
// **绝不经 IPC 回前端**（见 commands/ssh_key.rs 的 ssh_keygen_*）。
import { useState } from "react";
import { copyText } from "../../lib/clipboard";
import { useI18n } from "../../lib/i18n";
import { dialog, errMsg, sshKeys } from "../../lib/tauri";
import type { KeyGenAlgorithm, KeygenPreview } from "../../types";
import Button from "../common/Button";
import Select from "../common/Select";

/** 算法下拉：类型 + 位数合一（与后端 GenAlg::parse 的取值一致）。 */
const ALGS: { value: KeyGenAlgorithm; label: string }[] = [
  { value: "ed25519", label: "Ed25519" },
  { value: "ecdsa", label: "ECDSA P-256" },
  { value: "ecdsa-p384", label: "ECDSA P-384" },
  { value: "ecdsa-p521", label: "ECDSA P-521" },
  { value: "rsa", label: "RSA 2048" },
  { value: "rsa-3072", label: "RSA 3072" },
  { value: "rsa-4096", label: "RSA 4096" },
];

/** 默认私钥文件名（仿 OpenSSH 惯例 id_<alg>）。 */
function baseName(alg: string): string {
  return `id_${alg.replace(/-/g, "")}`;
}

export default function KeyGenerator() {
  const t = useI18n();
  const [alg, setAlg] = useState<KeyGenAlgorithm>("ed25519");
  const [pass, setPass] = useState("");
  const [pass2, setPass2] = useState("");
  const [busy, setBusy] = useState(false);
  const [preview, setPreview] = useState<KeygenPreview | null>(null);
  const [copied, setCopied] = useState(false);
  const [msg, setMsg] = useState("");
  const [error, setError] = useState("");

  async function generate() {
    if (pass !== pass2) {
      setError(t("kg.passMismatch"));
      return;
    }
    setError("");
    setMsg("");
    setPreview(null);
    setCopied(false);
    setBusy(true);
    try {
      setPreview(await sshKeys.keygenGenerate({ algorithm: alg, passphrase: pass || null }));
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setBusy(false);
      // 口令只作一次性入参，用后即清，前端不留存明文
      setPass("");
      setPass2("");
    }
  }

  async function copyPub() {
    if (!preview) return;
    if (await copyText(preview.publicKey)) {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    }
  }

  async function savePriv() {
    if (!preview) return;
    try {
      const path = await dialog.saveFile({ title: t("kg.savePrivTitle"), defaultName: baseName(alg) });
      if (!path) return;
      await sshKeys.keygenSavePrivate(preview.token, path);
      setMsg(t("kg.saved").replace("{p}", path));
    } catch (e) {
      setError(errMsg(e));
    }
  }

  async function savePub() {
    if (!preview) return;
    try {
      const path = await dialog.saveFile({
        title: t("kg.savePubTitle"),
        defaultName: `${baseName(alg)}.pub`,
      });
      if (!path) return;
      await sshKeys.keygenSavePublic(preview.token, path);
      setMsg(t("kg.saved").replace("{p}", path));
    } catch (e) {
      setError(errMsg(e));
    }
  }

  return (
    <div className="keygen ss-form">
      <div className="keygen-params">
        <label className="keygen-field">
          <span>{t("kg.alg")}</span>
          <Select
            className="field"
            value={alg}
            options={ALGS}
            onChange={(v) => setAlg(v as KeyGenAlgorithm)}
          />
        </label>
        <label className="keygen-field">
          <span>{t("kg.pass")}</span>
          <input
            type="password"
            value={pass}
            placeholder={t("kg.passPh")}
            autoComplete="off"
            spellCheck={false}
            onChange={(e) => setPass(e.target.value)}
          />
        </label>
        <label className="keygen-field">
          <span>{t("kg.passConfirm")}</span>
          <input
            type="password"
            value={pass2}
            placeholder={t("kg.passConfirmPh")}
            autoComplete="off"
            spellCheck={false}
            onChange={(e) => setPass2(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && !busy && void generate()}
          />
        </label>
        <div className="keygen-actions">
          <Button onClick={() => void generate()} disabled={busy}>
            {busy ? t("kg.generating") : t("kg.generate")}
          </Button>
        </div>
      </div>

      {error && <div className="form-error">{error}</div>}
      {msg && <div className="form-hint">{msg}</div>}

      {preview ? (
        <div className="keygen-out">
          <div className="keygen-fp">
            <span className="keygen-fp-k">{t("kg.fp")}</span>
            <code>{preview.fingerprint}</code>
            {preview.encrypted && <span className="keygen-badge">{t("kg.withPass")}</span>}
          </div>
          <label className="keygen-field">
            <span>{t("kg.pubLabel")}</span>
            <textarea
              className="keygen-pub"
              readOnly
              rows={4}
              value={preview.publicKey}
              spellCheck={false}
              onFocus={(e) => e.currentTarget.select()}
            />
          </label>
          <div className="keygen-actions">
            <Button variant="outline" onClick={() => void copyPub()}>
              {copied ? t("kg.copied") : t("kg.copy")}
            </Button>
            <Button variant="outline" onClick={() => void savePub()}>
              {t("kg.savePub")}
            </Button>
            <Button variant="outline" onClick={() => void savePriv()}>
              {t("kg.savePriv")}
            </Button>
          </div>
        </div>
      ) : (
        !busy && <div className="keygen-empty">{t("kg.empty")}</div>
      )}

      <div className="form-hint keygen-note">{t("kg.note")}</div>
    </div>
  );
}
