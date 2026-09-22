#!/usr/bin/env bash
# ============================================================
#  Mac 一键：按需构建 + 启动桌面程序（默认 release）
#
#  用法：
#    ./run.sh               # 缺产物或前端更新过则自动构建，再启动
#    ./run.sh --debug       # 改用 debug 构建（默认是 release）
#    ./run.sh --rebuild     # 强制重新构建后再启动
#    ./run.sh --build-only  # 只构建，不启动
#
#  为什么需要这个脚本：
#    1) 数据目录 = 「启动时的当前工作目录」下的 data/（见 data_dir.rs：
#       `working_dir()` 就是进程 cwd，配置本身也在里面，所以它不可能由配置指定）。
#       本脚本统一 cd 到 $DATA_ROOT（默认 $HOME），即 ~/data。
#       ⚠️ 直接从别处跑 target/debug/desktop 时 cwd 常是 target/debug/，
#       配置/标定会被 cargo clean 一起清掉。
#    2) 独立启动必须带 custom-protocol，并把前端 dist 嵌进二进制；
#       否则白窗口。tauri dev 不走这条路（靠 Vite）。
#    3) macosocr 是独立进程，漏编要到第一次 OCR 才炸——这里一并检查/编译。
#       它跟 desktop **同一个 profile**（都在 target/<profile>/ 下），
#       ⚠️ 但它是**按配置里的绝对路径**被拉起来的（`ocr_command`，
#       见 runtime.rs 的 live_ports），所以换了 profile 要同步改那一项，
#       否则任务里跑的还是旧的那份。本脚本启动时会核对并提示。
#    4) 默认 release：模板匹配这类纯计算循环在 debug 下慢 10~30 倍
#       （见 docs/todo.md T31）。⚠️ 换 profile 就是换了另一个二进制，
#       macOS 的屏幕录制 / 辅助功能授权要**各自**给一次，否则画面全黑、点击不生效。
# ============================================================

set -euo pipefail

# 中文提示里的变量一律写 ${VAR}：macOS 自带 bash 3.2，在 C locale 下会把
# 紧跟其后的**全角**字符（`（` `）` `，`）算进变量名，于是 "$PROFILE（"
# 变成去找一个名叫 "PROFILE（" 的变量 —— `set -u` 下直接
# unbound variable 退出（2026-09-22 踩过，报错行还指向 echo 那一行）。

ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"

# 启动时的工作目录。数据目录 = <它>/data，所以这一行决定"配置和任务日志落在哪"。
# 默认 $HOME（本机 = /Users/admin → /Users/admin/data，也就是一直在用的那份）。
# 要换地方：RPA_DATA_ROOT=/some/dir ./run.sh
DATA_ROOT="${RPA_DATA_ROOT:-$HOME}"

# desktop 的构建配置。release 是默认：这一步的开销几乎全是纯计算。
PROFILE="release"
DIST_INDEX="${ROOT}/apps/desktop/dist/index.html"
LOG="${DATA_ROOT}/data/startup.log"
CONFIG="${DATA_ROOT}/data/config.json"

FORCE_REBUILD=0
BUILD_ONLY=0
for arg in "$@"; do
  case "$arg" in
    --release|-r) PROFILE="release" ;;
    --debug|-d) PROFILE="debug" ;;
    --rebuild|-f) FORCE_REBUILD=1 ;;
    --build-only) BUILD_ONLY=1 ;;
    -h|--help)
      sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "[run] 未知参数：${arg}（支持 --release / --debug / --rebuild / --build-only / --help）" >&2
      exit 2
      ;;
  esac
done

# 两份产物都挂在 profile 下面、同名不同目录：desktop 与 macosocr 是
# 「同一套构建配置」的两半，debug / release 各一份，互不覆盖。
# 放在参数解析之后算，是因为解析阶段可能把 PROFILE 改掉。
EXE="${ROOT}/target/${PROFILE}/desktop"
OCR="${ROOT}/target/${PROFILE}/macosocr"

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
  # ⚠️ `crates/` 与工作区 Cargo 文件**必须**在里面：desktop 依赖 automation-core /
  # vision / platform-*，只改核心层而不同重编，`./run.sh` 会一边说"已就绪"
  # 一边把**旧**二进制拉起来 —— 界面看着正常，改的东西却一点没生效。
  # （2026-09-22 踩过：改完 runner/search.rs 后 release 二进制还是 12:17 那份。）
  local newer
  newer="$(find apps/desktop/src apps/desktop/src-tauri/tauri.conf.json apps/desktop/package.json \
    apps/desktop/vite.config.ts apps/desktop/index.html \
    crates Cargo.toml Cargo.lock \
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
  echo "[run] 编译 macosocr（${PROFILE}）…"
  mkdir -p "target/${PROFILE}"
  (
    cd tools/macosocr
    # `-O` 一直都在：它是 Swift 自己的优化开关，与 cargo 的 profile 无关，
    # 所以 debug/release 两份的机器码基本相同。这里分 profile 只是为了
    # 让"两份产物"在目录上对齐，不至于一个在 debug 一个在 release。
    swiftc -O -framework Vision -framework CoreGraphics \
      -o "../../target/${PROFILE}/macosocr" macosocr.swift
  )
  echo "[run] macosocr → $OCR"
}

