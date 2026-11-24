"""Offline checks for the fixed-context answer-arbitration harness."""
from __future__ import annotations

import json
import os
import sys
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import answer_arbitration as arbitration


class FinalAnswerExtractionTests(unittest.TestCase):
    def test_slot_table_cannot_trigger_false_positive(self):
        raw = (
            "STAGE 1 — SLOT TABLE:\n"
            "SLOT 1: X | relation | candidates=1:old; 9:new | selected serial=9 | value=new\n"
            "PATH: X -> new\n\n"
            "STAGE 2 — ANSWER:\nnew"
        )
        self.assertEqual(arbitration.extract_final_answer(raw, "B"), "new")

    def test_bare_answer_is_preserved(self):
        self.assertEqual(arbitration.extract_final_answer("  London  ", "B"), "London")

    def test_explicit_answer_field_is_extracted_from_arm_a(self):
        raw = "Reasoning mentions the old value.\nAnswer: London"
        self.assertEqual(arbitration.extract_final_answer(raw, "A"), "London")

    def test_insufficient_evidence_is_preserved(self):
        raw = "STAGE 2 — ANSWER:\ninsufficient evidence"
        self.assertEqual(arbitration.extract_final_answer(raw, "B"), "insufficient evidence")

    def test_path_without_final_answer_is_incomplete(self):
        raw = (
            "STAGE 1 — SLOT TABLE:\n"
            "SLOT 1: X | relation | candidates=1:old; 9:Paris | selected serial=9 | value=Paris\n"
            "PATH: X -> Paris"
        )
        answer = arbitration.extract_final_answer(raw, "B")
        self.assertEqual(answer, "")
        self.assertFalse(arbitration.is_complete_generation("stop", answer))


class MajorityTests(unittest.TestCase):
    def test_tie_has_no_majority(self):
        self.assertEqual(arbitration.strict_majority(["Rome", "Paris"]), (None, 1))

    def test_strict_majority_wins(self):
        self.assertEqual(arbitration.strict_majority(["Paris", "Rome", "Paris"]), ("Paris", 2))

    def test_three_way_tie_has_no_majority(self):
        self.assertEqual(
            arbitration.strict_majority(["Paris", "Rome", "London"]), (None, 1)
        )


class RedactionTests(unittest.TestCase):
    def test_bare_key_query_is_removed_from_endpoint(self):
        safe = arbitration.redact_url(
            "https://gateway.example/v1?foo=bar&key=query-secret"
        )
        self.assertNotIn("query-secret", safe)
        self.assertIn("foo=bar", safe)

    def test_whole_result_is_sanitized_before_serialization(self):
        with mock.patch.dict(
            os.environ, {"OPENAI_API_KEY": "sk-live-secret"}, clear=False
        ):
            safe = arbitration.redact_value(
                {
                    "raw_response": "echo sk-live-secret",
                    "prediction": "sk-live-secret",
                    "summary": {"answer": "sk-live-secret"},
                    "endpoint": "https://gateway.example/v1?key=query-secret",
                }
            )
        serialized = json.dumps(safe)
        self.assertNotIn("sk-live-secret", serialized)
        self.assertNotIn("query-secret", serialized)


def _contract_run(**overrides):
    run = {
        "provenance": {
            "prompt_a_sha256": "a" * 64,
            "prompt_b_sha256": "b" * 64,
            "context_source_sha256": "c" * 64,
            "chain_baseline_sha256": "d" * 64,
        },
        "models": {
            "answer_model": "bailian/deepseek-v4.1-flash",
            "base_url": "https://example.invalid/v1",
            "temperature": 0.0,
            "max_tokens": 3000,
            "extra_body": {"enable_thinking": False},
        },
    }
    for path, value in overrides.items():
        section, key = path.split("__", 1)
        run[section][key] = value
    return run


class ReplicationContractTests(unittest.TestCase):
    def test_identical_contract_has_no_drift(self):
        baseline = _contract_run()
        self.assertEqual(
            arbitration.replication_contract_drift(baseline, _contract_run()), {}
        )

    def test_prompt_drift_is_reported(self):
        drift = arbitration.replication_contract_drift(
            _contract_run(), _contract_run(provenance__prompt_b_sha256="e" * 64)
        )
        self.assertIn("prompt_b_sha256", drift)

    def test_context_drift_is_reported(self):
        drift = arbitration.replication_contract_drift(
            _contract_run(), _contract_run(provenance__context_source_sha256="f" * 64)
        )
        self.assertIn("context_source_sha256", drift)

    def test_model_setting_drift_is_reported(self):
        drift = arbitration.replication_contract_drift(
            _contract_run(), _contract_run(models__temperature=0.7)
        )
        self.assertEqual(drift["models.temperature"], (0.0, 0.7))

    def test_missing_field_counts_as_drift(self):
        changed = _contract_run()
        del changed["models"]["extra_body"]
        drift = arbitration.replication_contract_drift(_contract_run(), changed)
        self.assertIn("models.extra_body", drift)


class CaseGateTests(unittest.TestCase):
    def test_default_cases_exclude_no3(self):
        self.assertNotIn("no3", arbitration.DEFAULT_CASES)
        self.assertEqual(len(arbitration.DEFAULT_CASES), 6)


if __name__ == "__main__":
    unittest.main()
