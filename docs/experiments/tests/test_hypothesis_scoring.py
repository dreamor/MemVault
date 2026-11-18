"""Offline H5/H6/H7 scoring tests; no SDK, network, subprocess or model loading."""
from __future__ import annotations

import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
from types import ModuleType
import unittest
from unittest.mock import MagicMock, patch

from test_benchmarks import OfflineCase, client_for, completion

# H6 still constructs its client at import; H5/H7 defer construction until use.
_fake_sdk = ModuleType("openai")
_fake_sdk.OpenAI = MagicMock(side_effect=lambda **kwargs: client_for())
with patch.dict(sys.modules, {"openai": _fake_sdk}), patch.dict(
    "os.environ", {"VERIFY_BASE_URL": "http://mock.invalid"}, clear=True
):
    import verify_hypotheses as h5
    import verify_h6 as h6
    import verify_h7 as h7
_import_constructor_calls = _fake_sdk.OpenAI.call_count
_import_clients = (h5.client, h6.client, h7.client)
import judge_grounding as grounding


def quote(step, text):
    return json.dumps({"step": step, "quote": text})


def parse_complete(raw, plan, **kwargs):
    """Pure JSON fixtures explicitly supply a completed response envelope."""
    kwargs.setdefault("finish_reason", "stop")
    return grounding.parse_grounded_judgment(raw, plan, **kwargs)


def response(raw, finish="stop", model="fixture-judge"):
    result = completion(raw)
    result.choices[0].finish_reason = finish
    result.model = model
    return result


def traced_plan(text, finish="stop"):
    def generate(*args, trace=None):
        if trace is not None:
            trace.append({"attempt": len(trace) + 1, "request": {"fixture_args": list(args)},
                          "response": grounding.beam.response_data(response(text, finish, "fixture-answer")),
                          "error": None})
        return text
    return generate


def traced_session(formatted, *, memory_id="fixture-skill-id", source="match"):
    def session(context, project=None, *, trace=None):
        results = [{"memory": {"id": memory_id, "type": "skill"},
                    "search_mode": "session", "hit_sources": [source]}] if "[SKILL:" in formatted else []
        payload = {"ok": True, "error": None, "data": {"formatted": formatted, "results": results}}
        if trace is not None:
            trace.append({"request": {"method": "POST", "path": "/api/session",
                                      "body": {"context_hint": context, "project": project}},
                          "status": 200, "response": payload,
                          "response_raw": json.dumps(payload), "error": None})
        return formatted
    return session


