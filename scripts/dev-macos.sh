#!/usr/bin/env bash
# SailTerm macOS 稳定签名开发启动（替代 `pnpm tauri dev`）
#
# 为什么要它：`tauri dev` 跑的是 target/debug 里的裸二进制，只被 linker 做 ad-hoc 签名，
# 每次重编译 cdhash 都变；macOS 15+ 的「本地网络」权限只发给有 bundle 身份 + Info.plist
# 用途声明的 app，裸二进制拿不到、也不进设置列表（连内网静默 EHOSTUNREACH）。
#
# 本脚本在「cargo 编译」与「运行」之间：① 把 debug 二进制包进一个极简 .app bundle，② 用一把
# **稳定的本地自签名身份**给 bundle 签名（身份 + identifier 固定）→ 本地网络授权能长期粘住。
#
# 钥匙串另治：自签名证书**没有 Team ID**，现代 macOS 按 partition_id 放行钥匙串读取，而自签名
# 的 partition_id 只能落到 cdhash → 每次重编译都弹「始终允许」也留不住（无解，除非换带 Team ID
# 的 Apple 证书）。故本脚本编译时启用 `--features dev-file-vault`，让 dev 凭证走本机 AES-256-GCM
# 加密文件而非 OS 钥匙串 → 彻底零弹窗。发布构建不启用该 feature，仍走系统钥匙串。
#
# 一次性前置（建自签名代码签名证书，免 Apple 开发者账号）：
#   1) 打开「钥匙串访问」→ 菜单「证书助理」→「创建证书…」
#   2) 名称填：SailTerm Dev   （与下面 SAILTERM_DEV_IDENTITY 默认值一致）
#      身份类型：自签名根证书   证书类型：代码签名
#   3) 创建后，双击该证书 →「信任」→「代码签名」设为「始终信任」
#   4) 验证：security find-identity -v -p codesigning   能看到 "SailTerm Dev"
#
# 用法： bash scripts/dev-macos.sh        （或 pnpm dev:mac）
#       SAILTERM_DEV_IDENTITY="别的身份名" bash scripts/dev-macos.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

IDENTITY="${SAILTERM_DEV_IDENTITY:-SailTerm Dev}"
BIN="src-tauri/target/debug/sailterm"
APP="src-tauri/target/SailTermDev.app"
CONTENTS="$APP/Contents"
BUNDLE_ID="com.sailterm.dev"
ENTITLEMENTS="src-tauri/Dev.entitlements"
DEV_URL="http://localhost:1420"

[[ "$(uname)" == "Darwin" ]] || { echo "此脚本仅用于 macOS。"; exit 1; }
command -v pnpm  >/dev/null || { echo "缺 pnpm"; exit 1; }
command -v cargo >/dev/null || { echo "缺 cargo"; exit 1; }

# 前置校验：签名身份必须存在，否则给出创建指引后退出（避免签名这步报难懂的错）。
if ! security find-identity -v -p codesigning | grep -q "\"$IDENTITY\""; then
  echo "未找到代码签名身份：\"$IDENTITY\""
  echo "请先按本脚本顶部注释，在「钥匙串访问」里创建同名自签名代码签名证书，"
  echo "或用 SAILTERM_DEV_IDENTITY 指定一个已有身份（security find-identity -v -p codesigning 可列出）。"
  exit 1
fi

# 0) 残留清理（在起 Vite 之前）：上一轮会话的残留进程必须先收掉，否则有两个坑——
#    ① 残留 Vite 占着 1420，本次 pnpm dev 静默失败，app 连的是旧 Vite（同一份源码，无害但易误判）；
#    ② 旧 SailTermDev 实例还活着时，open 只会「唤醒」它而不新起进程——跑的仍是旧二进制，
#      「重启」看起来没生效（真机踩过：修复已编译进包，界面却还是旧行为）。
OLD_VITE="$(lsof -nP -tiTCP:1420 -sTCP:LISTEN 2>/dev/null || true)"
if [[ -n "$OLD_VITE" ]]; then
  echo "==> 1420 被残留进程占用（pid: ${OLD_VITE}），先收掉"
  kill $OLD_VITE 2>/dev/null || true
