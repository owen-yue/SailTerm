#!/usr/bin/env bash
# SailTerm Linux packaging: tauri build -> .deb + .AppImage, artifacts collected into dist-bundle
# Usage: bash scripts/package-linux.sh
# Must run on Linux (deb/AppImage 无法在 Windows/macOS 上交叉产出：需 dpkg-deb/patchelf/linuxdeploy)。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ "$(uname)" != "Linux" ]]; then
  echo "Linux packages must be built on Linux (dpkg-deb / patchelf / linuxdeploy)."
  exit 1
fi

command -v pnpm  >/dev/null || { echo "pnpm not found (Node 20+，见 CONTRIBUTING.md)"; exit 1; }
command -v cargo >/dev/null || { echo "cargo not found (rustup stable)"; exit 1; }

# Tauri v2 的 Linux 系统依赖基线（Ubuntu 22.04+ / Debian 12+ 均提供 4.1 系列）。
if ! pkg-config --exists webkit2gtk-4.1 || ! pkg-config --exists gtk+-3.0; then
  echo "缺少 Tauri Linux 系统库，先安装（Ubuntu/Debian）："
  echo "  sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev \\"
  echo "       libjavascriptcoregtk-4.1-dev librsvg2-dev patchelf file"
  exit 1
fi
command -v patchelf >/dev/null || { echo "patchelf not found（AppImage 必需）: sudo apt install patchelf"; exit 1; }

echo "=== tauri build (deb + appimage) ==="
# 注意：AppImage 打包时 tauri 会联网从 GitHub 下载 linuxdeploy 插件与 appimagetool
# （缓存于 ~/.cache/tauri）。访问不通时先导出 https_proxy，或预热该缓存目录。
pnpm tauri build --bundles deb,appimage

OUT="${ROOT}/src-tauri/target/release/bundle"
DEST="${ROOT}/dist-bundle"
mkdir -p "$DEST"

shopt -s nullglob
bundles=( "${OUT}"/deb/*.deb "${OUT}"/appimage/*.AppImage )
shopt -u nullglob
if (( ${#bundles[@]} == 0 )); then
  echo "未找到产物（$OUT/deb 与 $OUT/appimage 均为空）"
  exit 1
fi

# tauri 产物名已含版本与架构（SailTerm_<ver>_<arch>.deb / .AppImage），原名拷出即可
for f in "${bundles[@]}"; do
  cp -f "$f" "$DEST/"
done

echo "=== 产物 ==="
for f in "${bundles[@]}"; do
  echo "$(basename "$f")  ->  dist-bundle/"
done
(cd "$DEST" && sha256sum -- "${bundles[@]##*/}")
# 分发提示：AppImage 终端用户机需 libfuse2（Ubuntu 24.04 起默认 fuse3，需
# sudo apt install libfuse2 或改用 --appimage-extract-and-run）。deb 依赖见
# tauri.conf.json bundle.linux.deb.depends。