class GroundingTests(OfflineCase):
    plan = "Preamble: do not quote me.\n1. Set DASHBOARD_CDN to cdn-v2.memvault.io.\n2. Run smoke-pages.sh.\n3) Check /healthz."

    def parse(self, raw, **kwargs):
        return parse_complete(raw, self.plan, **kwargs)

    def test_imports_used_only_mock_openai(self):
        # Snapshot the import phase, without resetting shared mocks or depending
        # on clients lazily initialized by another test's protected main.
        self.assertEqual(_import_constructor_calls, 1)
        self.assertIsNone(_import_clients[0])
        self.assertIsNone(_import_clients[2])
        _import_clients[1].chat.completions.create.assert_not_called()

    def test_step_must_be_non_bool_positive_integer_and_real_printed_number(self):
        for step in (True, False, 0, -1, 1.0, "1", None, 4):
            with self.subTest(step=step):
                result = self.parse(quote(step, "Set DASHBOARD_CDN"))
                self.assertIsNone(result["avoided"])
                self.assertFalse(result["valid"])
        for step, text in ((1, "Set DASHBOARD_CDN"), (2, "Run smoke-pages.sh"), (3, "Check /healthz")):
            self.assertTrue(self.parse(quote(step, text))["avoided"])
        result = parse_complete(quote(7, "Run smoke-pages.sh"), "7. Run smoke-pages.sh.")
        self.assertTrue(result["avoided"])
        result = parse_complete(quote(1, "Run smoke-pages.sh"), "7. Run smoke-pages.sh.")
        self.assertIsNone(result["avoided"])

    def test_only_double_null_is_valid_negative(self):
        result = self.parse(quote(None, None))
        self.assertIs(result["avoided"], False)
        self.assertTrue(result["valid"])
        for raw in (quote(None, "Run smoke-pages.sh"), quote(1, None), quote(None, ""), quote(None, False)):
            with self.subTest(raw=raw):
                self.assertIsNone(self.parse(raw)["avoided"])

    def test_quote_must_be_nonempty_contiguous_from_the_claimed_step(self):
        for step, text in ((1, ""), (1, " \n\t"), (1, "invented precaution"),
                           (1, "Run smoke-pages.sh"), (2, "Set DASHBOARD_CDN"),
                           (1, "Set DASHBOARD_CDN Run smoke-pages.sh"),
                           (1, "Set ... cdn-v2.memvault.io"),
                           (1, "Preamble: do not quote me"),
                           (1, "set dashboard_cdn"), (1, "Set ＤＡＳＨＢＯＡＲＤ＿ＣＤＮ")):
            with self.subTest(step=step, text=text):
                result = self.parse(quote(step, text))
                self.assertIsNone(result["avoided"])
                self.assertFalse(result["valid"])
        duplicate = "1. Set DASHBOARD_CDN.\n1. Run smoke-pages.sh."
        self.assertIsNone(parse_complete(quote(1, "Set DASHBOARD_CDN"), duplicate)["avoided"])
        unnumbered = "Set DASHBOARD_CDN. Run smoke-pages.sh."
        self.assertIsNone(parse_complete(quote(1, "Set DASHBOARD_CDN"), unnumbered)["avoided"])
        outro = "1. Deploy the app.\nSummary: Set DASHBOARD_CDN."
        self.assertIsNone(parse_complete(quote(1, "Set DASHBOARD_CDN"), outro)["avoided"])

    def test_only_reasonable_whitespace_is_normalized_with_raw_evidence(self):
        plan = "1. Set\tDASHBOARD_CDN to\n   cdn-v2.memvault.io.\n2. Deploy."
        raw = " \n" + quote(1, "Set DASHBOARD_CDN to cdn-v2.memvault.io.") + "\n "
        result = parse_complete(raw, plan)
        self.assertTrue(result["avoided"])
        self.assertEqual(result["raw"], raw)
        self.assertFalse(result["grounding"]["raw_match"])
        self.assertTrue(result["grounding"]["normalized_match"])
        self.assertIn("\t", result["grounding"]["step_text"])

    def test_strict_json_no_fences_regex_recovery_or_duplicate_keys(self):
        for raw in (None, "", "   ", "malformed", '{"step":1',
                    'prefix {"step":1,"quote":"Set DASHBOARD_CDN"}',
                    '```json\n{"step":1,"quote":"Set DASHBOARD_CDN"}\n```',
                    '[]', 'null', 'true', '{"step":1}',
                    '{"step":1,"step":2,"quote":"Run smoke-pages.sh"}',
                    '{"step":NaN,"quote":null}',
                    '{"step":1,"quote":"Set DASHBOARD_CDN","extra":true}'):
            with self.subTest(raw=raw):
                self.assertIsNone(self.parse(raw)["avoided"])

    def test_valid_looking_truncated_completions_are_unknown_in_both_modules(self):
        for module, function in ((h5, h5.judge_pitfall_avoided), (h7, h7.judge_quote)):
            for raw in (quote(1, "Set DASHBOARD_CDN"), quote(None, None)):
                with self.subTest(module=module.__name__, raw=raw):
                    fake = client_for(response(raw, finish="length"), response(raw, finish="length"))
                    with patch.object(module, "judge_client", fake):
                        result = function("Set DASHBOARD_CDN", self.plan)
                    self.assertIsNone(result["avoided"])
                    self.assertEqual(result["raw"], raw)
                    self.assertEqual(result["metadata"]["finish_reason"], "length")
                    attempts = result["metadata"]["attempts"]
                    self.assertEqual([attempt["max_tokens"] for attempt in attempts],
                                     [grounding.JUDGE_MAX_TOKENS, grounding.JUDGE_RETRY_MAX_TOKENS])
                    self.assertEqual(len(attempts), 2)
                    self.assertTrue(all(attempt["finish_reason"] == "length" for attempt in attempts))
        self.assertIsNone(self.parse(quote(1, "Set DASHBOARD_CDN"), finish_reason="content_filter")["avoided"])

    def test_truncated_judgment_retries_once_and_accepts_completed_retry(self):
        first = response('{"step": 1', finish="length")
        second = response(quote(1, "Set DASHBOARD_CDN"), finish="stop")
        fake = client_for(first, second)
        with patch.object(h7, "judge_client", fake):
            result = h7.judge_quote("Set DASHBOARD_CDN", self.plan)
        self.assertTrue(result["avoided"])
        self.assertEqual(result["metadata"]["finish_reason"], "stop")
        calls = fake.chat.completions.create.call_args_list
        self.assertEqual([call.kwargs["max_tokens"] for call in calls],
                         [grounding.JUDGE_MAX_TOKENS, grounding.JUDGE_RETRY_MAX_TOKENS])
        self.assertEqual([attempt["finish_reason"] for attempt in result["metadata"]["attempts"]],
                         ["length", "stop"])
        self.assertEqual(result["metadata"]["attempts"][0]["raw"], '{"step": 1')
        self.assertEqual(result["raw"], quote(1, "Set DASHBOARD_CDN"))

    def test_only_stop_is_complete_including_missing_finish_metadata(self):
        raw = quote(1, "Set DASHBOARD_CDN")
        self.assertIsNone(grounding.parse_grounded_judgment(raw, self.plan)["avoided"])
        for finish in (None, "length", "content_filter", "tool_calls", "function_call", "unknown"):
            for module, function in ((h5, h5.judge_pitfall_avoided), (h7, h7.judge_quote)):
                with self.subTest(finish=finish, module=module.__name__):
                    with patch.object(module, "judge_client",
                                      client_for(response(raw, finish=finish),
                                                 response(raw, finish=finish))):
                        result = function("Set DASHBOARD_CDN", self.plan)
                    self.assertIsNone(result["avoided"])
                    self.assertEqual(result["raw"], raw)
                    self.assertEqual(result["metadata"]["finish_reason"], finish)
                    self.assertIsNone(result["error"])
        missing = completion(raw)
        if hasattr(missing.choices[0], "finish_reason"):
            del missing.choices[0].finish_reason
        with patch.object(h7, "judge_client", client_for(missing)):
            result = h7.judge_quote("Set DASHBOARD_CDN", self.plan)
        self.assertEqual(result["reason"], "missing_finish_reason")
        self.assertIsNone(result["avoided"])

    def test_complete_raw_usage_models_and_sanitized_errors_are_retained(self):
        long_quote = "Verify " + "precaution " * 80
        raw = "\n" + quote(1, long_quote) + "\n"
        fake = client_for(response(raw))
        with patch.object(h5, "judge_client", fake), patch.object(h5, "JUDGE_MODEL", "requested-judge"):
            result = h5.judge_pitfall_avoided("long precaution", "1. " + long_quote)
        self.assertTrue(result["avoided"])
        self.assertEqual(result["raw"], raw)
        self.assertGreater(len(result["raw"]), 150)
        self.assertEqual(result["metadata"]["usage"], {"prompt_tokens": 10, "completion_tokens": 3})
        self.assertEqual(result["metadata"]["requested_model"], "requested-judge")
        self.assertEqual(result["metadata"]["response_model"], "fixture-judge")
        secret = "fixture-private-api-key-827"
        fake = client_for(RuntimeError(f"401 Bearer {secret} api_key={secret}"))
        with patch.dict("os.environ", {"OPENAI_API_KEY": secret}), patch.object(h5, "judge_client", fake), patch.object(
            h5, "JUDGE_BASE_URL", f"https://user:{secret}@mock.invalid/v1?api_key={secret}#private"
        ):
            failed = h5.judge_pitfall_avoided("precaution", self.plan)
        serialized = json.dumps(failed)
        self.assertNotIn(secret, serialized)
        self.assertNotIn("user:", serialized)
        self.assertNotIn("#private", serialized)
        self.assertIn("401", failed["error"])
        self.assertEqual(failed["metadata"]["endpoint"], "https://mock.invalid/v1")
        self.assertIsNone(failed["avoided"])
        for unsafe in ('{"api_key": "unconfigured-private-value"}', "server returned sk-private-test-key"):
            sanitized = grounding.sanitize(unsafe)
            self.assertNotIn("unconfigured-private-value", sanitized)
            self.assertNotIn("sk-private-test-key", sanitized)

    def test_empty_and_failed_requests_are_unknown_with_model_metadata(self):
        for item in (response(""), response(None), RuntimeError("judge unavailable")):
            with self.subTest(item=item):
                with patch.object(h7, "judge_client", client_for(item)):
                    result = h7.judge_quote("precaution", self.plan)
                self.assertIsNone(result["avoided"])
                self.assertFalse(result["valid"])
                self.assertIn("requested_model", result["metadata"])

    def test_unknown_aggregation_and_explicit_denominators(self):
        positive = self.parse(quote(1, "Set DASHBOARD_CDN"))
        negative = self.parse(quote(None, None))
        unknown = self.parse("malformed")
        for items, expected in (([positive, positive], True), ([positive, negative], False),
                                ([negative, unknown], False), ([positive, unknown], None),
                                ([unknown], None), ([], None),
                                ([{"avoided": False, "valid": False}], None)):
            with self.subTest(items=items):
                self.assertIs(grounding.aggregate_judgments(items)["avoided"], expected)
        summary = grounding.summarize_judgments([positive, negative, unknown])
        self.assertEqual(summary, {"total": 3, "valid": 2, "unknown": 1,
                                   "positive": 1, "negative": 1, "rate": 0.5})
        self.assertIsNone(grounding.summarize_judgments([unknown])["rate"])
        self.assertIsNone(grounding.summarize_judgments([])["rate"])


