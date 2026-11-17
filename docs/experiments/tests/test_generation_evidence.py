"""Offline completion evidence and literal-Markdown judge contract tests."""
from __future__ import annotations

import copy
import json
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from test_benchmarks import OfflineCase, client_for, completion
import judge_grounding as grounding


def response(text, finish="stop", model="fixture-model"):
    result = completion(text, finish_reason=finish)
    result.model = model
    return result


def quote(step, text):
    return json.dumps({"step": step, "quote": text})


class EvidenceCase(OfflineCase):
    def setUp(self):
        super().setUp()
        self.stack.enter_context(patch.object(grounding.beam, "_REDACTION_VALUES", set()))

    def generate(self, text, finish="stop"):
        records = []
        original = response(text, finish)
        fake = client_for(original)
        returned = grounding.recorded_completion(fake, trace=records, model="requested-model",
                                                 messages=[{"role": "user", "content": "fixture question"}],
                                                 max_tokens=64, temperature=0.0)
        self.assertIs(returned, original)
        fake.chat.completions.create.assert_called_once()
        return grounding.generation_status(text, records), records


class RecordedCompletionTests(EvidenceCase):
    def test_records_full_request_before_single_call_and_returns_original_response(self):
        records = []
        original = response("fixture answer")
        request = {"model": "requested-model", "messages": [{"role": "user", "content": "fixture question"}],
                   "max_tokens": 64, "temperature": 0.0, "extra_body": {"enable_thinking": False}}
        fake = client_for()

        def create(**kwargs):
            self.assertEqual(len(records), 1)
            self.assertEqual(records[0], {"attempt": 1, "request": request,
                                          "response": None, "error": None})
            self.assertEqual(kwargs, request)
            return original

        fake.chat.completions.create.side_effect = create
        returned = grounding.recorded_completion(fake, trace=records, **request)
        self.assertIs(returned, original)
        fake.chat.completions.create.assert_called_once_with(**request)
        self.assertEqual(records[0]["response"], grounding.beam.response_data(original))
        request["messages"][0]["content"] = "changed later"
        self.assertEqual(records[0]["request"]["messages"][0]["content"], "fixture question")

    def test_no_trace_still_makes_one_call_and_returns_original(self):
        original = response("fixture answer")
        fake = client_for(original)
        self.assertIs(grounding.recorded_completion(fake, model="fixture-model"), original)
        fake.chat.completions.create.assert_called_once_with(model="fixture-model")

    def test_failed_request_is_preserved_and_original_exception_is_reraised_without_retry(self):
        records = []
        secret = "private-client-key-782"
        original_error = RuntimeError("failure with " + secret)
        fake = client_for()
        fake.api_key = secret
        request = {"model": "fixture-model", "messages": [{"role": "user", "content": secret}]}

        def create(**kwargs):
            self.assertEqual(len(records), 1)
            self.assertIsNone(records[0]["response"])
            self.assertIsNone(records[0]["error"])
            self.assertNotIn(secret, json.dumps(records))
            self.assertEqual(kwargs, request)  # Actual SDK input is not redacted.
            raise original_error

        fake.chat.completions.create.side_effect = create
        with self.assertRaises(RuntimeError) as caught:
            grounding.recorded_completion(fake, trace=records, **request)
        self.assertIs(caught.exception, original_error)
        fake.chat.completions.create.assert_called_once_with(**request)
        self.assertEqual(records[0]["error"], "failure with [REDACTED]")
        self.assertEqual(records[0]["request"]["messages"][0]["content"], "[REDACTED]")
        self.assertIsNone(records[0]["response"])

    def test_no_trace_errors_are_reraised_without_transport_retry(self):
        original_error = RuntimeError("fixture unavailable")
        fake = client_for(original_error)
        with self.assertRaises(RuntimeError) as caught:
            grounding.recorded_completion(fake, model="fixture-model")
        self.assertIs(caught.exception, original_error)
        fake.chat.completions.create.assert_called_once()

    def test_request_response_and_error_redaction_share_registered_secrets(self):
        secret = "private-extra-body-key-472"
        records = []
        original = response("echo " + secret)
        fake = client_for(original, RuntimeError("retry unavailable " + secret))
        grounding.recorded_completion(fake, trace=records, model="fixture-model",
                                      extra_body={"api_key": secret}, messages=[{"role": "user", "content": secret}])
        with self.assertRaises(RuntimeError):
            grounding.recorded_completion(fake, trace=records, model="fixture-model")
        self.assertEqual([record["attempt"] for record in records], [1, 2])
        self.assertNotIn(secret, json.dumps(records))
        self.assertEqual(records[0]["request"]["extra_body"]["api_key"], "[REDACTED]")
        self.assertEqual(records[0]["response"]["choices"][0]["message"]["content"], "echo [REDACTED]")
        self.assertEqual(original.choices[0].message.content, "echo " + secret)
        self.assertEqual(fake.chat.completions.create.call_count, 2)

    def test_sdk_model_dump_is_used_for_response_evidence(self):
        data = grounding.beam.response_data(response("fixture answer"))
        original = SimpleNamespace(model_dump=lambda *, mode: data if mode == "json" else None)
        records = []
        self.assertIs(grounding.recorded_completion(client_for(original), trace=records), original)
        self.assertEqual(records[0]["response"], data)

    def test_empty_exception_message_retains_the_error_type(self):
        records = []
        with self.assertRaises(RuntimeError):
            grounding.recorded_completion(client_for(RuntimeError()), trace=records)
        self.assertEqual(records[0]["error"], "RuntimeError")


