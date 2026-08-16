# piCoreCDSP v2 — Upstream Monitoring: Workflows & Scripts Overview

> **What is this?**
> piCoreCDSP never auto-upgrades upstream dependencies.  Instead a set of
> weekly workflows snapshot upstream sources, probe for new capabilities, and
> open GitHub issues when a local workaround can be deleted.  This document
> explains what runs when, who calls what, and what each script does.

---

## Schedule Overview (Every Monday UTC)

```
Monday
  03:00  upstream-sync.yml          ← snapshot upstream files, open PR
  03:30  upstream-release-watch.yml ← check latest releases (step summary only)
  04:00  upstream-capability-canary.yml ← build CamillaDSP, run probes, upload artifacts
  04:15  upstream-branch-watch.yml  ← check for v5/next* branches, open issues
         └── (triggers when canary completes)
              upstream-removal-check.yml ← detect FAIL→PASS flips, open issues,
                                           regenerate status.md, commit status.json
```

`canary.yml` runs **manual only** (`workflow_dispatch`) — the scheduled probe
work is covered by `upstream-capability-canary.yml`.

---

## Workflow Call Graph

```
┌──────────────────────────────────────────────────────────────────────────┐
│  SCHEDULE / workflow_dispatch                                             │
│                                                                           │
│  upstream-sync.yml (Mon 03:00)                                            │
│    │                                                                      │
│    ├─ scripts/upstream_sync.py                                            │
│    │    reads:  upstream/manifest.yml                                     │
│    │            upstream/status.json  (previous SHAs)                    │
│    │            upstream/capabilities.yml                                 │
│    │    writes: upstream/<source-id>/**  (sparse file snapshots)         │
│    │            upstream/status.json    (updated SHAs + timestamps)      │
│    │    output: /tmp/pr-body.md                                           │
│    │                                                                      │
│    ├─ scripts/upstream_dashboard.py                                       │
│    │    reads:  upstream/status.json                                      │
│    │            upstream/capabilities.yml                                 │
│    │    writes: upstream/status.md                                        │
│    │                                                                      │
│    └─ (if files changed) → git push branch + gh pr create                │
│         branch: chore/upstream-sync-YYYYMMDD-HHMM                        │
│         label:  upstream                                                  │
│                                                                           │
│  upstream-release-watch.yml (Mon 03:30)                                   │
│    │  NO scripts — calls GitHub API directly via `gh api`                │
│    │  Checks latest releases for CamillaDSP, CamillaGUI, pyCamillaDSP,  │
│    │  piCorePlayer, camilladsp-controller                                 │
│    └─ writes: GitHub Actions step summary only (no files, no issues)     │
│                                                                           │
│  upstream-capability-canary.yml (Mon 04:00)                               │
│    │  Also runs on: workflow_dispatch                                     │
│    │                pull_request touching upstream/**                     │
│    │                                                                      │
│    ├─ job: probe-camilladsp (matrix: master / next4.2.0 / next5)         │
│    │    ├─ checkout + build CamillaDSP from source                       │
│    │    ├─ probes/probe_camilla_capabilities.py                           │
│    │    │    reads:  upstream/capabilities.yml                            │
│    │    │    writes: /tmp/probe-results-<branch>.json                    │
│    │    ├─ probes/report_probe_results.py                                 │
│    │    │    reads:  /tmp/probe-results-<branch>.json                    │
│    │    │    writes: GitHub Actions step summary                          │
│    │    └─ uploads artifact: probe-results-<branch>                      │
│    │                                                                      │
│    └─ (removal check + dashboard regeneration delegated to)              │
│         upstream-removal-check.yml  (triggered by workflow_run below)    │
│                                                                           │
│  upstream-branch-watch.yml (Mon 04:15)                                    │
│    │  NO scripts — calls GitHub API directly via `gh api`                │
│    │  Watches for next*/v5*/5.* branches on CamillaDSP, CamillaGUI,     │
│    │  pyCamillaDSP, piCorePlayer kernel                                  │
│    └─ If pyCamillaDSP has v5 branches → gh issue create (deduped)        │
│                                                                           │
└──────────────────────────────────────────────────────────────────────────┘

┌──────────────────────────────────────────────────────────────────────────┐
│  TRIGGERED BY workflow_run (when upstream-capability-canary completes)   │
│                                                                           │
│  upstream-removal-check.yml                                               │
│    │                                                                      │
│    ├─ download artifacts: probe-results-* from the triggering canary run │
│    │                                                                      │
│    ├─ scripts/upstream_removal_check.py                                   │
│    │    reads:  upstream/status.json  (previous probe_results)           │
│    │            upstream/capabilities.yml                                 │
│    │            /tmp/probe-results/*.json                                 │
│    │    writes: upstream/status.json  (updated probe_results for next run)│
│    │            /tmp/removal-candidates.json                              │
│    │                                                                      │
│    ├─ (if FAIL→PASS flips found) → gh issue create (deduped)             │
│    │    labels: removal-candidate, upstream, capability                   │
│    │                                                                      │
│    ├─ scripts/upstream_dashboard.py                                       │
│    │    reads:  upstream/status.json                                      │
│    │            upstream/capabilities.yml                                 │
│    │            /tmp/probe-results/*.json                                 │
│    │    writes: upstream/status.md                                        │
│    │                                                                      │
│    └─ git commit + push status.json + status.md [skip ci]                │
│         (CI metadata only, direct push to default branch is intentional) │
│                                                                           │
└──────────────────────────────────────────────────────────────────────────┘

┌──────────────────────────────────────────────────────────────────────────┐
│  MANUAL ONLY (workflow_dispatch)                                          │
│                                                                           │
│  canary.yml                                                               │
│    ├─ job: probe-camilladsp (matrix: master / next4.2.0 / next5)         │
│    │    Same build + probe steps as upstream-capability-canary.yml       │
│    │    Use this for ad-hoc probes without triggering the full pipeline. │
│    ├─ job: watch-gui-branches (lists camillagui-backend branches/PRs)    │
│    └─ job: watch-pycamilladsp (checks latest pyCamillaDSP release)       │
│                                                                           │
└──────────────────────────────────────────────────────────────────────────┘
```

