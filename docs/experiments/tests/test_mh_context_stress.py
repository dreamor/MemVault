"""Offline checks for the deterministic long-context stress builder."""
from __future__ import annotations

import random
import sys
import unittest
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import mh_chain_candidates as mh
import mh_context_stress as stress


def fact(serial: int, subject: str, relation: str, obj: str) -> dict:
    line = f"{serial}. {subject} {relation} {obj}."
    parsed = mh.parse_facts(line)[0]
    assert parsed["serial"] == serial
    return parsed


class EligibilityTests(unittest.TestCase):
    def setUp(self):
        self.facts = [
            fact(1, "Alpha", "was created by", "Beta"),
            fact(2, "Alpha", "was created by", "Gamma"),
            fact(3, "Beta", "is a citizen of", "Oldland"),
            fact(4, "Gamma", "is a citizen of", "Newland"),
            fact(5, "Delta", "was born in the city of", "Rome"),
            fact(6, "Epsilon", "was born in the city of", "Paris"),
        ]
        self.by_serial = {item["serial"]: item for item in self.facts}
        self.question = {
            "golds": ["Newland"],
            "retrieved_serials": [1, 4, 5],
        }
        self.chain = {"support_chain": [{"serial": 1}, {"serial": 4}]}

    def test_excludes_delivered_chain_slots_entities_and_gold(self):
        eligible = stress.eligible_distractors(
            question=self.question,
            chain_question=self.chain,
            all_facts=self.facts,
            facts_by_serial=self.by_serial,
            mh=mh,
        )
        # 2 shares Alpha/creator, 3 shares Beta, 4 is support, 1/5 are delivered;
        # only the unrelated Epsilon fact remains.
        self.assertEqual([item["serial"] for item in eligible], [6])

    def test_requires_enough_eligible_facts(self):
        with self.assertRaises(ValueError):
            stress.build_stress_context(
                source={
                    "results": [{"questions": [{
                        "question_id": "factconsolidation_mh_6k_no0",
                        "golds": ["Newland"],
                        "retrieved_contents": ["1. Alpha was created by Beta."],
                        "retrieved_serials": [1],
                    }]}]
                },
                chains={"questions": [{
                    "question_id": "factconsolidation_mh_6k_no0",
                    "support_chain": [{"serial": 1}],
                }]},
                all_facts=self.facts,
                distractor_count=10,
                seed=1,
                mh=mh,
            )


class PlacementTests(unittest.TestCase):
    def test_preserves_original_order_and_adds_every_distractor(self):
        original = ["1. one", "2. two", "3. three"]
        distractors = [f"d{i}" for i in range(20)]
        merged = stress.distribute_distractors(
            original, distractors, rng=random.Random("test")
        )
        self.assertEqual(len(merged), 23)
        self.assertEqual([line for line in merged if not line.startswith("d")], original)
        self.assertEqual(Counter(line for line in merged if line.startswith("d")), Counter(distractors))

    def test_is_deterministic_for_the_same_seed(self):
        original = ["1. one", "2. two"]
        distractors = [f"d{i}" for i in range(8)]
        first = stress.distribute_distractors(original, distractors, rng=random.Random("x"))
        second = stress.distribute_distractors(original, distractors, rng=random.Random("x"))
        self.assertEqual(first, second)


if __name__ == "__main__":
    unittest.main()
