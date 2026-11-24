"""Offline checks for the deterministic SH latest-slot case selector."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import mh_chain_candidates as mh
import sh_latest_slot_cases as sh


CONTEXT = """1. Alpha is associated with the sport of oldball.
2. Alpha is associated with the sport of newball.
3. Beta is associated with the sport of oldball.
4. Gamma is famous for Old Work.
5. Delta is famous for Old Work.
"""


def question(case: str, text: str, gold: str, serials: list[int]) -> dict:
    return {
        "qa_pair_id": f"factconsolidation_sh_6k_{case}",
        "question": text,
        "golds": [gold],
        "retrieved_serials": serials,
        "retrieved_n": len(serials),
        "search_correct": False,
    }


def source(questions: list[dict]) -> dict:
    return {"results": [{"questions": questions}]}


class SelectionTests(unittest.TestCase):
    def test_selects_only_latest_gold_with_older_competitor(self):
        src = source([
            question("no0", "Which sport is Alpha associated with?", "newball", [1, 2]),
            question("no1", "Which sport is Beta associated with?", "oldball", [3]),
            question("no2", "What is Alpha associated with?", "newball", [1, 2, 3]),
        ])
        selected, excluded, counts = sh.select_cases(src, CONTEXT, mh)
        self.assertEqual([item["case"] for item in selected], ["no0", "no2"])
        self.assertEqual(selected[0]["selected_serial"], 2)
        self.assertEqual([row["serial"] for row in selected[0]["older_competitors_in_context"]], [1])
        self.assertEqual(counts["selected"], 2)
        self.assertEqual(counts["excluded"], 1)
        self.assertEqual(counts["selected_source_search_correct"], 0)

    def test_prefers_latest_gold_bearing_fact_when_old_value_contains_gold(self):
        src = source([
            question("no0", "Which sport is Alpha associated with?", "ball", [1, 2]),
        ])
        selected, excluded, _ = sh.select_cases(src, CONTEXT, mh)
        self.assertEqual([item["case"] for item in selected], ["no0"])
        self.assertEqual(selected[0]["selected_serial"], 2)
        self.assertEqual(excluded, [])

    def test_unmeasured_source_score_is_null_not_zero(self):
        q = question("no0", "Which sport is Alpha associated with?", "newball", [1, 2])
        del q["search_correct"]
        _, _, counts = sh.select_cases(source([q]), CONTEXT, mh)
        self.assertIsNone(counts["selected_source_search_correct"])

    def test_rejects_gold_not_latest(self):
        src = source([
            question("no0", "Which sport is Alpha associated with?", "oldball", [1, 2]),
        ])
        selected, excluded, _ = sh.select_cases(src, CONTEXT, mh)
        self.assertEqual(selected, [])
        self.assertEqual(excluded[0]["reason_class"], "gold_is_not_latest")

    def test_rejects_missing_older_competitor(self):
        src = source([
            question("no0", "Which sport is Beta associated with?", "oldball", [3]),
        ])
        selected, excluded, _ = sh.select_cases(src, CONTEXT, mh)
        self.assertEqual(selected, [])
        self.assertEqual(excluded[0]["reason_class"], "no_older_competitor_in_context")

    def test_rejects_gold_fact_without_question_named_subject(self):
        src = source([
            question("no0", "Which sport is somebody else associated with?", "newball", [1, 2]),
        ])
        selected, excluded, _ = sh.select_cases(src, CONTEXT, mh)
        self.assertEqual(selected, [])
        self.assertEqual(excluded[0]["reason_class"], "no_question_named_gold_fact")


if __name__ == "__main__":
    unittest.main()
