#!/usr/bin/env python3
"""
piCoreCDSP v2 — Upstream Sync Script (roadmap §62, Gate 14)

Reads upstream/manifest.yml, fetches sparse snapshots of relevant files from
each upstream source via the GitHub API, updates upstream/status.json, and
writes a human-readable change summary to stdout (which the CI workflow uses to
populate the PR body).

Usage:
    python3 scripts/upstream_sync.py \\
        --manifest upstream/manifest.yml \\
        --status   upstream/status.json \\
        --output-dir . \\
        [--dry-run]

Requires: GITHUB_TOKEN env var (read-only, public repos only needed).

IMPORTANT — Retention policy (roadmap §70):
    - This script stores ONLY the files listed in manifest.yml paths.
    - No full repository copies, no git history, no binaries.
    - status.json keeps the current + previous SHA per source.
    - The workflow creates a PR for any changes; never pushes to main directly.

IMPORTANT — No automatic adoption (roadmap §69):
    - This script is READ-ONLY with respect to piCoreCDSP source code.
    - It writes only to the upstream/ subdirectory.
    - It never modifies Rust source, Cargo.toml, or any production file.
"""

import argparse
import base64
import hashlib
import json
import os
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import yaml

try:
    import urllib.request
    import urllib.error
except ImportError:
    pass


GITHUB_API = "https://api.github.com"


# ── GitHub API helpers ────────────────────────────────────────────────────────