class GenerationStatusTests(EvidenceCase):
    def test_complete_status_retains_finish_model_and_usage(self):
        status, records = self.generate("1. Fixture answer.")
        self.assertEqual(status, {"schema_version": 1, "attempted": True, "valid": True,
                                  "status": "complete", "reason": None,
                                  "completion": grounding.beam.final_completion(records),
                                  "response_model": "fixture-model",
                                  "usage": {"prompt_tokens": 10, "completion_tokens": 3}})

    def test_only_stop_is_complete_and_length_does_not_retry(self):
        for finish in ("length", "content_filter", "tool_calls", "function_call", "unknown", None):
            with self.subTest(finish=finish):
                status, records = self.generate("1. Complete-looking answer.", finish)
                self.assertFalse(status["valid"])
                self.assertEqual(status["status"], "incomplete")
                self.assertEqual(len(records), 1)
                self.assertEqual(status["completion"]["finish_reason"], finish)
                self.assertIs(status["completion"]["truncated"], finish == "length")

    def test_empty_or_wrongly_typed_content_is_never_valid(self):
        for text in ("", " \n\t", None, False, 1, ["fixture answer"], {"content": "fixture answer"}):
            with self.subTest(text=text):
                status, _ = self.generate(text)
                self.assertFalse(status["valid"])
                self.assertEqual(status["status"], "incomplete")
                self.assertIsNotNone(status["reason"])

    def test_missing_or_wrongly_typed_envelopes_fail_closed(self):
        data = grounding.beam.response_data(response("fixture answer"))
        malformed = [None, "fixture answer", {}, {"choices": None}, {"choices": []},
                     {"choices": {}}, {"choices": [None]}, {"choices": [True]},
                     {"choices": [{}]}, {"choices": [{"message": None}]},
                     {"choices": [{"message": "fixture answer", "finish_reason": "stop"}]},
                     {"choices": [{"message": {}, "finish_reason": "stop"}]},
                     {"choices": [{"message": {"content": "fixture answer"}}]},
                     {"choices": [{"message": {"content": "fixture answer"}, "finish_reason": True}]},
                     {"choices": [{"message": {"content": "fixture answer"}, "finish_reason": ["stop"]}]}]
        for envelope in malformed:
            with self.subTest(envelope=envelope):
                status = grounding.generation_status("fixture answer", [{"response": envelope, "error": None}])
                self.assertFalse(status["valid"])
                self.assertEqual(status["status"], "incomplete")
                self.assertIsNotNone(status["reason"])
        self.assertTrue(grounding.generation_status("fixture answer", [{"response": data}])["valid"])
        self.assertFalse(grounding.generation_status("fixture answer", [None])["valid"])

    def test_optional_model_and_usage_are_none_when_absent(self):
        records = [{"response": grounding.beam.response_data(completion("fixture answer")), "error": None}]
        del records[0]["response"]["usage"]
        status = grounding.generation_status("fixture answer", records)
        self.assertTrue(status["valid"])
        self.assertIsNone(status["response_model"])
        self.assertIsNone(status["usage"])

    def test_matching_is_exact_not_whitespace_or_markdown_normalized(self):
        _, records = self.generate("Inspect `sample` and **verify** its label.")
        for text in ("Inspect sample and verify its label.", "Inspect `sample` and **verify** its label. ",
                     "inspect `sample` and **verify** its label.", None):
            with self.subTest(text=text):
                self.assertFalse(grounding.generation_status(text, records)["valid"])

    def test_no_records_is_not_attempted_even_when_text_is_present(self):
        for text in ("fixture answer", "", None):
            status = grounding.generation_status(text, [])
            self.assertFalse(status["attempted"])
            self.assertFalse(status["valid"])
            self.assertEqual(status["status"], "not_attempted")
            self.assertIsNone(status["completion"]["record_index"])
            self.assertIsNone(status["response_model"])
            self.assertIsNone(status["usage"])

    def test_failed_final_attempt_cannot_reuse_an_earlier_complete_response(self):
        _, records = self.generate("fixture answer")
        fake = client_for(RuntimeError("fixture unavailable"))
        with self.assertRaises(RuntimeError):
            grounding.recorded_completion(fake, trace=records, model="fixture-model")
        status = grounding.generation_status("fixture answer", records)
        self.assertTrue(status["attempted"])
        self.assertFalse(status["valid"])
        self.assertEqual(status["status"], "error")
        self.assertEqual(status["reason"], "request_error")
        self.assertEqual(status["completion"], grounding.beam.final_completion(records))
        self.assertTrue(status["completion"]["complete"])  # Earlier response is diagnostic only.
        failed = grounding.generation_status("", [records[-1]])
        self.assertEqual(failed["status"], "error")
        self.assertIsNone(failed["completion"]["record_index"])

    def test_pending_final_attempt_or_final_response_error_blocks_old_evidence(self):
        _, records = self.generate("fixture answer")
        for tail in ({"response": None, "error": None}, None,
                     {"response": copy.deepcopy(records[0]["response"]), "error": "failure"},
                     {"response": copy.deepcopy(records[0]["response"]), "error": ""}):
            with self.subTest(tail=tail):
                self.assertFalse(grounding.generation_status("fixture answer", [*records, tail])["valid"])

    def test_final_response_not_an_earlier_retry_is_used(self):
        _, records = self.generate("first answer")
        original = response("last answer", "length")
        grounding.recorded_completion(client_for(original), trace=records)
        status = grounding.generation_status("last answer", records)
        self.assertFalse(status["valid"])
        self.assertEqual(status["completion"]["record_index"], 1)
        self.assertTrue(status["completion"]["truncated"])
        complete = response("last answer")
        grounding.recorded_completion(client_for(complete), trace=records)
        self.assertTrue(grounding.generation_status("last answer", records)["valid"])
        self.assertFalse(grounding.generation_status("first answer", records)["valid"])

    def test_redacted_evidence_matches_raw_operational_text_without_mutating_it(self):
        secret = "private-registered-secret-126"
        grounding.register_secret(secret)
        raw = "Inspect `" + secret + "` and **verify** its label."
        status, records = self.generate(raw)
        self.assertTrue(status["valid"])
        self.assertNotIn(secret, json.dumps(records))
        self.assertEqual(records[0]["response"]["choices"][0]["message"]["content"],
                         "Inspect `[REDACTED]` and **verify** its label.")
        self.assertIn(secret, raw)


