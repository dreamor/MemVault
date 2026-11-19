"""Offline checks for the frozen judge-calibration replay metrics."""
from __future__ import annotations

import unittest

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


if __name__ == "__main__":
    unittest.main()
