#!/usr/bin/env python3
"""
piCoreCDSP v2 — Capability Probe Results Reporter
roadmap §42, Gate 10

Reads the JSON probe output produced by probe_camilla_capabilities.py and
formats it as Markdown for the GitHub Actions step summary.

Usage:
    python3 report_probe_results.py /tmp/probe-results.json >> $GITHUB_STEP_SUMMARY
"""

import json
import sys
from pathlib import Path


def main() -> None:
    if len(sys.argv) < 2:
        print("Usage: report_probe_results.py <results.json>")
        sys.exit(1)

    path = Path(sys.argv[1])
    if not path.is_file():
        print(f"_Results file not found: {path}_")
        sys.exit(0)

    data = json.loads(path.read_text())
    branch = data.get("branch", "unknown")
    timestamp = data.get("timestamp", "unknown")
    probes = data.get("probes", [])

    print(f"\n**Branch:** `{branch}`  ")
    print(f"**Probed at:** {timestamp}\n")

    if not probes:
        print("_No probe results recorded._")
        return

    print("| Capability | Status | Note |")
    print("|---|---|---|")
    for p in probes:
        cap = p.get("capability", "?")
        status = p.get("status", "?")
        note = p.get("note", "")
        icon = {"PASS": "✅ PASS", "FAIL": "❌ FAIL", "SKIP": "⏭ SKIP", "ERROR": "⚠️ ERROR"}.get(
            status, status
        )
        print(f"| `{cap}` | {icon} | {note} |")

    passed = sum(1 for p in probes if p.get("status") == "PASS")
    failed = sum(1 for p in probes if p.get("status") == "FAIL")
    other = len(probes) - passed - failed

    print(f"\n**Summary:** {passed} passed · {failed} failed · {other} skipped/error")

    if failed > 0:
        print(
            "\n> ℹ️ FAIL means the capability is not yet available upstream — "
            "this is expected while local workarounds are active.  A FAIL→PASS "
            "transition means the workaround can be reviewed for deletion."
        )


if __name__ == "__main__":
    main()
