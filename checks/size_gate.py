#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""规模门禁：读 baselines.toml，比对当前生产文件行数。

为什么要有它：`CONVENTIONS.md` §2 定了「生产文件 ≤500 行」，§9 记了一张
存量超标基线表。但在 2026-09-29 之前，那张表是**写在 Markdown 里靠人手动
更新**的 —— 同一个文件在不同段落出现了多个互相矛盾的数字（lib.rs 有
1903 / 1793 / 1894 / 2201 四个值）。手抄必然漂移，所以：

  · 基线的**唯一来源**改成本脚本读的 `baselines.toml`
  · 三种结果：`OK`（低于上限）/ `BASELINE`（在基线之内，仍超标）/ `VIOLATION`（超基线）

`VIOLATION` 退出码 1。它会被 pre-commit hook 与 CI 调用。

用法：
    python checks/size_gate.py            # 全量检查
    python checks/size_gate.py --json     # 机器可读输出
    python checks/size_gate.py --update   # 把当前行数写回 baselines.toml
                                          # ⚠️ 只在**确实拆小之后**用它；
                                          # 数字变大时它会把基线上调，等于放水。
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# 只检查这些根目录下的生产文件
SCAN_ROOTS = [
    "crates",
    "apps/desktop/src",
    "apps/desktop/src-tauri/src",
    "tools",
]
SOURCE_EXTS = {".rs", ".ts", ".tsx"}

# 排除：测试与示例不算「生产文件」
EXCLUDE_PATTERNS = [
    re.compile(r"/tests/"),
    re.compile(r"/examples/"),
    re.compile(r"/node_modules/"),
    re.compile(r"tests\.rs$"),
    re.compile(r"_tests\.rs$"),
    re.compile(r"/target/"),
    # 构建脚本 / 生成物
    re.compile(r"\.d\.ts$"),
]


def iter_source_files():
    for root in SCAN_ROOTS:
        base = REPO_ROOT / root
        if not base.exists():
            continue
        for p in base.rglob("*"):
            if not p.is_file():
                continue
            if p.suffix not in SOURCE_EXTS:
                continue
            rel = p.relative_to(REPO_ROOT).as_posix()
            if any(rx.search("/" + rel) for rx in EXCLUDE_PATTERNS):
                continue
            yield rel, p


def count_lines(path: Path) -> int:
    """行数按换行符计（与 `wc -l` 一致）：末行无换行不计。"""
    n = 0
    with path.open("rb") as f:
        for _ in f:
            n += 1
    return n


def load_baselines():
    """读 baselines.toml。

    刻意用手写解析而不是 tomli/tomllib：这个脚本要在 pre-commit 里跑，
    而 hook 环境不保证有第三方包；tomllib 在 3.11+ 才有。
    文件格式是我们自己定的、极简（[limits] / [baselines] 两节 + key = int），
    手写解析足够且零依赖。
    """
    path = REPO_ROOT / "baselines.toml"
    if not path.exists():
        print(f"[size_gate] 找不到 {path}", file=sys.stderr)
        sys.exit(2)

    limits = {"production_max_lines": 500, "test_max_lines": 3000}
    baselines: dict[str, int] = {}
    section = None

    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1].strip()
            continue
        if "=" not in line:
            continue
        key, _, val = line.partition("=")
        key = key.strip().strip('"').strip("'")
        val = val.split("#", 1)[0].strip()
        if section == "limits":
            try:
                limits[key] = int(val)
            except ValueError:
                pass
        elif section == "baselines":
            try:
                baselines[key] = int(val)
            except ValueError:
                pass
    return limits, baselines


