"""H5/H7 execution evidence and failure artifacts, with only local stdlib mocks."""
from __future__ import annotations

import contextlib
import copy
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import MagicMock, patch

from test_benchmarks import OfflineCase, client_for
from test_hypothesis_scoring import grounding, h5, h7, response, traced_plan, traced_session


SCENARIO5 = {
    "task_type": "fixture", "task": "Handle the fixture task.",
    "cause": "FEATURE_MODE must be strict", "precautions": ["Set FEATURE_MODE to strict"],
    "keyword_stems": ["feature_mode"],
}
SCENARIO7 = {
    "task_type": "fixture", "task": "Handle the fixture task.",
    "skill": {"content": "fixture runbook", "trigger": "fixture",
              "steps": ["Set FEATURE_MODE to strict"], "verification": "Check the fixture"},
    "keyword_stems": ["feature_mode"],
}
PLAN = "1. Set FEATURE_MODE to strict.\n2. Check the fixture."
NEGATIVE = '{"step":null,"quote":null}'


class FakeServer:
    def __init__(self, binary, *, evidence):
        self.evidence = evidence
        self.base = "http://mock.invalid"
        self.records = evidence.setdefault("records", [])
        evidence["startup"] = {"status": "ready", "error": None, "records": []}
        evidence["cleanup"] = {"process_reaped": False, "tempdir_removed": False, "errors": []}

    def create_skill(self, scenario):
        self.skill_id = "fixture-" + scenario["task_type"]
        return self.skill_id

    def create_decoys(self, namespace, count):
        self.decoys = (namespace, count)

    def session_formatted(self, context, project=None, *, trace=None):
        formatted = "[SKILL: fixture runbook]" if project is None else ""
        return traced_session(formatted, memory_id=self.skill_id)(context, project, trace=trace)

    def stop(self):
        self.evidence["cleanup"].update(process_reaped=True, tempdir_removed=True)
        return self.evidence["cleanup"]


class ExecutionCase(OfflineCase):
    def setUp(self):
        super().setUp()
        self.root = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
        self.stdout, self.stderr = io.StringIO(), io.StringIO()
        self.stack.enter_context(contextlib.redirect_stdout(self.stdout))
        self.stack.enter_context(contextlib.redirect_stderr(self.stderr))
        self.stack.enter_context(patch.object(h5, "H5_SCENARIOS", [copy.deepcopy(SCENARIO5)]))
        self.stack.enter_context(patch.object(h7, "SCENARIOS", [copy.deepcopy(SCENARIO7)]))
        self.stack.enter_context(patch.object(h5, "BASE_URL", "http://mock.invalid"))
        self.stack.enter_context(patch.object(h7, "BASE_URL", "http://mock.invalid"))
        self.stack.enter_context(patch.object(h5, "judge_client", client_for(*[response(NEGATIVE)] * 30)))
        self.stack.enter_context(patch.object(h7, "judge_client", client_for(*[response(NEGATIVE)] * 30)))
        self.stack.enter_context(patch.object(h7, "MemVaultServer", FakeServer))
        self.binary = self.root / "fixture-binary"
        self.binary.write_bytes(b"fixture binary bytes")
        self.stack.enter_context(patch.object(h7, "find_binary", return_value=self.binary))

    def h5_main(self, responses, samples=1):
        fake = client_for(*responses)
        self.stack.enter_context(patch.object(h5, "client", fake))
        out = self.root / "h5.json"
        argv = ["--hypothesis", "H5", "--samples", str(samples), "--out", str(out)]
        code = h5.main(argv)
        return code, json.loads(out.read_text()), fake, argv

    def h7_main(self, responses, *, samples=2):
        fake = client_for(*responses)
        self.stack.enter_context(patch.object(h7, "client", fake))
        out = self.root / "h7.json"
        argv = ["--rounds", "1", "--misfire-samples", str(samples), "--out", str(out)]
        code = h7.main(argv)
        return code, json.loads(out.read_text()), fake, argv


