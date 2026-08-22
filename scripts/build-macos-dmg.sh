#!/usr/bin/env bash
# macOS DMG 兜底打包：tauri build 的 create-dmg 依赖 Finder AppleScript 做装饰性
# 窗口布局，在无自动化权限/沙箱环境会以 -1712 (AppleEvent 超时) 失败。
# 本脚本用 hdiutil 直出标准只读 DMG（无装饰布局，内容完全一致）。
# 用法：npm run tauri build 之后执行 bash scripts/build-macos-dmg.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/src-tauri/target/release/bundle/macos/Harness Agent.app"
OUT_DIR="$ROOT/src-tauri/target/release/bundle/dmg"
VERSION="$(python3 -c "import json;print(json.load(open('$ROOT/package.json'))['version'])")"
ARCH="$(uname -m | sed 's/^arm64$/aarch64/')"  # 对齐 tauri 的目标三元组命名
OUT="$OUT_DIR/Harness Agent_${VERSION}_${ARCH}.dmg"

[ -d "$APP" ] || { echo "未找到 $APP，请先 npm run tauri build"; exit 1; }
mkdir -p "$OUT_DIR"
rm -f "$OUT"

hdiutil create -volname "Harness Agent" -srcfolder "$APP" -ov -format UDZO -o "$OUT" >/dev/null
echo "DMG 已生成: $OUT"
hdiutil verify "$OUT" >/dev/null && echo "校验通过"
