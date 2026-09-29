#!/usr/bin/env bash
# 自动发现并激活 Rust 工具链。
#
# 用法（**必须 source**，不能直接执行）：
#     source scripts/rust-env.sh
#
# 为什么需要它：
#   这个项目在多台机器上同步（Windows 台式 / Intel Mac / 其他开发机），
#   而 Rust 工具链的安装位置各不相同：
#     · 有的机器装在系统级（~/.cargo/bin，在 PATH 里）
#     · 有的机器用**项目自带的便携工具链**（`.toolchain/`，不在 PATH 里）
#     · 本机上工具链是**另一个项目**（同级的 `../rpa/.toolchain`）自带的
#
#   如果脚本里写死路径，换台机器就失效 —— 同级 `rpa` 项目的
#   `.cargo/config.toml` + `scripts/refresh_paths.py` 就是被这个问题逼出来的
#   （写死 `C:\Users\...\rpa\.toolchain\mingw64\bin\gcc.exe`，换机器要手动刷）。
#
#   这里改用**动态推导**，一次写好、所有机器通用：
#     1. 系统 PATH 里已经有 cargo        → 直接用，什么都不做
#     2. 本项目自带 `.toolchain/`        → 用它
#     3. 同级目录里有项目带 `.toolchain/` → 用它（本机的实际情况）
#
# 发现结果通过环境变量导出（RUSTUP_HOME / CARGO_HOME / PATH），
# 与 `rpa/scripts/env.sh` 的约定保持一致，两个项目可以共用同一套心智模型。
#
# 诊断（看它找到了哪个）：
#     source scripts/rust-env.sh && rust_env_report

# ---------------------------------------------------------------- 定位本项目根
# 用 BASH_SOURCE 动态推导，**不写死任何绝对路径**。
# 注意：这个文件必须被 source，所以 $0 是本脚本路径，但为稳妥仍用 BASH_SOURCE。
_rust_env_self="${BASH_SOURCE[0]:-$0}"
PROJECT_ROOT="$(cd "$(dirname "$_rust_env_self")/.." && pwd)"

# 目标三元组：本机无 MSVC、无管理员权限，统一用 GNU 目标
RUST_TARGET_TRIPLE="x86_64-pc-windows-gnu"
# macOS / Linux 上目标名不同，按 uname 自动切换
case "$(uname -s 2>/dev/null)" in
  Darwin) RUST_TARGET_TRIPLE="aarch64-apple-darwin" ;;
  Linux)  RUST_TARGET_TRIPLE="x86_64-unknown-linux-gnu" ;;
esac

# ---------------------------------------------------------------- 候选位置
# 依次尝试；第一个「工具链本体目录」存在的就用它。
# 工具链本体目录 = 里面有 cargo（或 cargo.exe）的那个 bin。
_rust_env_found=""

_rust_env_try() {
  # $1 = 描述；$2 = 候选 toolchain 本体 bin 目录；$3 = 候选 RUSTUP_HOME；$4 = 候选 CARGO_HOME
  [ -n "$_rust_env_found" ] && return 0
  [ -d "$2" ] || return 1

  local cargo_exe=""
  for cand in cargo cargo.exe; do
    if [ -x "$2/$cand" ]; then cargo_exe="$2/$cand"; break; fi
  done
  [ -n "$cargo_exe" ] || return 1

  export RUSTUP_HOME="$3"
  export CARGO_HOME="$4"

  # 链接器：仅在存在 MinGW 时导出（macOS/Linux 不需要，用了反而出错）
  local mingw_dir="$3/../mingw64/bin"
  if [ -x "$mingw_dir/gcc.exe" ]; then
    export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="$mingw_dir/gcc.exe"
    _rust_env_path_prefix="$4/bin:$2:$mingw_dir"
  else
    _rust_env_path_prefix="$4/bin:$2"
  fi

  export PATH="$_rust_env_path_prefix:$PATH"
  _rust_env_found="$1"
  return 0
}

