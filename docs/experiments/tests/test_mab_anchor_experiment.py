"""Offline contracts for the default-off MH anchor-retrieval experiment."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import benchmark_evidence as evidence
import memory_agent_bench as mab


class QuestionSelectionTests(unittest.TestCase):
    def args(self, **overrides):
        base = {"questions": 100, "question_ids": ""}
        base.update(overrides)
        return SimpleNamespace(**base)

    def test_empty_filter_keeps_first_n(self):
        row = {"questions": ["a", "b", "c"], "metadata": {"qa_pair_ids": ["q1", "q2", "q3"]}}
        self.assertEqual(mab.selected_question_indices(row, self.args(questions=2)), [0, 1])

    def test_exact_ids_preserve_requested_order(self):
        row = {"questions": ["a", "b", "c"], "metadata": {"qa_pair_ids": ["q1", "q2", "q3"]}}
        selected = mab.selected_question_indices(row, self.args(question_ids="q3,q1"))
        self.assertEqual(selected, [2, 0])

    def test_unknown_id_is_an_error(self):
        row = {"questions": ["a"], "metadata": {"qa_pair_ids": ["q1"]}}
        with self.assertRaisesRegex(ValueError, "unknown question ids"):
            mab.selected_question_indices(row, self.args(question_ids="q9"))


class EnvironmentTests(unittest.TestCase):
    def test_child_environment_sets_anchor_knobs(self):
        env = mab.child_environment("native", candidate_k=200, keyword_anchor_k=5)
        self.assertEqual(env["MEMVAULT_HYBRID_CANDIDATE_K"], "200")
        self.assertEqual(env["MEMVAULT_HYBRID_KEYWORD_ANCHORS"], "5")

    def test_isolation_preserves_only_experiment_knobs(self):
        requested = {
            "PATH": "/bin",
            "MEMVAULT_HYBRID_CANDIDATE_K": "200",
            "MEMVAULT_HYBRID_KEYWORD_ANCHORS": "5",
            "UNRELATED_SECRET": "do-not-forward",
        }
        import tempfile
        with tempfile.TemporaryDirectory() as root:
            env, _, _ = evidence.isolate_server_environment(root, "test.db", requested)
        self.assertEqual(env["MEMVAULT_HYBRID_CANDIDATE_K"], "200")
        self.assertEqual(env["MEMVAULT_HYBRID_KEYWORD_ANCHORS"], "5")
        self.assertNotIn("UNRELATED_SECRET", env)

    def test_parser_defaults_keep_experiment_off(self):
        args = mab.build_parser().parse_args([])
        self.assertIsNone(args.candidate_k)
        self.assertEqual(args.keyword_anchor_k, 0)
        self.assertEqual(args.question_ids, "")


class BridgeExtractionTests(unittest.TestCase):
    def test_extracts_ordered_bridge_rules_without_using_gold(self):
        contents = [
            "1531. The origianl broadcaster of Perfect Strangers is MTV.",
            "1562. The author of A Passage to India is Federico García Lorca.",
            "2183. Little Miss Sunshine was created in the country of United Kingdom.",
            "2035. Phoenix Inferno is associated with the sport of association football.",
        ]
        bridges = mab.extract_bridge_entities(contents, 3)
        self.assertEqual([bridge["query"] for bridge in bridges],
                         ["MTV", "Federico García Lorca", "United Kingdom"])
        self.assertEqual([bridge["rule"] for bridge in bridges],
                         ["generic_is", "generic_is", "country_of_origin"])
        self.assertEqual(bridges[0]["source_serial"], 1531)

    def test_zero_limit_disables_extraction(self):
        self.assertEqual(mab.extract_bridge_entities(["1. A is B."], 0), [])


if __name__ == "__main__":
    unittest.main()
