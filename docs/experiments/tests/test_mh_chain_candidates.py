"""Offline checks for the deterministic MH candidate-chain builder."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import mh_chain_candidates as mh


CONTEXT = """Here is a list of facts:
10. Alpha was created by Beta.
20. Alpha was created by Gamma.
30. Beta is a citizen of Oldland.
40. Gamma is a citizen of Newland.
50. Delta was born in the city of Rome.
"""


class ParseFactTests(unittest.TestCase):
    def test_parses_all_supported_templates(self):
        facts = mh.parse_facts(CONTEXT)
        self.assertEqual(len(facts), 5)
        self.assertTrue(all(fact["subject"] for fact in facts))
        self.assertEqual(facts[0]["relation"], "creator")
        self.assertEqual(facts[0]["object"], "Beta")
        self.assertEqual(facts[4]["relation"], "born_in_city")

    def test_unmatched_line_is_kept_without_a_triple(self):
        facts = mh.parse_facts("1. This sentence matches no template.\n")
        self.assertIsNone(facts[0]["subject"])

    def test_trailing_period_is_removed_once(self):
        facts = mh.parse_facts("7. The capital of Zeta is Washington, D.C..\n")
        self.assertEqual(facts[0]["object"], "Washington, D.C.")

    def test_blank_and_unnumbered_lines_are_skipped(self):
        facts = mh.parse_facts("\nHere is a list of facts:\n\n3. Alpha was created by Beta.\n")
        self.assertEqual(len(facts), 1)


class SlotGraphTests(unittest.TestCase):
    def setUp(self):
        self.facts = mh.parse_facts(CONTEXT)
        self.index = mh.slot_index(self.facts)

    def test_latest_graph_keeps_the_largest_serial(self):
        graph = mh.latest_graph(self.index)
        self.assertIn(
            {"relation": "creator", "object": "gamma", "serial": 20},
            graph["alpha"],
        )
        self.assertNotIn(
            {"relation": "creator", "object": "beta", "serial": 10},
            graph["alpha"],
        )

    def test_forward_graph_keeps_every_branch(self):
        graph = mh.forward_graph(self.index)
        objects = [edge["object"] for edge in graph["alpha"]]
        self.assertEqual(sorted(objects), ["beta", "gamma"])


class SearchTests(unittest.TestCase):
    def setUp(self):
        self.facts = mh.parse_facts(CONTEXT)
        self.index = mh.slot_index(self.facts)
        self.graph = mh.forward_graph(self.index)

    def test_finds_the_two_hop_path_to_gold(self):
        paths = mh.search_paths(self.graph, ["alpha"], "Newland")
        self.assertEqual(len(paths), 1)
        self.assertEqual([step["relation"] for step in paths[0]], ["creator", "citizenship"])

    def test_respects_max_depth(self):
        self.assertEqual(mh.search_paths(self.graph, ["alpha"], "Newland", max_depth=1), [])

    def test_missing_gold_returns_no_path(self):
        self.assertEqual(mh.search_paths(self.graph, ["alpha"], "Atlantis"), [])


class CandidateTests(unittest.TestCase):
    def setUp(self):
        self.facts = mh.parse_facts(CONTEXT)

    def test_latest_consistent_candidate_is_marked(self):
        cases = mh.build_candidates(
            ["What is the citizenship of the creator of Alpha?"], [["Newland"]], self.facts
        )
        self.assertEqual(len(cases), 1)
        case = cases[0]
        self.assertTrue(case["latest_consistent"])
        self.assertEqual(case["conflict_slots"], 1)
        self.assertEqual(case["conflict_signature"], ["alpha|creator"])

    def test_gold_following_an_older_branch_is_not_latest_consistent(self):
        cases = mh.build_candidates(
            ["What is the citizenship of the creator of Alpha?"], [["Oldland"]], self.facts
        )
        self.assertEqual(len(cases), 1)
        self.assertFalse(cases[0]["latest_consistent"])

    def test_frozen_case_numbers_are_skipped(self):
        questions = ["What is the citizenship of the creator of Alpha?"] * 4
        answers = [["Newland"]] * 4
        cases = mh.build_candidates(questions, answers, self.facts)
        self.assertEqual([case["case"] for case in cases], ["no0", "no1", "no2"])


class AnchorTests(unittest.TestCase):
    def test_gold_is_not_used_as_an_anchor(self):
        vocabulary = {"alpha", "newland"}
        anchors = mh.question_anchors("Is Newland the creator of Alpha?", vocabulary, "Newland")
        self.assertEqual(anchors, ["alpha"])

    def test_longest_entity_wins(self):
        vocabulary = {"alpha", "alpha beta"}
        anchors = mh.question_anchors("Question about Alpha Beta?", vocabulary, "other")
        self.assertEqual(anchors[0], "alpha beta")


class PacketTests(unittest.TestCase):
    def test_packet_lists_each_case_and_gold(self):
        facts = mh.parse_facts(CONTEXT)
        cases = mh.build_candidates(
            ["What is the citizenship of the creator of Alpha?"], [["Newland"]], facts
        )
        payload = {
            "label_status": "provisional_pending_human_confirmation",
            "dataset_evidence": {"dataset": "d", "subset": "s", "source_path": "p"},
            "corpus_facts": len(facts),
            "corpus_facts_parsed": len(facts),
            "frozen_cases": list(mh.FROZEN),
            "review_rule": "rule",
            "selected": cases,
        }
        packet = mh.render_packet(payload)
        self.assertIn("### no0 — gold `Newland`", packet)
        self.assertIn("confirm", packet)
        self.assertIn("association football", packet)


if __name__ == "__main__":
    unittest.main()