class GenerationPairSummaryTests(EvidenceCase):
    def pair(self, control, experiment, control_generation=None, experiment_generation=None):
        complete, _ = self.generate("fixture answer")
        return {"control": {"generation": complete if control_generation is None else control_generation,
                            "keyword_hit": control},
                "experiment": {"generation": complete if experiment_generation is None else experiment_generation,
                               "keyword_hit": experiment}}

    def test_complete_pairs_keep_all_transition_counts_and_rates(self):
        pairs = [self.pair(False, True), self.pair(True, False), self.pair(True, True), self.pair(False, False)]
        self.assertEqual(grounding.summarize_generation_pairs(pairs), {
            "policy": "paired_complete_generation_stem_hits", "n_requested": 4, "n_attempted": 4,
            "n": 4, "n_invalid": 0, "control_correct": 2, "experiment_correct": 2,
            "control": 0.5, "experiment": 0.5, "delta": 0.0,
            "improved": 1, "regressed": 1, "both_correct": 1, "both_incorrect": 1})

    def test_incomplete_or_unknown_arm_is_excluded_from_both_denominators(self):
        incomplete, _ = self.generate("fixture answer", "length")
        absent = grounding.generation_status("", [])
        pairs = [self.pair(False, True), self.pair(True, True, control_generation=incomplete),
                 self.pair(False, False, experiment_generation=absent), self.pair(True, None)]
        summary = grounding.summarize_generation_pairs(pairs)
        self.assertEqual((summary["n_requested"], summary["n_attempted"], summary["n"], summary["n_invalid"]),
                         (4, 4, 1, 3))
        self.assertEqual((summary["control_correct"], summary["experiment_correct"]), (0, 1))
        self.assertEqual((summary["control"], summary["experiment"], summary["delta"]), (0.0, 1.0, 1.0))
        self.assertEqual(summary["improved"], 1)

    def test_valid_and_hit_flags_must_be_actual_booleans(self):
        pairs = []
        for flag in (None, 0, 1, "true", "false", [], {}):
            pairs.extend([self.pair(flag, True), self.pair(False, flag)])
        pairs.append(self.pair(True, True, control_generation={"valid": 1, "attempted": True}))
        pairs.append(self.pair(True, True, experiment_generation={"valid": "true", "attempted": True}))
        summary = grounding.summarize_generation_pairs(pairs)
        self.assertEqual(summary["n"], 0)
        self.assertEqual(summary["n_invalid"], len(pairs))
        for key in ("control", "experiment", "delta"):
            self.assertIsNone(summary[key])
        for key in ("control_correct", "experiment_correct", "improved", "regressed", "both_correct", "both_incorrect"):
            self.assertEqual(summary[key], 0)

    def test_not_attempted_pairs_are_requested_but_not_attempted_and_rates_are_null(self):
        absent = grounding.generation_status("", [])
        bool_like = {"attempted": 1, "valid": False}
        pairs = [self.pair(None, None, absent, absent), self.pair(None, None, bool_like, absent)]
        summary = grounding.summarize_generation_pairs(pairs)
        self.assertEqual((summary["n_requested"], summary["n_attempted"], summary["n"], summary["n_invalid"]),
                         (2, 0, 0, 2))
        self.assertIsNone(summary["control"])
        self.assertIsNone(summary["experiment"])
        self.assertIsNone(summary["delta"])

    def test_empty_denominator_is_null_not_zero(self):
        self.assertEqual(grounding.summarize_generation_pairs([]), {
            "policy": "paired_complete_generation_stem_hits", "n_requested": 0, "n_attempted": 0,
            "n": 0, "n_invalid": 0, "control_correct": 0, "experiment_correct": 0,
            "control": None, "experiment": None, "delta": None,
            "improved": 0, "regressed": 0, "both_correct": 0, "both_incorrect": 0})


