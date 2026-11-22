#!/usr/bin/env bash
# SailTerm macOS packaging: tauri build (app, signed) -> hdiutil dmg
# Usage: bash scripts/package-macos.sh
# Must run on macOS (codesign / hdiutil).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ "$(uname)" != "Darwin" ]]; then
  echo "macOS packages must be built on a Mac (codesign + dmg)."
  exit 1
fi

command -v pnpm >/dev/null || { echo "pnpm not found"; exit 1; }
command -v cargo >/dev/null || { echo "cargo not found"; exit 1; }

# Optional: both Intel and Apple Silicon
# rustup target add x86_64-apple-darwin aarch64-apple-darwin

# 用稳定自签身份签名（关键）：默认的 ad-hoc 链接签名会让 app 进不了 macOS「本地网络」TCC
# 可授权集——主进程连局域网被静默拒（SSH 报 No route to host os error 65）。用 SailTerm Dev
# 证书签出稳定 identifier（com.sailterm.desktop），app 才能弹授权框并被授权。
# 找不到证书时回退 ad-hoc（仅供不需要内网功能的场景）。
SIGN_IDENTITY="SailTerm Dev"
if security find-identity -v -p codesigning 2>/dev/null | grep -q "\"${SIGN_IDENTITY}\""; then
  echo "签名身份: ${SIGN_IDENTITY} (稳定 identifier, 本地网络可授权)"
  TAURI_SIGN_CFG="{\"bundle\":{\"macOS\":{\"signingIdentity\":\"${SIGN_IDENTITY}\"}}}"
else
  echo "警告: 未找到签名身份 ${SIGN_IDENTITY}, 回退为 ad-hoc 签名。"
  echo "      ad-hoc 包无法授权「本地网络」, SSH / 内网功能会报 No route to host。"
  echo "      需要内网功能请先创建该自签证书（见 RELEASE.md）。"
  TAURI_SIGN_CFG="{}"
fi

# 只让 tauri 产 app（含签名）。dmg 自己用 hdiutil 造：Tauri 的 bundle_dmg.sh 靠 AppleScript
# 驱动 Finder 排版，在非交互/CI 环境频繁失败，且失败会连累整个 build 退出、产物拷贝不执行。
echo "=== tauri build (app, signed) ==="
pnpm tauri build --bundles app --config "${TAURI_SIGN_CFG}"

OUT="${ROOT}/src-tauri/target/release/bundle"
APP="${OUT}/macos/SailTerm.app"
[[ -d "$APP" ]] || { echo "未找到产出的 .app: $APP"; exit 1; }

DEST="${ROOT}/dist-bundle"
mkdir -p "$DEST"

# 覆盖式拷贝（先删旧 app 避免 cp -R 合并残留）
rm -rf "${DEST}/SailTerm.app"
cp -R "$APP" "${DEST}/"
echo "app -> ${DEST}/SailTerm.app"

# 清掉可能残留的挂载卷（未命名/Untitled/SailTerm），避免 dmg 步骤冲突
for v in /Volumes/*; do
  case "$(basename "$v")" in
    未命名|Untitled|SailTerm) hdiutil detach "$v" -force >/dev/null 2>&1 || diskutil unmount force "$v" >/dev/null 2>&1 || true ;;
  esac
done

# hdiutil 造标准「拖到 Applications」安装盘
VER="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist" 2>/dev/null || echo 0.1.0)"
ARCH="$(uname -m)"; [[ "$ARCH" == "arm64" ]] && ARCH="aarch64"
DMG="${DEST}/SailTerm_${VER}_${ARCH}.dmg"
STAGE="$(mktemp -d)"
cp -R "$APP" "${STAGE}/SailTerm.app"
ln -s /Applications "${STAGE}/Applications"
rm -f "$DMG"
hdiutil create -volname "SailTerm" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
rm -rf "$STAGE"

echo "=== 产物 ==="
find "$DEST" -maxdepth 1 \( -name '*.app' -o -name '*.dmg' \) -print
# 对外分发需 Developer ID 签名 + notarytool 公证，见 RELEASE.md；此处仅本机自用。
