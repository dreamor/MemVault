"""MAB validity contracts; stdlib-only, no real network, subprocess or model calls."""
from __future__ import annotations

import contextlib
import copy
import io
import itertools
import json
from pathlib import Path
from types import SimpleNamespace
import sys
import tempfile
import unittest
from unittest.mock import MagicMock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import memory_agent_bench as mab

FIXTURE = Path(__file__).parent / "fixtures/tiny_rows.json"


class FakeHTTP:
    def __init__(self, body=b"ok", status=200):
        self.body, self.status = body, status

    def __enter__(self):
        return self

    def __exit__(self, *args):
        pass

    def read(self):
        return self.body


def completion(text, finish_reason="stop"):
    return SimpleNamespace(choices=[SimpleNamespace(message=SimpleNamespace(content=text), finish_reason=finish_reason)],
                           usage=SimpleNamespace(prompt_tokens=10, completion_tokens=3))


def client_for(*texts):
    create = MagicMock(side_effect=[completion(t) if isinstance(t, str) else t for t in texts])
    return SimpleNamespace(chat=SimpleNamespace(completions=SimpleNamespace(create=create)))


class FakeServer:
    hits = None
    instances = []

    def __init__(self, binary, mode="keyword", provider="off", environment=None,
                 session_agent="codex", session_project=None, **kwargs):
        self.records, self.facts, self.mode = [], [], mode
        self.session_agent, self.session_project = session_agent, session_project
        self.stopped = False
        self.readiness = {"status": "ready", "note": "not query proof"}
        self.instances.append(self)

    def seed_fact(self, text, namespace):
        self.facts.append(text)
        return "memory-" + str(len(self.facts))

    def search(self, question, top_k, namespace):
        hits = copy.deepcopy(self.hits) if self.hits is not None else [
            {"memory": {"content": self.facts[2], "id": "memory-3"}, "search_mode": self.mode}]
        self.records.append({"request": {"path": "/api/search", "mode": self.mode},
                             "response": {"data": hits}, "response_raw": json.dumps({"data": hits})})
        return hits, 0.001

    def stop(self):
        self.stopped = True


class SeedEvidenceServer(FakeServer):
    seed_metadata = {"embedded": True, "vector_pending": False}

    def seed_fact(self, text, namespace):
        memory_id = super().seed_fact(text, namespace)
        data = {"id": memory_id, "action": "saved", **self.seed_metadata}
        self.records.append({"request": {"method": "POST", "path": "/api/memories"},
                             "response": {"ok": True, "data": data},
                             "response_raw": json.dumps({"ok": True, "data": data})})
        return memory_id

    def seed_managed_fact(self, text, namespace, assertion, mode):
        memory_id = self.seed_fact(text, namespace)
        return {"id": memory_id, "written": True, "fact_outcome": "inserted", **self.seed_metadata}