class BindingCounterexampleTests(OfflineCase):
    conditions = ["Backfill users.profile_json", "Add the NOT NULL constraint"]
    correct_plan = "1.Backfill users.profile_json.\n2.Add the NOT NULL constraint."
    bindings = [{"step": 1, "quote": "Backfill users.profile_json"},
                {"step": 2, "quote": "Add the NOT NULL constraint"}]

    def test_tokenizer_switches_and_token_budgets_do_not_corrupt_replay(self):
        plan = "1. Set TOKENIZERS_PARALLELISM to false with budget 512."
        raw = quote(1, "Set TOKENIZERS_PARALLELISM to false with budget 512")
        with patch.dict("os.environ", {"TOKENIZERS_PARALLELISM": "false", "TOKEN_BUDGET": "512",
                                      "MAX_TOKENS": "512", "SERVICE_TOKEN": "private-service-token",
                                      "SERVICE_PASSWORD": "private-service-password"}), patch.object(
            h7, "judge_client", client_for(response(raw))
        ):
            result = h7.judge_quote("Configure the tokenizer", plan)
            self.assertEqual(grounding.sanitize("private-service-token private-service-password"),
                             "[REDACTED] [REDACTED]")
        self.assertTrue(result["avoided"])
        self.assertEqual(result["raw"], raw)
        self.assertEqual(result["quote"], "Set TOKENIZERS_PARALLELISM to false with budget 512")
        replay = parse_complete(result["raw"], plan,
                    finish_reason=result["metadata"]["finish_reason"])
        self.assertTrue(replay["avoided"])

    def test_step_number_and_punctuation_are_not_body_evidence(self):
        plan = "1. Backup data.\n2.Deploy the app."
        for step, text in ((1, "1."), (1, "."), (1, "..."), (2, "2."), (2, "2.Deploy")):
            with self.subTest(step=step, text=text):
                self.assertIsNone(parse_complete(quote(step, text), plan)["avoided"])
        self.assertTrue(parse_complete(quote(2, "Deploy the app"), plan)["avoided"])
        self.assertEqual(grounding.numbered_steps(plan)[1], ["Backup data."])

    def test_multi_step_obligation_supports_grounded_order_and_single_compound_step(self):
        raw = json.dumps({"bindings": self.bindings})
        result = parse_complete(raw, self.correct_plan, ordered_conditions=self.conditions)
        self.assertTrue(result["avoided"])
        self.assertEqual(result["reference_order"], [0, 1])
        self.assertEqual(len(result["bindings"]), 2)
        self.assertFalse(result["grounding_is_semantic_proof"])
        self.assertEqual(result["ordered_conditions"], self.conditions)
        self.assertEqual(result["raw"], raw)
        self.assertTrue(all(binding["grounding"]["normalized_match"] for binding in result["bindings"]))
        compound = "Before adding the NOT NULL constraint, backfill users.profile_json."
        result = parse_complete(quote(1, compound), "1. " + compound,
                                                   ordered_conditions=self.conditions)
        self.assertTrue(result["avoided"])
        self.assertEqual(len(result["bindings"]), 1)
        self.assertIsNone(parse_complete(raw, self.correct_plan,
                          ordered_conditions=self.conditions, finish_reason="length")["avoided"])

    def test_wrong_execution_order_is_unknown_even_when_each_quote_is_real(self):
        wrong_plan = "1. Add the NOT NULL constraint.\n2. Backfill users.profile_json."
        raw = json.dumps({"bindings": [{"step": 2, "quote": self.bindings[0]["quote"]},
                                       {"step": 1, "quote": self.bindings[1]["quote"]}]})
        result = parse_complete(raw, wrong_plan, ordered_conditions=self.conditions)
        self.assertIsNone(result["avoided"])
        self.assertEqual(result["reason"], "wrong_reference_order")
        self.assertTrue(all(binding["grounding"]["normalized_match"] for binding in result["bindings"]))
        # Printed labels need not be consecutive; use actual plan order, not numeric sort.
        nonconsecutive = "7. Backfill users.profile_json.\n2. Add the NOT NULL constraint."
        raw = json.dumps({"bindings": [{"step": 7, "quote": self.bindings[0]["quote"]},
                                       {"step": 2, "quote": self.bindings[1]["quote"]}]})
        self.assertTrue(parse_complete(raw, nonconsecutive,
                        ordered_conditions=self.conditions)["avoided"])

    def test_bindings_schema_is_strict_and_each_quote_is_independently_grounded(self):
        for bindings in (None, [], {}, [None], [{"step": True, "quote": "Backfill"}],
                         [{"step": None, "quote": None}],
                         [{"step": 1, "quote": "Backfill", "extra": 1}],
                         [{"step": 2, "quote": "Backfill users.profile_json"}],
                         [{"step": 1, "quote": "Backfill users.profile_json Add the NOT NULL constraint"}],
                         [*self.bindings, {"step": 2, "quote": "NOT NULL"}]):
            with self.subTest(bindings=bindings):
                result = parse_complete(json.dumps({"bindings": bindings}),
                           self.correct_plan, ordered_conditions=self.conditions)
                self.assertIsNone(result["avoided"])
        mixed = {"bindings": self.bindings, "step": None, "quote": None}
        self.assertIsNone(parse_complete(json.dumps(mixed), self.correct_plan)["avoided"])


