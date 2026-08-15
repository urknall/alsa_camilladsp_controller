#!/usr/bin/env python3
"""Unit tests for check_upstream_tracking.py.

Runs offline: all GitHub API calls are patched, so no network access or
token is required. Exercises both the pre-existing commit-diff detection
and the path-existence check added to catch stale tracked paths (the
failure mode that let the BlueALSA tracking doc silently drift before it
was manually caught and corrected).
"""

from __future__ import annotations

import unittest
from unittest.mock import patch
import urllib.error

import check_upstream_tracking as cut


def make_source(**overrides) -> cut.Source:
    defaults = dict(
        id="example",
        repository="owner/repo",
        tracked_paths=["src/tracked.c"],
        last_reviewed_commit="a" * 40,
        last_reviewed_tag=None,
        review_date="2026-01-01",
        label="upstream/example",
        priority="MEDIUM",
        doc="docs/EXAMPLE_TRACKING.md",
    )
    defaults.update(overrides)
    return cut.Source(**defaults)


def http_error(code: int) -> urllib.error.HTTPError:
    return urllib.error.HTTPError(url="http://x", code=code, msg="err", hdrs=None, fp=None)


class MissingTrackedPathsTests(unittest.TestCase):
    def test_all_paths_present_returns_empty(self):
        source = make_source(tracked_paths=["a.c", "b.c"])
        with patch.object(cut, "_api_request", return_value={"type": "file"}):
            self.assertEqual(cut.missing_tracked_paths(source, token=None), [])

    def test_renamed_path_is_reported_missing(self):
        source = make_source(tracked_paths=["src/bluealsa-pcm.c"])
        with patch.object(cut, "_api_request", side_effect=http_error(404)):
            missing = cut.missing_tracked_paths(source, token=None)
        self.assertEqual(missing, ["src/bluealsa-pcm.c"])

    def test_mixed_present_and_missing(self):
        source = make_source(tracked_paths=["present.c", "gone.c"])

        def fake(url, token):
            if "gone.c" in url:
                raise http_error(404)
            return {"type": "file"}

        with patch.object(cut, "_api_request", side_effect=fake):
            missing = cut.missing_tracked_paths(source, token=None)
        self.assertEqual(missing, ["gone.c"])

    def test_non_404_error_is_not_treated_as_missing(self):
        source = make_source(tracked_paths=["rate-limited.c"])
        with patch.object(cut, "_api_request", side_effect=http_error(403)):
            missing = cut.missing_tracked_paths(source, token=None)
        self.assertEqual(missing, [])

    def test_empty_tracked_paths_returns_empty(self):
        source = make_source(tracked_paths=[])
        with patch.object(cut, "_api_request") as mocked:
            missing = cut.missing_tracked_paths(source, token=None)
        mocked.assert_not_called()
        self.assertEqual(missing, [])


class BuildMissingPathIssueBodyTests(unittest.TestCase):
    def test_body_contains_marker_and_missing_paths(self):
        source = make_source(id="bluealsa", tracked_paths=["src/bluealsa-pcm.c"])
        body = cut.build_missing_path_issue_body(source, ["src/bluealsa-pcm.c"])
        self.assertIn("<!-- upstream-tracking:bluealsa:missing-paths:src/bluealsa-pcm.c -->", body)
        self.assertIn("src/bluealsa-pcm.c", body)
        self.assertIn("docs/EXAMPLE_TRACKING.md", body)


class ProcessSourceTests(unittest.TestCase):
    def test_dry_run_reports_missing_path_without_calling_issue_apis(self):
        source = make_source()
        with patch.object(cut, "missing_tracked_paths", return_value=["src/tracked.c"]), \
             patch.object(cut, "latest_commit_for_source", return_value=None), \
             patch.object(cut, "open_issue_if_new") as mocked_open:
            found = cut.process_source(source, repo="owner/repo", token="tok", dry_run=True)
        self.assertTrue(found)
        mocked_open.assert_not_called()

    def test_missing_path_and_new_commit_both_open_issues(self):
        source = make_source()
        commit = {
            "sha": "b" * 40,
            "html_url": "http://example/commit/b",
            "commit": {"message": "did a thing", "committer": {"date": "2026-02-01"}},
        }
        with patch.object(cut, "missing_tracked_paths", return_value=["src/tracked.c"]), \
             patch.object(cut, "latest_commit_for_source", return_value=commit), \
             patch.object(cut, "open_issue_if_new") as mocked_open:
            found = cut.process_source(source, repo="owner/repo", token="tok", dry_run=False)
        self.assertTrue(found)
        self.assertEqual(mocked_open.call_count, 2)

    def test_no_missing_paths_and_same_commit_is_up_to_date(self):
        source = make_source()
        commit = {
            "sha": source.last_reviewed_commit,
            "html_url": "http://example/commit/a",
            "commit": {"message": "noop", "committer": {"date": "2026-01-01"}},
        }
        with patch.object(cut, "missing_tracked_paths", return_value=[]), \
             patch.object(cut, "latest_commit_for_source", return_value=commit), \
             patch.object(cut, "open_issue_if_new") as mocked_open:
            found = cut.process_source(source, repo="owner/repo", token="tok", dry_run=False)
        self.assertFalse(found)
        mocked_open.assert_not_called()

    def test_no_commit_found_but_missing_path_still_reports_finding(self):
        source = make_source()
        with patch.object(cut, "missing_tracked_paths", return_value=["src/tracked.c"]), \
             patch.object(cut, "latest_commit_for_source", return_value=None), \
             patch.object(cut, "open_issue_if_new") as mocked_open:
            found = cut.process_source(source, repo="owner/repo", token="tok", dry_run=False)
        self.assertTrue(found)
        mocked_open.assert_called_once()


class OpenIssueIfNewTests(unittest.TestCase):
    def test_skips_creation_when_issue_already_open(self):
        source = make_source()
        with patch.object(cut, "find_existing_issue_by_marker",
                           return_value={"html_url": "http://existing"}), \
             patch.object(cut, "ensure_label") as mocked_label, \
             patch.object(cut, "_api_request") as mocked_request:
            cut.open_issue_if_new(
                "owner/repo", source, marker="m", title="t", body="b", token="tok",
            )
        mocked_label.assert_not_called()
        mocked_request.assert_not_called()

    def test_creates_issue_when_none_exists(self):
        source = make_source()
        with patch.object(cut, "find_existing_issue_by_marker", return_value=None), \
             patch.object(cut, "ensure_label") as mocked_label, \
             patch.object(cut, "_api_request", return_value={"html_url": "http://new"}) as mocked_request:
            cut.open_issue_if_new(
                "owner/repo", source, marker="m", title="t", body="b", token="tok",
            )
        mocked_label.assert_called_once_with("owner/repo", source.label, "tok")
        mocked_request.assert_called_once()


if __name__ == "__main__":
    unittest.main()