class ValidityTests(unittest.TestCase):
    def setUp(self):
        self.stack = contextlib.ExitStack()
        self.addCleanup(self.stack.close)
        self.network = self.stack.enter_context(patch("urllib.request.urlopen", side_effect=AssertionError("unexpected network")))
        self.popen = self.stack.enter_context(patch("subprocess.Popen", side_effect=AssertionError("unexpected process")))
        self.stack.enter_context(patch("subprocess.run", return_value=SimpleNamespace(returncode=0, stdout="fixture")))
        self.stack.enter_context(patch("time.sleep"))
        self.stack.enter_context(patch.object(mab, "free_port", return_value=1234))
        self.stack.enter_context(patch.dict("os.environ", {"HOME": "/fixture/parent-home"}, clear=True))
        self.row = json.loads(FIXTURE.read_text())["mab"]

    def run_main(self, extra=(), server=FakeServer, texts=("Current City", "Current City"), row=None, client_error=None):
        directory = self.stack.enter_context(tempfile.TemporaryDirectory())
        source, output = Path(directory) / "source.parquet", Path(directory) / "result.json"
        source.write_bytes(b"fixture source bytes")
        argv = ["--parquet", str(source), "--binary", str(source), "--subsets", "tiny", "--questions", "1",
                "--arm", "both", "--out", str(output), *extra]
        def load(path, evidence):
            evidence.update({"source_sha256": mab.content_hash(path)})
            return [copy.deepcopy(self.row if row is None else row)]
        client = client_for(*texts)
        stdout, stderr = io.StringIO(), io.StringIO()
        with patch.object(mab, "load_rows", side_effect=load), patch.object(mab, "fetch_parquet", return_value=source), \
             patch.object(mab, "MemVaultServer", server), patch.object(mab, "make_client", return_value=client, side_effect=client_error) as make_client, \
             contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = mab.main(argv)
        self.assertTrue(output.is_file())
        return code, json.loads(output.read_text()), stdout.getvalue() + stderr.getvalue(), client, make_client

    def server_http(self, health):
        def open_url(request, **kwargs):
            if mab.urllib.parse.urlparse(request.full_url).path == "/health":
                return FakeHTTP()
            self.assertTrue(request.full_url.endswith("/api/health"))
            return FakeHTTP(json.dumps(health).encode())
        return open_url

    def test_native_parser_and_legacy_local_alias_are_distinct(self):
        for provider in ("native", "local", "ollama"):
            args = mab.build_parser().parse_args(["--embedding-provider", provider])
            self.assertEqual(args.embedding_provider, provider)
            self.assertEqual(mab.child_environment(provider)["MEMVAULT_EMBEDDING_PROVIDER"], provider)

    def test_native_cache_parent_expand_resolve_and_whitelist(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict("os.environ", {
                "MEMVAULT_EMBEDDING_CACHE_DIR": "/fixture/unrequested-cache",
                "MEMVAULT_HOME": "/fixture/real-vault", "OPENAI_API_KEY": "fixture-parent-key",
                "MEMVAULT_STORAGE_BACKEND": "markdown"}):
            env = mab.child_environment("native", native_cache_dir="~/cache/../native-models")
            self.assertEqual(env["MEMVAULT_EMBEDDING_CACHE_DIR"], "/fixture/parent-home/native-models")
            self.assertNotIn("MEMVAULT_EMBEDDING_CACHE_DIR", mab.child_environment("native"))
            with patch("subprocess.Popen", return_value=MagicMock()) as started, \
                 patch("urllib.request.urlopen", return_value=FakeHTTP()), patch("tempfile.mkdtemp", return_value=directory):
                server = mab.MemVaultServer(Path("/fixture/binary"), environment=env)
                child = started.call_args.kwargs["env"]
                self.assertEqual(child["MEMVAULT_EMBEDDING_CACHE_DIR"], env["MEMVAULT_EMBEDDING_CACHE_DIR"])
                self.assertEqual(child["HOME"], str(Path(directory).resolve()))
                self.assertEqual(child["MEMVAULT_HOME"], child["HOME"])
                self.assertEqual(child["MEMVAULT_STORAGE_BACKEND"], "sqlite")
                self.assertNotIn("OPENAI_API_KEY", child)
                self.assertEqual(Path(child["MEMVAULT_ENV_FILE"]).read_bytes(), b"")
                server.stop()

    def test_cache_empty_and_non_native_combinations_rejected(self):
        for provider, cache in (("native", ""), ("native", "  "), ("off", "cache"), ("local", "cache")):
            with self.subTest(provider=provider, cache=cache):
                with self.assertRaises(ValueError):
                    mab.child_environment(provider, native_cache_dir=cache)
                with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                    mab.main(["--embedding-provider", provider, "--native-cache-dir", cache])
                self.assertEqual(error.exception.code, 2)
        self.popen.assert_not_called()
        self.network.assert_not_called()

    def test_readiness_timeout_requires_positive_finite_value(self):
        for value in ("0", "-1", "nan", "inf"):
            with self.subTest(value=value), contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                mab.main(["--embedding-ready-timeout", value])
            self.assertEqual(error.exception.code, 2)

    def test_keyword_does_not_require_embeddings_and_records_liveness(self):
        with tempfile.TemporaryDirectory() as directory, patch("tempfile.mkdtemp", return_value=directory), \
             patch("subprocess.Popen", return_value=MagicMock()), patch("urllib.request.urlopen", return_value=FakeHTTP()) as opened:
            server = mab.MemVaultServer(Path("/fixture/binary"), provider="off")
            self.assertEqual(server.readiness["status"], "not_required")
            self.assertEqual(opened.call_count, 1)
            self.assertEqual(server.records[0]["response_raw"], "ok")
            server.stop()

    def test_semantic_requires_expected_and_ready_real_booleans(self):
        for health in ({"embedding_expected": False, "embedding_ready": False},
                       {"embedding_expected": "true", "embedding_ready": True},
                       {"embedding_expected": True, "embedding_ready": 1},
                       {"embedding_expected": True}):
            with self.subTest(health=health), tempfile.TemporaryDirectory() as parent:
                owned = Path(parent) / "owned"
                owned.mkdir()
                process = MagicMock()
                with patch("tempfile.mkdtemp", return_value=str(owned)), patch("subprocess.Popen", return_value=process), \
                     patch("urllib.request.urlopen", side_effect=self.server_http(health)), self.assertRaises(RuntimeError) as error:
                    mab.MemVaultServer(Path("/fixture/binary"), mode="semantic")
                evidence = error.exception.startup_evidence
                self.assertEqual(evidence["readiness"]["status"], "failed")
                self.assertEqual(evidence["readiness"]["records"][0]["response"], health)
                self.assertEqual(json.loads(evidence["readiness"]["records"][0]["response_raw"]), health)
                process.terminate.assert_called_once()
                process.wait.assert_called_once()
                self.assertFalse(owned.exists())

    def test_readiness_timeout_retains_last_raw_health_and_cleans_process(self):
        health = {"embedding_expected": True, "embedding_ready": False}
        with tempfile.TemporaryDirectory() as parent:
            owned = Path(parent) / "owned"
            owned.mkdir()
            process = MagicMock()
            process.poll.return_value = None
            with patch("tempfile.mkdtemp", return_value=str(owned)), patch("subprocess.Popen", return_value=process), \
                 patch("urllib.request.urlopen", side_effect=self.server_http(health)), \
                 patch.object(mab.time, "monotonic", side_effect=itertools.count(0, 0.01)), self.assertRaisesRegex(RuntimeError, "timed out") as error:
                mab.MemVaultServer(Path("/fixture/binary"), mode="hybrid", readiness_timeout=0.1)
            records = error.exception.startup_evidence["readiness"]["records"]
            self.assertGreater(len(records), 0)
            self.assertEqual(json.loads(records[-1]["response_raw"]), health)
            process.terminate.assert_called_once()
            self.assertFalse(owned.exists())

    def test_ready_health_is_not_query_mode_proof(self):
        health = {"embedding_expected": True, "embedding_ready": True}
        with tempfile.TemporaryDirectory() as directory, patch("tempfile.mkdtemp", return_value=directory), \
             patch("subprocess.Popen", return_value=MagicMock()), patch("urllib.request.urlopen", side_effect=self.server_http(health)):
            server = mab.MemVaultServer(Path("/fixture/binary"), mode="hybrid", provider="native")
            self.assertEqual(server.readiness["status"], "ready")
            self.assertEqual(mab.retrieval_modes([], "hybrid")["status"], "unknown")
            self.assertFalse(mab.retrieval_modes([{"search_mode": "keyword"}], "hybrid")["valid"])
            server.stop()

    def test_every_hit_must_report_legal_exact_mode(self):
        cases = (([], "unknown"), ([{}], "unknown"), ([{"search_mode": "hybrid"}, {}], "unknown"),
                 ([{"search_mode": "hybrid"}, {"search_mode": "bogus"}], "unknown"),
                 ([{"search_mode": "hybrid"}, {"search_mode": None}], "unknown"),
                 ([{"search_mode": "hybrid"}, {"search_mode": "keyword"}], "degraded"),
                 ([{"search_mode": "keyword"}, {}], "degraded"),
                 ([{"search_mode": "hybrid", "fallback_reason": "fixture fallback"}], "degraded"),
                 ([{"search_mode": "hybrid", "fallback": True}], "degraded"),
                 ([{"search_mode": "hybrid"}, {"search_mode": "hybrid"}], "valid"))
        for hits, expected in cases:
            with self.subTest(hits=hits):
                result = mab.retrieval_modes(hits, "hybrid")
                self.assertEqual(result["status"], expected)
                self.assertEqual(result["valid"], expected == "valid")
        result = mab.retrieval_modes([{"search_mode": "hybrid"}, {}], "hybrid")
        self.assertEqual(result["actual_mode_by_hit"], ["hybrid", None])
        self.assertEqual(result["n_unknown_hits"], 1)
        self.assertIsNone(result["fallback_observed"])

    def test_valid_complete_and_dry_run_root_status(self):
        code, out, text, _, _ = self.run_main()
        self.assertEqual(code, 0)
        self.assertEqual(out["run_status"]["status"], "scoring_complete")
        self.assertEqual(out["run_status"]["n_valid_scoring_pairs"], 1)
        self.assertIn("SCORING COMPLETE", text)
        code, out, text, client, make_client = self.run_main(extra=("--dry-run",))
        self.assertEqual(code, 0)
        self.assertEqual(out["run_status"]["status"], "dry_run_complete")
        self.assertNotIn("SCORING COMPLETE", text)
        make_client.assert_not_called()
        client.chat.completions.create.assert_not_called()
        self.assertTrue(FakeServer.instances[-1].stopped)

    def test_invalid_modes_save_artifacts_and_do_not_claim_score(self):
        for hits, expected in (([], "unknown"), ([{"search_mode": "keyword"}, {}], "unknown"),
                               ([{"search_mode": "semantic"}], "degraded")):
            with self.subTest(hits=hits):
                class BadModes(FakeServer):
                    pass
                BadModes.hits = hits
                code, out, text, _, _ = self.run_main(server=BadModes)
                self.assertEqual(code, 1)
                self.assertEqual(out["run_status"]["status"], "invalid")
                self.assertEqual(out["run_status"]["retrieval_mode_counts"][expected], 1)
                self.assertEqual(out["run_status"]["n_valid_scoring_pairs"], 0)
                question = out["results"][0]["questions"][0]
                self.assertEqual(question["retrieved_objects"], hits)
                self.assertIn("search_prediction", question)
                self.assertIsNone(out["results"][0]["summary"]["search_accuracy"])
                self.assertNotIn("SCORING COMPLETE", text)

    def test_unknown_modes_make_dry_run_invalid_too(self):
        class Empty(FakeServer):
            hits = []
        code, out, _, _, make_client = self.run_main(server=Empty, extra=("--dry-run",))
        self.assertEqual(code, 1)
        self.assertEqual(out["run_status"]["status"], "invalid")
        make_client.assert_not_called()

    def test_zero_questions_artifact_nonzero_no_model_or_process(self):
        code, out, text, _, make_client = self.run_main(extra=("--questions", "0"))
        self.assertEqual(code, 1)
        self.assertIn("zero_questions", out["run_status"]["errors"])
        self.assertNotIn("SCORING COMPLETE", text)
        make_client.assert_not_called()
        self.popen.assert_not_called()

    def test_failed_query_keeps_question_slot_and_raw_request(self):
        class Failing(FakeServer):
            def search(self, question, top_k, namespace):
                self.records.append({"request": {"path": "/api/search"}, "response_raw": "fixture partial raw",
                                     "error": "fixture endpoint failed"})
                raise RuntimeError("fixture endpoint failed")
        code, out, text, _, _ = self.run_main(server=Failing)
        self.assertEqual(code, 1)
        self.assertEqual(out["run_status"]["n_question_errors"], 1)
        self.assertEqual(out["run_status"]["n_request_errors"], 1)
        q = out["results"][0]["questions"][0]
        self.assertEqual(q["search_records"][0]["response_raw"], "fixture partial raw")
        self.assertFalse(q["execution_complete"])
        self.assertEqual(q["question"], self.row["questions"][0])
        self.assertNotIn("SCORING COMPLETE", text)
        self.assertTrue(Failing.instances[-1].stopped)

    def test_partial_model_run_preserves_predictions_and_error_counts(self):
        row = copy.deepcopy(self.row)
        row["questions"] *= 2
        row["answers"] *= 2
        row["metadata"]["qa_pair_ids"] = ["tiny_no1", "tiny_no2"]
        code, out, text, _, _ = self.run_main(extra=("--questions", "2"), row=row,
            texts=("Current City", "Current City", "wrong", RuntimeError("401 fixture denied")))
        self.assertEqual(code, 1)
        self.assertEqual(out["run_status"]["n_expected_questions"], 2)
        self.assertEqual(out["run_status"]["n_completed_questions"], 1)
        self.assertEqual(out["run_status"]["n_valid_scoring_pairs"], 1)
        self.assertGreater(out["run_status"]["n_request_errors"], 0)
        self.assertIn("partial_run", out["run_status"]["errors"])
        q = out["results"][0]["questions"][1]
        self.assertEqual(q["search_prediction"], "wrong")
        self.assertIn("401", q["control_answer_records"][0]["error"])
        self.assertNotIn("SCORING COMPLETE", text)

    def test_client_constructor_failure_still_writes_root_artifact(self):
        code, out, text, _, _ = self.run_main(client_error=RuntimeError("fixture client configuration failure"))
        self.assertEqual(code, 1)
        self.assertEqual(out["run_error"], "fixture client configuration failure")
        self.assertEqual(out["run_status"]["status"], "invalid")
        self.assertEqual(out["server_records"], [])
        self.assertNotIn("SCORING COMPLETE", text)
        self.popen.assert_not_called()

    def test_server_constructor_failure_still_writes_root_artifact(self):
        class FailingConstructor:
            def __init__(self, *args, **kwargs):
                raise RuntimeError("fixture constructor failed")
        code, out, text, _, _ = self.run_main(server=FailingConstructor)
        self.assertEqual(code, 1)
        self.assertEqual(out["run_error"], "fixture constructor failed")
        self.assertEqual(out["run_status"]["status"], "invalid")
        self.assertEqual(out["server_records"], [])
        self.assertNotIn("SCORING COMPLETE", text)

    def test_popen_failure_cleans_partial_tmp_and_retains_isolation(self):
        with tempfile.TemporaryDirectory() as parent:
            owned = Path(parent) / "owned"
            owned.mkdir()
            with patch("tempfile.mkdtemp", return_value=str(owned)), \
                 patch("subprocess.Popen", side_effect=OSError("fixture cannot spawn")), self.assertRaisesRegex(RuntimeError, "cannot spawn") as error:
                mab.MemVaultServer(Path("/fixture/binary"))
            self.assertFalse(owned.exists())
            evidence = error.exception.startup_evidence
            self.assertEqual(evidence["isolation"]["data_dir"], str(owned.resolve()))
            self.assertEqual(evidence["isolation"]["env_file_sha256"], mab.hashlib.sha256(b"").hexdigest())
            self.assertEqual(evidence["records"], [])

    def test_environment_construction_failure_cleans_tmp_before_popen(self):
        with tempfile.TemporaryDirectory() as parent:
            owned = Path(parent) / "owned"
            owned.mkdir()
            with patch("tempfile.mkdtemp", return_value=str(owned)), \
                 patch.object(mab, "isolate_server_environment", side_effect=RuntimeError("fixture isolation failure")), \
                 self.assertRaisesRegex(RuntimeError, "isolation failure") as error:
                mab.MemVaultServer(Path("/fixture/binary"))
            self.assertFalse(owned.exists())
            self.assertEqual(error.exception.startup_evidence["isolation"]["data_dir"], str(owned.resolve()))
            self.popen.assert_not_called()

    def test_startup_failure_main_saves_readiness_and_sanitized_raw_evidence(self):
        with tempfile.TemporaryDirectory() as parent:
            owned = Path(parent) / "owned"
            owned.mkdir()
            process = MagicMock()
            health = {"embedding_expected": False, "embedding_ready": False,
                      "detail": "X-MemVault-Api-Key: fixture-startup-secret"}
            mkdtemp = tempfile.mkdtemp
            def temp_dir(*args, **kwargs):
                return str(owned) if kwargs.get("prefix") == "memvault-mab-" else mkdtemp(*args, **kwargs)
            with patch("tempfile.mkdtemp", side_effect=temp_dir), patch("subprocess.Popen", return_value=process), \
                 patch("urllib.request.urlopen", side_effect=self.server_http(health)):
                code, out, text, _, _ = self.run_main(server=mab.MemVaultServer,
                                                     extra=("--search-mode", "hybrid", "--embedding-provider", "off"))
            self.assertEqual(code, 1)
            self.assertEqual(out["run_status"]["status"], "invalid")
            self.assertEqual(out["harness_spec"]["memvault"]["readiness"]["status"], "failed")
            self.assertGreater(len(out["server_records"]), 1)
            self.assertNotIn("fixture-startup-secret", json.dumps(out))
            self.assertNotIn("SCORING COMPLETE", text)
            process.terminate.assert_called_once()
            self.assertFalse(owned.exists())

    def test_historical_scores_without_mode_contract_not_rewritten(self):
        questions = [{"golds": ["Current City"], "search_prediction": "Current City", "search_correct": True,
                      "control_prediction": "wrong", "control_correct": False}]
        before = copy.deepcopy(questions)
        scores = mab.question_score_contract(questions, "both")
        self.assertEqual(scores["paired_search_control"]["n"], 1)
        self.assertEqual(questions, before)

    def test_first_managed_context_parse_failure_retains_artifact_and_cleans_server(self):
        row = copy.deepcopy(self.row)
        row["context"] = None
        for mode in ("managed_raw", "resolved"):
            with self.subTest(mode=mode):
                code, out, text, _, _ = self.run_main(row=row, extra=("--ingest-mode", mode, "--dry-run"))
                self.assertEqual(code, 1)
                self.assertEqual(out["run_status"]["status"], "invalid")
                self.assertNotIn("UnboundLocalError", out["run_error"])
                entry = out["results"][0]
                self.assertFalse(entry["context_parse_complete"])
                self.assertIsNone(entry["context_parse_error_input"])
                self.assertIsNone(entry["ingest_diagnostics"]["n_corpus_facts"])
                self.assertIsNone(entry["ingest_diagnostics"]["n_seed_unattempted"])
                self.assertEqual(entry["facts"], [])
                self.assertTrue(FakeServer.instances[-1].stopped)
                self.assertNotIn("SCORING COMPLETE", text)

    def test_semantic_and_hybrid_require_every_seed_post_vector_metadata(self):
        cases = (({"embedded": False, "vector_pending": False}, "invalid"),
                 ({"embedded": True, "vector_pending": True}, "invalid"),
                 ({"embedded": False, "vector_pending": True}, "invalid"),
                 ({"embedded": True}, "unknown"), ({}, "unknown"),
                 ({"embedded": "true", "vector_pending": False}, "unknown"))
        for search_mode in ("semantic", "hybrid"):
            for metadata, expected in cases:
                with self.subTest(search_mode=search_mode, metadata=metadata):
                    class NotEmbedded(SeedEvidenceServer):
                        seed_metadata = metadata
                    code, out, text, client, _ = self.run_main(server=NotEmbedded,
                        extra=("--search-mode", search_mode, "--embedding-provider", "native"))
                    self.assertEqual(code, 1)
                    self.assertEqual(out["run_status"]["seed_embedding_counts"][expected], 1)
                    self.assertIn("invalid_or_unknown_seed_embeddings", out["run_status"]["errors"])
                    fact = out["results"][0]["facts"][0]
                    self.assertEqual(fact["seed_embedding"]["status"], expected)
                    self.assertEqual(fact["seed_records"][0]["response"]["data"],
                                     {"id": "memory-1", "action": "saved", **metadata})
                    self.assertEqual(json.loads(fact["seed_records"][0]["response_raw"])["data"]["id"], "memory-1")
                    self.assertEqual(fact["memory_id"], "memory-1")
                    self.assertEqual(len(out["results"][0]["facts"]), 1)
                    client.chat.completions.create.assert_not_called()
                    self.assertNotIn("SCORING COMPLETE", text)

    def test_managed_seed_pending_or_missing_is_invalid_before_snapshot(self):
        for ingest in ("managed_raw", "resolved"):
            for metadata in ({"embedded": False, "vector_pending": True}, {}):
                with self.subTest(ingest=ingest, metadata=metadata):
                    class NotEmbedded(SeedEvidenceServer):
                        seed_metadata = metadata
                    code, out, _, client, _ = self.run_main(server=NotEmbedded,
                        extra=("--search-mode", "hybrid", "--ingest-mode", ingest))
                    self.assertEqual(code, 1)
                    fact = out["results"][0]["facts"][0]
                    self.assertEqual(fact["memory_id"], "memory-1")
                    self.assertIn(fact["seed_embedding"]["status"], ("invalid", "unknown"))
                    self.assertIn("seed embedding evidence", fact["seed_error"])
                    self.assertNotIn("store_snapshot", out["results"][0])
                    client.chat.completions.create.assert_not_called()

    def test_successful_semantic_seed_vector_proof_and_keyword_exemption(self):
        code, out, text, _, _ = self.run_main(server=SeedEvidenceServer,
            extra=("--search-mode", "semantic", "--embedding-provider", "native"))
        self.assertEqual(code, 0)
        self.assertEqual(out["run_status"]["seed_embedding_counts"]["valid"], len(out["results"][0]["facts"]))
        self.assertIn("SCORING COMPLETE", text)
        class NotEmbedded(SeedEvidenceServer):
            seed_metadata = {"embedded": False, "vector_pending": True}
        code, out, _, _, _ = self.run_main(server=NotEmbedded)
        self.assertEqual(code, 0)
        self.assertEqual(out["run_status"]["seed_embedding_counts"]["not_required"], len(out["results"][0]["facts"]))

    def test_seed_bare_stale_or_wrong_returned_id_is_unknown(self):
        for data, requested_id in (({"id": "current", "fact_outcome": "stale", "written": False}, "current"),
                                   ({"id": "another", "embedded": True, "vector_pending": False}, "current"),
                                   ({"embedded": True, "vector_pending": False}, "current"),
                                   ({"id": "current", "embedded": True, "vector_pending": False}, None)):
            with self.subTest(data=data):
                records = [{"request": {"method": "POST", "path": "/api/memories"},
                            "response": {"ok": True, "data": data}}]
                evidence = mab.seed_embedding_evidence(records, requested_id, True)
                self.assertEqual(evidence["status"], "unknown")
                self.assertFalse(evidence["valid"])
        self.assertEqual(mab.seed_embedding_evidence([], "current", True)["status"], "unknown")

    def test_last_nonempty_length_missing_or_filtered_finish_invalidates_scoring(self):
        for reason in ("length", None, "content_filter", "tool_calls"):
            with self.subTest(reason=reason):
                code, out, text, _, _ = self.run_main(texts=(completion("Current City", reason), "Current City"))
                self.assertEqual(code, 1)
                q = out["results"][0]["questions"][0]
                self.assertTrue(q["search_correct"])  # unchanged official raw diagnostic
                self.assertTrue(q["search_completion"]["nonempty"])
                self.assertFalse(q["search_completion"]["complete"])
                self.assertFalse(q["search_completion"]["valid"])
                self.assertEqual(out["run_status"]["n_incomplete_answers"], 1)
                self.assertEqual(out["run_status"]["n_valid_scoring_pairs"], 0)
                self.assertIsNone(out["results"][0]["summary"]["search_accuracy"])
                self.assertNotIn("SCORING COMPLETE", text)

    def test_three_empty_responses_preserve_raw_but_never_complete_scoring(self):
        code, out, text, _, _ = self.run_main(texts=("", "  ", "\t", "Current City"))
        self.assertEqual(code, 1)
        q = out["results"][0]["questions"][0]
        self.assertEqual(len(q["search_answer_records"]), 3)
        self.assertEqual(q["search_prediction"], "")
        self.assertFalse(q["search_completion"]["nonempty"])
        self.assertFalse(q["search_completion"]["valid"])
        self.assertEqual(q["search_completion"]["record_index"], 2)
        self.assertEqual(out["run_status"]["n_valid_scoring_pairs"], 0)
        self.assertIn("incomplete_answers", out["run_status"]["errors"])
        self.assertNotIn("SCORING COMPLETE", text)

    def test_control_finish_must_be_valid_even_with_correct_raw_prediction(self):
        code, out, _, _, _ = self.run_main(texts=("Current City", completion("Current City", "length")))
        self.assertEqual(code, 1)
        q = out["results"][0]["questions"][0]
        self.assertTrue(q["control_correct"])
        self.assertFalse(q["control_completion"]["valid"])
        self.assertEqual(out["results"][0]["diagnostics"]["validated_scores"]["unpaired_by_arm"]["control"]["n_scored"], 0)
        self.assertEqual(out["run_status"]["n_valid_scoring_pairs"], 0)

    def test_final_stop_after_empty_retry_is_valid_not_earlier_length(self):
        code, out, _, _, _ = self.run_main(texts=(completion("", "length"), "Current City", "Current City"))
        self.assertEqual(code, 0)
        q = out["results"][0]["questions"][0]
        self.assertTrue(q["search_completion"]["valid"])
        self.assertEqual(q["search_completion"]["finish_reason"], "stop")
        self.assertEqual(q["search_completion"]["record_index"], 1)

    def test_legacy_mode_object_without_contract_preserves_stored_scores(self):
        q = {"golds": ["Current City"], "search_prediction": "Current City", "search_correct": True,
             "control_prediction": "wrong", "control_correct": False,
             "retrieval_mode": {"requested_mode": "hybrid", "actual_modes": ["keyword"], "fallback_observed": True}}
        before = copy.deepcopy(q)
        self.assertEqual(mab.question_score_contract([q], "both")["paired_search_control"]["n"], 1)
        self.assertEqual(q, before)
        for marker in ({"valid": False}, {"schema_version": 1}):
            new_q = copy.deepcopy(q)
            new_q["retrieval_mode"].update(marker)
            self.assertEqual(mab.question_score_contract([new_q], "both")["paired_search_control"]["n"], 0)
        q["search_completion"] = {"schema_version": 1, "valid": False}
        self.assertEqual(mab.question_score_contract([q], "both")["paired_search_control"]["n"], 0)

    def test_each_normalized_empty_gold_is_invalid_intake_not_official_scorer_change(self):
        for bad_gold in ("the", "", "!!!", "A, an THE", "  "):
            for golds in ([bad_gold], ["Current City", bad_gold]):
                with self.subTest(golds=golds):
                    row = copy.deepcopy(self.row)
                    row["answers"] = [golds]
                    code, out, text, client, _ = self.run_main(row=row)
                    self.assertEqual(code, 1)
                    self.assertEqual(out["run_status"]["n_invalid_intake_questions"], 1)
                    q = out["results"][0]["questions"][0]
                    self.assertEqual(q["golds"], golds)
                    self.assertFalse(q["intake_valid"])
                    self.assertEqual(q["search_records"], [])
                    self.assertNotIn("search_correct", q)
                    self.assertIn("normalization", q["error"])
                    client.chat.completions.create.assert_not_called()
                    self.assertNotIn("SCORING COMPLETE", text)
            self.assertTrue(mab.substring_exact_match("anything", [bad_gold]))


if __name__ == "__main__":
    unittest.main()
