import json
import unittest
from pathlib import Path
from unittest.mock import patch

import yaml

from scripts import upstream_sync


class GhRequestTests(unittest.TestCase):
    def test_gh_request_sends_github_token_as_bearer_auth(self) -> None:
        seen = {}

        class FakeResponse:
            def __enter__(self):
                return self

            def __exit__(self, exc_type, exc, tb):
                return False

            def read(self):
                return json.dumps({"ok": True}).encode()

        def fake_urlopen(request, timeout):
            headers = {key.lower(): value for key, value in request.header_items()}
            seen["authorization"] = headers.get("authorization")
            seen["accept"] = headers.get("accept")
            seen["api_version"] = headers.get("x-github-api-version")
            seen["timeout"] = timeout
            return FakeResponse()

        with patch("urllib.request.urlopen", side_effect=fake_urlopen):
            response = upstream_sync.gh_request("repos/example/project", "test-token")

        self.assertEqual(response, {"ok": True})
        self.assertEqual(seen["authorization"], "Bearer " + "test-token")
        self.assertEqual(seen["accept"], "application/vnd.github+json")
        self.assertEqual(seen["api_version"], "2022-11-28")
        self.assertEqual(seen["timeout"], 30)


class UpstreamSyncWorkflowTests(unittest.TestCase):
    def test_workflow_declares_issue_fallback_for_pr_policy_block(self) -> None:
        workflow_path = (
            Path(__file__).resolve().parent.parent
            / ".github"
            / "workflows"
            / "upstream-sync.yml"
        )
        workflow = yaml.safe_load(workflow_path.read_text())

        self.assertEqual(workflow["permissions"]["issues"], "write")

        steps = workflow["jobs"]["sync"]["steps"]
        open_pr_step = next(
            (step for step in steps if step.get("id") == "open_pr"),
            None,
        )
        fallback_step = next(
            (
                step
                for step in steps
                if step.get("name") == "Fallback when PR creation is blocked"
            ),
            None,
        )
        self.assertIsNotNone(open_pr_step, "Expected a workflow step with id='open_pr'")
        self.assertIsNotNone(
            fallback_step,
            "Expected a workflow step named 'Fallback when PR creation is blocked'",
        )

        self.assertIn(
            "GitHub Actions is not permitted to create or approve pull requests",
            open_pr_step["run"],
        )
        self.assertEqual(open_pr_step["env"]["BRANCH"], "${{ steps.push_branch.outputs.branch }}")
        self.assertEqual(open_pr_step["env"]["PR_TITLE"], "${{ steps.push_branch.outputs.pr_title }}")
        self.assertEqual(fallback_step["env"]["REPO"], "${{ github.repository }}")
        self.assertIn("--body-file /tmp/upstream-sync-issue-body.md", fallback_step["run"])
        self.assertEqual(
            fallback_step["if"],
            "steps.diff.outputs.changed == 'true' && "
            "steps.open_pr.outputs.blocked_by_policy == 'true'",
        )


if __name__ == "__main__":
    unittest.main()
