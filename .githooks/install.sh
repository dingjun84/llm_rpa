#!/bin/sh
# 安装项目自带的 git hooks。
#
# 为什么用 `.githooks/` 而不是直接写 `.git/hooks/`：
# `.git/` 不入库，写在那里的 hook 团队其他人拿不到、换台机器也要重配一遍。
# 放在 `.githooks/` 里随仓库走，配合 `core.hooksPath` 指过去，
# 每个克隆只需执行一次本脚本。
#
# 用法：
#     sh .githooks/install.sh
#
# 卸载（恢复 git 默认行为）：
#     git config --unset core.hooksPath

set -eu

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# 给 hook 补可执行位（Windows 上 clone 下来常常没有）
chmod +x .githooks/pre-commit 2>/dev/null || true

git config core.hooksPath .githooks

printf '%s\n' "已启用项目 hooks：core.hooksPath = .githooks"
printf '%s\n' "  pre-commit 会跑：规模门禁 / 架构门禁 / dbg!·console.log 残留 / clippy"
printf '%s\n' "  clippy 需要 Rust 工具链，位置由 scripts/rust-env.sh 自动发现"
printf '%s\n' "    （系统 PATH -> 本项目 .toolchain/ -> 同级目录的 .toolchain/）"
printf '%s\n' "  确需跳过：git commit --no-verify"