def write_baselines(limits, baselines):
    path = REPO_ROOT / "baselines.toml"
    if not path.exists():
        print("[size_gate] 没有 baselines.toml，--update 不会新建它", file=sys.stderr)
        sys.exit(2)

    text = path.read_text(encoding="utf-8")
    # 逐行替换 [baselines] 节内 key 的数字；节内没有的 key 追加到节尾
    out, section = [], None
    seen = set()
    insert_at = None
    for raw in text.splitlines():
        line = raw
        stripped = raw.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            if section == "baselines" and insert_at is None:
                insert_at = len(out)
            section = stripped[1:-1].strip()
        if section == "baselines" and "=" in stripped and not stripped.startswith("#"):
            key = stripped.partition("=")[0].strip().strip('"').strip("'")
            if key in baselines:
                pad = max(1, 60 - len(f'"{key}"'))
                line = f'"{key}"{" " * pad}= {baselines[key]}'
                seen.add(key)
        out.append(line)
    missing = [k for k in baselines if k not in seen]
    if missing and insert_at is not None:
        for k in missing:
            pad = max(1, 60 - len(f'"{k}"'))
            out.insert(insert_at, f'"{k}"{" " * pad}= {baselines[k]}')
    path.write_text("\n".join(out) + "\n", encoding="utf-8")


def main() -> int:
    ap = argparse.ArgumentParser(description="生产文件规模门禁")
    ap.add_argument("--json", action="store_true", help="输出 JSON")
    ap.add_argument("--update", action="store_true", help="把当前行数写回 baselines.toml")
    ap.add_argument("--quiet", action="store_true", help="只报违规")
    args = ap.parse_args()

    limits, baselines = load_baselines()
    ceiling = limits["production_max_lines"]

    ok, on_baseline, violations = [], [], []
    seen_keys = set()

    for rel, path in sorted(iter_source_files()):
        n = count_lines(path)
        if rel in baselines:
            seen_keys.add(rel)
            cap = baselines[rel]
            if n > cap:
                violations.append((rel, n, cap))
            else:
                on_baseline.append((rel, n, cap))
        else:
            if n > ceiling:
                # 不在基线里又超上限 = 新增代码破了规矩，一律算违规
                violations.append((rel, n, ceiling))

    stale = sorted(k for k in baselines if k not in seen_keys)

    if args.update:
        new_base = dict(baselines)
        for rel, n, _ in on_baseline + violations:
            if rel in new_base:
                new_base[rel] = n
        for rel, n, _ in violations:
            if rel not in new_base:
                new_base[rel] = n
        write_baselines(limits, new_base)
        print(f"[size_gate] 已把 {len(new_base)} 条写回 baselines.toml")
        return 0

    if args.json:
        print(json.dumps({
            "ceiling": ceiling,
            "violations": [{"file": f, "lines": n, "cap": c} for f, n, c in violations],
            "on_baseline": [{"file": f, "lines": n, "cap": c} for f, n, c in on_baseline],
            "stale_baseline_entries": stale,
        }, ensure_ascii=False, indent=2))
        return 1 if violations else 0

    if violations:
        print("=" * 68)
        print("规模门禁：拦下 %d 个文件" % len(violations))
        print("=" * 68)
        for f, n, c in violations:
            print(f"  ✗ {f}")
            print(f"      {n} 行，上限 {c} 行（超出 {n - c}）")
        print()
        print("  · 在基线里的文件：只许减不许增，拆小了顺手把 baselines.toml 改小")
        print("  · 不在基线里的新文件：一律受 %d 行上限约束，不许给新代码开口子" % ceiling)
        print("  · 拆分做法见 CONVENTIONS.md §9「先搬出一段自成一类的功能」")
        print("=" * 68)
        return 1

    if not args.quiet:
        print(f"[size_gate] OK — {len(on_baseline)} 个存量超标文件仍在各自基线之内，"
              f"无新增超标文件（生产文件上限 {ceiling} 行）")
        if stale:
            print(f"[size_gate] 提示：baselines.toml 里这 {len(stale)} 条已找不到对应文件，"
                  f"可以删掉：")
            for s in stale:
                print(f"    - {s}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
