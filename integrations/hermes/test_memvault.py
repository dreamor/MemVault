"""Offline stdlib contract tests; urlopen is mocked, with no model or store."""

import importlib.util
import io
import json
import os
from pathlib import Path
import unittest
from unittest.mock import patch
import urllib.error
import urllib.parse


NAMED_AGENT = "hermes-contract-agent"
FAKE_KEY = "fake-hermes-api-key"
ENV_KEYS = (
    "MEMVAULT_HTTP_URL",
    "MEMVAULT_AGENT_ID",
    "MEMVAULT_API_KEY",
    "MEMVAULT_HOOK_EXTRACT",
    "MEMVAULT_TIMEOUT",
)


class MemVaultAuthTests(unittest.TestCase):
    def load_plugin(self, handler, **env):
        # Each test reloads actor config and restores every env/global mutation.
        self.enterContext(patch.dict(os.environ))
        for key in ENV_KEYS:
            os.environ.pop(key, None)
        os.environ.update({
            "MEMVAULT_HTTP_URL": "http://memvault.test",
            "MEMVAULT_HOOK_EXTRACT": "1",
            **env,
        })
        self.requests = []

        def urlopen(req, timeout):
            url = urllib.parse.urlsplit(req.full_url)
            self.assertEqual(url.netloc, "memvault.test")
            self.assertEqual(timeout, 5)
            request = {
                "url": url.path + (f"?{url.query}" if url.query else ""),
                "method": req.get_method(),
                "headers": {key.lower(): value for key, value in req.header_items()},
                "body": json.loads(req.data),
            }
            status, text = handler(request)
            self.requests.append({**request, "status": status})
            if status != 200:
                # Real urllib HTTPError, as urlopen raises for HTTP 401.
                raise urllib.error.HTTPError(
                    req.full_url, status, "Unauthorized", {}, io.BytesIO(text.encode())
                )
            return io.BytesIO(text.encode())

        self.enterContext(patch("urllib.request.urlopen", side_effect=urlopen))
        spec = importlib.util.spec_from_file_location(
            "hermes_memvault_contract", Path(__file__).with_name("__init__.py")
        )
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module, module.MemVaultPlugin()

    def keyed_response(self, request):
        # /extract authenticates headers; JSON agent_id is not authorization.
        actor = request["headers"].get("x-memvault-agent-id", "admin")
        if actor != NAMED_AGENT or request["headers"].get("x-memvault-api-key") != FAKE_KEY:
            return 401, "unauthorized"
        self.assertEqual(request["method"], "POST")
        self.assertEqual(request["body"]["agent_id"], actor)
        if request["url"] == "/api/session?output=plain":
            return 200, "remember this"
        self.assertEqual(request["url"], "/api/extract")
        self.assertEqual(request["body"]["source_id"], actor)
        self.assertIs(request["body"]["auto_save"], True)
        return self.extract_response()

    @staticmethod
    def extract_response():
        return 200, json.dumps({
            "ok": True,
            "data": {"memories": [{"content": "fake draft"}], "saved_ids": ["fake-draft-1"]},
        })

    def test_named_actor_authorizes_recall_and_extract_with_lazy_key(self):
        _, plugin = self.load_plugin(self.keyed_response, MEMVAULT_AGENT_ID=NAMED_AGENT)
        # Provisioned after import: key lookup must remain lazy.
        os.environ["MEMVAULT_API_KEY"] = FAKE_KEY
        self.assertEqual(plugin.pre_llm_call({"session_id": "session-1"}), "remember this")
        self.assertEqual(plugin.extract_session("remember this"), 1)
        self.assertEqual(
            [request["url"] for request in self.requests],
            ["/api/session?output=plain", "/api/extract"],
        )
        for request in self.requests:
            self.assertEqual(request["status"], 200)
            self.assertEqual(request["headers"]["x-memvault-agent-id"], NAMED_AGENT)
            self.assertEqual(request["headers"]["x-memvault-api-key"], FAKE_KEY)
            self.assertEqual(request["body"]["agent_id"], NAMED_AGENT)

    def test_default_actor_is_hermes_on_recall_and_extract_without_key(self):
        def response(request):
            if request["url"] == "/api/session?output=plain":
                return 200, "remember this"
            return self.extract_response()

        _, plugin = self.load_plugin(response)
        self.assertEqual(plugin.pre_llm_call({"session_id": "session-1"}), "remember this")
        self.assertEqual(plugin.extract_session("remember this"), 1)
        self.assertEqual(len(self.requests), 2)
        for request in self.requests:
            self.assertEqual(request["headers"]["x-memvault-agent-id"], "hermes")
            self.assertEqual(request["body"]["agent_id"], "hermes")
            self.assertNotIn("x-memvault-api-key", request["headers"])

    def test_wrong_key_401_does_not_mark_session_or_report_extraction_success(self):
        _, plugin = self.load_plugin(
            self.keyed_response,
            MEMVAULT_AGENT_ID=NAMED_AGENT,
            MEMVAULT_API_KEY="fake-wrong-api-key",
        )
        state = {"session_id": "session-1"}
        self.assertIsNone(plugin.pre_llm_call(state))
        self.assertIsNone(plugin.pre_llm_call(state))
        self.assertEqual(plugin._injected_sessions, set())
        self.assertIsNone(plugin.extract_session("remember this"))
        self.assertEqual([request["status"] for request in self.requests], [401, 401, 401])

        os.environ["MEMVAULT_API_KEY"] = FAKE_KEY
        self.assertEqual(plugin.pre_llm_call(state), "remember this")
        self.assertIsNone(plugin.pre_llm_call(state), "successful recall is injected only once")
        self.assertEqual(plugin.extract_session("remember this"), 1)
        self.assertEqual([request["status"] for request in self.requests], [401, 401, 401, 200, 200])

    def test_valid_key_for_another_actor_is_rejected(self):
        _, plugin = self.load_plugin(
            self.keyed_response,
            MEMVAULT_AGENT_ID="hermes-other-contract-agent",
            MEMVAULT_API_KEY=FAKE_KEY,
        )
        self.assertIsNone(plugin.pre_llm_call({"session_id": "session-1"}))
        self.assertIsNone(plugin.extract_session("remember this"))
        self.assertEqual([request["status"] for request in self.requests], [401, 401])
        self.assertEqual(plugin._injected_sessions, set())

    def test_missing_key_returns_401_without_false_success(self):
        _, plugin = self.load_plugin(self.keyed_response, MEMVAULT_AGENT_ID=NAMED_AGENT)
        self.assertIsNone(plugin.pre_llm_call({"session_id": "session-1"}))
        self.assertIsNone(plugin.extract_session("remember this"))
        self.assertEqual([request["status"] for request in self.requests], [401, 401])
        self.assertEqual(plugin._injected_sessions, set())

    def test_empty_success_still_marks_recall_as_injected(self):
        _, plugin = self.load_plugin(lambda request: (200, ""))
        state = {"session_id": "session-1"}
        self.assertIsNone(plugin.pre_llm_call(state))
        self.assertIsNone(plugin.pre_llm_call(state))
        self.assertEqual(len(self.requests), 1)
        self.assertEqual(plugin._injected_sessions, {"session-1"})

    def test_transport_failure_remains_best_effort_and_retryable(self):
        def offline(request):
            raise urllib.error.URLError("offline")

        _, plugin = self.load_plugin(offline)
        state = {"session_id": "session-1"}
        self.assertIsNone(plugin.pre_llm_call(state))
        self.assertIsNone(plugin.extract_session("remember this"))
        self.assertEqual(plugin._injected_sessions, set())


if __name__ == "__main__":
    unittest.main()
