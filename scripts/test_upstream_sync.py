import json
import unittest
from unittest.mock import patch

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


if __name__ == "__main__":
    unittest.main()