fi
OLD_APP="$(pgrep -f 'SailTermDev.app/Contents/MacOS/sailterm' || true)"
if [[ -n "$OLD_APP" ]]; then
  echo "==> 检测到旧 SailTermDev 实例（pid: ${OLD_APP}），先退出旧实例再启动（否则 open 只会唤醒它，跑的还是旧二进制）"
  kill $OLD_APP 2>/dev/null || true
fi
[[ -n "$OLD_VITE$OLD_APP" ]] && sleep 1

# 1) 起前端 dev server（vite @ :1420），退出时一并收掉。
pnpm dev &
VITE_PID=$!
cleanup() { kill "$VITE_PID" 2>/dev/null || true; }
trap cleanup EXIT INT TERM

# 2) 等 vite 就绪，再编译 Rust 后端（与 tauri dev 同 profile：debug）。
echo "==> 等待 Vite 就绪 $DEV_URL"
for _ in $(seq 1 60); do
  curl -s -o /dev/null "$DEV_URL" && break
  sleep 0.5
done

echo "==> cargo build (debug, dev-file-vault)"
# 启用 dev-file-vault：凭证走本机加密文件而非 OS 钥匙串，规避自签名（无 Team ID）
# dev 身份下 partition_id 按 cdhash 分区、每次重编译反复弹「始终允许」也留不住的问题。
cargo build --manifest-path src-tauri/Cargo.toml --features dev-file-vault

# 3) 组一个极简 .app bundle：只把现成的 debug 二进制拷进去 + 写一份带 NSLocalNetworkUsageDescription
#    的 Info.plist，不重编译、不走 release 打包，秒级完成（本地网络权限必须有 bundle 身份才拿得到）。
echo "==> 组装 $APP"
rm -rf "$APP"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "$BIN" "$CONTENTS/MacOS/sailterm"
# 图标有就带上，没有不报错
[[ -f src-tauri/icons/icon.icns ]] && cp src-tauri/icons/icon.icns "$CONTENTS/Resources/icon.icns" || true
cat > "$CONTENTS/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>         <string>sailterm</string>
    <key>CFBundleIdentifier</key>         <string>$BUNDLE_ID</string>
    <key>CFBundleName</key>               <string>SailTerm Dev</string>
    <key>CFBundleDisplayName</key>        <string>SailTerm Dev</string>
    <key>CFBundlePackageType</key>        <string>APPL</string>
    <key>CFBundleVersion</key>            <string>0.1.0</string>
    <key>CFBundleShortVersionString</key> <string>0.1.0</string>
    <key>CFBundleIconFile</key>           <string>icon.icns</string>
    <key>LSMinimumSystemVersion</key>     <string>10.13</string>
    <key>NSHighResolutionCapable</key>    <true/>
    <key>NSLocalNetworkUsageDescription</key>
    <string>SailTerm 需要访问本地网络，以便连接局域网内的 SSH / Telnet 主机，并使用 Ping、端口扫描、路由追踪等网络工具。</string>
</dict>
</plist>
PLIST

# 4) 用稳定身份给整个 bundle 签名（覆盖 linker 的 ad-hoc 签名）：钥匙串 ACL 与本地网络授权都认
#    这把固定身份 + identifier，重编译 cdhash 变也无妨，首次授权后长期不再弹。
echo "==> codesign --sign \"$IDENTITY\" ($BUNDLE_ID)"
codesign --force --sign "$IDENTITY" \
  --identifier "$BUNDLE_ID" \
  --entitlements "$ENTITLEMENTS" \
  --timestamp=none \
  "$APP"

# 5) 以 bundle 身份启动（open 走 LaunchServices，TCC 才会把网络请求归到本 app）。debug 构建仍加载
#    $DEV_URL；-W 阻塞到 app 退出，期间脚本存活 → trap 收掉 vite。首次连内网会弹一次「本地网络」
#    授权（点「允许」后长期不再弹）；钥匙串因 dev-file-vault 不再参与，读凭证零弹窗。
echo "==> 启动 SailTerm（dev，稳定签名 .app）"
open -W "$APP"
