#!/usr/bin/env python3
"""UI 翻訳表 (assets/i18n/*.json) の抽出・検査ツール。

使い方:
    python3 scripts/i18n_tool.py stats                     # ファイル別の未訳件数
    python3 scripts/i18n_tool.py todo src/ui_dialogs       # 指定範囲の未訳を JSON で出す
    python3 scripts/i18n_tool.py merge new.json            # 訳を翻訳表へ取り込む
    python3 scripts/i18n_tool.py check                     # 翻訳表の検査 (CI 向け、問題があれば exit 1)

抽出対象は src/ 以下の Rust 文字列リテラルのうち、日本語を含むもの。
`#[cfg(test)]` モジュール、ログ・panic 系の呼び出しの引数は除く。
`format!` / `write!` 系の書式文字列は `{0}` `{1}` … の可変部分を持つパターンとして扱う。
詳細は docs/i18n.md。
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "src"
CATALOG = ROOT / "assets" / "i18n" / "zh-Hans.json"

JA_RE = re.compile(r"[぀-ヿ㐀-䶿一-鿿ｦ-ﾟ]")
# 可変部分のない表示用ではない文字列 (ログ・panic など) の直前に現れる呼び出し。
EXCLUDED_CALL_RE = re.compile(
    r"(?:\blog\s*\(|\blog!\s*\(|\b(?:trace|debug|info|warn|error)!\s*\(|"
    r"\be?println!\s*\(|\be?print!\s*\(|\bpanic!\s*\(|\bunreachable!\s*\(|"
    r"\btodo!\s*\(|\bunimplemented!\s*\(|\bassert(?:_eq|_ne)?!\s*\(|"
    r"\bdebug_assert(?:_eq|_ne)?!\s*\(|\.expect\s*\(|\bperf_log\w*\s*\(|"
    r"\blog_\w+\s*\(|\bemit_startup\s*\()\s*(?:format!\s*\(\s*)?$"
)
FORMAT_CALL_RE = re.compile(r"\b(?:format|format_args)!\s*\(\s*$")
WRITE_CALL_RE = re.compile(r"\b(?:write|writeln)!\s*\(\s*[^,()]+,\s*$")
PLACEHOLDER_RE = re.compile(r"\{\{|\}\}|\{[^{}]*\}")


@dataclass
class Literal:
    value: str
    path: Path
    line: int
    is_format: bool


@dataclass
class Entry:
    key: str
    kind: str  # "strings" | "patterns"
    locations: list[str] = field(default_factory=list)


def rust_unescape(body: str) -> str:
    out = []
    i = 0
    n = len(body)
    while i < n:
        ch = body[i]
        if ch != "\\":
            out.append(ch)
            i += 1
            continue
        i += 1
        if i >= n:
            break
        esc = body[i]
        if esc == "\n" or (esc == "\r" and body[i + 1 : i + 2] == "\n"):
            # 行継続: 改行と次行の先頭空白を捨てる。
            i += 1 if esc == "\n" else 2
            while i < n and body[i] in " \t\r\n":
                i += 1
            continue
        simple = {"n": "\n", "t": "\t", "r": "\r", "0": "\0", "\\": "\\", '"': '"', "'": "'"}
        if esc in simple:
            out.append(simple[esc])
            i += 1
        elif esc == "x":
            out.append(chr(int(body[i + 1 : i + 3], 16)))
            i += 3
        elif esc == "u":
            end = body.index("}", i)
            out.append(chr(int(body[i + 2 : end], 16)))
            i = end + 1
        else:
            out.append(esc)
            i += 1
    return "".join(out)


def scan_rust(path: Path) -> list[Literal]:
    """Rust ソースから文字列リテラルを取り出す (コメント・char・テストモジュールを除く)。"""
    text = path.read_text(encoding="utf-8")
    n = len(text)
    i = 0
    line = 1
    literals: list[Literal] = []
    depth = 0
    skip_until_depth: int | None = None
    pending_test_attr = False

    def count_lines(a: int, b: int) -> int:
        return text.count("\n", a, b)

    while i < n:
        ch = text[i]
        # コメント
        if text.startswith("//", i):
            end = text.find("\n", i)
            end = n if end < 0 else end
            i = end
            continue
        if text.startswith("/*", i):
            nest = 1
            j = i + 2
            while j < n and nest:
                if text.startswith("/*", j):
                    nest += 1
                    j += 2
                elif text.startswith("*/", j):
                    nest -= 1
                    j += 2
                else:
                    j += 1
            line += count_lines(i, j)
            i = j
            continue
        # 属性 #[cfg(test)]
        if text.startswith("#[cfg(test)]", i):
            pending_test_attr = True
            i += len("#[cfg(test)]")
            continue
        # 生文字列 r"..." / r#"..."# / br / cr
        m = re.compile(r'(?:b|c)?r(#*)"').match(text, i)
        if m and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")):
            hashes = m.group(1)
            start = m.end()
            end = text.index('"' + hashes, start)
            value = text[start:end]
            is_bytes = text[i] in "bc"
            if skip_until_depth is None and not is_bytes:
                literals.append(Literal(value, path, line, _is_format_context(text, i)))
            line += count_lines(i, end)
            i = end + 1 + len(hashes)
            continue
        # 通常の文字列 "..." / b"..." / c"..."
        if ch == '"' or (
            ch in "bc"
            and text.startswith('"', i + 1)
            and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_"))
        ):
            is_bytes = ch != '"'
            start = i + (2 if is_bytes else 1)
            j = start
            while True:
                if text[j] == "\\":
                    j += 2
                    continue
                if text[j] == '"':
                    break
                j += 1
            if skip_until_depth is None and not is_bytes:
                literals.append(
                    Literal(rust_unescape(text[start:j]), path, line, _is_format_context(text, i))
                )
            line += count_lines(i, j)
            i = j + 1
            continue
        # char リテラルと lifetime
        if ch == "'":
            m = re.compile(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'\n])'").match(text, i)
            if m:
                i = m.end()
                continue
            i += 1
            continue
        if ch == "\n":
            line += 1
        elif ch == "{":
            depth += 1
            if pending_test_attr and skip_until_depth is None:
                head = text[max(0, i - 200) : i]
                if re.search(r"\bmod\s+\w+\s*$", head):
                    skip_until_depth = depth
            pending_test_attr = False
        elif ch == "}":
            if skip_until_depth is not None and depth == skip_until_depth:
                skip_until_depth = None
            depth -= 1
        elif ch == ";":
            pending_test_attr = False
        i += 1
    return literals


def _is_format_context(text: str, pos: int) -> str | bool:
    before = text[max(0, pos - 160) : pos]
    if EXCLUDED_CALL_RE.search(before):
        return "excluded"
    return bool(FORMAT_CALL_RE.search(before) or WRITE_CALL_RE.search(before))


def format_to_pattern(fmt: str) -> str | None:
    """Rust の書式文字列を `{0}` `{1}` … のパターンへ変換する。可変部分が無ければ None。"""
    index = 0

    def repl(m: re.Match[str]) -> str:
        nonlocal index
        token = m.group(0)
        if token in ("{{", "}}"):
            return token
        hole = "{" + str(index) + "}"
        index += 1
        return hole

    pattern = PLACEHOLDER_RE.sub(repl, fmt)
    return pattern if index else None


# ファイル名・パス・時刻・識別子をつなぐ書式は、利用者の文字列を訳してしまうため除く。
PATH_LIKE_RE = re.compile(r"\{\d+\}[._\\/:-]\{\d+\}|\{\d+\}[.\\/:]$|^[.#\\]|::|\{\d+\}\\|\\\{\d+\}|\n")


def is_generic_pattern(pattern: str) -> bool:
    literal = pattern_to_plain(PLACEHOLDER_RE.sub("", pattern))
    return (
        bool(literal.strip())
        and not any(ch.isalnum() for ch in literal)
        and not re.search(r"\{\d+\}\{\d+\}", pattern)
        and not PATH_LIKE_RE.search(pattern)
        and not literal.strip() in ("'", '"', '""', "''")
    )


def pattern_to_plain(pattern: str) -> str:
    return pattern.replace("{{", "{").replace("}}", "}")


def collect(paths: list[Path]) -> dict[tuple[str, str], Entry]:
    entries: dict[tuple[str, str], Entry] = {}
    for base in paths:
        files = [base] if base.is_file() else sorted(base.rglob("*.rs"))
        for path in files:
            posix = path.as_posix()
            if (
                path.name.endswith("_test.rs")
                or path.name == "tests.rs"
                or "/tests/" in posix
                or "/src/bin/" in posix
            ):
                continue
            for lit in scan_rust(path):
                if lit.is_format == "excluded":
                    continue
                if not JA_RE.search(lit.value):
                    # ラベルをつなぐだけの書式 ("{} - {}" など) は記号だけのパターンにする。
                    pattern = format_to_pattern(lit.value) if lit.is_format else None
                    if pattern is not None and is_generic_pattern(pattern):
                        entry = entries.setdefault(("generic", pattern), Entry(pattern, "generic"))
                        entry.locations.append(f"{path.relative_to(ROOT).as_posix()}:{lit.line}")
                    continue
                if lit.is_format:
                    pattern = format_to_pattern(lit.value)
                    if pattern is None:
                        key, kind = pattern_to_plain(lit.value), "strings"
                    else:
                        key, kind = pattern, "patterns"
                else:
                    key, kind = lit.value, "strings"
                entry = entries.setdefault((kind, key), Entry(key, kind))
                entry.locations.append(f"{path.relative_to(ROOT).as_posix()}:{lit.line}")
    return entries


def load_catalog() -> dict:
    if not CATALOG.exists():
        return {"strings": {}, "patterns": {}}
    data = json.loads(CATALOG.read_text(encoding="utf-8"))
    data.setdefault("strings", {})
    data.setdefault("patterns", {})
    return data


def save_catalog(data: dict) -> None:
    ordered = {k: v for k, v in data.items() if k not in ("strings", "patterns")}
    ordered["strings"] = dict(sorted(data["strings"].items()))
    ordered["patterns"] = dict(sorted(data["patterns"].items()))
    CATALOG.parent.mkdir(parents=True, exist_ok=True)
    CATALOG.write_text(json.dumps(ordered, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")


def holes(text: str) -> list[str]:
    return sorted(m.group(0) for m in PLACEHOLDER_RE.finditer(text) if m.group(0) not in ("{{", "}}"))


def pattern_problems(key: str, value: str) -> list[str]:
    problems = []
    key_holes = holes(key)
    if key_holes != sorted(f"{{{i}}}" for i in range(len(key_holes))):
        problems.append("原文の可変部分は {0}, {1}, … を 1 回ずつ使う")
    if not set(holes(value)) <= set(key_holes):
        problems.append("訳文に原文に無い可変部分がある")
    if set(holes(value)) != set(key_holes):
        problems.append("訳文で可変部分が欠けている")
    if re.search(r"\{\d+\}\{\d+\}", key):
        problems.append("可変部分が隣接していて照合できない")
    literal = pattern_to_plain(PLACEHOLDER_RE.sub("", key))
    if not key_holes or not literal.strip():
        problems.append("可変部分と、空白以外の固定部分が必要")
    return problems


def cmd_stats(args: argparse.Namespace) -> int:
    catalog = load_catalog()
    entries = collect([Path(p) if Path(p).is_absolute() else ROOT / p for p in args.paths or ["src"]])
    per_file: dict[str, list[int]] = defaultdict(lambda: [0, 0])
    entries = {k: e for k, e in entries.items() if e.kind != "generic"}
    for entry in entries.values():
        done = entry.key in catalog[entry.kind]
        for file in {loc.rsplit(":", 1)[0] for loc in entry.locations}:
            per_file[file][0] += 1
            per_file[file][1] += int(done)
    rows = sorted(per_file.items(), key=lambda kv: -(kv[1][0] - kv[1][1]))
    total = sum(v[0] for v in per_file.values())
    done = sum(v[1] for v in per_file.values())
    for file, (count, translated) in rows[: args.limit]:
        print(f"{count - translated:6d} 未訳 / {count:6d}  {file}")
    uniq_total = len(entries)
    uniq_done = sum(1 for e in entries.values() if e.key in catalog[e.kind])
    print(f"--\n異なり語: {uniq_done}/{uniq_total} 訳済み  (ファイル別延べ {done}/{total})")
    return 0


def cmd_todo(args: argparse.Namespace) -> int:
    catalog = load_catalog()
    entries = collect([Path(p) if Path(p).is_absolute() else ROOT / p for p in args.paths])
    todo = {"strings": {}, "patterns": {}}
    for entry in sorted(entries.values(), key=lambda e: e.locations[0]):
        if entry.kind == "generic" or entry.key in catalog[entry.kind]:
            continue
        todo[entry.kind][entry.key] = "" if not args.locations else entry.locations[0]
    out = json.dumps(todo, ensure_ascii=False, indent=1)
    if args.output:
        Path(args.output).write_text(out + "\n", encoding="utf-8")
        print(f"{len(todo['strings'])} strings / {len(todo['patterns'])} patterns -> {args.output}")
    else:
        print(out)
    return 0


def cmd_merge(args: argparse.Namespace) -> int:
    catalog = load_catalog()
    added = 0
    for file in args.files:
        new = json.loads(Path(file).read_text(encoding="utf-8"))
        for kind in ("strings", "patterns"):
            for key, value in new.get(kind, {}).items():
                if not value:
                    continue
                if kind == "patterns" and (problems := pattern_problems(key, value)):
                    print(f"skip pattern {key!r}: {', '.join(problems)}", file=sys.stderr)
                    continue
                if catalog[kind].get(key) != value:
                    added += 1
                catalog[kind][key] = value
    save_catalog(catalog)
    print(f"merged {added} entries -> {CATALOG.relative_to(ROOT)}")
    return 0


def cmd_generic(args: argparse.Namespace) -> int:
    catalog = load_catalog()
    entries = collect([Path(p) if Path(p).is_absolute() else ROOT / p for p in args.paths])
    added = 0
    for entry in entries.values():
        if entry.kind == "generic" and entry.key not in catalog["patterns"]:
            catalog["patterns"][entry.key] = entry.key
            added += 1
    save_catalog(catalog)
    print(f"added {added} symbol-only patterns")
    return 0


def cmd_check(args: argparse.Namespace) -> int:
    catalog = load_catalog()
    errors: list[str] = []
    warnings: list[str] = []
    for key, value in catalog["patterns"].items():
        for problem in pattern_problems(key, value):
            errors.append(f"pattern {key!r}: {problem}")
    for kind in ("strings", "patterns"):
        for key, value in catalog[kind].items():
            if not value.strip():
                errors.append(f"{kind} {key!r}: 訳文が空")
            if key.count("\n") != value.count("\n") and kind == "patterns":
                warnings.append(f"{kind} {key!r}: 改行の数が違う")
    # 訳文が別の原文と一致すると、訳文がもう一度訳されてしまう。
    for key, value in catalog["strings"].items():
        other = catalog["strings"].get(value)
        if other is not None and value != key and other != value:
            errors.append(f"訳文 {value!r} (原文 {key!r}) が別の原文として登録されている")
    if args.stale:
        entries = collect([SRC])
        known = {("patterns" if e.kind == "generic" else e.kind, e.key) for e in entries.values()}
        for kind in ("strings", "patterns"):
            for key in catalog[kind]:
                if (kind, key) not in known:
                    warnings.append(f"ソースに見当たらない {kind}: {key!r}")
    for message in warnings:
        print("warning:", message)
    for message in errors:
        print("error:", message)
    print(f"{len(catalog['strings'])} strings / {len(catalog['patterns'])} patterns, "
          f"{len(errors)} errors, {len(warnings)} warnings")
    return 1 if errors else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("stats", help="ファイル別の未訳件数")
    p.add_argument("paths", nargs="*")
    p.add_argument("--limit", type=int, default=60)
    p.set_defaults(func=cmd_stats)
    p = sub.add_parser("todo", help="未訳の原文を JSON で出す")
    p.add_argument("paths", nargs="+")
    p.add_argument("-o", "--output")
    p.add_argument("--locations", action="store_true", help="訳文欄に出現位置を入れる")
    p.set_defaults(func=cmd_todo)
    p = sub.add_parser("merge", help="訳を翻訳表へ取り込む")
    p.add_argument("files", nargs="+")
    p.set_defaults(func=cmd_merge)
    p = sub.add_parser("generic", help="記号だけの書式を訳文=原文で翻訳表へ足す")
    p.add_argument("paths", nargs="+")
    p.set_defaults(func=cmd_generic)
    p = sub.add_parser("check", help="翻訳表を検査する")
    p.add_argument("--stale", action="store_true", help="ソースに無い原文も警告する")
    p.set_defaults(func=cmd_check)
    args = parser.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