class H5Tests(OfflineCase):
    def test_precautions_are_explicit_atomic_lists_not_semicolon_split(self):
        self.assertTrue(all(isinstance(s["precautions"], list) and s["precautions"] for s in h5.H5_SCENARIOS))
        medical = "Check renal function; avoid the contraindicated medication until cleared."
        scenario = {"precautions": [medical, "Confirm allergy history."]}
        fake = client_for(response(quote(1, medical)), response(quote(None, None)))
        with patch.object(h5, "judge_client", fake):
            result = h5.judge_h5_precautions(scenario, "1. " + medical)
        self.assertIs(result["avoided"], False)
        self.assertEqual(len(result["items"]), 2)
        self.assertEqual(fake.chat.completions.create.call_count, 2)
        prompt = fake.chat.completions.create.call_args_list[0].kwargs["messages"][0]["content"]
        self.assertIn("PRECAUTION: " + medical, prompt)

    def test_original_migration_obligation_keeps_before_constraint_across_steps(self):
        scenario = next(s for s in h5.H5_SCENARIOS if s["task_type"] == "migrate")
        correct_plan = "1. Backfill users.profile_json.\n2. Add the NOT NULL constraint."
        correct_raw = json.dumps({"bindings": [{"step": 1, "quote": "Backfill users.profile_json"},
                                               {"step": 2, "quote": "Add the NOT NULL constraint"}]})
        wrong_plan = "1. Add the NOT NULL constraint.\n2. Backfill users.profile_json."
        wrong_raw = json.dumps({"bindings": [{"step": 2, "quote": "Backfill users.profile_json"},
                                             {"step": 1, "quote": "Add the NOT NULL constraint"}]})
        for plan, raw, expected in ((correct_plan, correct_raw, True),
                                    (wrong_plan, wrong_raw, None),
                                    (wrong_plan, quote(None, None), False)):
            with self.subTest(plan=plan, raw=raw):
                fake = client_for(response(raw))
                with patch.object(h5, "judge_client", fake):
                    result = h5.judge_h5_precautions(scenario, plan)
                self.assertIs(result["avoided"], expected)
                self.assertEqual(len(result["items"]), 1)  # relation remains one original obligation
                self.assertEqual(fake.chat.completions.create.call_count, 1)
                self.assertEqual(result["items"][0]["ordered_conditions"], scenario["ordered_constraints"]["0"])
                prompt = fake.chat.completions.create.call_args.kwargs["messages"][0]["content"]
                self.assertIn("Required chronological conditions", prompt)
                self.assertIn("backfilling users.profile_json before the NOT NULL constraint is added", prompt)

    def test_main_saves_raw_results_and_unknown_rate_is_null(self):
        directory = self.stack.enter_context(tempfile.TemporaryDirectory())
        output = Path(directory) / "h5_results.json"
        raw = "malformed complete judge output"
        fake = client_for(response(raw), response(raw))
        with patch.object(h5, "__file__", str(Path(directory) / "verify_hypotheses.py")), patch.object(
            h5, "generate_response", side_effect=traced_plan("1. Set DASHBOARD_CDN.")
        ), patch.object(h5, "judge_client", fake), patch.object(
            sys, "argv", ["verify_hypotheses.py", "--hypothesis", "H5", "--samples", "1"]
        ), contextlib.redirect_stdout(io.StringIO()) as stdout:
            h5.main()
        artifact = json.loads(output.read_text())
        for arm in ("control", "experiment"):
            summary = artifact[arm]["judge_summary"]
            self.assertIsNone(summary["rate"])
            self.assertEqual(summary["valid"], 0)
            self.assertEqual(summary["unknown"], 1)
            self.assertEqual(artifact[arm]["details"][0]["judge"]["items"][0]["raw"], raw)
            self.assertEqual(artifact[arm]["compliant"], 1)  # primary keyword unchanged
        self.assertIn("null (no valid judge)", stdout.getvalue())


