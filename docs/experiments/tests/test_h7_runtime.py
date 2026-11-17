"""H7 lifecycle/REST evidence contracts; stdlib mocks only, no server or models."""
from __future__ import annotations

import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import MagicMock, patch
import urllib.error

from test_benchmarks import FakeHTTP, OfflineCase
# Reuse the SDK-free H-module import fixture, including when unittest discovers
# this file before the scoring tests (their import-call-count contract stays intact).
from test_hypothesis_scoring import h7


class H7RuntimeTests(OfflineCase):
    def setUp(self):
        super().setUp()
        self.root = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
        self.stack.enter_context(patch.object(h7, "free_port", return_value=4242))
        self.real_mkdtemp = tempfile.mkdtemp
        self.stack.enter_context(patch.object(
            h7.tempfile, "mkdtemp", side_effect=lambda **kwargs: self.real_mkdtemp(dir=self.root, **kwargs)))
        self.process = MagicMock(pid=12345)
        self.process.poll.return_value = None
        self.process.wait.return_value = 0
        self.popen.side_effect = None
        self.popen.return_value = self.process
        self.network.side_effect = lambda *args, **kwargs: FakeHTTP()

    def server(self, **kwargs):
        server = h7.MemVaultServer(Path("/fixture/memvault-mcp"), **kwargs)
        self.addCleanup(server.stop)
        return server

    def response(self, data, *, ok=True, error=None, status=200):
        self.network.side_effect = None
        self.network.return_value = FakeHTTP(json.dumps({"ok": ok, "error": error, "data": data}).encode(), status)

    def test_caller_evidence_exists_before_first_allocation_and_is_retained(self):
        evidence = {"caller_metadata": "keep", "records": []}

        def allocate_port():
            self.assertEqual(evidence["startup"]["status"], "starting")
            self.assertEqual(evidence["cleanup"]["errors"], [])
            self.assertIsInstance(evidence["records"], list)
            return 4242

        with patch.object(h7, "free_port", side_effect=allocate_port):
            server = self.server(evidence=evidence)
        self.assertIs(server.evidence, evidence)
        self.assertIs(server.records, evidence["records"])
        self.assertEqual(evidence["caller_metadata"], "keep")
        startup = evidence["startup"]
        self.assertEqual(startup["status"], "ready")
        self.assertEqual(startup["pid"], self.process.pid)
        self.assertEqual(startup["records"][0]["response_raw"], "ok")
        self.assertIs(startup["records"][0], server.records[0])

    def test_startup_popen_failure_cleans_and_exposes_evidence(self):
        evidence = {}
        server = h7.MemVaultServer.__new__(h7.MemVaultServer)

        def fail(*args, **kwargs):
            self.assertTrue(Path(evidence["startup"]["tempdir"]).is_dir())
            self.assertIs(server.evidence, evidence)
            self.assertIsNone(server.proc)
            raise OSError("fixture Popen failure")

        self.popen.side_effect = fail
        with self.assertRaises(RuntimeError) as caught:
            server.__init__(Path("/fixture/missing"), evidence=evidence)
        self.assertIs(caught.exception.startup_evidence, evidence)
        self.assertEqual(evidence["startup"]["status"], "failed")
        self.assertIn("Popen failure", evidence["startup"]["error"])
        self.assertTrue(evidence["cleanup"]["process_reaped"])
        self.assertTrue(evidence["cleanup"]["tempdir_removed"])
        self.assertFalse(Path(evidence["startup"]["tempdir"]).exists())
        self.network.assert_not_called()
        self.assertIs(server.stop(), evidence["cleanup"])

    def test_startup_interrupt_reaps_process_and_preserves_interrupted_health(self):
        evidence = {}
        self.network.side_effect = KeyboardInterrupt()
        with self.assertRaises(KeyboardInterrupt) as caught:
            self.server(evidence=evidence)
        self.assertIs(caught.exception.startup_evidence, evidence)
        self.assertEqual(evidence["startup"]["error"], "KeyboardInterrupt")
        self.assertEqual(evidence["startup"]["records"][0]["error"], "KeyboardInterrupt")
        self.assertTrue(evidence["cleanup"]["process_reaped"])
        self.assertTrue(evidence["cleanup"]["tempdir_removed"])
        self.assertFalse(Path(evidence["startup"]["tempdir"]).exists())
        self.process.terminate.assert_called_once()
        self.process.wait.assert_called_once()

    def test_environment_setup_failure_removes_owned_tempdir(self):
        evidence = {}
        with patch.object(h7.beam, "isolate_server_environment", side_effect=OSError("env-file failure")):
            with self.assertRaises(RuntimeError) as caught:
                self.server(evidence=evidence)
        self.assertIs(caught.exception.startup_evidence, evidence)
        self.popen.assert_not_called()
        self.assertTrue(evidence["cleanup"]["tempdir_removed"])
        self.assertFalse(Path(evidence["startup"]["tempdir"]).exists())

    def test_temp_allocation_failure_has_initialized_cleanup_and_runtime_error(self):
        evidence = {}
        with patch.object(h7.tempfile, "mkdtemp", side_effect=OSError("temp failure")):
            with self.assertRaises(RuntimeError) as caught:
                self.server(evidence=evidence)
        self.assertIs(caught.exception.startup_evidence, evidence)
        self.assertTrue(evidence["cleanup"]["process_reaped"])
        self.assertTrue(evidence["cleanup"]["tempdir_removed"])
        self.popen.assert_not_called()

    def test_startup_health_failures_retain_trace_and_cleanup_by_deadline(self):
        for payload, status in ((b'"ok"', 200), (b'{"ok": true}', 200), (b"not ok", 200), (b"ok", 503)):
            with self.subTest(payload=payload, status=status):
                evidence = {}
                self.network.side_effect = None
                self.network.return_value = FakeHTTP(payload, status)
                with patch.object(h7.time, "monotonic", side_effect=[0.0, 0.0, 0.2, 1.1]):
                    with self.assertRaises(RuntimeError) as caught:
                        self.server(evidence=evidence, readiness_timeout=1.0)
                self.assertIs(caught.exception.startup_evidence, evidence)
                health = evidence["startup"]["records"][0]
                self.assertEqual(health["response_raw"], payload.decode())
                self.assertEqual(health["status"], status)
                self.assertIsNotNone(health["error"])
                self.assertTrue(evidence["cleanup"]["process_reaped"])
                self.assertTrue(evidence["cleanup"]["tempdir_removed"])

    def test_startup_health_transport_failure_is_traced_before_raise(self):
        evidence = {}

        def fail(request, **kwargs):
            self.assertEqual(evidence["records"][-1]["request"]["path"], "/health")
            raise OSError("offline health failure")

        self.network.side_effect = fail
        with patch.object(h7.time, "monotonic", side_effect=[0.0, 0.0, 0.2, 1.1]):
            with self.assertRaises(RuntimeError):
                self.server(evidence=evidence, readiness_timeout=1.0)
        self.assertIn("offline health failure", evidence["startup"]["records"][0]["error"])
        self.assertTrue(evidence["cleanup"]["tempdir_removed"])

    def test_health_request_timeout_is_capped_and_late_success_is_not_ready(self):
        evidence = {}
        with patch.object(h7.time, "monotonic", side_effect=[0.0, 0.0, 0.3]):
            with self.assertRaises(RuntimeError):
                self.server(evidence=evidence, readiness_timeout=0.2)
        self.assertEqual(self.network.call_args.kwargs["timeout"], 0.2)
        self.assertEqual(evidence["startup"]["records"][0]["response_raw"], "ok")
        self.assertEqual(evidence["startup"]["status"], "failed")
        self.assertTrue(evidence["cleanup"]["tempdir_removed"])

    def test_startup_late_crash_after_health_is_reaped(self):
        evidence = {}
        self.process.poll.side_effect = [None, 17, 17]
        with self.assertRaises(RuntimeError) as caught:
            self.server(evidence=evidence)
        self.assertIn("exited during startup", str(caught.exception))
        self.assertEqual(evidence["startup"]["records"][0]["response_raw"], "ok")
        self.process.terminate.assert_not_called()
        self.process.wait.assert_called_once_with(timeout=5)
        self.assertTrue(evidence["cleanup"]["tempdir_removed"])

    def test_crash_after_failed_health_is_detected_without_waiting_full_timeout(self):
        evidence = {}
        self.network.side_effect = OSError("not listening")
        self.process.poll.side_effect = [None, 17, 17]
        with self.assertRaises(RuntimeError) as caught:
            self.server(evidence=evidence)
        self.assertIn("exited during startup", str(caught.exception))
        self.assertEqual(len(evidence["startup"]["records"]), 1)
        self.process.wait.assert_called_once_with(timeout=5)
        self.assertTrue(evidence["cleanup"]["tempdir_removed"])

    def test_stop_terminate_wait_kill_wait_then_remove_and_is_idempotent(self):
        server = self.server()
        directory = Path(server.dbdir)
        self.process.reset_mock()
        events = []
        self.process.terminate.side_effect = lambda: events.append("terminate")
        self.process.kill.side_effect = lambda: events.append("kill")

        def wait(*, timeout):
            self.assertEqual(timeout, 5)
            self.assertTrue(directory.exists())
            events.append("wait")
            if events == ["terminate", "wait"]:
                raise subprocess.TimeoutExpired("fixture", timeout)
            return 0

        self.process.wait.side_effect = wait
        real_remove = h7.shutil.rmtree

        def remove(path):
            events.append("remove")
            return real_remove(path)

        with patch.object(h7.shutil, "rmtree", side_effect=remove) as removal:
            cleanup = server.stop()
            self.assertEqual(events, ["terminate", "wait", "kill", "wait", "remove"])
            self.assertTrue(cleanup["process_reaped"])
            self.assertTrue(cleanup["tempdir_removed"])
            self.assertEqual(cleanup["errors"], [])
            self.assertIs(cleanup, server.evidence["cleanup"])
            self.assertIs(server.stop(), cleanup)
            removal.assert_called_once()
        self.assertFalse(directory.exists())

    def test_stop_reaps_already_exited_process_without_terminate(self):
        server = self.server()
        self.process.reset_mock()
        self.process.poll.return_value = 9
        cleanup = server.stop()
        self.process.terminate.assert_not_called()
        self.process.kill.assert_not_called()
        self.process.wait.assert_called_once_with(timeout=5)
        self.assertTrue(cleanup["process_reaped"])
        self.assertTrue(cleanup["tempdir_removed"])

    def test_runtime_late_crash_preserves_session_trace_and_is_reaped_on_stop(self):
        server = self.server()
        self.process.reset_mock()
        self.process.poll.return_value = 23
        self.network.side_effect = OSError("server crashed after readiness")
        trace = []
        with self.assertRaises(OSError):
            server.session_formatted("runtime context", trace=trace)
        self.assertEqual(len(trace), 1)
        self.assertIn("crashed after readiness", trace[0]["error"])
        self.assertEqual(server.evidence["startup"]["status"], "ready")
        cleanup = server.stop()
        self.process.terminate.assert_not_called()
        self.process.wait.assert_called_once_with(timeout=5)
        self.assertTrue(cleanup["process_reaped"])
        self.assertTrue(cleanup["tempdir_removed"])

    def test_kill_wait_failure_keeps_ownership_and_retry_can_cleanup(self):
        server = self.server()
        directory = Path(server.dbdir)
        self.process.reset_mock()
        self.process.wait.side_effect = [subprocess.TimeoutExpired("fixture", 5),
                                         subprocess.TimeoutExpired("fixture after kill", 5)]
        with patch.object(h7.shutil, "rmtree", wraps=h7.shutil.rmtree) as removal:
            cleanup = server.stop()
            self.process.kill.assert_called_once_with()
            self.assertEqual(self.process.wait.call_count, 2)
            removal.assert_not_called()
            self.assertFalse(cleanup["process_reaped"])
            self.assertFalse(cleanup["tempdir_removed"])
            self.assertTrue(directory.exists())
            self.assertEqual(server._owned_tempdir, directory)
            self.assertTrue(cleanup["errors"])
            self.process.wait.side_effect = None
            self.assertIs(server.stop(), cleanup)
            self.assertTrue(cleanup["process_reaped"])
            self.assertTrue(cleanup["tempdir_removed"])
            removal.assert_called_once()

    def test_terminate_and_kill_oserrors_are_recorded_without_masking_wait(self):
        server = self.server()
        self.process.reset_mock()
        self.process.terminate.side_effect = OSError("terminate unavailable")
        self.process.wait.side_effect = [subprocess.TimeoutExpired("fixture", 5), 0]
        self.process.kill.side_effect = OSError("kill unavailable")
        cleanup = server.stop()
        self.assertEqual(self.process.wait.call_count, 2)
        self.assertTrue(cleanup["process_reaped"])
        self.assertTrue(cleanup["tempdir_removed"])
        self.assertTrue(any("terminate unavailable" in error for error in cleanup["errors"]))
        self.assertTrue(any("kill unavailable" in error for error in cleanup["errors"]))

    def test_cleanup_oserror_preserves_temp_ownership_and_retries_without_process_work(self):
        server = self.server()
        directory = Path(server.dbdir)
        with patch.object(h7.shutil, "rmtree", side_effect=OSError("fixture removal failure")):
            cleanup = server.stop()
        self.assertTrue(cleanup["process_reaped"])
        self.assertFalse(cleanup["tempdir_removed"])
        self.assertTrue(directory.is_dir())
        self.assertIn("fixture removal failure", cleanup["errors"][0])
        process_calls = list(self.process.method_calls)
        self.assertIs(server.stop(), cleanup)
        self.assertTrue(cleanup["tempdir_removed"])
        self.assertEqual(self.process.method_calls, process_calls)

    def test_startup_primary_error_is_not_masked_by_cleanup_error(self):
        evidence = {}
        self.network.side_effect = OSError("primary health failure")
        self.process.wait.side_effect = [subprocess.TimeoutExpired("fixture", 5), OSError("secondary reap failure")]
        server = h7.MemVaultServer.__new__(h7.MemVaultServer)
        self.addCleanup(server.stop)
        with patch.object(h7.time, "monotonic", side_effect=[0.0, 0.0, 0.2, 1.1]):
            with self.assertRaises(RuntimeError) as caught:
                server.__init__(Path("/fixture/server"), evidence=evidence, readiness_timeout=1.0)
        self.assertIn("did not become healthy", str(caught.exception))
        self.assertIn("primary health failure", evidence["startup"]["records"][0]["error"])
        self.assertIn("secondary reap failure", evidence["cleanup"]["errors"][0])
        self.assertFalse(evidence["cleanup"]["tempdir_removed"])
        self.process.wait.side_effect = None
        self.assertTrue(server.stop()["tempdir_removed"])

    def test_cleanup_never_deletes_user_dirs_links_or_cache_targets(self):
        user = self.root / "user-home"
        user.mkdir()
        cache = user / "cache"
        cache.mkdir()
        marker = cache / "keep.txt"
        marker.write_text("user-owned")
        user_link = self.root / "user-link"
        user_link.symlink_to(user, target_is_directory=True)
        server = self.server()
        (Path(server.dbdir) / "cache-link").symlink_to(cache, target_is_directory=True)
        self.assertTrue(server.stop()["tempdir_removed"])
        self.assertEqual(marker.read_text(), "user-owned")
        self.assertTrue(user_link.is_symlink())

    def test_replaced_root_symlink_is_not_deleted_and_retry_retains_ownership(self):
        server = self.server()
        directory = Path(server.dbdir)
        saved = directory.with_name(directory.name + "-saved")
        directory.rename(saved)
        user = self.root / "user-owned"
        user.mkdir()
        directory.symlink_to(user, target_is_directory=True)
        cleanup = server.stop()
        self.assertFalse(cleanup["tempdir_removed"])
        self.assertTrue(directory.is_symlink())
        self.assertTrue(user.is_dir())
        self.assertTrue(cleanup["errors"])
        directory.unlink()
        saved.rename(directory)
        self.assertTrue(server.stop()["tempdir_removed"])

    def test_environment_pollution_cannot_escape_sqlite_temp_home_or_provider_off(self):
        polluted = {"HOME": "/user/home", "TMPDIR": "/user/tmp", "TEMP": "/user/temp", "TMP": "/user/tmp2",
                    "MEMVAULT_HOME": "/user/vault", "MEMVAULT_DB": "/user/db", "MEMVAULT_ENV_FILE": "/user/.env",
                    "MEMVAULT_STORAGE_BACKEND": "markdown", "MEMVAULT_AGENT_CONFIG": "/user/agents.yaml",
                    "MEMVAULT_MARKDOWN_DIR": "/user/markdown", "MEMVAULT_EMBEDDING_PROVIDER": "openai",
                    "MEMVAULT_EMBEDDING_API_KEY": "fixture-provider-secret", "MEMVAULT_EMBEDDING_CACHE_DIR": "/user/cache",
                    "OPENAI_API_KEY": "fixture-openai-secret", "UNRELATED_SETTING": "drop", "PATH": "/fixture/bin"}
        with patch.dict("os.environ", polluted, clear=True):
            server = self.server()
        command = self.popen.call_args.args[0]
        env = self.popen.call_args.kwargs["env"]
        home = Path(server.dbdir).resolve()
        for key in ("HOME", "TMPDIR", "TEMP", "TMP", "MEMVAULT_HOME"):
            self.assertEqual(env[key], str(home))
        self.assertEqual(env["MEMVAULT_STORAGE_BACKEND"], "sqlite")
        self.assertEqual(env["MEMVAULT_EMBEDDING_PROVIDER"], "off")
        self.assertEqual(env["PATH"], polluted["PATH"])
        self.assertEqual(command[command.index("--env-file") + 1], env["MEMVAULT_ENV_FILE"])
        self.assertEqual(Path(env["MEMVAULT_ENV_FILE"]).read_bytes(), b"")
        self.assertEqual(command[command.index("--db") + 1], str(home / "h7.db"))
        for key in ("MEMVAULT_AGENT_CONFIG", "MEMVAULT_MARKDOWN_DIR", "MEMVAULT_EMBEDDING_API_KEY",
                    "MEMVAULT_EMBEDDING_CACHE_DIR", "OPENAI_API_KEY", "UNRELATED_SETTING"):
            self.assertNotIn(key, env)
        self.assertNotIn("fixture-provider-secret", json.dumps(server.evidence))

    def test_numeric_settings_reject_nonpositive_nonfinite_bool_before_side_effects(self):
        invalid = (0, -1, float("nan"), float("inf"), float("-inf"), True, False, "1", None)
        for value in invalid:
            with self.subTest(readiness_timeout=value):
                with self.assertRaises(ValueError):
                    self.server(readiness_timeout=value)
        self.popen.assert_not_called()
        self.network.assert_not_called()
        self.assertEqual(list(self.root.iterdir()), [])
        server = self.server()
        self.network.reset_mock()
        before = len(server.records)
        for value in invalid:
            with self.subTest(timeout=value):
                with self.assertRaises(ValueError):
                    server._req("POST", "/api/session", {}, timeout=value)
            with self.subTest(count=value):
                with self.assertRaises(ValueError):
                    server.create_decoys("project:test", value)
        with self.assertRaises(ValueError):
            server.create_decoys("project:test", 1.5)
        self.network.assert_not_called()
        self.assertEqual(len(server.records), before)

    def test_rest_request_records_before_http_and_preserves_unsanitized_body(self):
        server = self.server()
        secret = "fixture-http-secret"
        body = {"api_key": secret, "context_hint": "keep context"}

        def send(request, **kwargs):
            self.assertEqual(json.loads(request.data), body)
            self.assertEqual(kwargs["timeout"], 4.5)
            record = server.records[-1]
            self.assertEqual(record["request"]["body"]["api_key"], "[REDACTED]")
            self.assertIsNone(record["response"])
            return FakeHTTP(json.dumps({"ok": True, "error": None, "data": {"api_key": secret}}).encode())

        self.network.side_effect = send
        response = server._req("POST", "/api/session", body, timeout=4.5)
        self.assertEqual(response["data"]["api_key"], secret)
        record = server.records[-1]
        self.assertEqual(record["status"], 200)
        self.assertIsNone(record["error"])
        self.assertEqual(record["response"]["data"]["api_key"], "[REDACTED]")
        self.assertNotIn(secret, record["response_raw"])

    def test_http_error_keeps_raw_parsed_status_and_session_call_trace(self):
        server = self.server()
        trace = []
        payload = {"ok": False, "error": "denied password=fixture-sensitive", "data": None}
        raw = json.dumps(payload).encode()
        error = urllib.error.HTTPError(server.base + "/api/session", 401, "Unauthorized", {}, io.BytesIO(raw))
        self.network.side_effect = error
        with self.assertRaises(urllib.error.HTTPError):
            server.session_formatted("context", trace=trace)
        self.assertEqual(len(trace), 1)
        self.assertIs(trace[0], server.records[-1])
        self.assertEqual(trace[0]["status"], 401)
        self.assertFalse(trace[0]["response"]["ok"])
        self.assertNotIn("fixture-sensitive", json.dumps(trace))
        self.assertIn("Unauthorized", trace[0]["error"])

    def test_http_error_body_read_failure_does_not_mask_primary_error(self):
        server = self.server()
        stream = MagicMock()
        stream.read.side_effect = OSError("body read failed")
        error = urllib.error.HTTPError(server.base + "/api/session", 500, "primary HTTP failure", {}, stream)
        self.network.side_effect = error
        with self.assertRaises(urllib.error.HTTPError) as caught:
            server._req("POST", "/api/session", {})
        self.assertIs(caught.exception, error)
        self.assertIn("body read failed", server.records[-1]["response_read_error"])
        self.assertIn("primary HTTP failure", server.records[-1]["error"])

    def test_rest_api_and_envelope_schema_failures_are_not_valid_empty_injections(self):
        invalid = [{"ok": False, "error": "server error", "data": {}},
                   {"ok": True, "error": "unexpected error", "data": {}},
                   {"ok": 1, "error": None, "data": {}},
                   {"ok": "true", "error": None, "data": {}},
                   {"ok": True, "data": {}}, {"error": None, "data": {}},
                   {"ok": True, "error": None}, {"ok": True, "error": None, "data": None},
                   {"ok": True, "error": None, "data": []}, [], None]
        server = self.server()
        for payload in invalid:
            with self.subTest(payload=payload):
                self.network.side_effect = None
                self.network.return_value = FakeHTTP(json.dumps(payload).encode())
                trace = []
                with self.assertRaises(RuntimeError):
                    server.session_formatted("context", trace=trace)
                self.assertEqual(len(trace), 1)
                self.assertEqual(trace[0]["response"], payload)
                self.assertIsNotNone(trace[0]["error"])

    def test_malformed_json_retains_raw_and_status_before_parse_error(self):
        server = self.server()
        self.network.side_effect = None
        self.network.return_value = FakeHTTP(b"not-json", 200)
        trace = []
        with self.assertRaises(json.JSONDecodeError):
            server.session_formatted("context", trace=trace)
        self.assertEqual(trace[0]["response_raw"], "not-json")
        self.assertEqual(trace[0]["status"], 200)
        self.assertIsNone(trace[0]["response"])
        self.assertIsNotNone(trace[0]["error"])

    def test_transport_exception_records_before_raise_and_copies_per_call_trace(self):
        server = self.server()
        self.response({"formatted": "first"})
        earlier = []
        self.assertEqual(server.session_formatted("first context", trace=earlier), "first")
        trace = []

        def fail(request, **kwargs):
            self.assertEqual(server.records[-1]["request"]["body"]["context_hint"], "next context")
            raise OSError("password=fixture-private offline")

        self.network.side_effect = fail
        with self.assertRaises(OSError):
            server.session_formatted("next context", trace=trace)
        self.assertEqual(len(trace), 1)
        self.assertEqual(len(earlier), 1)
        self.assertIsNot(trace[0], earlier[0])
        self.assertNotIn("fixture-private", json.dumps(trace))
        self.assertIn("offline", trace[0]["error"])

    def test_session_missing_or_nonstring_formatted_is_error_and_traced(self):
        server = self.server()
        for data in ({}, {"formatted": None}, {"formatted": False}, {"formatted": 0}, {"formatted": []}):
            with self.subTest(data=data):
                self.response(data)
                trace = []
                with self.assertRaises(RuntimeError):
                    server.session_formatted("context", trace=trace)
                self.assertEqual(len(trace), 1)
                self.assertIn("formatted", trace[0]["error"])

    def test_explicit_empty_formatted_and_memory_skipped_list_are_valid(self):
        server = self.server()
        self.response({"formatted": "", "skipped_channel": None, "skipped": [{"id": "memory", "reason": "budget"}]})
        trace = []
        self.assertEqual(server.session_formatted("context", project="test-project", trace=trace), "")
        self.assertEqual(len(trace), 1)
        self.assertIsNone(trace[0]["error"])
        self.assertEqual(json.loads(self.network.call_args.args[0].data),
                         {"agent_id": "claude-desktop", "context_hint": "context", "project": "test-project"})
        self.response({"formatted": "text"})
        self.assertEqual(server.session_formatted("context", project=""), "text")
        self.assertNotIn("project", json.loads(self.network.call_args.args[0].data))

    def test_known_rest_skipped_channel_is_not_valid_empty_injection(self):
        server = self.server()
        self.response({"formatted": "", "count": 0, "format": "none", "skipped": [], "skipped_channel": "sync"})
        trace = []
        with self.assertRaises(RuntimeError):
            server.session_formatted("context", trace=trace)
        self.assertEqual(len(trace), 1)
        self.assertEqual(trace[0]["response"]["data"]["skipped_channel"], "sync")
        self.assertIn("skipped", trace[0]["error"])

    def test_create_skill_preserves_body_and_rejects_bad_ids(self):
        server = self.server()
        scenario = {"skill": {"content": "fixture skill", "trigger": "fixture trigger",
                              "steps": ["step one", "step two"], "verification": "fixture verified"}}
        self.response({"id": "memory-fixture"})
        self.assertEqual(server.create_skill(scenario), "memory-fixture")
        self.assertEqual(json.loads(self.network.call_args.args[0].data),
                         {"content": "fixture skill", "type": "skill", "priority": "REFERENCE", "agent_id": "h7",
                          "human_reviewed": True, "ai_generated": False, "skill_trigger": "fixture trigger",
                          "skill_steps": ["step one", "step two"], "skill_verification": "fixture verified"})
        for data in ({}, {"id": None}, {"id": ""}, {"id": " \n"}, {"id": True}, {"id": 7}, {"id": []}):
            with self.subTest(data=data):
                self.response(data)
                with self.assertRaises(RuntimeError):
                    server.create_skill(scenario)
                self.assertIn("id", server.records[-1]["error"])

    def test_create_decoys_preserves_body_and_validates_every_create(self):
        server = self.server()
        self.network.side_effect = lambda *args, **kwargs: FakeHTTP(json.dumps(
            {"ok": True, "error": None, "data": {"id": "decoy-fixture"}}).encode())
        self.network.reset_mock()
        server.create_decoys("project:fixture", 2)
        self.assertEqual(self.network.call_count, 2)
        for index, call in enumerate(self.network.call_args_list):
            self.assertEqual(json.loads(call.args[0].data),
                             {"content": f"project note number {index} about internal trivia", "type": "fact",
                              "priority": "REFERENCE", "namespace": "project:fixture", "agent_id": "h7",
                              "human_reviewed": True, "ai_generated": False})
        self.response({"id": ""})
        self.network.reset_mock()
        with self.assertRaises(RuntimeError):
            server.create_decoys("project:fixture", 3)
        self.assertEqual(self.network.call_count, 1)


if __name__ == "__main__":
    unittest.main()
