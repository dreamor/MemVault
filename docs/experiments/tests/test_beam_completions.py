"""Completion validity and path isolation, without model or process calls."""
from __future__ import annotations

import contextlib
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import test_benchmarks as support
from test_benchmarks import OfflineCase, beam, client_for, completion, record


class BeamCompletionTests(OfflineCase):
    run_main = support.BeamTests.run_main
    valid_judge_text = support.BeamTests.valid_judge_text

    def test_default_budget_matches_historical_baseline(self):
        self.assertEqual(beam.build_parser().parse_args([]).max_tokens, 1200)

    def test_metadata_uses_final_success_not_earlier_empty_truncation(self):
        client = client_for(completion("", "length"), completion("complete", "stop"))
        trace = []
        self.assertEqual(beam.answer(client, "question", "rule", 1200, trace), "complete")
        metadata = beam.final_completion(trace)
        self.assertEqual(metadata["record_index"], 1)
        self.assertEqual(metadata["finish_reason"], "stop")
        self.assertFalse(metadata["truncated"])

    def test_unknown_finish_reason_is_explicit_missing_evidence(self):
        metadata = beam.final_completion([{"response": {"choices": []}}])
        self.assertIsNone(metadata["finish_reason"])
        self.assertFalse(metadata["finish_reason_available"])

    def test_truncated_control_retains_raw_and_invalidates_pair(self):
        good = self.valid_judge_text()
        client = client_for(completion("cut control", "length"), "experiment", good)
        code, output, text, _, _ = self.run_main(client, [self.probe["raw_annotation"]])
        self.assertEqual(code, 1)
        self.assertEqual(output["run_status"]["status"], "invalid")
        self.assertIn("truncated_answers", output["run_status"]["errors"])
        row = output["probes"][0]
        self.assertEqual(row["control_response"], "cut control")
        self.assertEqual(row["control_records"][0]["response"]["choices"][0]["finish_reason"], "length")
        self.assertTrue(row["control_truncated"])
        self.assertFalse(row["control_judge"]["parsed"])
        self.assertEqual(row["control_judge"]["records"], [])
        self.assertEqual(output["summary"]["n_judged"], 0)
        self.assertIsNone(output["summary"]["experiment_adherence"])
        self.assertEqual(output["summary"]["n_truncated_control"], 1)
        self.assertEqual(output["validity"]["paired_labels"]["n"], 0)
        self.assertNotIn("SCORING COMPLETE", text)
        self.assertEqual(client.chat.completions.create.call_count, 3)

    def test_truncated_experiment_cannot_inflate_headline(self):
        client = client_for("control", completion("cut experiment", "length"), self.valid_judge_text())
        code, output, _, _, _ = self.run_main(client, [self.probe["raw_annotation"]])
        self.assertEqual(code, 1)
        self.assertEqual(output["summary"]["n_truncated_experiment"], 1)
        self.assertEqual(output["summary"]["n_judged"], 0)
        self.assertEqual(output["validity"]["paired_labels"]["n"], 0)

    def test_complete_json_from_truncated_judge_is_invalid(self):
        good = self.valid_judge_text()
        client = client_for("control", "experiment", completion(good, "length"), good)
        code, output, _, _, _ = self.run_main(client, [self.probe["raw_annotation"]])
        self.assertEqual(code, 1)
        judge = output["probes"][0]["control_judge"]
        self.assertTrue(judge["parsed"])
        self.assertEqual(judge["raw_verdict"], good)
        self.assertFalse(judge["validation"]["valid"])
        self.assertIn("judge_truncated", judge["validation"]["errors"])
        self.assertIn("truncated_judges", output["run_status"]["errors"])
        self.assertEqual(output["summary"]["n_truncated_judges"], 1)
        self.assertEqual(output["summary"]["n_judged"], 0)

    def test_truncated_pair_is_not_regraded_for_stability(self):
        good = self.valid_judge_text()
        client = client_for(completion("cut control", "length"), "experiment", good,
                            "control", "experiment", good, good, good, good)
        code, output, _, _, _ = self.run_main(
            client, [self.probe["raw_annotation"]] * 2, extra=("--judge-stability", "2"))
        self.assertEqual(code, 1)
        self.assertEqual(output["summary"]["n_judged"], 1)
        self.assertEqual(output["judge_stability"]["eligible_n"], 1)
        self.assertEqual(output["judge_stability"]["n"], 1)
        self.assertEqual(output["judge_stability"]["n_valid_arm_pairs"], 2)

    def test_truncated_stability_judge_is_not_an_agreement(self):
        good = self.valid_judge_text()
        client = client_for("control", "experiment", good, good, completion(good, "length"), good)
        code, output, _, _, _ = self.run_main(
            client, [self.probe["raw_annotation"]], extra=("--judge-stability", "1"))
        self.assertEqual(code, 1)
        self.assertIn("invalid_stability_grades", output["run_status"]["errors"])
        self.assertEqual(output["judge_stability"]["n_valid_arm_pairs"], 1)
        self.assertFalse(output["judge_stability"]["items"][0]["arms"]["control"]["valid_pair"])

    def test_complete_retry_remains_valid_with_final_metadata(self):
        good = self.valid_judge_text()
        client = client_for(completion("", "length"), "control", "experiment", good, good)
        code, output, _, _, _ = self.run_main(client, [self.probe["raw_annotation"]])
        self.assertEqual(code, 0)
        self.assertFalse(output["probes"][0]["control_truncated"])
        self.assertEqual(output["probes"][0]["control_completion"]["record_index"], 1)
        self.assertEqual(output["summary"]["n_judged"], 1)
        self.assertEqual(client.chat.completions.create.call_args_list[0].kwargs["max_tokens"], 1200)

    def test_modern_unknown_or_aborted_answers_are_invalid(self):
        for reason in (None, "content_filter", "tool_calls"):
            with self.subTest(reason=reason):
                client = client_for(completion("incomplete control", reason), "experiment", self.valid_judge_text())
                code, output, _, _, _ = self.run_main(client, [self.probe["raw_annotation"]])
                self.assertEqual(code, 1)
                self.assertTrue(output["probes"][0]["control_incomplete"])
                self.assertFalse(output["probes"][0]["control_completion"]["complete"])
                self.assertIn("incomplete_answers", output["run_status"]["errors"])
                self.assertEqual(output["summary"]["n_judged"], 0)
                self.assertEqual(output["summary"]["n_incomplete_control"], 1)
                self.assertEqual(client.chat.completions.create.call_count, 3)

    def test_modern_unknown_or_aborted_judges_are_not_scores(self):
        for reason in (None, "content_filter", "tool_calls"):
            with self.subTest(reason=reason):
                good = self.valid_judge_text()
                client = client_for("control", "experiment", completion(good, reason), good)
                code, output, _, _, _ = self.run_main(client, [self.probe["raw_annotation"]])
                self.assertEqual(code, 1)
                grade = output["probes"][0]["control_judge"]
                self.assertTrue(grade["parsed"])
                self.assertFalse(grade["validation"]["valid"])
                self.assertIn("judge_incomplete", grade["validation"]["errors"])
                self.assertIn("incomplete_judges", output["run_status"]["errors"])
                self.assertEqual(output["summary"]["n_judged"], 0)
                self.assertEqual(output["summary"]["n_incomplete_judges"], 1)

    def test_malformed_modern_completion_evidence_cannot_be_legacy(self):
        for metadata in (None, {}, {"finish_reason": None}):
            with self.subTest(metadata=metadata):
                row = record(self.probe)
                row["control_completion"] = metadata
                self.assertFalse(beam.grade_for_record(row, "control")["valid"])
                self.assertEqual(beam.summarise([row])["n_judged"], 0)

    def test_malformed_judge_completion_is_invalid_without_crashing_summary(self):
        row = record(self.probe)
        row["control_judge"]["completion"] = None
        self.assertFalse(beam.grade_for_record(row, "control")["valid"])
        summary = beam.summarise([row])
        self.assertEqual(summary["n_judged"], 0)
        self.assertEqual(summary["n_incomplete_judges"], 1)

    def test_historical_labels_are_not_rewritten_for_absent_completion_fields(self):
        row = record(self.probe, c=False, e=True)
        summary = beam.summarise([row])
        self.assertEqual(summary["n_judged"], 1)
        self.assertEqual(summary["control_adherence"], 0)
        self.assertEqual(summary["experiment_adherence"], 1)

    def test_server_isolation_accepts_a_symlink_alias_of_owned_root(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            real = root / "real"
            real.mkdir()
            alias = root / "alias"
            try:
                alias.symlink_to(real, target_is_directory=True)
            except (OSError, NotImplementedError) as exc:
                self.skipTest(f"symlinks unavailable: {exc}")
            result = unittest.TestResult()
            with patch("tempfile.TemporaryDirectory", return_value=contextlib.nullcontext(str(alias))):
                support.EvidenceTests("test_both_servers_override_vault_configuration_before_popen").run(result)
            self.assertEqual(result.errors, [])
            self.assertEqual(result.failures, [])


if __name__ == "__main__":
    unittest.main()