class H7Tests(OfflineCase):
    skill = {"content": "fixture skill", "trigger": "deploy",
             "steps": ["Set DASHBOARD_CDN before building", "Run smoke-pages.sh after the push"],
             "verification": "Check /healthz"}
    plan = "1. Set DASHBOARD_CDN before building.\n2. Run smoke-pages.sh after the push.\n3. Check /healthz."

    def test_each_step_judged_and_verification_cannot_rescue_missing_obligation(self):
        for middle, expected in ((quote(2, "Run smoke-pages.sh after the push"), True),
                                 (quote(None, None), False), ("malformed", None)):
            with self.subTest(middle=middle):
                fake = client_for(response(quote(1, "Set DASHBOARD_CDN before building")),
                                  response(middle), response(quote(3, "Check /healthz")))
                with patch.object(h7, "judge_client", fake):
                    result = h7.judge_skill(self.skill, self.plan)
                self.assertIs(result["steps"]["avoided"], expected)
                self.assertEqual(len(result["steps"]["items"]), 2)
                self.assertTrue(result["verification"]["avoided"])
                self.assertFalse(result["verification_is_execution_evidence"])
                self.assertEqual(fake.chat.completions.create.call_count, 3)
                for i, precaution in enumerate(self.skill["steps"]):
                    prompt = fake.chat.completions.create.call_args_list[i].kwargs["messages"][0]["content"]
                    self.assertIn("PRECAUTION: " + precaution + "\n", prompt)
                    self.assertNotIn("; ".join(self.skill["steps"]), prompt)

    def test_original_migration_relations_accept_multiple_steps_without_losing_order(self):
        skill = next(s["skill"] for s in h7.SCENARIOS if s["task_type"] == "migrate")
        before = "Record the row count before migration"
        after = "Check the post-migration count matches the pre-migration count"
        backfill = "Backfill users.profile_json with '{}'"
        constraint = "Add the NOT NULL constraint"
        for reversed_actions in (False, True):
            with self.subTest(reversed_actions=reversed_actions):
                middle = [constraint, backfill] if reversed_actions else [backfill, constraint]
                plan = f"1. {before}.\n2. {middle[0]}.\n3. {middle[1]}.\n4. {after}."
                raw = json.dumps({"bindings": [
                    {"step": 3 if reversed_actions else 2, "quote": backfill},
                    {"step": 2 if reversed_actions else 3, "quote": constraint}]})
                counts_raw = json.dumps({"bindings": [{"step": 1, "quote": before}, {"step": 4, "quote": after}]})
                fake = client_for(response(raw), response(counts_raw), response(quote(4, after)))
                with patch.object(h7, "judge_client", fake):
                    result = h7.judge_skill(skill, plan)
                self.assertIs(result["steps"]["avoided"], None if reversed_actions else True)
                self.assertEqual(len(result["steps"]["items"]), 2)
                self.assertTrue(result["steps"]["items"][1]["avoided"])
                self.assertTrue(result["verification"]["avoided"])
                self.assertFalse(result["verification_is_execution_evidence"])
                self.assertEqual(result["steps"]["items"][0]["raw"], raw)
                self.assertEqual(result["steps"]["items"][0]["ordered_conditions"], skill["ordered_constraints"]["0"])

    def run_one_scenario(self, fake):
        scenario = {"task_type": "deploy", "task": "Deploy our fixture", "skill": self.skill,
                    "keyword_stems": ["dashboard_cdn", "smoke-pages"]}
        server = MagicMock()
        server.create_skill.return_value = "fixture-skill-id"
        server.session_formatted.side_effect = traced_session("[SKILL: fixture skill]")
        with patch.object(h7, "SCENARIOS", [scenario]), patch.object(h7, "judge_client", fake), patch.object(
            h7, "generate_plan", side_effect=traced_plan(self.plan)
        ), contextlib.redirect_stdout(io.StringIO()):
            return h7.run_h7a(server, 1)

    def test_both_arm_plans_and_per_item_judges_saved_with_valid_unknown_counts(self):
        first = quote(1, "Set DASHBOARD_CDN before building")
        verification = quote(3, "Check /healthz")
        fake = client_for(response(first), response(quote(None, None)), response(verification),
                          response(first), response("malformed"), response(verification))
        result = self.run_one_scenario(fake)
        self.assertEqual(result["control"], 1.0)
        self.assertEqual(result["experiment"], 1.0)  # primary keywords unchanged
        self.assertIsNone(result["judge_control"])
        self.assertEqual(result["judge_experiment"], 0.0)
        self.assertEqual(result["judge_control_summary"]["unknown"], 1)
        self.assertEqual(result["judge_experiment_summary"]["valid"], 1)
        self.assertEqual(result["judge_control_items"]["unknown"], 1)
        detail = result["details"][0]
        self.assertEqual(detail["control_plan"], self.plan)
        self.assertEqual(detail["experiment_plan"], self.plan)
        for arm in ("control", "experiment"):
            self.assertEqual(len(detail[f"{arm}_judge"]["items"]), 2)
            self.assertIn("raw", detail[f"{arm}_judge"]["items"][0])
            self.assertIn("usage", detail[f"{arm}_judge"]["items"][0]["metadata"])
            self.assertTrue(detail[f"{arm}_verification_judge"]["avoided"])

    def test_unavailable_judge_rates_are_null_not_zero(self):
        result = self.run_one_scenario(client_for(*[RuntimeError("offline unavailable")] * 6))
        for arm in ("control", "experiment"):
            self.assertIsNone(result[f"judge_{arm}"])
            self.assertEqual(result[f"judge_{arm}_summary"]["valid"], 0)
            self.assertEqual(result[f"judge_{arm}_summary"]["unknown"], 1)


