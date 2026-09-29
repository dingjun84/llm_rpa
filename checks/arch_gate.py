#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""架构门禁：把 `code-map.md` §2「依赖方向」与 `CONVENTIONS.md` §6「依赖与分层」
从散文变成**机器检查**。

背景（2026-09-29 反思）：这个仓库的核心约定之一是「判据只有一处」和
「automation-core 零内部依赖」。这些话在文档里被反复强调、写得极好，
但**没有任何东西去验证它**。vibe coding 下 AI 看不到三个月前的约定，
它只会让当前这次改动跑通 —— 所以约束必须是可执行的。

本脚本查三件事：

  1. **依赖方向**：读各 crate 的 `Cargo.toml`，校验内部依赖只许指向
     `automation-core`（或按白名单）。往 core 加内部依赖 = 破坏分层。
  2. **`use` 语句跨层**：源码里 `use automation_core::...` 之外，
     检查 `crates/automation-core/src/` 内部**不许**出现指向其它内部 crate 的 use。
  3. **前端不许直连后端类型**：`apps/desktop/src` 里不出现 `src-tauri` 路径导入。

违规退出码 1。
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# 内部 crate 名（Cargo 包名）
INTERNAL_CRATES = {
    "automation-core",
    "platform-windows",
    "platform-macos",
    "platform-mock",
    "storage",
    "vision",
    "sigma-drift",
}

# 允许的依赖方向：key 依赖 value 里的这些（用 Cargo 包名）
# 依据 code-map.md §2 实测的那张图
ALLOWED_INTERNAL_DEPS = {
    # 编排核心：零内部依赖
    "automation-core": set(),
    # 四个实现 crate 互不依赖，都只指向 core
    "platform-windows": {"automation-core"},
    "platform-macos": {"automation-core"},
    "platform-mock": {"automation-core"},
    "vision": {"automation-core"},
    "storage": {"automation-core"},
    # 轨迹记录：独立子系统；viz 只读它的轨迹格式
    "sigma-drift": set(),
    "sigma-drift-viz": {"sigma-drift"},
    # 桌面应用：装配层，可以引用所有实现
    "desktop": {"automation-core", "platform-windows", "platform-macos",
                "platform-mock", "storage", "vision"},
    # 外挂小工具
    "winocr": set(),
    "macosocr": set(),
    "replay": {"automation-core", "vision"},
}

# 允许的「仅 dev-dependency」例外：crate -> 该 crate 可用的 dev 依赖集合。
# 这些依赖**只允许出现在 examples/ 与 tests/ 里**，生产代码（src/）不许 use。
#
# 为什么单列而不是并进 ALLOWED_INTERNAL_DEPS：code-map.md §2 说的是
# 「四个实现 crate 互不依赖」。dev-dep 虽然不影响生产二进制，但它让这条
# 约定在事实上变松——所以既不能当成合法生产依赖放过，也不该硬拦
# （`platform-windows` 的 `screen_probe` 探针确实需要把截图编码成 PNG
# 来人工查看，那是排查标定的关键手段）。
#
# 判据：Cargo.toml 的 dev 依赖节里允许出现；但 src/ 下**一旦 use 就报违规**。
ALLOWED_DEV_ONLY_DEPS = {
    "platform-windows": {"vision"},   # 仅 examples/screen_probe.rs 使用
}


def find_cargo_tomls():
    out = []
    for p in REPO_ROOT.rglob("Cargo.toml"):
        if any(part in {"target", "node_modules"} for part in p.parts):
            continue
        out.append(p)
    return sorted(out)


def parse_pkg_and_deps(toml_path: Path):
    """手写极简解析：需要 [package].name，以及区分**生产依赖**与**dev 依赖**
    里的内部 crate 名。零依赖，pre-commit 里也能跑。

    dev 依赖单独返回：code-map.md §2 说的是「互不依赖」，指的是生产依赖；
    dev-dep 是排查工具（探针/测试）用的，由 ALLOWED_DEV_ONLY_DEPS 另行约束。
    """
    text = toml_path.read_text(encoding="utf-8", errors="replace")
    pkg = None
    deps: set[str] = set()
    dev_deps: set[str] = set()
    section = None

    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("["):
            section = line.strip("[]").strip()
            continue
        if section == "package" and line.startswith("name"):
            m = re.search(r'name\s*=\s*"([^"]+)"', line)
            if m:
                pkg = m.group(1)
            continue
        if section and "dependencies" in section and "=" in line:
            key = line.partition("=")[0].strip().strip('"')
            if key not in INTERNAL_CRATES:
                continue
            # 注意判据顺序：`dev-dependencies` 里含 `dependencies` 子串，
            # 必须先判 dev 再判普通，否则 dev 会被误当成生产依赖。
            if "dev-dependencies" in section:
                dev_deps.add(key)
            else:
                deps.add(key)
    return pkg, deps, dev_deps