# --- 候选 1：系统 PATH 里已有 cargo（完全不需要改动） ---
if command -v cargo >/dev/null 2>&1; then
  _rust_env_found="系统 PATH（$(command -v cargo)）"
fi

# --- 候选 2：本项目自带的便携工具链 ---
if [ -z "$_rust_env_found" ] && [ -d "$PROJECT_ROOT/.toolchain" ]; then
  _rust_env_try "本项目 .toolchain" \
    "$PROJECT_ROOT/.toolchain/rustup/toolchains/stable-$RUST_TARGET_TRIPLE/bin" \
    "$PROJECT_ROOT/.toolchain/rustup" \
    "$PROJECT_ROOT/.toolchain/cargo"
  # 有些布局把 toolchains 直接放在 .toolchain 下（不经 rustup 中转）
  [ -z "$_rust_env_found" ] && _rust_env_try "本项目 .toolchain（扁平布局）" \
    "$PROJECT_ROOT/.toolchain/bin" \
    "$PROJECT_ROOT/.toolchain" \
    "$PROJECT_ROOT/.toolchain"
fi

# --- 候选 3：同级目录中**任意**项目自带的工具链 ---
# 这是本机的实际情况：llm_rpa 自己没有工具链，同级的 rpa 项目有。
# 动态遍历同级目录，**不写死 "rpa" 这个名字** —— 项目改名/换机器也不用改这里。
if [ -z "$_rust_env_found" ]; then
  _rust_env_parent="$(dirname "$PROJECT_ROOT")"
  for _sib in "$_rust_env_parent"/*/; do
    [ -d "$_sib.toolchain" ] || continue
    _rust_env_try "同级 $(basename "$_sib")/.toolchain" \
      "${_sib}.toolchain/rustup/toolchains/stable-$RUST_TARGET_TRIPLE/bin" \
      "${_sib}.toolchain/rustup" \
      "${_sib}.toolchain/cargo"
    [ -n "$_rust_env_found" ] && break
  done
fi

# --- 候选 4：rustup 默认位置（~/.rustup）---
if [ -z "$_rust_env_found" ] && [ -d "$HOME/.rustup" ]; then
  _rust_env_try "用户级 ~/.rustup" \
    "$HOME/.rustup/toolchains/stable-$RUST_TARGET_TRIPLE/bin" \
    "$HOME/.rustup" \
    "$HOME/.cargo"
fi

# ---------------------------------------------------------------- 报告函数
rust_env_report() {
  if [ -z "$_rust_env_found" ]; then
    echo "[rust-env] ✗ 没找到 Rust 工具链"
    return 1
  fi
  echo "[rust-env] 来源     = $_rust_env_found"
  echo "[rust-env] 项目根   = $PROJECT_ROOT"
  echo "[rust-env] RUSTUP_HOME = ${RUSTUP_HOME:-<未设置>}"
  echo "[rust-env] CARGO_HOME  = ${CARGO_HOME:-<未设置>}"
  echo "[rust-env] $(cargo --version 2>/dev/null || echo '! cargo 未找到')"
  echo "[rust-env] $(rustc --version 2>/dev/null || echo '! rustc 未找到')"
}

# ---------------------------------------------------------------- 结果
if [ -z "$_rust_env_found" ]; then
  echo "[rust-env] ! 没找到 Rust 工具链。" >&2
  echo "[rust-env]   cargo / clippy / test 相关检查会被跳过（不会报错，但也就没检查）。" >&2
  echo "[rust-env]   修法二选一：" >&2
  echo "[rust-env]     a) 装一个系统级 Rust（https://rustup.rs）" >&2
  echo "[rust-env]     b) 在本项目或同级目录放一份 .toolchain/" >&2
  # 不清空 PATH —— 调用方可能本来就有能用的工具链（只是不在标准位置）
  return 0 2>/dev/null || exit 1
fi

# 非交互式调用（如 hook）下静默，交互式下报一行
if [ -t 1 ]; then
  echo "[rust-env] 工具链已激活：$_rust_env_found"
fi
