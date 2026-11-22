#!/usr/bin/env bash
# 一次性创建本地自签名「代码签名」证书，供 scripts/dev-macos.sh 稳定签名使用。
#
# 解决什么：macOS 上 `tauri dev` 跑 ad-hoc 签名的裸二进制，cdhash 每次重编译都变 →
# ① 每次启动弹「登录」钥匙串密码（存 SSH 密码 / AI Key）；② 本地网络授权不粘。用一把
# **稳定的本地自签名身份**给开发二进制签名后，这两个问题都消失。
#
# 重要（开源协作须知）：证书只落在**你本机**的登录钥匙串里，**绝不进 git 仓库**（私钥入库
# = 安全事故）。所以别人 clone 本仓库后，各自跑一次本脚本即可（免 Apple 开发者账号）。
# 正式发布用的 Developer ID 签名 + 公证是另一回事，由维护者的 Apple 账号在 CI 里做。
#
# 用法： bash scripts/make-dev-cert.sh            # 默认身份名 "SailTerm Dev"
#       bash scripts/make-dev-cert.sh "别的名字"
# 之后： pnpm dev:mac
set -euo pipefail

[[ "$(uname)" == "Darwin" ]] || { echo "此脚本仅用于 macOS。"; exit 1; }
command -v openssl >/dev/null || { echo "缺 openssl，请先：brew install openssl@3"; exit 1; }

NAME="${1:-SailTerm Dev}"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

# 已存在同名有效身份则直接退出（幂等，可重复跑）。
if security find-identity -v -p codesigning | grep -q "\"$NAME\""; then
  echo "已存在代码签名身份：\"$NAME\"，无需重复创建。"
  echo "下一步：pnpm dev:mac"
  exit 0
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# 1) 生成含 codeSigning 扩展的自签名证书（用配置文件写扩展，OpenSSL / LibreSSL 通用）。
cat > "$TMP/cs.cnf" <<CNF
[req]
distinguished_name = dn
x509_extensions    = v3_cs
prompt             = no
[dn]
CN = $NAME
[v3_cs]
basicConstraints   = critical,CA:FALSE
keyUsage           = critical,digitalSignature,keyEncipherment
extendedKeyUsage   = codeSigning
CNF

openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
  -keyout "$TMP/key.pem" -out "$TMP/cert.pem" -config "$TMP/cs.cnf"

# 2) 私钥 + 证书打包成 p12 导入登录钥匙串，并预授权 /usr/bin/codesign 免提示取用私钥。
#    -legacy：OpenSSL 3.x 默认用 AES + SHA-256 MAC，macOS `security import` 不认（报
#    "MAC verification failed"）；-legacy 回退旧式 RC2/3DES + SHA1，security 可导入。
#    用一次性随机口令（非空）：空口令的 -legacy p12 仍会 MAC 校验失败；p12 在临时目录用后即销毁。
PW="$(openssl rand -hex 16)"
openssl pkcs12 -export -legacy -inkey "$TMP/key.pem" -in "$TMP/cert.pem" \
  -out "$TMP/dev.p12" -passout "pass:$PW"
security import "$TMP/dev.p12" -k "$KEYCHAIN" -P "$PW" -T /usr/bin/codesign

# 3) 把该证书设为「代码签名」用途信任（用户域，不动系统域）。find-identity -v 只列受信身份，
#    codesign 也需身份受信才算有效。此步 macOS 可能弹出授权框，输入你的登录密码确认即可。
security add-trusted-cert -p codeSign -k "$KEYCHAIN" "$TMP/cert.pem"

echo "已创建并信任代码签名身份：\"$NAME\""
security find-identity -v -p codesigning | grep "$NAME" || true
echo "下一步：pnpm dev:mac"
