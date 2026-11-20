"""Offline checks for the fixed-context answer-arbitration harness."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

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


class CaseGateTests(unittest.TestCase):
    def test_default_cases_exclude_no3(self):
        self.assertNotIn("no3", arbitration.DEFAULT_CASES)
        self.assertEqual(len(arbitration.DEFAULT_CASES), 6)


if __name__ == "__main__":
    unittest.main()