class H5ExecutionTests(ExecutionCase):
    def test_complete_pair_captures_requests_params_and_provenance(self):
        code, artifact, fake, argv = self.h5_main([response("1. Review the task."), response(PLAN)])
        self.assertEqual(code, 0)
        self.assertEqual(artifact["schema_version"], 2)
        self.assertEqual(artifact["run_status"]["status"], "scoring_complete")
        self.assertEqual(artifact["primary_summary"]["n"], 1)
        self.assertEqual(artifact["primary_summary"]["delta"], 1.0)
        self.assertEqual(artifact["harness_spec"]["argv"], argv)
        self.assertEqual(artifact["harness_spec"]["mechanism"], "manual_lesson_block")
        self.assertIsNone(artifact["harness_spec"]["evidence"]["binary_path"])
        self.assertEqual(fake.chat.completions.create.call_count, 2)
        for arm in ("control", "experiment"):
            detail = artifact[arm]["details"][0]
            self.assertTrue(detail["generation"]["valid"])
            self.assertEqual(detail["generation"]["response_model"], "fixture-judge")
            self.assertEqual(detail["records"][0]["request"]["max_tokens"], 800)
            self.assertEqual(detail["records"][0]["request"]["temperature"], 0.3)
            self.assertEqual(len(detail["judge_records"]), 1)
        self.assertEqual(artifact["control"]["details"][0]["injected_block"], "")
        self.assertIn("FEATURE_MODE", artifact["experiment"]["details"][0]["injected_block"])
        self.popen.assert_not_called()
        self.network.assert_not_called()

    def test_truncated_stem_hits_are_raw_only_and_never_judged(self):
        code, artifact, fake, _ = self.h5_main([response(PLAN, "length"), response(PLAN, "length")])
        self.assertEqual(code, 1)
        self.assertEqual(artifact["primary_summary"]["n"], 0)
        self.assertIsNone(artifact["primary_summary"]["delta"])
        self.assertEqual(artifact["control"]["compliant"], 1)
        self.assertEqual(artifact["experiment"]["compliant"], 1)
        self.assertEqual(artifact["run_status"]["status"], "invalid")
        self.assertEqual(fake.chat.completions.create.call_count, 2)
        h5.judge_client.chat.completions.create.assert_not_called()
        self.assertNotIn("CONFIRMED", self.stdout.getvalue())
        for arm in ("control", "experiment"):
            detail = artifact[arm]["details"][0]
            self.assertTrue(detail["generation"]["completion"]["truncated"])
            self.assertEqual(detail["judge"]["status"], "unknown")

    def test_one_incomplete_arm_excludes_its_complete_partner_from_primary(self):
        code, artifact, _, _ = self.h5_main([response(PLAN), response(PLAN, "length")])
        self.assertEqual(code, 1)
        self.assertTrue(artifact["control"]["details"][0]["generation"]["valid"])
        self.assertEqual(artifact["primary_summary"]["n_invalid"], 1)
        self.assertEqual(artifact["secondary_summary"]["n"], 0)
        self.assertIsNone(artifact["primary_summary"]["experiment"])

    def test_empty_completed_text_is_not_a_negative_valid_pair(self):
        code, artifact, _, _ = self.h5_main([response(""), response(PLAN)])
        self.assertEqual(code, 1)
        self.assertEqual(artifact["control"]["details"][0]["generation"]["reason"], "empty_completion")
        self.assertEqual(artifact["primary_summary"]["n"], 0)

    def test_runtime_error_retains_control_failed_request_and_unattempted_slots(self):
        secret = "private-fixture-key-8380"
        with patch.dict("os.environ", {"OPENAI_API_KEY": secret}):
            code, artifact, _, _ = self.h5_main([response(PLAN), RuntimeError(secret)], samples=2)
        self.assertEqual(code, 1)
        self.assertEqual(len(artifact["control"]["details"]), 2)
        self.assertEqual(artifact["control"]["details"][0]["plan"], PLAN)
        self.assertEqual(artifact["experiment"]["details"][0]["generation"]["reason"], "request_error")
        self.assertEqual(artifact["control"]["details"][1]["generation"]["status"], "not_attempted")
        self.assertEqual(artifact["primary_summary"]["n_attempted"], 1)
        self.assertNotIn(secret, json.dumps(artifact) + self.stderr.getvalue())

    def test_keyboard_interrupt_records_generation_and_returns_130(self):
        code, artifact, _, _ = self.h5_main([KeyboardInterrupt()])
        self.assertEqual(code, 130)
        self.assertEqual(artifact["run_error"]["type"], "KeyboardInterrupt")
        self.assertEqual(artifact["control"]["details"][0]["records"][0]["error"], "KeyboardInterrupt")
        self.assertNotIn("CONFIRMED", self.stdout.getvalue())

    def test_system_exit_from_client_is_not_a_successful_run(self):
        code, artifact, _, _ = self.h5_main([SystemExit(0)])
        self.assertEqual(code, 1)
        self.assertEqual(artifact["run_error"]["type"], "SystemExit")
        self.assertEqual(artifact["run_status"]["status"], "invalid")

    def test_judge_interrupt_keeps_pre_call_request_without_continuing(self):
        judge = client_for(KeyboardInterrupt())
        with patch.object(h5, "judge_client", judge):
            code, artifact, fake, _ = self.h5_main([response(PLAN), response(PLAN)])
        self.assertEqual(code, 130)
        self.assertEqual(fake.chat.completions.create.call_count, 1)
        detail = artifact["control"]["details"][0]
        self.assertEqual(detail["judge_records"][0]["error"], "KeyboardInterrupt")
        self.assertIn("PRECAUTION:", detail["judge_records"][0]["request"]["messages"][0]["content"])

    def test_unknown_secondary_does_not_invalidate_complete_primary(self):
        with patch.object(h5, "judge_client", client_for(response("malformed"), response("malformed"))):
            code, artifact, _, _ = self.h5_main([response("1. Review the task."), response(PLAN)])
        self.assertEqual(code, 0)
        self.assertEqual(artifact["primary_summary"]["n"], 1)
        self.assertEqual(artifact["secondary_summary"]["n"], 0)
        self.assertIsNone(artifact["secondary_summary"]["delta"])

    def test_source_identity_and_scenario_selection_are_frozen_before_generation(self):
        source = self.root / "verify_hypotheses.py"
        source.write_text("before execution")
        before = hashlib.sha256(source.read_bytes()).hexdigest()
        original_cause = h5.H5_SCENARIOS[0]["cause"]
        calls = 0

        def generate(**kwargs):
            nonlocal calls
            calls += 1
            source.write_text("changed after execution start")
            h5.H5_SCENARIOS[0]["cause"] = "mutated global fixture"
            return response(PLAN)

        fake = client_for()
        fake.chat.completions.create.side_effect = generate
        out = self.root / "snapshot.json"
        with patch.object(h5, "__file__", str(source)), patch.object(h5, "client", fake):
            self.assertEqual(h5.main(["--hypothesis", "H5", "--samples", "1", "--out", str(out)]), 0)
        artifact = json.loads(out.read_text())
        self.assertEqual(calls, 2)
        self.assertEqual(artifact["harness_spec"]["evidence"]["harness_sha256"], before)
        self.assertEqual(artifact["harness_spec"]["selection"]["scenarios"][0]["cause"], original_cause)
        self.assertIn(original_cause, artifact["experiment"]["details"][0]["injected_block"])

    def test_failed_write_cannot_print_confirmed_or_claim_saved(self):
        fake = client_for(response("1. Review the task."), response(PLAN))
        with patch.object(h5, "client", fake), patch.object(Path, "write_text", side_effect=PermissionError("blocked")):
            code = h5.main(["--hypothesis", "H5", "--samples", "1", "--out", str(self.root / "blocked.json")])
        self.assertEqual(code, 1)
        self.assertNotIn("CONFIRMED", self.stdout.getvalue())
        self.assertNotIn("details written", self.stdout.getvalue())
        self.assertIn("cannot write", self.stderr.getvalue())

    def test_nonpositive_cli_samples_are_rejected_before_side_effects(self):
        for samples in ("0", "-1"):
            with self.subTest(samples=samples), self.assertRaises(SystemExit) as caught:
                h5.main(["--hypothesis", "H5", "--samples", samples])
            self.assertEqual(caught.exception.code, 2)
        self.popen.assert_not_called()
        h5.judge_client.chat.completions.create.assert_not_called()

    def test_legacy_text_generator_preserves_request_and_none_content_behavior(self):
        fake = client_for(response(None))
        with patch.object(h5, "client", fake):
            self.assertIsNone(h5.generate_response("system", "user"))
        self.assertEqual(fake.chat.completions.create.call_count, 1)
        self.assertEqual(fake.chat.completions.create.call_args.kwargs["max_tokens"], 800)
        self.assertEqual(fake.chat.completions.create.call_args.kwargs["temperature"], 0.3)


