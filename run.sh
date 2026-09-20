#!/usr/bin/env bash
# ============================================================
#  Mac 一键：按需构建 + 启动桌面程序
#  数据目录固定在仓库根下的 data/（对齐 Windows run.cmd）
#
#  用法：
#    ./run.sh           # 缺产物或前端更新过则自动构建，再启动
#    ./run.sh --rebuild # 强制重新构建后再启动
#    ./run.sh --build-only  # 只构建，不启动
#
#  为什么需要这个脚本：
#    1) 数据目录跟「当前工作目录」走。直接跑 target/debug/desktop 时，
#       cwd 常是 target/debug/，配置/标定会被 cargo clean 清掉。
#       本脚本先 cd 到仓库根再启动，数据落在 <仓库>/data/。
#    2) 独立启动必须带 custom-protocol，并把前端 dist 嵌进二进制；
#       否则白窗口。tauri dev 不走这条路（靠 Vite）。
#    3) macosocr 是独立进程，漏编要到第一次 OCR 才炸——这里一并检查/编译。
# ============================================================

set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"

EXE="${ROOT}/target/debug/desktop"
OCR="${ROOT}/target/debug/macosocr"
DIST_INDEX="${ROOT}/apps/desktop/dist/index.html"
LOG="${ROOT}/data/startup.log"

FORCE_REBUILD=0
BUILD_ONLY=0
for arg in "$@"; do
  case "$arg" in
    --rebuild|-f) FORCE_REBUILD=1 ;;
    --build-only) BUILD_ONLY=1 ;;
    -h|--help)
      sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "[run] 未知参数：$arg（支持 --rebuild / --build-only / --help）" >&2
      exit 2
      ;;
  esac
done

export PATH="${HOME}/.cargo/bin:${HOME}/.local/node/bin:/usr/local/bin:/opt/homebrew/bin:${PATH:-}"

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "[run] 找不到命令：$1" >&2
    echo "[run] 请先安装，并保证在 PATH 里（cargo / node / npm / swiftc）。" >&2
    exit 1
  fi
}

need_desktop_build() {
  [[ "$FORCE_REBUILD" -eq 1 ]] && return 0
  [[ ! -x "$EXE" ]] && return 0
  [[ ! -f "$DIST_INDEX" ]] && return 0
  # 前端源或配置比二进制新 → 重编，避免白屏/旧界面
  local newer
  newer="$(find apps/desktop/src apps/desktop/src-tauri/tauri.conf.json apps/desktop/package.json \
    apps/desktop/vite.config.ts apps/desktop/index.html \
    -type f -newer "$EXE" 2>/dev/null | head -1 || true)"
  [[ -n "$newer" ]] && return 0
  # dist 比二进制新（只跑过 npm run build）也要链进 exe
  [[ "$DIST_INDEX" -nt "$EXE" ]] && return 0
  return 1
}

need_ocr_build() {
  [[ "$FORCE_REBUILD" -eq 1 ]] && return 0
  [[ ! -x "$OCR" ]] && return 0
  [[ tools/macosocr/macosocr.swift -nt "$OCR" ]] && return 0
  return 1
}

build_ocr() {
  need_cmd swiftc
  echo "[run] 编译 macosocr …"
  mkdir -p target/debug
  (
    cd tools/macosocr
    swiftc -O -framework Vision -framework CoreGraphics \
      -o ../../target/debug/macosocr macosocr.swift
  )
  echo "[run] macosocr → $OCR"
}

build_desktop() {
  need_cmd node
  need_cmd npm
  need_cmd cargo
  echo "[run] 构建前端（apps/desktop）…"
  (cd apps/desktop && npm install --no-fund --no-audit && npm run build)
  echo "[run] 构建 desktop（features=custom-protocol，嵌前端资源）…"
  # 正在运行则先停掉，否则 macOS 可能无法覆盖二进制
  if pgrep -x desktop >/dev/null 2>&1; then
    echo "[run] 检测到 desktop 正在运行，先结束以便覆盖二进制…"
    pkill -x desktop || true
    sleep 1
  fi
  CARGO_INCREMENTAL=0 cargo build -p desktop --features custom-protocol
  echo "[run] desktop → $EXE"
}

echo "[run] 工作目录： $ROOT"
echo "[run] 数据目录： $ROOT/data"

if need_ocr_build; then
  build_ocr
else
  echo "[run] macosocr 已就绪"
fi

if need_desktop_build; then
  build_desktop
else
  echo "[run] desktop 已就绪（含嵌好的前端）"
fi

if [[ ! -x "$EXE" || ! -x "$OCR" ]]; then
  echo "[run] 构建后仍缺少可执行文件：" >&2
  [[ -x "$EXE" ]] || echo "[run]   - $EXE" >&2
  [[ -x "$OCR" ]] || echo "[run]   - $OCR" >&2
  exit 1
fi

if [[ "$BUILD_ONLY" -eq 1 ]]; then
  echo "[run] --build-only：构建完成，不启动。"
  exit 0
fi

# 上一次的启动日志先删掉：它只描述「这一次」启动。
rm -f "$LOG"

echo "[run] 界面    ： $EXE"
echo "[run] OCR 程序： $OCR"
echo

if ! open -n "$EXE" 2>/dev/null; then
  "$EXE" >/dev/null 2>&1 &
fi

# 启动期失败常发生在窗口出现前；等几秒确认进程还在。
sleep 4

if pgrep -x desktop >/dev/null 2>&1 || pgrep -f "${ROOT}/target/debug/desktop" >/dev/null 2>&1; then
  echo "[run] 已启动。关掉本终端不影响它。"
  exit 0
fi

echo "[run] ============================================================"
echo "[run] 启动失败：desktop 没起来，或者起来之后立刻退出了。"
echo "[run] ============================================================"
echo

if [[ -f "$LOG" ]]; then
  echo "[run] 它自己的启动日志：$LOG"
  echo "[run] ------------------------------------------------------------"
  cat "$LOG"
  echo "[run] ------------------------------------------------------------"
  echo
  echo "[run] 日志里最后一条成功记录，就是它走到的地方；再往后那条就是失败原因。"
else
  echo "[run] 连 $LOG 都没生成。"
  echo "[run] 说明它在写第一行日志之前就没了 —— 多半是被系统拦下（检查屏幕录制/辅助功能），"
  echo "[run] 或二进制本身有问题。可先手动跑：$EXE"
fi

echo
echo "[run] 把上面这些内容发给开发者即可定位。"
if [[ -t 0 ]]; then
  read -r -p "[run] 按回车退出…" _
fi
exit 1
