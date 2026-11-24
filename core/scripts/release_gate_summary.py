#!/usr/bin/env python3
"""Summarises the `release-gate.yml` run: job results, per-section results of each OS and a 3-OS
performance comparison, written as Markdown for `$GITHUB_STEP_SUMMARY`.

Pure Python 3 stdlib. Exit code 1 when any job did not succeed or any report failed.

  release_gate_summary.py --reports DIR --job build-and-assemble=success --job gate-linux=failure ...
  release_gate_summary.py --self-test

`DIR` holds one sub-directory per OS (any name) with a `release-bundle-report.json` inside, as
produced by `verify_release_bundle.py` and uploaded by the gate jobs.
"""

from __future__ import annotations

import argparse
import json
import sys
import tempfile
from pathlib import Path
from typing import Sequence

PERF_COLUMNS = (
    ("open_ms", "open (ms)"),
    ("filmstrip_ms", "filmstrip (ms)"),
    ("scrub_ms", "scrub (ms)"),
    ("seek_ms", "seek (ms)"),
    ("trim_ms", "trim (ms)"),
    ("trim_burn_ms", "trim+burn (ms)"),
)


def load_reports(root: Path) -> dict[str, dict]:
    reports: dict[str, dict] = {}
    for path in sorted(root.rglob("release-bundle-report.json")):
        data = json.loads(path.read_text(encoding="utf-8"))
        label = str(data.get("host", {}).get("bundle") or path.parent.name)
        reports[label] = data
    return reports


def render(job_results: dict[str, str], reports: dict[str, dict]) -> tuple[str, bool]:
    ok = all(result == "success" for result in job_results.values()) and all(r.get("pass") for r in reports.values())
    lines = ["# Release gate", "", f"**{'PASS' if ok else 'FAIL'}**", "", "| job | result |", "| --- | --- |"]
    for job, result in job_results.items():
        lines.append(f"| {job} | {'PASS' if result == 'success' else result.upper()} |")
    if reports:
        section_names: list[str] = []
        for report in reports.values():
            for sec in report.get("sections", []):
                if sec["name"] not in section_names:
                    section_names.append(sec["name"])
        lines += ["", "## Sections per OS", "", "| section | " + " | ".join(reports) + " |", "| --- | " + " | ".join("---" for _ in reports) + " |"]
        for name in section_names:
            cells = []
            for report in reports.values():
                sec = next((s for s in report.get("sections", []) if s["name"] == name), None)
                if sec is None:
                    cells.append("-")
                else:
                    failed = sum(1 for c in sec["checks"] if not c["pass"])
                    cells.append(f"{'PASS' if sec['pass'] else 'FAIL'} ({len(sec['checks'])}" + (f", {failed} failed)" if failed else ")"))
            lines.append(f"| {name} | " + " | ".join(cells) + " |")
        gops = sorted({gop for r in reports.values() for gop in r.get("perf", {})})
        if gops:
            lines += ["", "## Performance (FHD30, [47..=647] mid-GOP trim)", ""]
            header = "| OS | GOP | " + " | ".join(label for _, label in PERF_COLUMNS) + " |"
            lines += [header, "| --- | --- | " + " | ".join("---:" for _ in PERF_COLUMNS) + " |"]
            for label, report in reports.items():
                for gop in gops:
                    row = report.get("perf", {}).get(gop, {})
                    lines.append(f"| {label} | {gop} | " + " | ".join(str(row.get(key, "")) for key, _ in PERF_COLUMNS) + " |")
        failures = [(label, s["name"], c["name"], c["detail"]) for label, r in reports.items() for s in r.get("sections", []) for c in s["checks"] if not c["pass"]]
        if failures:
            lines += ["", "## Failed checks", ""]
            for label, section, check, detail in failures[:60]:
                lines.append(f"- `{label}` / `{section}` / {check}: {detail.replace(chr(10), ' ')[:200]}")
    return "\n".join(lines) + "\n", ok


def self_test() -> int:
    good = {"host": {"bundle": "arb-linux-x86_64.zip"}, "pass": True, "perf": {"1s": {"open_ms": 3.0, "seek_ms": 11.0}}, "sections": [{"name": "mcp", "pass": True, "checks": [{"name": "a", "pass": True, "detail": ""}]}]}
    bad = {"host": {"bundle": "arb-windows-x86_64.zip"}, "pass": False, "perf": {}, "sections": [{"name": "mcp", "pass": False, "checks": [{"name": "tools", "pass": False, "detail": "16 tools"}]}]}
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        for name, data in (("linux", good), ("windows", bad)):
            (root / name).mkdir()
            (root / name / "release-bundle-report.json").write_text(json.dumps(data), encoding="utf-8")
        reports = load_reports(root)
        assert set(reports) == {"arb-linux-x86_64.zip", "arb-windows-x86_64.zip"}, reports.keys()
        text, ok = render({"gate-linux": "success", "gate-windows": "success"}, reports)
        assert not ok and "16 tools" in text and "Failed checks" in text
        text, ok = render({"gate-linux": "success"}, {"arb-linux-x86_64.zip": good})
        assert ok and "PASS" in text and "arb-linux-x86_64.zip | 1s | 3.0" in text, text
        text, ok = render({"gate-linux": "success", "gate-windows": "cancelled"}, {"arb-linux-x86_64.zip": good})
        assert not ok and "CANCELLED" in text
    print("release_gate_summary.py --self-test: OK")
    return 0


def main(argv: Sequence[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--reports", type=Path, help="directory holding release-bundle-report.json files")
    ap.add_argument("--job", action="append", default=[], metavar="NAME=RESULT", help="job result (success / failure / cancelled / skipped)")
    ap.add_argument("--out", type=Path, help="append the Markdown here (e.g. $GITHUB_STEP_SUMMARY)")
    args = ap.parse_args(argv)
    if args.self_test:
        return self_test()
    jobs = dict(item.split("=", 1) for item in args.job)
    reports = load_reports(args.reports) if args.reports and args.reports.is_dir() else {}
    text, ok = render(jobs, reports)
    print(text)
    if args.out:
        with args.out.open("a", encoding="utf-8") as fh:
            fh.write(text)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
