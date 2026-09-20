#!/usr/bin/env bash
# ============================================================
#  启动桌面程序，并把「数据目录」固定在仓库根下的 data/
#
#  为什么需要这个脚本：
#    数据目录是「程序运行当前路径」下的 data/。直接双击
#    target/debug/desktop 时，那个"当前路径"往往是 target/debug/，
#    于是配置、图标库、任务日志、证据图全落在构建产物里 ——
#    一次 cargo clean 就全没了（标定要重做一遍）。
#    这个脚本先切到仓库根，再启动，数据就落在 <仓库>/data/。
#
#  想换个地方放数据：把整个仓库拷过去，或者改这里的 ROOT。
#
#  两个可执行文件都要在，缺一个就在这儿拦下来：
#    desktop   —— 界面本体
#    macosocr  —— 本地 OCR 程序（tools/macosocr，用 swiftc 单独编）
#
#  为什么必须在这儿查 macosocr：它是**独立进程**，任务跑到第一次识别时才去
#  启动它。缺了的话界面照常打开、任务也点得动，一直到 SearchingContact
#  才报「No such file or directory」——那时人已经盯着屏幕等半天了。
#
#  ★ 启动之后要**等一会儿再确认它还在**，理由见下面「为什么要确认」那段。
# ============================================================

set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
EXE="${ROOT}/target/debug/desktop"
OCR="${ROOT}/target/debug/macosocr"
LOG="${ROOT}/data/startup.log"

MISSING=""
[[ -x "$EXE" ]] || MISSING+=" desktop"
[[ -x "$OCR" ]] || MISSING+=" macosocr"

if [[ -n "$MISSING" ]]; then
  echo "[run] 缺少：${MISSING}"
  echo
  echo "[run] 两个都要建。命令**分行**跑，别连成一行："
  echo "[run]"
  echo "[run]   # 界面（带 custom-protocol，资源才嵌得进二进制）"
  echo "[run]   CARGO_INCREMENTAL=0 cargo build -p desktop --features custom-protocol"
  echo "[run]"
  echo "[run]   # OCR（独立工具，cargo build -p desktop 不会编它）"
  echo "[run]   mkdir -p target/debug"
  echo "[run]   (cd tools/macosocr && swiftc -O -framework Vision -framework CoreGraphics \\"
  echo "[run]       -o ../../target/debug/macosocr macosocr.swift)"
  echo
  echo "[run] 注意：macosocr 不是 Cargo workspace 的默认产物；"
  echo "[run]       漏编的症状很能骗人——界面能开、任务能点，第一次 OCR 才炸。"
  echo
  echo "[run] 若报「Text file busy / 无法覆盖」，说明程序还开着：先关掉窗口再构建。"
  echo
  read -r -p "[run] 按回车退出…" _
  exit 1
fi

cd "$ROOT"

# 上一次的启动日志先删掉：它只描述「这一次」启动，留着旧的会误导排查。
rm -f "$LOG"

echo "[run] 工作目录： $(pwd)"
echo "[run] 数据目录： $(pwd)/data"
echo "[run] 界面    ： $EXE"
echo "[run] OCR 程序： $OCR"
echo

# 从仓库根启动（cwd 已是 ROOT）。优先用 open，失败则后台直接跑。
if ! open -n "$EXE" 2>/dev/null; then
  "$EXE" >/dev/null 2>&1 &
fi

# ── 为什么要确认 ────────────────────────────────────────────
#  启动期的失败（数据目录建不出来、状态初始化失败、WebView 起不来）
#  全都发生在**窗口出现之前**。那种情况下程序会立刻退出，而这个脚本
#  自己也是立刻结束的 —— 两件事叠在一起，操作者看到的就只有「窗口闪了一下」，
#  没有任何文字可看。所以这里等几秒、确认进程还在，并把它的启动日志打出来。
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
  echo "[run] 说明它在写第一行日志之前就没了 —— 那多半不是本程序自己的问题，"
  echo "[run] 而是进程被系统拦下了（尤其缺「屏幕录制 / 辅助功能」时），或者二进制本身有问题。"
  echo "[run] 可以先手动跑一次：$EXE"
fi

echo
echo "[run] 把上面这些内容发给开发者即可定位。"
read -r -p "[run] 按回车退出…" _
exit 1