---

## File Reference

| File | Purpose |
|---|---|
| `upstream/manifest.yml` | Declarative list of every upstream source, branch, and file patterns to snapshot |
| `upstream/capabilities.yml` | Registry of every local workaround + its removal criterion (roadmap §61) |
| `upstream/status.json` | Machine-readable state: current/previous SHAs per source, last probe results |
| `upstream/status.md` | Human-readable dashboard — auto-generated, never edit by hand |
| `upstream/<source-id>/` | Sparse file snapshots from upstream (what the sync script fetches) |

---

## Script Reference

### `scripts/upstream_sync.py`

Reads `manifest.yml`, fetches sparse file snapshots from every upstream source
via the GitHub API, updates `status.json` with current SHAs, and writes
`/tmp/pr-body.md` for the auto-opened PR.

**Requires:** `GITHUB_TOKEN` env var (read-only, public repos only).  
**Read-only:** never touches piCoreCDSP Rust source or `Cargo.toml`.

```
upstream_sync.py
  --manifest upstream/manifest.yml
  --status   upstream/status.json
  --capabilities upstream/capabilities.yml
  --output-dir .
  --pr-body-file /tmp/pr-body.md
```

---

### `scripts/upstream_dashboard.py`

Reads `status.json` + `capabilities.yml` + optional probe result JSONs and
regenerates `upstream/status.md`.  Safe to run at any time.

```
upstream_dashboard.py
  --status       upstream/status.json
  --capabilities upstream/capabilities.yml
  --probes       /tmp/probe-results-*.json   # optional
  --output       upstream/status.md
```

---

### `scripts/upstream_removal_check.py`

Compares new probe results against the previous run's results stored in
`status.json`.  For any capability that flipped FAIL → PASS, emits a
`/tmp/removal-candidates.json` record.  The workflow then calls
`gh issue create` for each candidate.  Also updates `status.json` with
the current probe results for the next comparison.

```
upstream_removal_check.py
  --status       upstream/status.json
  --capabilities upstream/capabilities.yml
  --probes       /tmp/probe-results-master.json  /tmp/probe-results-next4.2.0.json ...
  --output       /tmp/removal-candidates.json
```

---

### `probes/probe_camilla_capabilities.py`

Black-box WebSocket probe: starts the real CamillaDSP binary, exercises every
capability listed in `capabilities.yml`, and writes `probe-results-<branch>.json`.
Only runs in CI (needs a built CamillaDSP binary).

### `probes/report_probe_results.py`

Formats a probe result JSON file as Markdown for the GitHub Actions step summary.

### `scripts/test_upstream_sync.py`

Unit tests for `upstream_sync.py`.  Run with `python3 -m pytest scripts/`.

---

## What Happens When a Probe Flips FAIL → PASS

```
upstream-capability-canary.yml runs (Monday 04:00)
  → uploads probe-results-*.json artifacts

upstream-removal-check.yml is triggered (workflow_run)
  → upstream_removal_check.py compares new vs. old probe_results in status.json
  → if capability X was FAIL last week and PASS this week:
      → gh issue create "Removal candidate: `X` probe now PASS"
         labels: removal-candidate, upstream, capability
  → updates status.json + regenerates status.md
  → commits + pushes [skip ci]
```

**No code is ever deleted automatically.**  The issue is a prompt for a human
to verify the removal criteria in `capabilities.yml` and perform the deletion
manually (including hardware validation at Gate 12).

---

## Running Manually

To populate `status.md` locally (dry-run, no GitHub token needed for dashboard
but sync needs token):

```bash
# Regenerate dashboard from existing status.json (no token needed):
python3 scripts/upstream_dashboard.py \
  --status upstream/status.json \
  --capabilities upstream/capabilities.yml \
  --output upstream/status.md

# Run full sync (requires GITHUB_TOKEN):
GITHUB_TOKEN=ghp_... python3 scripts/upstream_sync.py \
  --manifest upstream/manifest.yml \
  --status   upstream/status.json \
  --capabilities upstream/capabilities.yml \
  --output-dir .
```

To trigger any workflow manually: **Actions tab → select workflow → Run workflow**.

---

_See roadmap §60–§70 for the full design rationale._