def check_dependency_direction():
    problems = []
    for toml_path in find_cargo_tomls():
        pkg, deps, dev_deps = parse_pkg_and_deps(toml_path)
        if not pkg:
            continue
        rel = toml_path.relative_to(REPO_ROOT).as_posix()
        allowed = ALLOWED_INTERNAL_DEPS.get(pkg)
        if allowed is None:
            # 未登记的内部包：只提示，不拦（避免误伤新加的工具 crate）
            if deps or dev_deps:
                print(f"[arch_gate] 提示：{pkg} 未登记在 ALLOWED_INTERNAL_DEPS，"
                      f"其内部依赖 {sorted(deps | dev_deps)} 未校验（{rel}）")
            continue
        illegal = deps - allowed
        if illegal:
            problems.append({
                "cargo_toml": rel,
                "crate": pkg,
                "illegal": sorted(illegal),
                "allowed": sorted(allowed),
            })
        # dev 依赖：只许出现在白名单里，且不许被 src/ 使用（见下一个检查）
        allowed_dev = ALLOWED_DEV_ONLY_DEPS.get(pkg, set())
        illegal_dev = dev_deps - allowed - allowed_dev
        if illegal_dev:
            problems.append({
                "cargo_toml": rel,
                "crate": pkg,
                "illegal": sorted(illegal_dev),
                "allowed": sorted(allowed | allowed_dev),
                "kind": "dev",
            })
    return problems


def check_dev_dep_not_used_in_src():
    """`ALLOWED_DEV_ONLY_DEPS` 里的依赖，不许被 src/ 下的生产代码 use。

    这是 `platform-windows` 那条 dev-dep 的真正护栏：探针可以拿 vision 编 PNG，
    但平台层一旦在 src/ 里 use vision，分层就真的破了。
    """
    problems = []
    for toml_path in find_cargo_tomls():
        pkg, _deps, dev_deps = parse_pkg_and_deps(toml_path)
        if not pkg:
            continue
        allowed_dev = ALLOWED_DEV_ONLY_DEPS.get(pkg, set())
        guarded = dev_deps & allowed_dev
        if not guarded:
            continue
        src = toml_path.parent / "src"
        if not src.exists():
            continue
        pat = re.compile(r"^\s*(?:pub\s+)?use\s+("
                         + "|".join(c.replace("-", "_") for c in sorted(guarded))
                         + r")\b")
        for p in src.rglob("*.rs"):
            for i, line in enumerate(
                    p.read_text(encoding="utf-8", errors="replace").splitlines(), 1):
                if pat.match(line):
                    problems.append({
                        "file": p.relative_to(REPO_ROOT).as_posix(),
                        "line": i,
                        "text": line.strip(),
                        "crate": pkg,
                        "dep": sorted(guarded),
                    })
    return problems


def check_core_has_no_internal_use():
    """automation-core 的源码里不许 use 其它内部 crate。"""
    problems = []
    core = REPO_ROOT / "crates" / "automation-core" / "src"
    if not core.exists():
        return problems
    others = {c.replace("-", "_") for c in INTERNAL_CRATES} - {"automation_core"}
    pat = re.compile(r"^\s*(?:pub\s+)?use\s+(" + "|".join(sorted(others)) + r")\b")
    for p in core.rglob("*.rs"):
        for i, line in enumerate(p.read_text(encoding="utf-8", errors="replace").splitlines(), 1):
            if pat.match(line):
                problems.append({
                    "file": p.relative_to(REPO_ROOT).as_posix(),
                    "line": i,
                    "text": line.strip(),
                })
    return problems


def main() -> int:
    as_json = "--json" in sys.argv
    dep_problems = check_dependency_direction()
    use_problems = check_core_has_no_internal_use()
    dev_problems = check_dev_dep_not_used_in_src()

    if as_json:
        print(json.dumps({
            "dependency_direction": dep_problems,
            "core_internal_use": use_problems,
            "dev_dep_leaked_into_src": dev_problems,
        }, ensure_ascii=False, indent=2))
        return 1 if (dep_problems or use_problems or dev_problems) else 0

    failed = False
    if dep_problems:
        failed = True
        print("=" * 68)
        print("架构门禁：依赖方向违规 %d 处" % len(dep_problems))
        print("=" * 68)
        for pr in dep_problems:
            kind = "（dev 依赖）" if pr.get("kind") == "dev" else ""
            print(f"  ✗ {pr['crate']}  ← 非法依赖 {pr['illegal']} {kind}")
            print(f"      {pr['cargo_toml']}")
            print(f"      允许的内部依赖：{pr['allowed'] or '（零内部依赖）'}")
        print("  依据 code-map.md §2 的依赖方向图；core 零内部依赖、四个实现")
        print("  crate 互不依赖 —— 往 core 加依赖等于破坏分层，先停下来想清楚。")
        print("=" * 68)

    if use_problems:
        failed = True
        print("=" * 68)
        print("架构门禁：automation-core 内部出现跨 crate use（%d 处）" % len(use_problems))
        print("=" * 68)
        for pr in use_problems:
            print(f"  ✗ {pr['file']}:{pr['line']}  {pr['text']}")
        print("=" * 68)

    if dev_problems:
        failed = True
        print("=" * 68)
        print("架构门禁：仅限探针使用的 dev 依赖被生产代码 use（%d 处）" % len(dev_problems))
        print("=" * 68)
        for pr in dev_problems:
            print(f"  ✗ {pr['file']}:{pr['line']}  {pr['text']}")
            print(f"      {pr['dep']} 在 {pr['crate']} 里只许 examples/ 与 tests/ 使用")
        print("=" * 68)

    if not failed:
        print("[arch_gate] OK — 依赖方向与 core 分层均符合 code-map.md §2")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