class H7ExecutionTests(ExecutionCase):
    def test_complete_main_has_generation_injection_cleanup_and_unique_evidence(self):
        code, artifact, fake, argv = self.h7_main([response(PLAN), response(PLAN)], samples=40)
        self.assertEqual(code, 0)
        self.assertEqual(fake.chat.completions.create.call_count, 2)
        self.assertEqual(artifact["harness_spec"]["argv"], argv)
        self.assertEqual(artifact["harness_spec"]["evidence"]["binary_sha256"],
                         hashlib.sha256(self.binary.read_bytes()).hexdigest())
        self.assertTrue(artifact["server_evidence"]["cleanup"]["tempdir_removed"])
        self.assertEqual(artifact["h7a"]["primary_summary"]["n"], 1)
        detail = artifact["h7a"]["details"][0]
        self.assertTrue(detail["injection_status"]["valid"])
        self.assertIn("[SKILL:", detail["injection"])
        self.assertEqual(detail["control_records"][0]["request"]["max_tokens"], 500)
        self.assertEqual(len(detail["experiment_judge_records"]), 2)
        b = artifact["h7b"]
        self.assertEqual((b["valid_samples"], b["unique_source_contexts"], b["repeated_samples"]), (40, 20, 20))
        self.assertEqual(len(b["details"]), 40)
        self.assertTrue(all(d["formatted"] == "" and d["valid"] for d in b["details"]))
        self.assertIsNone(b["population_rate_bound"])

    def test_startup_failure_keeps_artifact_before_server_assignment(self):
        def fail(binary, *, evidence):
            evidence["startup"] = {"status": "failed", "error": "fixture startup"}
            evidence["cleanup"] = {"process_reaped": True, "tempdir_removed": True, "errors": []}
            raise RuntimeError("fixture startup")
        with patch.object(h7, "MemVaultServer", side_effect=fail):
            code, artifact, fake, _ = self.h7_main([])
        self.assertEqual(code, 1)
        self.assertEqual(artifact["run_error"]["phase"], "startup")
        self.assertEqual(artifact["server_evidence"]["startup"]["status"], "failed")
        self.assertEqual(artifact["h7a"]["primary_summary"]["n_requested"], 1)
        self.assertEqual(artifact["h7a"]["primary_summary"]["n"], 0)
        self.assertIsNone(artifact["h7b"]["rate"])
        fake.chat.completions.create.assert_not_called()

    def test_failed_experiment_keeps_completed_control_and_injection(self):
        code, artifact, _, _ = self.h7_main([response(PLAN), RuntimeError("generation failed")])
        self.assertEqual(code, 1)
        d = artifact["h7a"]["details"][0]
        self.assertEqual(d["control_plan"], PLAN)
        self.assertTrue(d["injection_status"]["valid"])
        self.assertEqual(d["experiment_generation"]["reason"], "request_error")
        self.assertEqual(artifact["h7a"]["primary_summary"]["n"], 0)
        self.assertTrue(artifact["server_evidence"]["cleanup"]["process_reaped"])

    def test_truncated_plans_are_never_a_valid_skill_success(self):
        code, artifact, _, _ = self.h7_main([response(PLAN, "length"), response(PLAN, "length")])
        self.assertEqual(code, 1)
        self.assertEqual(artifact["h7a"]["raw_control_hits"], 1)
        self.assertIsNone(artifact["h7a"]["control"])
        self.assertEqual(artifact["h7a"]["primary_summary"]["n"], 0)
        h7.judge_client.chat.completions.create.assert_not_called()
        self.assertNotIn("CONFIRMED", self.stdout.getvalue())

    def test_missing_skill_is_an_explicit_invariant_not_an_assertion(self):
        class NoSkill(FakeServer):
            def session_formatted(self, context, project=None, *, trace=None):
                return traced_session("")(context, project, trace=trace)
        with patch.object(h7, "MemVaultServer", NoSkill):
            code, artifact, fake, _ = self.h7_main([response(PLAN)])
        self.assertEqual(code, 1)
        self.assertEqual(fake.chat.completions.create.call_count, 1)
        self.assertEqual(artifact["h7a"]["details"][0]["injection_status"]["reason"], "missing_expected_skill")
        self.assertIn("expected a matched skill", artifact["run_error"]["message"])

    def test_bad_h7b_envelope_is_not_a_clean_observation(self):
        class BadSession(FakeServer):
            def session_formatted(self, context, project=None, *, trace=None):
                if project:
                    trace.append({"status": 200, "error": None, "response": {"ok": False, "error": "bad", "data": {"formatted": ""}}})
                    return ""
                return super().session_formatted(context, project, trace=trace)
        with patch.object(h7, "MemVaultServer", BadSession):
            code, artifact, _, _ = self.h7_main([response(PLAN), response(PLAN)])
        self.assertEqual(code, 1)
        b = artifact["h7b"]
        self.assertEqual((b["attempted_samples"], b["valid_samples"], b["invalid_samples"]), (1, 0, 1))
        self.assertIsNone(b["rate"])
        self.assertIsNone(b["details"][0]["injected_skill"])
        self.assertNotIn("CONFIRMED", self.stdout.getvalue())

    def test_cleanup_failure_does_not_replace_generation_failure(self):
        class FailedCleanup(FakeServer):
            def stop(self):
                raise RuntimeError("cleanup failed")
        with patch.object(h7, "MemVaultServer", FailedCleanup):
            code, artifact, _, _ = self.h7_main([RuntimeError("first failure")])
        self.assertEqual(code, 1)
        self.assertEqual(artifact["run_error"]["message"], "first failure")
        self.assertIn("cleanup failed", artifact["server_evidence"]["cleanup"]["errors"])
        self.assertIn("cleanup_failed", artifact["run_status"]["errors"])

    def test_failed_write_cannot_print_confirmed(self):
        fake = client_for(response("1. Review the task."), response(PLAN))
        with patch.object(h7, "client", fake), patch.object(Path, "write_text", side_effect=PermissionError("blocked")):
            code = h7.main(["--rounds", "1", "--misfire-samples", "1", "--out", str(self.root / "blocked.json")])
        self.assertEqual(code, 1)
        self.assertNotIn("CONFIRMED", self.stdout.getvalue())
        self.assertIn("cannot write", self.stderr.getvalue())

    def test_nonpositive_cli_counts_never_start_server(self):
        fake = MagicMock()
        with patch.object(h7, "MemVaultServer", fake):
            for flag in ("--rounds", "--misfire-samples"):
                with self.subTest(flag=flag), self.assertRaises(SystemExit) as caught:
                    h7.main([flag, "0"])
                self.assertEqual(caught.exception.code, 2)
        fake.assert_not_called()

    def test_other_skill_identity_cannot_count_as_matched_treatment(self):
        class WrongSkill(FakeServer):
            def session_formatted(self, context, project=None, *, trace=None):
                return traced_session("[SKILL: other runbook]", memory_id="other-id")(context, project, trace=trace)
        with patch.object(h7, "MemVaultServer", WrongSkill):
            code, artifact, fake, _ = self.h7_main([response(PLAN)])
        self.assertEqual(code, 1)
        self.assertEqual(fake.chat.completions.create.call_count, 1)
        self.assertEqual(artifact["h7a"]["details"][0]["injection_status"]["reason"], "unexpected_skill_identity")
        self.assertNotIn("CONFIRMED", self.stdout.getvalue())

    def test_generic_retrieval_of_expected_id_is_not_an_explicit_match(self):
        class GenericSkill(FakeServer):
            def session_formatted(self, context, project=None, *, trace=None):
                return traced_session("[SKILL: fixture runbook]", memory_id=self.skill_id,
                                      source="kw#1")(context, project, trace=trace)
        with patch.object(h7, "MemVaultServer", GenericSkill):
            code, artifact, _, _ = self.h7_main([response(PLAN)])
        self.assertEqual(code, 1)
        self.assertEqual(artifact["h7a"]["details"][0]["injection_status"]["reason"],
                         "expected_skill_not_explicitly_matched")

    def test_matched_skill_requires_typed_unique_receipt(self):
        for field, value in (("type", "fact"), ("search_mode", "hybrid"), ("hit_sources", ["kw#1"])):
            with self.subTest(field=field):
                records = []
                formatted = traced_session("[SKILL: fixture]", memory_id="expected")("task", trace=records)
                hit = records[0]["response"]["data"]["results"][0]
                if field == "type":
                    hit["memory"][field] = value
                else:
                    hit[field] = value
                self.assertFalse(h7.matched_skill_status("expected", formatted, records)["valid"])
        records = []
        formatted = traced_session("[SKILL: fixture]", memory_id="expected")("task", trace=records)
        hits = records[0]["response"]["data"]["results"]
        hits.append(copy.deepcopy(hits[0]))
        self.assertFalse(h7.matched_skill_status("expected", formatted, records)["valid"])

    def test_unclassified_interrupted_session_never_enters_clean_denominator(self):
        for classification in (None, 0, "false"):
            with self.subTest(classification=classification):
                result = h7.initial_h7b(1, ["context"])
                result["details"].append({"context": "context", "records": [], "valid": True,
                                          "injected_skill": classification})
                summary = h7.summarize_h7b(result)
                self.assertEqual(summary["valid_samples"], 0)
                self.assertEqual(summary["invalid_samples"], 1)
                self.assertIsNone(summary["rate"])
                self.assertEqual(summary["status"], "invalid")

    def test_explicit_binary_is_hashed_only_once_when_identity_is_unchanged(self):
        fake = client_for(response(PLAN), response(PLAN))
        out = self.root / "binary-hash.json"
        with patch.object(h7, "client", fake), patch.object(
            h7.beam, "content_hash", wraps=h7.beam.content_hash
        ) as hash_file:
            code = h7.main(["--rounds", "1", "--misfire-samples", "1", "--binary", str(self.binary),
                            "--out", str(out)])
        self.assertEqual(code, 0)
        reads = [c for c in hash_file.call_args_list if c.args and c.args[0] is not None
                 and Path(c.args[0]).resolve() == self.binary.resolve()]
        self.assertEqual(len(reads), 1)

    def test_programmatic_result_reuse_is_rejected_before_writes(self):
        result = h7.initial_h7a(1, [SCENARIO7])
        result["skill_ids"]["fixture"] = "old-id"
        server = MagicMock()
        with self.assertRaises(ValueError):
            h7.run_h7a(server, 1, result=result, scenarios=[SCENARIO7])
        server.create_skill.assert_not_called()
        b = h7.initial_h7b(1, ["context"])
        b["details"].append({})
        with self.assertRaises(ValueError):
            h7.run_h7b(server, 1, result=b, contexts=["context"])
        server.create_decoys.assert_not_called()


