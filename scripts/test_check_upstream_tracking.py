#!/usr/bin/env python3
"""Unit tests for check_upstream_tracking.py.

Runs offline: all GitHub API calls are patched, so no network access or
token is required. Exercises both the pre-existing commit-diff detection
and the path-existence check added to catch stale tracked paths (the
failure mode that let the BlueALSA tracking doc silently drift before it
was manually caught and corrected), as well as the release/tag detection
check that consults ``last_reviewed_tag`` (previously recorded in the
manifest but never actually queried).
"""

from __future__ import annotations

import os
import tempfile
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
        run_tests_on_release=False,
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


class LatestReleaseTagForSourceTests(unittest.TestCase):
    def test_returns_tag_name_from_latest_release(self):
        source = make_source()
        with patch.object(cut, "_api_request", return_value={"tag_name": "v2.0.0"}):
            self.assertEqual(cut.latest_release_tag_for_source(source, token=None), "v2.0.0")

    def test_falls_back_to_newest_tag_when_no_releases_published(self):
        source = make_source()

        def fake(url, token):
            if "/releases/latest" in url:
                raise http_error(404)
            self.assertIn("/tags", url)
            return [{"name": "v1.9.0"}]

        with patch.object(cut, "_api_request", side_effect=fake):
            self.assertEqual(cut.latest_release_tag_for_source(source, token=None), "v1.9.0")

    def test_returns_none_when_no_releases_or_tags_exist(self):
        source = make_source()

        def fake(url, token):
            if "/releases/latest" in url:
                raise http_error(404)
            return []

        with patch.object(cut, "_api_request", side_effect=fake):
            self.assertIsNone(cut.latest_release_tag_for_source(source, token=None))

    def test_returns_none_on_non_404_release_error(self):
        source = make_source()
        with patch.object(cut, "_api_request", side_effect=http_error(403)):
            self.assertIsNone(cut.latest_release_tag_for_source(source, token=None))

    def test_returns_none_on_tag_lookup_error(self):
        source = make_source()

        def fake(url, token):
            if "/releases/latest" in url:
                raise http_error(404)
            raise http_error(500)

        with patch.object(cut, "_api_request", side_effect=fake):
            self.assertIsNone(cut.latest_release_tag_for_source(source, token=None))


class CheckReleaseForSourceTests(unittest.TestCase):
    def test_no_last_reviewed_tag_is_skipped_entirely(self):
        source = make_source(last_reviewed_tag=None)
        with patch.object(cut, "latest_release_tag_for_source") as mocked_latest, \
             patch.object(cut, "open_issue_if_new") as mocked_open:
            result = cut.check_release_for_source(source, "owner/repo", token="tok", dry_run=False)
        self.assertIsNone(result)
        mocked_latest.assert_not_called()
        mocked_open.assert_not_called()

    def test_matching_tag_reports_up_to_date(self):
        source = make_source(last_reviewed_tag="v1.2.16.1")
        with patch.object(cut, "latest_release_tag_for_source", return_value="v1.2.16.1"), \
             patch.object(cut, "open_issue_if_new") as mocked_open:
            result = cut.check_release_for_source(source, "owner/repo", token="tok", dry_run=False)
        self.assertIsNone(result)
        mocked_open.assert_not_called()

    def test_newer_tag_opens_issue_and_returns_tag(self):
        source = make_source(last_reviewed_tag="v1.2.16.1")
        with patch.object(cut, "latest_release_tag_for_source", return_value="v1.3.0"), \
             patch.object(cut, "open_issue_if_new") as mocked_open:
            result = cut.check_release_for_source(source, "owner/repo", token="tok", dry_run=False)
        self.assertEqual(result, "v1.3.0")
        mocked_open.assert_called_once()

    def test_dry_run_reports_tag_without_opening_issue(self):
        source = make_source(last_reviewed_tag="v1.2.16.1")
        with patch.object(cut, "latest_release_tag_for_source", return_value="v1.3.0"), \
             patch.object(cut, "open_issue_if_new") as mocked_open:
            result = cut.check_release_for_source(source, "owner/repo", token="tok", dry_run=True)
        self.assertEqual(result, "v1.3.0")
        mocked_open.assert_not_called()

    def test_unknown_latest_tag_is_treated_as_no_finding(self):
        source = make_source(last_reviewed_tag="v1.2.16.1")
        with patch.object(cut, "latest_release_tag_for_source", return_value=None), \
             patch.object(cut, "open_issue_if_new") as mocked_open:
            result = cut.check_release_for_source(source, "owner/repo", token="tok", dry_run=False)
        self.assertIsNone(result)
        mocked_open.assert_not_called()


class BuildNewReleaseIssueBodyTests(unittest.TestCase):
    def test_body_contains_marker_and_tag(self):
        source = make_source(id="alsa-lib", last_reviewed_tag="v1.2.16.1")
        body = cut.build_new_release_issue_body(source, "v1.3.0")
        self.assertIn("<!-- upstream-tracking:alsa-lib:new-release:v1.3.0 -->", body)
        self.assertIn("v1.3.0", body)
        self.assertIn("v1.2.16.1", body)

    def test_body_mentions_automated_test_run_when_run_tests_on_release(self):
        source = make_source(run_tests_on_release=True)
        body = cut.build_new_release_issue_body(source, "v1.3.0")
        self.assertIn("automatically triggered", body)

    def test_body_omits_test_run_note_when_not_flagged(self):
        source = make_source(run_tests_on_release=False)
        body = cut.build_new_release_issue_body(source, "v1.3.0")
        self.assertNotIn("automatically triggered", body)


class OutputKeyTests(unittest.TestCase):
    def test_hyphens_are_replaced_with_underscores(self):
        self.assertEqual(cut.output_key("alsa-lib"), "alsa_lib")

    def test_already_valid_id_is_unchanged(self):
        self.assertEqual(cut.output_key("camilladsp"), "camilladsp")


class EmitGithubOutputTests(unittest.TestCase):
    def test_noop_when_github_output_env_var_unset(self):
        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop("GITHUB_OUTPUT", None)
            # Must not raise even though no file path is configured.
            cut.emit_github_output("alsa_lib_release_detected", "true")

    def test_appends_key_value_line_to_github_output_file(self):
        with tempfile.NamedTemporaryFile(mode="w", delete=False) as fh:
            path = fh.name
        try:
            with patch.dict(os.environ, {"GITHUB_OUTPUT": path}):
                cut.emit_github_output("alsa_lib_release_detected", "true")
                cut.emit_github_output("alsa_lib_release_tag", "v1.3.0")
            with open(path, encoding="utf-8") as fh:
                contents = fh.read()
            self.assertIn("alsa_lib_release_detected=true\n", contents)
            self.assertIn("alsa_lib_release_tag=v1.3.0\n", contents)
        finally:
            os.unlink(path)


if __name__ == "__main__":
    unittest.main()
