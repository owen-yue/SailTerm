#!/usr/bin/env bash
# IPC 边界守护：`src/lib/tauri` 是前端访问 @tauri-apps 的唯一出口（端口-适配器收口）。
# 业务代码（组件/store/tools）需要新后端能力时，应在 lib/tauri 加包装，而非直接 import。
# 挂在 `pnpm check` 里防边界随功能迭代再次被打洞。
set -euo pipefail
cd "$(dirname "$0")/.."

hits=$(grep -rn --include='*.ts' --include='*.tsx' -E '(from |import\()"@tauri-apps/' src \
  | grep -v '^src/lib/tauri/' || true)

if [ -n "$hits" ]; then
  echo "✗ IPC 边界被突破：以下文件直接 import 了 @tauri-apps（应经 src/lib/tauri 适配层）："
  echo "$hits"
  exit 1
fi
echo "✓ IPC 边界完好（@tauri-apps 仅 lib/tauri 引用）"
