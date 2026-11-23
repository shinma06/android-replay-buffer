#!/usr/bin/env python3
"""Verify all Cargo workspace and dependency licenses are strictly GPL/AGPL-free.

Uses `cargo metadata --format-version 1` and evaluates SPDX license expressions:
- Fails with exit code 1 if any crate requires a GPL/AGPL/LGPL license or lacks
  an approved permissive satisfaction path.
- If a crate offers a permissive OR-clause alongside a copyleft clause
  (e.g., `MIT OR Apache-2.0 OR LGPL-2.1-or-later`), logs the elected permissive
  clause clearly and passes.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

# Approved permissive SPDX license IDs (and standard permissive WITH exceptions).
APPROVED_PERMISSIVE = frozenset(
    {
        "MIT",
        "Apache-2.0",
        "Apache-2.0 WITH LLVM-exception",
        "BSD-2-Clause",
        "BSD-3-Clause",
        "ISC",
        "Zlib",
        "CC0-1.0",
        "Unlicense",
        "0BSD",
        "BSL-1.0",
        "Unicode-3.0",
        "Unicode-DFS-2016",
    }
)

COPYLEFT_RE = re.compile(r"(^|[-.])(A?L?GPL)($|[-.])", re.IGNORECASE)
TOKEN_RE = re.compile(r"\(|\)|\bAND\b|\bOR\b|\bWITH\b|/|[A-Za-z0-9.+_-]+")


class SpdxParseError(ValueError):
    """Raised when an SPDX license expression cannot be parsed."""


def tokenize(expr: str) -> list[str]:
    tokens = TOKEN_RE.findall(expr)
    if not tokens:
        raise SpdxParseError(f"empty license expression: {expr!r}")
    return tokens


class Parser:
    """Recursive-descent parser for SPDX license expressions (with legacy `/` = OR)."""

    def __init__(self, tokens: list[str]) -> None:
        self.tokens = tokens
        self.pos = 0

    def peek(self) -> str | None:
        return self.tokens[self.pos] if self.pos < len(self.tokens) else None

    def consume(self, expected: str | None = None) -> str:
        tok = self.peek()
        if tok is None:
            raise SpdxParseError("unexpected end of SPDX expression")
        if expected is not None and tok != expected:
            raise SpdxParseError(f"expected {expected!r}, got {tok!r}")
        self.pos += 1
        return tok

    def parse(self) -> tuple:
        node = self.parse_or()
        if self.peek() is not None:
            raise SpdxParseError(f"unexpected trailing token: {self.peek()!r}")
        return node

    def parse_or(self) -> tuple:
        items = [self.parse_and()]
        while self.peek() in ("OR", "/"):
            self.consume()
            items.append(self.parse_and())
        return items[0] if len(items) == 1 else ("OR", items)

    def parse_and(self) -> tuple:
        items = [self.parse_atom()]
        while self.peek() == "AND":
            self.consume()
            items.append(self.parse_atom())
        return items[0] if len(items) == 1 else ("AND", items)

    def parse_atom(self) -> tuple:
        tok = self.peek()
        if tok == "(":
            self.consume("(")
            node = self.parse_or()
            self.consume(")")
            return node
        if tok in (None, ")", "AND", "OR", "/", "WITH"):
            raise SpdxParseError(f"unexpected token {tok!r}")
        ident = self.consume()
        if self.peek() == "WITH":
            self.consume("WITH")
            exc = self.consume()
            return ("LIC", f"{ident} WITH {exc}")
        return ("LIC", ident)


def collect_licenses(node: tuple) -> set[str]:
    kind = node[0]
    if kind == "LIC":
        return {node[1]}
    result: set[str] = set()
    for child in node[1]:
        result.update(collect_licenses(child))
    return result


def find_permissive_election(node: tuple) -> list[str] | None:
    """Return a list of approved permissive licenses satisfying `node`, or None."""
    kind = node[0]
    if kind == "LIC":
        lic = node[1]
        return [lic] if lic in APPROVED_PERMISSIVE else None
    if kind == "OR":
        for child in node[1]:
            elected = find_permissive_election(child)
            if elected is not None:
                return elected
        return None
    if kind == "AND":
        elected_all: list[str] = []
        for child in node[1]:
            elected = find_permissive_election(child)
            if elected is None:
                return None
            elected_all.extend(elected)
        return elected_all
    raise SpdxParseError(f"unknown AST node: {kind!r}")


def evaluate_expression(expr: str) -> tuple[bool, list[str], bool]:
    """Return (ok, elected_licenses, contains_copyleft)."""
    ast = Parser(tokenize(expr)).parse()
    all_lics = collect_licenses(ast)
    contains_copyleft = any(COPYLEFT_RE.search(lic) for lic in all_lics)
    elected = find_permissive_election(ast)
    if elected is None:
        return (False, [], contains_copyleft)
    return (True, sorted(set(elected)), contains_copyleft)


def run_self_test() -> int:
    cases = [
        ("MIT OR Apache-2.0", True, False),
        ("BSD-2-Clause", True, False),
        ("MIT OR Apache-2.0 OR LGPL-2.1-or-later", True, True),
        ("Unlicense/MIT", True, False),
        ("Apache-2.0 WITH LLVM-exception", True, False),
        ("GPL-3.0-only", False, True),
        ("AGPL-3.0-or-later", False, True),
        ("MIT AND GPL-2.0-or-later", False, True),
        ("(MIT OR Apache-2.0) AND GPL-3.0-only", False, True),
        ("LGPL-2.1-only", False, True),
    ]
    for expr, expect_ok, expect_copyleft in cases:
        ok, _, has_copyleft = evaluate_expression(expr)
        if ok != expect_ok or has_copyleft != expect_copyleft:
            print(
                f"SELF-TEST FAIL for {expr!r}: got (ok={ok}, copyleft={has_copyleft}), "
                f"expected ({expect_ok}, {expect_copyleft})",
                file=sys.stderr,
            )
            return 1
    print(f"check-licenses self-test: {len(cases)}/{len(cases)} cases passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Check Cargo crate licenses.")
    parser.add_argument(
        "--manifest-path",
        type=Path,
        default=Path(__file__).resolve().parent.parent / "Cargo.toml",
        help="Path to workspace Cargo.toml",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run SPDX parser self-tests and exit",
    )
    args = parser.parse_args()

    if args.self_test:
        return run_self_test()

    cmd = [
        "cargo",
        "metadata",
        "--format-version",
        "1",
        "--manifest-path",
        str(args.manifest_path),
    ]
    # `cargo metadata` emits UTF-8 JSON; decoding via the OS default codepage
    # (cp1252 on Windows CI) raises UnicodeDecodeError and leaves stdout=None.
    proc = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )
    if proc.returncode != 0:
        print(proc.stderr, file=sys.stderr)
        return proc.returncode

    metadata = json.loads(proc.stdout)
    packages = metadata.get("packages", [])

    failures: list[str] = []
    elected_notes: list[str] = []

    for pkg in sorted(packages, key=lambda p: (p["name"], p["version"])):
        name = pkg["name"]
        version = pkg["version"]
        expr = pkg.get("license")
        license_file = pkg.get("license_file")
        if not expr:
            failures.append(
                f"{name} {version}: missing SPDX `license` field (license_file={license_file!r})"
            )
            continue

        try:
            ok, elected, has_copyleft = evaluate_expression(expr)
        except SpdxParseError as err:
            failures.append(f"{name} {version}: unparseable license {expr!r}: {err}")
            continue

        if not ok:
            reason = "forbidden GPL/AGPL/LGPL license" if has_copyleft else "unapproved license"
            failures.append(f"{name} {version}: {reason}: {expr}")
        elif has_copyleft:
            elected_str = " AND ".join(elected)
            elected_notes.append(
                f"ELECTED permissive clause [{elected_str}] for {name} {version} (raw SPDX: {expr})"
            )

    for note in elected_notes:
        print(f"license-check: {note}")

    if failures:
        print("license-check: FAILED", file=sys.stderr)
        for item in failures:
            print(f"  - {item}", file=sys.stderr)
        return 1

    print(f"license-check: OK ({len(packages)} packages checked, 0 GPL/AGPL violations)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
