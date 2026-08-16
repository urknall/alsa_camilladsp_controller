#!/usr/bin/env python3
"""Upstream tracking checker (Phase 5 / Phase 19 / Phase 20 / Phase 20a-c).

Reads ``docs/upstream-tracking.yml`` and, for each tracked upstream source,
checks whether the latest commit touching its tracked paths is newer than
the recorded ``last_reviewed_commit``. When a newer commit is found, this
script opens (or updates) a GitHub issue in *this* repository asking for a
manual review. It never modifies the manifest, never merges or applies
upstream changes, and never auto-closes issues.

For sources that record a ``last_reviewed_tag`` (e.g. alsa-lib, which is
tracked by release rather than by tracked file paths), this script also
independently checks the newest published release/tag against
``last_reviewed_tag`` and opens a distinct "new release" issue when it is
stale. For sources flagged ``run_tests_on_release: true`` in the manifest,
a newly detected release is additionally surfaced via ``$GITHUB_OUTPUT`` so
the calling workflow can trigger the native ioplug test suite against it.

Design goals:
  * Idempotent: re-running does not create duplicate issues for the same
    upstream commit or release tag.
  * Safe by default: with no ``GITHUB_TOKEN``, or when ``--dry-run`` is
    passed, the script only prints what it *would* do.
  * No behavioural changes to tracked repositories or this one beyond
    opening an issue - see the "never auto-merge" policy documented in
    docs/upstream-tracking.yml and each docs/*_TRACKING.md file.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from typing import Any, Callable, Optional

try:
    import yaml
except ImportError:  # pragma: no cover - exercised only when dependency missing
    print(
        "error: PyYAML is required (pip install pyyaml)",
        file=sys.stderr,
    )
    raise

GITHUB_API = "https://api.github.com"
MARKER_PREFIX = "<!-- upstream-tracking:"
EXIT_OK = 0
EXIT_MANIFEST_ERROR = 1
EXIT_INCONCLUSIVE = 2


@dataclass
class Source:
    id: str
    repository: str
    tracked_paths: list[str] = field(default_factory=list)
    last_reviewed_commit: str = ""
    last_reviewed_tag: Optional[str] = None
    review_date: str = ""
    label: str = ""
    priority: str = ""
    doc: str = ""
    run_tests_on_release: bool = False

    @staticmethod
    def from_dict(data: dict[str, Any]) -> "Source":
        return Source(
            id=data["id"],
            repository=data["repository"],
            tracked_paths=list(data.get("tracked_paths") or []),
            last_reviewed_commit=data.get("last_reviewed_commit") or "",
            last_reviewed_tag=data.get("last_reviewed_tag"),
            review_date=data.get("review_date") or "",
            label=data.get("label") or "",
            priority=data.get("priority") or "",
            doc=data.get("doc") or "",
            run_tests_on_release=bool(data.get("run_tests_on_release", False)),
        )


@dataclass
class SourceCheckStatus:
    source: Source
    update_available: bool = False
    inconclusive: bool = False
    errors: list[str] = field(default_factory=list)


def load_manifest(path: str) -> list[Source]:
    with open(path, "r", encoding="utf-8") as fh:
        data = yaml.safe_load(fh) or {}
    return [Source.from_dict(item) for item in data.get("sources", [])]


def _api_request(url: str, token: Optional[str], method: str = "GET",
                  body: Optional[dict[str, Any]] = None) -> Any:
    headers = {
        "Accept": "application/vnd.github+json",
        "User-Agent": "alsa_camilladsp_controller-upstream-tracking",
    }
    if token:
        headers["Authorization"] = "Bearer " + token
    data = json.dumps(body).encode("utf-8") if body is not None else None
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    with urllib.request.urlopen(req, timeout=30) as resp:  # noqa: S310 - trusted GitHub API
        return json.loads(resp.read().decode("utf-8"))


def missing_tracked_paths(
    source: Source,
    token: Optional[str],
    record_inconclusive: Optional[Callable[[str], None]] = None,
) -> list[str]:
    """Return the subset of ``source.tracked_paths`` that no longer exist upstream.

    This catches the failure mode that let the BlueALSA tracking doc go
    stale silently: an upstream rename/restructure (e.g. ``src/bluealsa-pcm.c``
    -> ``src/asound/bluealsa-pcm.c``) makes the commit-history query for the
    old path return zero results forever, which looks identical to "no new
    changes" rather than "the tracked path is gone". Checking path existence
    directly at the repository's default branch HEAD surfaces that case
    explicitly instead of relying on a human noticing during an unrelated
    review pass.
    """
    missing: list[str] = []
    for path in source.tracked_paths:
        url = f"{GITHUB_API}/repos/{source.repository}/contents/{urllib.parse.quote(path)}"
        try:
            _api_request(url, token)
        except urllib.error.HTTPError as exc:
            if exc.code == 404:
                missing.append(path)
            else:
                print(f"warning: could not verify path {path!r} for "
                      f"{source.repository}: {exc}", file=sys.stderr)
                if record_inconclusive:
                    record_inconclusive(f"path verification failed for {path!r}: {exc}")
    return missing


def build_missing_path_issue_body(source: Source, missing: list[str]) -> str:
    marker_paths = ",".join(sorted(missing))
    paths = ", ".join(f"`{p}`" for p in missing)
    return (
        f"{MARKER_PREFIX}{source.id}:missing-paths:{marker_paths} -->\n\n"
        f"Tracked path(s) for **{source.id}** (`{source.repository}`) no "
        f"longer exist upstream (checked against the default branch):\n\n"
        f"- Missing: {paths}\n"
        f"- Last reviewed commit: `{source.last_reviewed_commit or '(none)'}` "
        f"(reviewed {source.review_date or 'unknown'})\n"
        f"- Tracking doc: `{source.doc}`\n\n"
        "This usually means the upstream project renamed or restructured the "
        "tracked file(s) (as happened previously with BlueALSA's "
        "`src/bluealsa-pcm.c` -> `src/asound/bluealsa-pcm.c` move, which the "
        "commit-diff check alone did not catch). Nothing has been changed in "
        "this repository automatically. Please review manually:\n\n"
        f"1. Find the tracked file's new location (if any) in "
        f"`{source.repository}`.\n"
        f"2. Update `tracked_paths` in `docs/upstream-tracking.yml` and the "
        f"path references in `{source.doc}` to match.\n"
        "3. Re-run this check (or wait for the next scheduled run) to confirm "
        "the updated paths resolve.\n"
    )


def latest_commit_for_source(
    source: Source,
    token: Optional[str],
    record_inconclusive: Optional[Callable[[str], None]] = None,
) -> Optional[dict[str, Any]]:
    """Return the newest commit dict touching any of the source's tracked paths.

    When ``tracked_paths`` is empty, returns the latest commit on the
    repository's default branch instead.
    """
    paths = source.tracked_paths or [None]
    newest: Optional[dict[str, Any]] = None
    for path in paths:
        url = f"{GITHUB_API}/repos/{source.repository}/commits?per_page=1"
        if path:
            url += f"&path={urllib.parse.quote(path)}"
        try:
            commits = _api_request(url, token)
        except urllib.error.HTTPError as exc:
            print(f"warning: could not fetch commits for {source.repository} "
                  f"(path={path!r}): {exc}", file=sys.stderr)
            if record_inconclusive:
                record_inconclusive(f"commit lookup failed for {path or '(default branch)'}: {exc}")
            continue
        if not commits:
            continue
        commit = commits[0]
        commit_date = commit["commit"]["committer"]["date"]
        if newest is None or commit_date > newest["commit"]["committer"]["date"]:
            newest = commit
    return newest


def latest_release_tag_for_source(
    source: Source,
    token: Optional[str],
    record_inconclusive: Optional[Callable[[str], None]] = None,
) -> Optional[str]:
    """Return the newest published release tag for ``source.repository``.

    Falls back to the newest git tag when the repository has no published
    GitHub Releases (some projects, e.g. alsa-lib, only push annotated tags).
    Returns ``None`` when neither is available or the API call fails.
    """
    url = f"{GITHUB_API}/repos/{source.repository}/releases/latest"
    try:
        release = _api_request(url, token)
        tag = release.get("tag_name")
        if tag:
            return tag
    except urllib.error.HTTPError as exc:
        if exc.code != 404:
            print(f"warning: could not fetch latest release for "
                  f"{source.repository}: {exc}", file=sys.stderr)
            if record_inconclusive:
                record_inconclusive(f"release lookup failed: {exc}")
            return None

    url = f"{GITHUB_API}/repos/{source.repository}/tags?per_page=1"
    try:
        tags = _api_request(url, token)
    except urllib.error.HTTPError as exc:
        print(f"warning: could not fetch tags for {source.repository}: {exc}", file=sys.stderr)
        if record_inconclusive:
            record_inconclusive(f"tag lookup failed: {exc}")
        return None
    if not tags:
        return None
    return tags[0].get("name")


def build_new_release_issue_body(source: Source, tag: str) -> str:
    tests_note = (
        "The native ioplug test suite has been automatically triggered against "
        "this release in the companion job of this workflow run; check its "
        "result before doing anything else.\n\n"
        if source.run_tests_on_release else ""
    )
    return (
        f"{MARKER_PREFIX}{source.id}:new-release:{tag} -->\n\n"
        f"A newer release/tag was published for **{source.id}** "
        f"(`{source.repository}`).\n\n"
        f"- Last reviewed tag: `{source.last_reviewed_tag or '(none)'}` "
        f"(reviewed {source.review_date or 'unknown'})\n"
        f"- New tag: `{tag}`\n"
        f"- Tracking doc: `{source.doc}`\n\n"
        "This is an automated notification only. Nothing has been changed, "
        f"merged, or cherry-picked in this repository.\n\n{tests_note}"
        "Please review manually:\n\n"
        "1. Determine whether the release notes mention ioplug-relevant "
        "behaviour changes (`pcm_ioplug.h`, `pcm_external.h`, "
        "`hw_params`/`sw_params` semantics).\n"
        f"2. If relevant, port the concept/fix and add or update a regression "
        f"test, then update `{source.doc}`.\n"
        "3. Update `last_reviewed_tag` / `last_reviewed_commit` / "
        "`review_date` in `docs/upstream-tracking.yml` once reviewed, then "
        "close this issue.\n"
    )


def check_release_for_source(source: Source, repo: str, token: Optional[str],
                              dry_run: bool,
                              record_inconclusive: Optional[Callable[[str], None]] = None) -> Optional[str]:
    """Check ``source.last_reviewed_tag`` against the newest published release/tag.

    This is intentionally independent of the commit-diff check above: a
    project such as alsa-lib merges commits to its default branch far more
    often than it cuts a release, so relying on ``last_reviewed_commit``
    alone means ``last_reviewed_tag`` was recorded in the manifest but never
    actually consulted - the exact gap flagged in review. Returns the newly
    detected tag (used to drive CI job outputs) if one was found, else None.
    """
    if not source.last_reviewed_tag:
        return None

    tag = latest_release_tag_for_source(
        source,
        token,
        record_inconclusive=record_inconclusive,
    )
    if tag is None:
        print(f"[{source.id}] could not determine latest release/tag, "
              "skipping release check")
        return None
    if tag == source.last_reviewed_tag:
        print(f"[{source.id}] release up to date ({tag})")
        return None

    print(f"[{source.id}] new release detected: {tag} "
          f"(last reviewed tag: {source.last_reviewed_tag})")

    if dry_run or not token:
        print(f"[{source.id}] dry-run: would ensure label {source.label!r} "
              "and open/skip new-release issue")
        return tag

    marker = f"{MARKER_PREFIX}{source.id}:new-release:{tag} -->"
    title = f"Upstream release detected: {source.id} ({tag})"
    body = build_new_release_issue_body(source, tag)
    open_issue_if_new(repo, source, marker, title, body, token)
    return tag


def output_key(source_id: str) -> str:
    """Sanitise a source id into a valid GitHub Actions output-key prefix."""
    return source_id.replace("-", "_")


def emit_github_output(key: str, value: str) -> None:
    """Append a ``key=value`` line to ``$GITHUB_OUTPUT`` if it is set.

    No-op outside of a GitHub Actions step (e.g. when run locally or under
    the unit tests), so callers never need to special-case that.
    """
    path = os.environ.get("GITHUB_OUTPUT")
    if not path:
        return
    with open(path, "a", encoding="utf-8") as fh:
        fh.write(f"{key}={value}\n")


def find_existing_issue_by_marker(repo: str, marker: str, token: str) -> Optional[dict[str, Any]]:
    url = f"{GITHUB_API}/search/issues?q={urllib.parse.quote(marker)}+repo:{repo}+in:body+type:issue"
    try:
        result = _api_request(url, token)
    except urllib.error.HTTPError as exc:
        print(f"warning: issue search failed for marker {marker!r}: {exc}", file=sys.stderr)
        return None
    items = result.get("items", [])
    return items[0] if items else None


def open_issue_if_new(repo: str, source: Source, marker: str, title: str, body: str,
                       token: str) -> None:
    existing = find_existing_issue_by_marker(repo, marker, token)
    if existing:
        print(f"[{source.id}] issue already open: {existing.get('html_url')}")
        return
    ensure_label(repo, source.label, token)
    try:
        issue = _api_request(
            f"{GITHUB_API}/repos/{repo}/issues",
            token,
            method="POST",
            body={"title": title, "body": body, "labels": [source.label] if source.label else []},
        )
        print(f"[{source.id}] opened issue: {issue.get('html_url')}")
    except urllib.error.HTTPError as exc:
        print(f"error: could not create issue for {source.id}: {exc}", file=sys.stderr)


def ensure_label(repo: str, label: str, token: str) -> None:
    url = f"{GITHUB_API}/repos/{repo}/labels/{urllib.parse.quote(label)}"
    try:
        _api_request(url, token)
        return
    except urllib.error.HTTPError as exc:
        if exc.code != 404:
            print(f"warning: could not check label {label!r}: {exc}", file=sys.stderr)
            return
    try:
        _api_request(
            f"{GITHUB_API}/repos/{repo}/labels",
            token,
            method="POST",
            body={"name": label, "color": "1d76db",
                  "description": "Detected upstream reference/dependency change"},
        )
    except urllib.error.HTTPError as exc:
        print(f"warning: could not create label {label!r}: {exc}", file=sys.stderr)


def build_issue_body(source: Source, commit: dict[str, Any]) -> str:
    sha = commit["sha"]
    short_sha = sha[:7]
    html_url = commit.get("html_url", f"https://github.com/{source.repository}/commit/{sha}")
    message = commit["commit"]["message"].splitlines()[0]
    compare_url = (
        f"https://github.com/{source.repository}/compare/"
        f"{source.last_reviewed_commit}...{sha}"
        if source.last_reviewed_commit else html_url
    )
    paths = ", ".join(f"`{p}`" for p in source.tracked_paths) or "(whole repository)"
    return (
        f"{MARKER_PREFIX}{source.id}:{sha} -->\n\n"
        f"A newer commit touching tracked paths was found for **{source.id}** "
        f"(`{source.repository}`).\n\n"
        f"- Tracked paths: {paths}\n"
        f"- Last reviewed commit: `{source.last_reviewed_commit or '(none)'}` "
        f"(reviewed {source.review_date or 'unknown'})\n"
        f"- New commit: [`{short_sha}`]({html_url}) — {message}\n"
        f"- Compare: {compare_url}\n"
        f"- Priority: {source.priority or 'unspecified'}\n"
        f"- Tracking doc: `{source.doc}`\n\n"
        "This is an automated notification only. Nothing has been changed, "
        "merged, or cherry-picked in this repository. Please review the "
        "change manually:\n\n"
        "1. Determine whether it touches any of the topic categories listed "
        f"in `{source.doc}`.\n"
        "2. If relevant, port the concept/fix and add or update a regression "
        "test, then update `last_reviewed_commit` / `review_date` in "
        "`docs/upstream-tracking.yml` and the tracking doc's Review Log.\n"
        "3. If not relevant, mark this reviewed (update the manifest the same "
        "way) and close this issue.\n"
    )


def process_source(
    source: Source,
    repo: str,
    token: Optional[str],
    dry_run: bool,
    record_inconclusive: Optional[Callable[[str], None]] = None,
) -> bool:
    """Returns True if an update was found (issue created or would be created)."""
    any_finding = False
    had_errors = False

    def note_inconclusive(reason: str) -> None:
        nonlocal had_errors
        had_errors = True
        if record_inconclusive is not None:
            record_inconclusive(reason)

    # Path-existence check runs first and independently of the commit-diff
    # check below: a renamed/removed tracked path can otherwise go unnoticed
    # forever, since the commit-history query for a path that no longer
    # exists simply returns no results (indistinguishable from "no changes").
    if source.tracked_paths:
        missing = missing_tracked_paths(
            source,
            token,
            record_inconclusive=note_inconclusive,
        )
        if missing:
            any_finding = True
            print(f"[{source.id}] tracked path(s) no longer exist upstream: "
                  f"{', '.join(missing)}")
            marker = f"{MARKER_PREFIX}{source.id}:missing-paths:{','.join(sorted(missing))} -->"
            if dry_run or not token:
                print(f"[{source.id}] dry-run: would ensure label {source.label!r} "
                      "and open/skip missing-path issue")
            else:
                title = f"Upstream tracking stale: {source.id} tracked path(s) missing"
                body = build_missing_path_issue_body(source, missing)
                open_issue_if_new(repo, source, marker, title, body, token)
        elif not had_errors:
            print(f"[{source.id}] all tracked paths present upstream")

    commit = latest_commit_for_source(
        source,
        token,
        record_inconclusive=note_inconclusive,
    )
    if commit is None:
        if had_errors:
            print(f"[{source.id}] commit check inconclusive")
        else:
            print(f"[{source.id}] could not determine latest commit, skipping")
        return any_finding
    sha = commit["sha"]
    if sha == source.last_reviewed_commit:
        print(f"[{source.id}] up to date ({sha[:7]})")
        return any_finding

    print(f"[{source.id}] new commit detected: {sha[:7]} "
          f"(last reviewed: {source.last_reviewed_commit[:7] if source.last_reviewed_commit else 'none'})")
    any_finding = True

    if dry_run or not token:
        print(f"[{source.id}] dry-run: would ensure label {source.label!r} and open/skip issue")
        return any_finding

    marker = f"{MARKER_PREFIX}{source.id}:{sha} -->"
    title = f"Upstream change detected: {source.id} ({sha[:7]})"
    body = build_issue_body(source, commit)
    open_issue_if_new(repo, source, marker, title, body, token)
    return any_finding


def check_source(source: Source, repo: str, token: Optional[str], dry_run: bool) -> SourceCheckStatus:
    status = SourceCheckStatus(source=source)

    def record_inconclusive(reason: str) -> None:
        status.inconclusive = True
        status.errors.append(reason)

    try:
        if process_source(source, repo, token, dry_run, record_inconclusive=record_inconclusive):
            status.update_available = True

        new_tag = check_release_for_source(
            source,
            repo,
            token,
            dry_run,
            record_inconclusive=record_inconclusive,
        )
        if new_tag:
            status.update_available = True
            if source.run_tests_on_release:
                key = output_key(source.id)
                emit_github_output(f"{key}_release_detected", "true")
                emit_github_output(f"{key}_release_tag", new_tag)
    except Exception as exc:  # noqa: BLE001 - keep checking remaining sources
        status.inconclusive = True
        status.errors.append(str(exc))
        print(f"error: failed processing {source.id}: {exc}", file=sys.stderr)

    return status


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", default="docs/upstream-tracking.yml")
    parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", ""),
                         help="owner/repo to open issues in (defaults to $GITHUB_REPOSITORY)")
    parser.add_argument("--dry-run", action="store_true",
                         help="only print what would happen; never call write APIs")
    args = parser.parse_args(argv)

    token = os.environ.get("GITHUB_TOKEN")
    sources = load_manifest(args.manifest)
    if not sources:
        print(f"error: no sources found in {args.manifest}", file=sys.stderr)
        return EXIT_MANIFEST_ERROR

    statuses = [check_source(source, args.repo, token, args.dry_run) for source in sources]
    any_updates = any(status.update_available for status in statuses)
    inconclusive = [status for status in statuses if status.inconclusive]

    if any_updates:
        print("UPDATE AVAILABLE:")
        for status in statuses:
            if status.update_available:
                print(f"  - {status.source.id}")

    if inconclusive:
        print("COULD NOT CHECK:")
        for status in inconclusive:
            if status.errors:
                print(f"  - {status.source.id}: {'; '.join(status.errors)}")
            else:
                print(f"  - {status.source.id}")

    if inconclusive:
        if any_updates:
            print("Upstream changes were detected, but one or more checks were inconclusive.")
        else:
            print("One or more tracked upstream sources could not be checked completely.")
        return EXIT_INCONCLUSIVE

    if any_updates:
        print("Upstream changes were detected. See issues above (or dry-run output).")
    else:
        print("All tracked upstream sources are up to date.")
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
