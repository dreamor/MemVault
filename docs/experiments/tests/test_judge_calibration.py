"""Offline checks for the frozen judge-calibration replay metrics."""
from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

import judge_calibration as calibration


def contract(**overrides):
    base = {
        "completion_ok": True,
        "json_parse_ok": True,
        "literal_quote_ok": None,
        "order_ok": None,
        "binding_count_ok": None,
        "unknown": False,
        "status": "positive",
        "reason": "grounded_judge_claim",
    }
    base.update(overrides)
    return base


def attempt(status, reason, *, contract_overrides=None):
    return {
        "status": status,
        "reason": reason,
        "contract": contract(status=status, reason=reason, **(contract_overrides or {})),
    }


def record(case_id, label, statuses, reasons=None):
    reasons = reasons or ["grounded_judge_claim"] * len(statuses)
    repeats = [attempt(status, reason) for status, reason in zip(statuses, reasons)]
    return {
        "case_id": case_id,
        "label_used": label,
        "label_source": "provisional",
        "consensus_status": calibration.consensus(statuses),
        "repeat_agreement": calibration.consensus(statuses) != "inconsistent",
        "repeats": repeats,
    }


def case_fixture(case_id="case-a", *, human_label=None, judge_input=None,
                 source_artifact="artifact.json", source_artifact_sha256="0" * 64):
    return {
        "case_id": case_id,
        "benchmark": "H7",
        "source_artifact": source_artifact,
        "source_artifact_sha256": source_artifact_sha256,
        "pair_id": "pair-a",
        "task_type": "deploy",
        "arm": "experiment",
        "kind": "step",
        "obligation_index": 0,
        "obligation_source": "skill.steps",
        "obligation": "do x",
        "claim_scope": "plan_contains_required_action_not_execution_success",
        "judge_input": judge_input or {
            "precaution": "do x",
            "plan": "1. do x",
            "ordered_conditions": None,
        },
        "human_label": human_label,
    }


def document_fixture(cases, *, source_baseline="base.json",
                     source_baseline_sha256="1" * 64):
    return {
        "source_baseline": source_baseline,
        "source_baseline_sha256": source_baseline_sha256,
        "cases": cases,
    }


class ContractFlagTests(unittest.TestCase):
    def test_grounded_positive_claim_passes_contract(self):
        result = {
            "status": "positive",
            "reason": "grounded_judge_claim",
            "ordered_conditions": ["a", "b"],
            "bindings": [
                {"grounding": {"normalized_match": True}},
                {"grounding": {"normalized_match": True}},
            ],
        }
        flags = calibration.contract_flags(result)
        self.assertTrue(flags["completion_ok"])
        self.assertTrue(flags["json_parse_ok"])
        self.assertTrue(flags["literal_quote_ok"])
        self.assertTrue(flags["order_ok"])
        self.assertTrue(flags["binding_count_ok"])

    def test_ungrounded_quote_is_json_valid_but_not_literal(self):
        result = {
            "status": "unknown",
            "reason": "ungrounded_quote",
            "ordered_conditions": None,
            "bindings": [{"grounding": {"normalized_match": False}}],
        }
        flags = calibration.contract_flags(result)
        self.assertTrue(flags["json_parse_ok"])
        self.assertFalse(flags["unknown"] is False)
        self.assertTrue(flags["unknown"])
        self.assertIsNone(flags["literal_quote_ok"])

    def test_incomplete_completion_fails_every_contract_layer(self):
        result = {"status": "unknown", "reason": "incomplete_completion"}
        flags = calibration.contract_flags(result)
        self.assertFalse(flags["completion_ok"])
        self.assertFalse(flags["json_parse_ok"])
        self.assertIsNone(flags["literal_quote_ok"])


class SummaryTests(unittest.TestCase):
    def test_confusion_matrix_uses_consensus_and_excludes_boundary(self):
        records = [
            record("tp", "positive", ["positive", "positive"]),
            record("tn", "negative", ["negative", "negative"]),
            record("fp", "negative", ["positive", "positive"]),
            record("fn", "positive", ["negative", "negative"]),
            record("boundary", "boundary", ["unknown", "unknown"]),
            record("unstable", "positive", ["positive", "negative"]),
        ]
        summary = calibration.summarize(records)
        self.assertEqual(summary["true_positive"], 1)
        self.assertEqual(summary["true_negative"], 1)
        self.assertEqual(summary["false_positive"], 1)
        self.assertEqual(summary["false_negative"], 1)
        self.assertEqual(summary["boundary_cases"], 1)
        self.assertEqual(summary["repeat_agreement_rate"], 0.8333)
        self.assertEqual(summary["inconsistent_cases"], ["unstable"])


