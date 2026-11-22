// 敏感文件识别（的 🔒 标记）：命中即在文件名后加锁标，提醒用户
// 「这里面大概率是凭证/密钥，别随手拖来拖去、别贴进 AI 对话」。
//
// 只做**文件名层面**的提示，不读内容、不做拦截；真正的脱敏在后端。

/** `.env`、`.env.local`、`.env.production` 之类。 */
const ENV_FILE = /^\.env(\.|$)/i;

/** 凭据/密钥类扩展名。 */
const SECRET_EXT = /\.(pem|key|crt|cer|p12|pfx|jks|keystore|kdbx|gpg|asc|ppk)$/i;

/** 常见凭证文件名（精确名或 `name.*` 形式）。 */
const SECRET_NAMES = [
  "id_rsa",
  "id_ed25519",
  "id_ecdsa",
  "id_dsa",
  "authorized_keys",
  "known_hosts",
  "shadow",
  "master.passwd",
  ".netrc",
  ".npmrc",
  ".pypirc",
  ".pgpass",
  ".my.cnf",
  ".git-credentials",
  ".htpasswd",
];

/** 该文件名是否疑似含凭证。目录不参与判定（目录名撞车太常见）。 */
export function isSensitive(name: string, isDir = false): boolean {
  if (isDir) return false;
  if (ENV_FILE.test(name) || SECRET_EXT.test(name)) return true;
  const lower = name.toLowerCase();
  return SECRET_NAMES.some((n) => lower === n || lower.startsWith(`${n}.`));
}