class LiteralMarkdownJudgeTests(EvidenceCase):
    body = "Inspect `sample` and **verify** its label."
    plan = "1. " + body

    def test_exact_markdown_quote_is_positive_but_stripped_quote_is_unknown(self):
        literal = grounding.parse_grounded_judgment(quote(1, self.body), self.plan, finish_reason="stop")
        self.assertTrue(literal["valid"])
        self.assertTrue(literal["avoided"])
        for stripped in ("Inspect sample and verify its label.", "Inspect sample and **verify** its label.",
                         "Inspect `sample` and verify its label.", "inspect `sample` and **verify** its label.",
                         "Inspect `sample`, and **verify** its label."):
            with self.subTest(stripped=stripped):
                result = grounding.parse_grounded_judgment(quote(1, stripped), self.plan, finish_reason="stop")
                self.assertFalse(result["valid"])
                self.assertIsNone(result["avoided"])
                self.assertEqual(result["reason"], "ungrounded_quote")

    def test_prompt_distinguishes_semantic_paraphrases_from_literal_quote_copy(self):
        fake = client_for(response(quote(1, self.body)))
        result = grounding.judge_quote(fake, "requested-judge", "Check the sample label", self.plan)
        self.assertTrue(result["avoided"])
        request = fake.chat.completions.create.call_args.kwargs
        prompt = request["messages"][0]["content"]
        for text in ("Semantic paraphrases count", "quoted evidence must NEVER be paraphrased",
                     "Copy each quote EXACTLY and literally from the Markdown source", "Preserve backticks",
                     "**bold markers**", "punctuation and case", "do not silently strip Markdown",
                     "actual plan execution order, not numerically sorted", "Generic quoting example only"):
            self.assertIn(text, prompt)
        self.assertNotIn("backfilling", prompt)
        self.assertNotIn("NOT NULL", prompt)
        attempt = result["metadata"]["attempts"][0]
        self.assertEqual(attempt["request"], request)
        self.assertEqual(attempt["prompt_version"], grounding.JUDGE_PROMPT_VERSION)
        self.assertEqual(result["metadata"]["prompt_version"], grounding.JUDGE_PROMPT_VERSION)
        self.assertEqual(attempt["raw"], quote(1, self.body))
        self.assertEqual(attempt["response_model"], "fixture-model")
        self.assertEqual(attempt["usage"], {"prompt_tokens": 10, "completion_tokens": 3})
        self.assertIsNone(attempt["error"])

    def test_length_retry_keeps_both_full_requests_and_existing_budgets(self):
        fake = client_for(response('{"step":1', "length"), response(quote(1, self.body)))
        result = grounding.judge_quote(fake, "requested-judge", "Check the sample label", self.plan)
        self.assertTrue(result["avoided"])
        attempts = result["metadata"]["attempts"]
        self.assertEqual(len(attempts), 2)
        self.assertEqual([attempt["max_tokens"] for attempt in attempts],
                         [grounding.JUDGE_MAX_TOKENS, grounding.JUDGE_RETRY_MAX_TOKENS])
        self.assertEqual([attempt["finish_reason"] for attempt in attempts], ["length", "stop"])
        self.assertEqual(attempts[0]["raw"], '{"step":1')
        for attempt, call in zip(attempts, fake.chat.completions.create.call_args_list):
            self.assertEqual(attempt["request"], call.kwargs)
            self.assertEqual(attempt["prompt_version"], grounding.JUDGE_PROMPT_VERSION)
        self.assertEqual(attempts[0]["request"]["messages"], attempts[1]["request"]["messages"])

    def test_retry_request_is_already_in_metadata_when_the_sdk_is_called(self):
        metadata_seen = {}
        parse = grounding.parse_grounded_judgment
        fake = client_for()

        def remember_metadata(*args, **kwargs):
            metadata_seen["value"] = kwargs["metadata"]
            return parse(*args, **kwargs)

        def create(**kwargs):
            if fake.chat.completions.create.call_count == 1:
                return response('{"step":1', "length")
            attempts = metadata_seen["value"]["attempts"]
            self.assertEqual(len(attempts), 2)
            self.assertEqual(attempts[1]["request"], kwargs)
            self.assertEqual(attempts[1]["prompt_version"], grounding.JUDGE_PROMPT_VERSION)
            self.assertIsNone(attempts[1]["raw"])
            self.assertIsNone(attempts[1]["error"])
            return response(quote(1, self.body))

        fake.chat.completions.create.side_effect = create
        with patch.object(grounding, "parse_grounded_judgment", side_effect=remember_metadata):
            result = grounding.judge_quote(fake, "requested-judge", "Check the label", self.plan)
        self.assertTrue(result["valid"])
        self.assertEqual(fake.chat.completions.create.call_count, 2)

    def test_failed_retry_retains_its_full_sanitized_request_and_previous_raw(self):
        secret = "private-judge-client-key-813"
        first_raw = '{"step":1'
        fake = client_for(response(first_raw, "length"), RuntimeError("retry failed " + secret))
        fake.api_key = secret
        raw_plan = "1. Inspect `" + secret + "` and **verify** its label."
        result = grounding.judge_quote(fake, "requested-judge", "Check the label", raw_plan)
        self.assertFalse(result["valid"])
        self.assertEqual(result["reason"], "request_error")
        self.assertEqual(fake.chat.completions.create.call_count, 2)
        attempts = result["metadata"]["attempts"]
        self.assertEqual(len(attempts), 2)
        self.assertEqual(attempts[0]["raw"], first_raw)
        self.assertIsNone(result["raw"])  # the final attempt returned no response
        self.assertIsNone(result["metadata"]["finish_reason"])
        self.assertIsNone(attempts[1]["raw"])
        self.assertIsNone(attempts[1]["usage"])
        self.assertIsNone(attempts[1]["response_model"])
        self.assertEqual(attempts[1]["error"], "retry failed [REDACTED]")
        self.assertEqual(attempts[1]["request"]["max_tokens"], grounding.JUDGE_RETRY_MAX_TOKENS)
        self.assertNotIn(secret, json.dumps(result))
        self.assertIn(secret, fake.chat.completions.create.call_args.kwargs["messages"][0]["content"])
        self.assertIn("**verify**", attempts[1]["request"]["messages"][0]["content"])

    def test_initial_judge_failure_still_records_request_and_prompt_version(self):
        fake = client_for(RuntimeError("fixture judge unavailable"))
        result = grounding.judge_quote(fake, "requested-judge", "Check the label", self.plan)
        fake.chat.completions.create.assert_called_once()
        attempt = result["metadata"]["attempts"][0]
        self.assertEqual(attempt["request"], fake.chat.completions.create.call_args.kwargs)
        self.assertEqual(attempt["prompt_version"], grounding.JUDGE_PROMPT_VERSION)
        self.assertEqual(attempt["error"], "fixture judge unavailable")
        self.assertFalse(result["valid"])

    def test_ungrounded_or_malformed_stop_output_does_not_retry_or_strip_markdown(self):
        for raw in (quote(1, "Inspect sample and verify its label."), "malformed"):
            with self.subTest(raw=raw):
                fake = client_for(response(raw))
                result = grounding.judge_quote(fake, "requested-judge", "Check the label", self.plan)
                fake.chat.completions.create.assert_called_once()
                self.assertFalse(result["valid"])
                self.assertIsNone(result["avoided"])
                self.assertEqual(result["raw"], raw)


if __name__ == "__main__":
    unittest.main()
