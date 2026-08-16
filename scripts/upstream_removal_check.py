#!/usr/bin/env python3
"""
piCoreCDSP v2 — Upstream Removal Candidate Checker (roadmap §66, Gate 14)

Compares current probe results against previous results stored in
upstream/status.json.  When a capability probe flips FAIL→PASS, it emits
the information needed for the upstream-removal-check.yml workflow to open
a GitHub issue.

Usage:
    python3 scripts/upstream_removal_check.py \\
        --status       upstream/status.json \\
        --capabilities upstream/capabilities.yml \\
        --probes       /tmp/probe-results-master.json [additional...] \\
        --output       /tmp/removal-candidates.json

The upstream-removal-check.yml workflow reads removal-candidates.json and
calls `gh issue create` for each entry.

IMPORTANT (roadmap §69): this script never auto-deletes or auto-modifies
piCoreCDSP source code.  It only produces a list of candidates for human review.
"""

import argparse
import json
import sys
from pathlib import Path

import yaml


def load_json(path: str) -> dict:
    p = Path(path)
    return json.loads(p.read_text()) if p.exists() else {}


def load_yaml(path: str) -> dict:
    p = Path(path)
    if not p.exists():
        return {}
    return yaml.safe_load(p.read_text()) or {}


def collect_current_results(probe_files: list[str]) -> dict[str, str]:
    """
    Aggregate probe results across all branches.
    A capability is PASS if it passes on ANY branch.
    """
    aggregated: dict[str, str] = {}
    for pf in probe_files:
        data = load_json(pf)
        for probe in data.get("probes", []):
            key = probe.get("capability", "?")
            status = probe.get("status", "?")
            if status == "PASS":
                aggregated[key] = "PASS"
            elif key not in aggregated:
                aggregated[key] = status
    return aggregated


def get_previous_results(status: dict) -> dict[str, str]:
    """Extract previous probe results from status.json."""
    return status.get("probe_results", {})


def find_flip_to_pass(
    previous: dict[str, str],
    current: dict[str, str],
) -> list[str]:
    """Return capability keys that flipped from FAIL/ERROR/unknown to PASS."""
    flipped = []
    for key, curr_status in current.items():
        if curr_status == "PASS":
            prev_status = previous.get(key, "FAIL")
            if prev_status != "PASS":
                flipped.append(key)
    return flipped


def build_removal_candidate(
    key: str,
    capabilities: dict,
) -> dict:
    """Build a removal-candidate record for a flipped capability."""
    workaround_caps = capabilities.get("capabilities", [])
    upstream_caps = capabilities.get("upstream_capabilities", [])

    local_code = []
    removal_when = "(see upstream/capabilities.yml)"
    description = f"Capability `{key}` probe has flipped FAIL → PASS."

    for cap in workaround_caps:
        if cap.get("key") == key:
            local_code = cap.get("local_code", [])
            removal_when = (cap.get("removal_when") or "").strip()
            description = (cap.get("description") or description).strip()

    for cap in upstream_caps:
        probe_key = cap.get("probe_key") or cap.get("key")
        if probe_key == key:
            local_code = local_code or cap.get("local_code", [])

    return {
        "capability_key": key,
        "description": description,
        "local_code": local_code,
        "removal_when": removal_when,
        "issue_title": f"Removal candidate: `{key}` probe now PASS",
        "issue_labels": ["removal-candidate", "upstream", "capability"],
        "issue_body": _format_issue_body(key, description, local_code, removal_when),
    }


def _format_issue_body(
    key: str,
    description: str,
    local_code: list[str],
    removal_when: str,
) -> str:
    lines = [
        f"## Removal Candidate: `{key}`",
        "",
        "The upstream capability probe for this key has **flipped FAIL → PASS**.",
        "This means upstream may now provide what the local workaround implements.",
        "",
        "### Capability",
        "",
        f"`{key}`",
        "",
        "### Description",
        "",
        description,
        "",
        "### Potentially Removable Local Code",
        "",
    ]
    for lc in local_code:
        lines.append(f"- `{lc}`")
    if not local_code:
        lines.append("_See `upstream/capabilities.yml` for local_code._")

    lines += [
        "",
        "### Removal Criterion",
        "",
        removal_when or "_See upstream/capabilities.yml._",
        "",
        "### Required Validation Before Removal (roadmap §49, §66)",
        "",
        "- [ ] Probe confirmed on `master` AND `next4.2.0`/`next5`.",
        "- [ ] Inactive-state handling verified (rate change while DSP stopped).",
        "- [ ] GUI Apply survival verified (apply without save, then rate change).",
        "- [ ] Config switch survival verified (A→B→rate change).",
        "- [ ] `$samplerate$` token case verified if applicable.",
        "- [ ] Resampler mode verified (devices.capture_samplerate path).",
        "- [ ] Hardware validation completed (Gate 12 hardware gate): "
              "44.1 / 48 / 96 / 192 kHz rate families, multiple producers.",
        "- [ ] Regression test suite green after deletion.",
        "",
        "**DO NOT delete local code until all items above are checked.**",
        "",
        "---",
        "_Opened automatically by `upstream-removal-check.yml` — roadmap §66._",
    ]
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(description="Detect FAIL→PASS capability flips")
    parser.add_argument("--status", default="upstream/status.json")
    parser.add_argument("--capabilities", default="upstream/capabilities.yml")
    parser.add_argument("--probes", nargs="*", default=[])
    parser.add_argument("--output", required=True,
                        help="Output JSON file with removal candidates")
    args = parser.parse_args()

    status = load_json(args.status)
    capabilities = load_yaml(args.capabilities)

    previous = get_previous_results(status)
    current = collect_current_results(args.probes or [])

    flipped = find_flip_to_pass(previous, current)

    candidates = [build_removal_candidate(k, capabilities) for k in flipped]

    Path(args.output).write_text(json.dumps(candidates, indent=2))
    print(f"Removal candidates: {len(candidates)}")
    for c in candidates:
        print(f"  ✅→ {c['capability_key']}: {c['issue_title']}")

    # Update status.json with current results (for next run's comparison).
    status["probe_results"] = current
    if not args.probes:
        print("(no probe files provided — status not updated)")
    else:
        Path(args.status).write_text(json.dumps(status, indent=2))
        print(f"Status updated with current probe results.")

    # Exit 1 if there are candidates (tells the workflow to open issues).
    sys.exit(1 if candidates else 0)


if __name__ == "__main__":
    main()