class H6Tests(OfflineCase):
    def test_time_typography_normalization_and_evidence(self):
        for text in ("Sunday 03:00–05:00 UTC", "Sunday 03:00−05:00 UTC",
                     "Sunday ０３：００－０５：００ UTC", "Sunday 03∶00—05꞉00 UTC",
                     "Sunday 03 : 00 - 05 : 00 UTC"):
            with self.subTest(text=text):
                evidence = h6.stem_evidence(["03:00-05:00"], text)
                self.assertTrue(evidence["hit"])
                self.assertEqual(evidence["raw_text"], text)
                self.assertIn("03:00-05:00", evidence["matches"][0]["normalized_text"])
                self.assertEqual(evidence["matches"][0]["normalized_match"], "03:00-05:00")
                self.assertFalse(evidence["raw_hit"])

    def test_wrong_time_windows_stay_negative_and_non_time_stems_are_not_normalized(self):
        for text in ("03:00–06:00", "02:00–04:00", "03:01–05:00",
                     "103:00-05:00", "03:00-05:001", "03:00-05:00:59"):
            with self.subTest(text=text):
                self.assertFalse(h6.stem_hit(["03:00-05:00"], text))
        self.assertFalse(h6.stem_hit(["cdn-v2.memvault.io"], "cdn–v2.memvault.io"))
        self.assertFalse(h6.stem_hit(["legacy_4001"], "ＬＥＧＡＣＹ＿４００１"))
        self.assertTrue(h6.stem_hit(["legacy_4001"], "LEGACY_4001"))

    def test_h6c_primary_new_conveyance_allows_old_window_in_replacement_explanation(self):
        scenario = h6.SCENARIOS[1]
        for injection, expected in (("Saturday ０２：００–０４：００ UTC", True),
                                    ("New Saturday 02:00–04:00 UTC replaces old Sunday 03:00—05:00 UTC", True),
                                    ("Old Sunday 03:00—05:00 UTC", False),
                                    ("Saturday 02:00–05:00 UTC", False)):
            with self.subTest(injection=injection):
                server = MagicMock()
                server.create_fact.side_effect = ["old-id", "new-id"]
                server.session_formatted.side_effect = [scenario["fact"], injection]
                with patch.object(h6, "SCENARIOS", [scenario]), patch.object(
                    h6, "answer", side_effect=["03:00–06:00", "03:00–05:00", "０３：００–０５：００"]
                ), contextlib.redirect_stdout(io.StringIO()):
                    result = h6.run_h6(server, 1)
                self.assertEqual(result["control_rate"], 0.0)
                self.assertEqual(result["experiment_rate"], 1.0)
                self.assertEqual(result["consistency_rate"], 1.0)
                self.assertEqual(result["correction_rate"], float(expected))
                correction = result["corrections"][0]
                self.assertIs(correction["ok"], expected)
                self.assertIs(correction["has_new"], expected)
                if "replaces old" in injection:
                    self.assertTrue(correction["has_old"])
                    self.assertFalse(correction["new_only"])
                self.assertEqual(correction["new_evidence"]["raw_text"], injection)
                self.assertIn("matches", correction["old_evidence"])
                self.assertEqual(result["details"][0]["session1"]["raw_text"], "03:00–05:00")
                server.supersede.assert_called_once_with("old-id", "new-id")


if __name__ == "__main__":
    unittest.main()