class FingerprintTests(unittest.TestCase):
    def test_editable_labels_do_not_change_input_fingerprint(self):
        first = document_fixture([case_fixture(human_label=None)])
        second = document_fixture([case_fixture(human_label="positive")])
        second["cases"][0]["assistant_audit_label"] = "negative"
        second["cases"][0]["notes"] = "review note"

        self.assertEqual(
            calibration.immutable_inputs_sha256(first),
            calibration.immutable_inputs_sha256(second),
        )
        self.assertNotEqual(
            calibration.human_labels_sha256(first),
            calibration.human_labels_sha256(second),
        )

    def test_judge_input_change_changes_input_fingerprint(self):
        first = document_fixture([case_fixture()])
        second = document_fixture([case_fixture(judge_input={
            "precaution": "do x",
            "plan": "1. never do x",
            "ordered_conditions": None,
        })])
        self.assertNotEqual(
            calibration.immutable_inputs_sha256(first),
            calibration.immutable_inputs_sha256(second),
        )

    def test_preflight_verifies_source_hashes_before_replay(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            baseline = root / "base.json"
            artifact = root / "artifact.json"
            baseline.write_text("{}")
            artifact.write_text("{}")
            document = document_fixture(
                [case_fixture(
                    source_artifact=str(artifact),
                    source_artifact_sha256=calibration.sha256_file(artifact),
                )],
                source_baseline=str(baseline),
                source_baseline_sha256=calibration.sha256_file(baseline),
            )

            provenance = calibration.preflight(document)
            self.assertFalse(provenance["human_labels_complete"])
            self.assertEqual(
                provenance["source_artifacts"][str(artifact)],
                calibration.sha256_file(artifact),
            )

            document["cases"][0]["source_artifact_sha256"] = "0" * 64
            with self.assertRaisesRegex(ValueError, "source_artifact hash mismatch"):
                calibration.preflight(document)

    def test_declared_input_fingerprint_mismatch_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            baseline = root / "base.json"
            artifact = root / "artifact.json"
            baseline.write_text("{}")
            artifact.write_text("{}")
            document = document_fixture(
                [case_fixture(
                    source_artifact=str(artifact),
                    source_artifact_sha256=calibration.sha256_file(artifact),
                )],
                source_baseline=str(baseline),
                source_baseline_sha256=calibration.sha256_file(baseline),
            )
            document["immutable_inputs_sha256"] = "0" * 64
            with self.assertRaisesRegex(ValueError, "immutable_inputs_sha256 mismatch"):
                calibration.preflight(document)

    def test_declared_label_fingerprint_mismatch_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            baseline = root / "base.json"
            artifact = root / "artifact.json"
            baseline.write_text("{}")
            artifact.write_text("{}")
            document = document_fixture(
                [case_fixture(
                    source_artifact=str(artifact),
                    source_artifact_sha256=calibration.sha256_file(artifact),
                )],
                source_baseline=str(baseline),
                source_baseline_sha256=calibration.sha256_file(baseline),
            )
            document["human_labels_sha256"] = "0" * 64
            with self.assertRaisesRegex(ValueError, "human_labels_sha256 mismatch"):
                calibration.preflight(document)

    def test_repeated_source_artifact_must_keep_consistent_declared_hash(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            baseline = root / "base.json"
            artifact = root / "artifact.json"
            baseline.write_text("{}")
            artifact.write_text("{}")
            actual = calibration.sha256_file(artifact)
            document = document_fixture(
                [
                    case_fixture(
                        "case-a",
                        source_artifact=str(artifact),
                        source_artifact_sha256=actual,
                    ),
                    case_fixture(
                        "case-b",
                        source_artifact=str(artifact),
                        source_artifact_sha256="0" * 64,
                    ),
                ],
                source_baseline=str(baseline),
                source_baseline_sha256=calibration.sha256_file(baseline),
            )
            with self.assertRaisesRegex(ValueError, "source_artifact hash mismatch"):
                calibration.preflight(document)

    def test_external_input_pin_is_enforced(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            baseline = root / "base.json"
            artifact = root / "artifact.json"
            baseline.write_text("{}")
            artifact.write_text("{}")
            document = document_fixture(
                [case_fixture(
                    source_artifact=str(artifact),
                    source_artifact_sha256=calibration.sha256_file(artifact),
                )],
                source_baseline=str(baseline),
                source_baseline_sha256=calibration.sha256_file(baseline),
            )
            actual = calibration.immutable_inputs_sha256(document)
            calibration.preflight(document, expected_inputs_sha256=actual)
            with self.assertRaisesRegex(ValueError, "expected inputs fingerprint mismatch"):
                calibration.preflight(document, expected_inputs_sha256="0" * 64)

    def test_human_label_gate_is_opt_in(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            baseline = root / "base.json"
            artifact = root / "artifact.json"
            baseline.write_text("{}")
            artifact.write_text("{}")
            document = document_fixture(
                [case_fixture(
                    source_artifact=str(artifact),
                    source_artifact_sha256=calibration.sha256_file(artifact),
                )],
                source_baseline=str(baseline),
                source_baseline_sha256=calibration.sha256_file(baseline),
            )

            calibration.preflight(document)
            with self.assertRaisesRegex(ValueError, "human_label missing"):
                calibration.preflight(document, require_human_labels=True)

            document["cases"][0]["human_label"] = "positive"
            provenance = calibration.preflight(document, require_human_labels=True)
            self.assertTrue(provenance["human_labels_complete"])


if __name__ == "__main__":
    unittest.main()
