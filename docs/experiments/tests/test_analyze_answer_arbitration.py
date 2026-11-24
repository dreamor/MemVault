"""Offline checks for the answer-arbitration analyser."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import analyze_answer_arbitration as analysis


def run(case, arm, repeat, prediction, *, correct, complete=True, latency=100):
    return {
        "case": case,
        "arm": arm,
        "repeat": repeat,
        "prediction": prediction,
        "normalised_prediction": analysis.normalise(prediction),
        "correct": correct,
        "complete": complete,
        "finish_reason": "stop" if complete else "length",
        "usage": {"prompt_tokens": 10, "completion_tokens": 1, "total_tokens": 11},
        "latency_ms": latency,
    }


def results():
    runs = []
    for repeat in (1, 2, 3):
        runs.append(run("no1", "A", repeat, "old", correct=False, latency=100 * repeat))
        runs.append(run("no1", "B", repeat, "new", correct=True, latency=200 * repeat))
        runs.append(run("no2", "A", repeat, "new", correct=True, latency=300 * repeat))
        runs.append(run("no2", "B", repeat, "new", correct=True, latency=400 * repeat))
        runs.append(run("no3", "A", repeat, "", correct=False, complete=False, latency=500))
        runs.append(run("no3", "B", repeat, "new", correct=True, latency=600))
    return {
        "purpose": "synthetic",
        "created_at": "2026-11-24T00:00:00+00:00",
        "models": {"answer_model": "m", "temperature": 0.0, "max_tokens": 10, "extra_body": None},
        "design": {"cases": ["no1", "no2", "no3"]},
        "cases": [
            {"case": "no1", "golds": ["new"]},
            {"case": "no2", "golds": ["new"]},
            {"case": "no3", "golds": ["new"]},
        ],
        "runs": runs,
    }


class AggregateTests(unittest.TestCase):
    def test_completion_accuracy_and_paired_counts(self):
        out = analysis.analyse(results())
        arms = out["aggregate"]["arms"]
        self.assertEqual(arms["A"]["n"], 9)
        self.assertEqual(arms["A"]["complete_runs"], 6)
        self.assertEqual(arms["A"]["correct_runs"], 3)
        self.assertAlmostEqual(arms["A"]["accuracy"], 0.5)
        self.assertAlmostEqual(arms["A"]["completion_rate"], 6 / 9)
        self.assertEqual(arms["B"]["complete_runs"], 9)
        self.assertAlmostEqual(arms["B"]["accuracy"], 1.0)

        paired = out["aggregate"]["paired"]
        self.assertEqual(paired["n_cases"], 2)
        self.assertEqual(paired["excluded_incomplete_cases"], ["no3"])
        self.assertEqual(paired["a_majority_correct"], 1)
        self.assertEqual(paired["b_majority_correct"], 2)
        self.assertEqual(paired["b_improved"], 1)
        self.assertEqual(paired["b_regressed"], 0)
        self.assertEqual(paired["a_consistent_cases"], 2)
        self.assertEqual(paired["b_consistent_cases"], 3)

    def test_cost_uses_recorded_usage_and_latency(self):
        out = analysis.analyse(results())
        arms = out["aggregate"]["arms"]
        self.assertEqual(arms["A"]["total_tokens"], 99)
        self.assertEqual(arms["A"]["prompt_tokens_per_call"], 10.0)
        self.assertEqual(arms["B"]["latency_samples"], 9)
        self.assertIsNotNone(arms["B"]["latency_p50_ms"])
        self.assertIsNotNone(arms["B"]["latency_mean_ms"])


class OldBranchTests(unittest.TestCase):
    def test_counts_exact_older_same_slot_selection(self):
        labels = {
            "selected": [
                {
                    "case": "no1",
                    "older_competitors_in_context": [{"serial": 1, "object": "old", "fact": "1. X old."}],
                }
            ]
        }
        out = analysis.analyse(results(), labels)
        old = out["old_branch_analysis"]
        self.assertTrue(old["available"])
        self.assertEqual(old["A"]["wrong_complete_runs"], 3)
        self.assertEqual(old["A"]["wrong_run_equal_to_older_same_slot_value"], 3)
        self.assertEqual(old["A"]["cases"], ["no1"])
        self.assertEqual(old["B"]["wrong_run_equal_to_older_same_slot_value"], 0)

    def test_substring_mention_is_not_an_old_branch_choice(self):
        labels = {
            "selected": [
                {
                    "case": "no1",
                    "older_competitors_in_context": [{"serial": 1, "object": "old", "fact": "1. X old."}],
                }
            ]
        }
        data = results()
        for row in data["runs"]:
            if row["case"] == "no1" and row["arm"] == "A":
                row["prediction"] = "the old record is unreliable"
        out = analysis.analyse(data, labels)
        self.assertEqual(
            out["old_branch_analysis"]["A"]["wrong_run_equal_to_older_same_slot_value"], 0
        )

    def test_absent_labels_report_attribution_unavailable(self):
        out = analysis.analyse(results())
        self.assertFalse(out["old_branch_analysis"]["available"])
        self.assertIsNone(out["old_branch_analysis"]["A"])


if __name__ == "__main__":
    unittest.main()