def gh_request(path: str, token: str) -> Any:
    """Perform a GitHub API GET request and return parsed JSON."""
    url = f"{GITHUB_API}/{path.lstrip('/')}"
    req = urllib.request.Request(
        url,
        headers={
            "Authorization": "Bearer " + token,
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.loads(resp.read().decode())
    except urllib.error.HTTPError as exc:
        if exc.code == 404:
            return None
        raise


def resolve_sha(repo: str, ref: str, token: str) -> str | None:
    """Resolve a branch/tag name to its current commit SHA."""
    if ref == "auto":
        data = gh_request(f"repos/{repo}", token)
        if data is None:
            return None
        ref = data.get("default_branch", "main")
    data = gh_request(f"repos/{repo}/git/ref/heads/{ref}", token)
    if data is None:
        # Try as a tag.
        data = gh_request(f"repos/{repo}/git/ref/tags/{ref}", token)
    if data is None:
        return None
    obj = data.get("object", {})
    sha = obj.get("sha")
    # Dereference a tag object to the commit SHA.
    if obj.get("type") == "tag":
        tag_data = gh_request(f"repos/{repo}/git/tags/{sha}", token)
        if tag_data:
            sha = tag_data.get("object", {}).get("sha", sha)
    return sha


def fetch_file(repo: str, path: str, sha: str, token: str) -> bytes | None:
    """Fetch a single file from a repo at a specific commit SHA."""
    data = gh_request(f"repos/{repo}/contents/{path}?ref={sha}", token)
    if data is None or isinstance(data, list):
        return None
    content = data.get("content", "")
    encoding = data.get("encoding", "base64")
    if encoding == "base64":
        return base64.b64decode(content.replace("\n", ""))
    return content.encode()


def list_tree(repo: str, sha: str, token: str) -> list[str]:
    """Return all file paths in the tree (non-recursive for large repos)."""
    data = gh_request(f"repos/{repo}/git/trees/{sha}?recursive=1", token)
    if data is None or data.get("truncated"):
        return []
    return [item["path"] for item in data.get("tree", []) if item["type"] == "blob"]


def match_paths(all_paths: list[str], patterns: list[str]) -> list[str]:
    """Filter paths by glob-like patterns from the manifest."""
    import fnmatch
    result = []
    for p in all_paths:
        for pat in patterns:
            if fnmatch.fnmatch(p, pat):
                result.append(p)
                break
    return result


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()[:16]


# ── Sync logic ────────────────────────────────────────────────────────────────

def sync_source(
    source: dict,
    token: str,
    output_dir: Path,
    dry_run: bool,
) -> dict:
    """
    Sync a single source entry from the manifest.
    Returns a result dict with: id, sha, fetched_at, files_changed, skipped, error.
    """
    src_id = source["id"]
    repo = source["repo"]
    ref = source.get("ref", "master")
    patterns = source.get("paths", [])
    local_dir = output_dir / source.get("local_dir", f"upstream/{src_id}")

    print(f"  [{src_id}] Resolving {repo}@{ref} ...", flush=True)
    sha = resolve_sha(repo, ref, token)
    if sha is None:
        print(f"  [{src_id}] Branch/ref not found — skipping.", flush=True)
        return {
            "id": src_id, "sha": None, "fetched_at": None,
            "files_changed": 0, "skipped": True, "error": "ref not found",
        }

    print(f"  [{src_id}] SHA = {sha[:12]}", flush=True)
    print(f"  [{src_id}] Listing tree ...", flush=True)
    all_paths = list_tree(repo, sha, token)
    matched = match_paths(all_paths, patterns)
    print(f"  [{src_id}] {len(matched)} files matched ({len(all_paths)} total)", flush=True)

    files_changed = 0
    fetched_at = datetime.now(timezone.utc).isoformat()

    for file_path in matched:
        dest = local_dir / file_path
        if dry_run:
            print(f"  [{src_id}]  (dry) would fetch: {file_path}", flush=True)
            continue

        content = fetch_file(repo, file_path, sha, token)
        if content is None:
            continue

        dest.parent.mkdir(parents=True, exist_ok=True)
        existing = dest.read_bytes() if dest.exists() else None
        if existing != content:
            dest.write_bytes(content)
            files_changed += 1

        time.sleep(0.05)  # be gentle with the API rate limit

    # Write a metadata file for this snapshot.
    if not dry_run:
        meta = {
            "source_id": src_id,
            "repo": repo,
            "ref": ref,
            "sha": sha,
            "fetched_at": fetched_at,
            "files": matched,
        }
        local_dir.mkdir(parents=True, exist_ok=True)
        (local_dir / ".snapshot.json").write_text(json.dumps(meta, indent=2))

    return {
        "id": src_id,
        "sha": sha,
        "fetched_at": fetched_at,
        "files_changed": files_changed,
        "skipped": False,
        "error": None,
    }


def classify_changes(
    results: list[dict],
    manifest: dict,
    capabilities: dict,
) -> list[dict]:
    """
    For each changed source, find which capabilities are affected.
    Returns a list of {source_id, capabilities, local_code} dicts.
    """
    cap_map = {}
    for src in manifest.get("sources", []):
        cap_map[src["id"]] = src.get("capabilities", [])

    affected = []
    for r in results:
        if r.get("skipped") or r.get("files_changed", 0) == 0:
            continue
        caps = cap_map.get(r["id"], [])
        local_codes = []
        for cap_key in caps:
            for cap in capabilities.get("upstream_capabilities", []):
                if cap["key"] == cap_key:
                    local_codes.extend(cap.get("local_code", []))
        affected.append({
            "source_id": r["id"],
            "sha": r["sha"],
            "files_changed": r["files_changed"],
            "capabilities": caps,
            "local_code": sorted(set(local_codes)),
        })
    return affected


def format_pr_body(results: list[dict], affected: list[dict], manifest: dict) -> str:
    """Format the pull request body for the upstream sync PR."""
    lines = [
        "## Upstream Sync",
        "",
        "Auto-generated by `upstream-sync.yml`.  "
        "This PR brings sparse upstream snapshots up to date.",
        "",
        "**Do not merge without reviewing the capability impact below.**",
        "**No piCoreCDSP production code is changed by this PR.**",
        "",
        "---",
        "",
        "### Changed Sources",
        "",
    ]

    changed = [r for r in results if not r.get("skipped") and r.get("files_changed", 0) > 0]
    if not changed:
        lines.append("_No upstream files changed since last sync._")
    else:
        lines.append("| Source | SHA | Files Changed |")
        lines.append("|---|---|---|")
        for r in changed:
            lines.append(f"| `{r['id']}` | `{r['sha'][:12]}` | {r['files_changed']} |")

    lines += ["", "### Skipped Sources", ""]
    skipped = [r for r in results if r.get("skipped")]
    if not skipped:
        lines.append("_None._")
    else:
        for r in skipped:
            lines.append(f"- `{r['id']}`: {r.get('error', 'skipped')}")

    lines += ["", "### Capability Impact", ""]
    if not affected:
        lines.append("_No capability areas affected._")
    else:
        for a in affected:
            lines.append(f"#### `{a['source_id']}` → {a['files_changed']} file(s) changed")
            lines.append("")
            lines.append(f"**Capabilities:** {', '.join(f'`{c}`' for c in a['capabilities']) or '_none_'}")
            lines.append("")
            if a["local_code"]:
                lines.append("**Potentially affected local code:**")
                for lc in a["local_code"]:
                    lines.append(f"- `{lc}`")
            lines.append("")

    lines += [
        "---",
        "",
        "### Checklist",
        "",
        "- [ ] Review changed upstream files for semantic changes.",
        "- [ ] Check if any capability probe now passes that previously failed.",
        "- [ ] If a probe flipped FAIL→PASS: the `upstream-removal-check.yml` workflow",
        "      will open a removal-candidate issue automatically.",
        "- [ ] Confirm no piCoreCDSP production code needs updating.",
        "- [ ] Merge only after review — do not auto-merge.",
    ]

    return "\n".join(lines)


# ── Main ─────────────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser(description="Sync upstream source snapshots")
    parser.add_argument("--manifest", default="upstream/manifest.yml")
    parser.add_argument("--status", default="upstream/status.json")
    parser.add_argument("--capabilities", default="upstream/capabilities.yml")
    parser.add_argument("--output-dir", default=".")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--pr-body-file", default=None,
                        help="Write PR body to this file (for GitHub Actions)")
    args = parser.parse_args()

    token = os.environ.get("GITHUB_TOKEN")
    if not token:
        print("ERROR: GITHUB_TOKEN environment variable not set.", file=sys.stderr)
        sys.exit(1)

    manifest_path = Path(args.manifest)
    status_path = Path(args.status)
    capabilities_path = Path(args.capabilities)
    output_dir = Path(args.output_dir)

    if not manifest_path.exists():
        print(f"ERROR: manifest not found: {manifest_path}", file=sys.stderr)
        sys.exit(1)

    manifest = yaml.safe_load(manifest_path.read_text())
    status = json.loads(status_path.read_text()) if status_path.exists() else {"sources": {}}
    capabilities = yaml.safe_load(capabilities_path.read_text()) if capabilities_path.exists() else {}

    results = []
    for source in manifest.get("sources", []):
        result = sync_source(source, token, output_dir, args.dry_run)
        results.append(result)

        # Update status.json with the new SHA (keep previous for diffing).
        src_id = source["id"]
        prev = status.get("sources", {}).get(src_id, {})
        status.setdefault("sources", {})[src_id] = {
            "sha": result.get("sha"),
            "previous_sha": prev.get("sha"),
            "fetched_at": result.get("fetched_at"),
            "skipped": result.get("skipped", False),
            "error": result.get("error"),
        }

    status["_generated_at"] = datetime.now(timezone.utc).isoformat()

    if not args.dry_run:
        status_path.write_text(json.dumps(status, indent=2))
        print(f"\nStatus written to {status_path}")

    affected = classify_changes(results, manifest, capabilities)
    pr_body = format_pr_body(results, affected, manifest)

    if args.pr_body_file:
        Path(args.pr_body_file).write_text(pr_body)
        print(f"PR body written to {args.pr_body_file}")
    else:
        print("\n" + pr_body)

    changed_count = sum(1 for r in results if not r.get("skipped") and r.get("files_changed", 0) > 0)
    print(f"\nSync complete: {changed_count} source(s) changed.")
    sys.exit(0)


if __name__ == "__main__":
    main()