build_desktop() {
  need_cmd node
  need_cmd npm
  need_cmd cargo
  echo "[run] 构建前端（apps/desktop）…"
  (cd apps/desktop && npm install --no-fund --no-audit && npm run build)
  echo "[run] 构建 desktop（${PROFILE}，features=custom-protocol，嵌前端资源）…"
  # 正在运行则先停掉，否则 macOS 可能无法覆盖二进制
  if pgrep -x desktop >/dev/null 2>&1; then
    echo "[run] 检测到 desktop 正在运行，先结束以便覆盖二进制…"
    pkill -x desktop || true
    sleep 1
  fi
  # 这里写成 if/else 而不是拼数组：macOS 自带 bash 3.2，
  # `set -u` 下空数组的 "${arr[@]}" 会报 unbound variable。
  if [[ "$PROFILE" == "release" ]]; then
    cargo build --release -p desktop --features custom-protocol
  else
    # debug 关掉增量：target 目录被 cargo clean 清过之后，
    # 增量缓存本身的开销比省下的编译时间还大。
    CARGO_INCREMENTAL=0 cargo build -p desktop --features custom-protocol
  fi
  echo "[run] desktop → $EXE"
}

echo "[run] 构建目录： $ROOT"
echo "[run] 构建配置： $PROFILE"
echo "[run] 数据目录： ${DATA_ROOT}/data"

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

# 数据目录 = 启动时的 cwd + /data，所以启动前必须把 cwd 换成 $DATA_ROOT。
# 下面直接跑二进制，它**就**继承这里的 cwd（`open` 虽然也继承，但会多弹一个
# 终端窗口，见启动那一段的说明）。
# 构建阶段不能提前 cd：上面的 find 用的是相对路径，要在仓库根执行。
cd "$DATA_ROOT"

# 上一次的启动日志先删掉：它只描述「这一次」启动。
rm -f "$LOG"

echo "[run] 界面    ： $EXE"
echo "[run] OCR 程序： $OCR"
echo "[run] 工作目录： $(pwd)"

# 把 OCR 编出来，不代表任务会用它：真正用的是**配置里那个路径**
# （`ocr_command`，见 runtime.rs 的 live_ports 与「目标窗口」页的
# 「本地 OCR 程序路径」）。换了 profile 之后两边几乎一定会不一致，
# 那时任务跑的还是旧那份——不报错，只是白换。所以在这里点出来。
# 只提示，**不**替人改配置：那份文件是用户数据，脚本碰它风险更大。
if [[ -f "$CONFIG" ]] && ! grep -qF -- "$OCR" "$CONFIG"; then
  echo
  echo "[run] ⚠️ 配置里的 ocr_command 不是上面这一份："
  echo "[run]    $CONFIG"
  echo "[run]    任务真正拉起的是配置里那个路径。要换成 $PROFILE 这份，"
  echo "[run]    到界面「本地 OCR 程序路径」填：$OCR"
fi
echo

# ⚠️ 不要用 `open -n "$EXE"` 来启动。
#
# 这个二进制是**裸可执行文件**，没有 .app 包；`open` 遇到这种文件会把
# 它交给 LaunchServices 兜底，而兜底的关联程序就是 Terminal.app——
# 于是每次启动都多弹一个终端窗口（还带一个 `-zsh` 中间层），
# 里面跑着同一份程序。2026-09-22 实测确认：desktop 的父进程是 `-zsh`，
# 再上面是 `open -n` 拉起的**新 Terminal.app 实例**（`-n` 连实例都不复用）。
#
# 直接跑就行：cwd 上面已经 cd 到 $DATA_ROOT，与 `open` 继承 cwd 的效果一致。
# nohup + 重定向：关掉本终端后它继续活着，也不会占着这个 tty 往外写。
nohup "$EXE" >/dev/null 2>&1 &

# 启动期失败常发生在窗口出现前；等几秒确认进程还在。
sleep 4

if pgrep -x desktop >/dev/null 2>&1 || pgrep -f "$EXE" >/dev/null 2>&1; then
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