class JudgeFailureEvidenceTests(OfflineCase):
    def test_failed_retry_metadata_describes_last_attempt_not_previous_response(self):
        fake = client_for(response("partial", "length", "first-model"), RuntimeError("retry failed"))
        trace = []
        result = grounding.judge_quote(fake, "requested-model", "Precaution", "1. Precaution.", trace=trace)
        self.assertEqual(len(trace), 2)
        self.assertEqual(trace[0]["finish_reason"], "length")
        self.assertEqual(trace[1]["error"], "retry failed")
        self.assertIsNone(result["metadata"]["response_model"])
        self.assertIsNone(result["metadata"]["finish_reason"])
        self.assertEqual(result["metadata"]["max_tokens"], grounding.JUDGE_RETRY_MAX_TOKENS)
        self.assertEqual(result["reason"], "request_error")

    def test_literal_quotes_remain_strict_after_request_prompt_changes(self):
        plan = "1. Set **FEATURE_MODE** to `strict`."
        good = json.dumps({"step": 1, "quote": "Set **FEATURE_MODE** to `strict`."})
        bad = json.dumps({"step": 1, "quote": "Set FEATURE_MODE to strict."})
        self.assertTrue(grounding.parse_grounded_judgment(good, plan, finish_reason="stop")["valid"])
        self.assertFalse(grounding.parse_grounded_judgment(bad, plan, finish_reason="stop")["valid"])


if __name__ == "__main__":
    unittest.main()
