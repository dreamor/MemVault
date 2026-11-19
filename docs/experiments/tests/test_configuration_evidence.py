"""Offline regressions for protected H5/H7 configuration and shared redaction."""
from __future__ import annotations

import contextlib
import importlib
import io
import json
import os
from pathlib import Path
import sys
import tempfile
from types import ModuleType, SimpleNamespace
from unittest.mock import MagicMock, patch

from test_benchmarks import OfflineCase, client_for
from test_hypothesis_scoring import response


@contextlib.contextmanager
def isolated_hypotheses(environment=None):
    """Load fresh modules, restoring every module/env entry when the test exits."""
    sdk = ModuleType("openai")
    sdk.OpenAI = MagicMock(side_effect=lambda **kwargs: client_for())
    env = {"VERIFY_BASE_URL": "http://mock.invalid", **(environment or {})}
    with patch.dict(sys.modules), patch.dict(os.environ, env, clear=True):
        for name in ("beam_adherence", "benchmark_evidence", "judge_grounding",
                     "verify_hypotheses", "verify_h7"):
            sys.modules.pop(name, None)
        sys.modules["openai"] = sdk
        h5 = importlib.import_module("verify_hypotheses")
        h7 = importlib.import_module("verify_h7")
        yield h5, h7, sys.modules["judge_grounding"], sdk


class ConfigurationEvidenceTests(OfflineCase):
    def invoke(self, module, name):
        root = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
        output = root / (name + ".json")
        argv = (["--hypothesis", "H5", "--samples", "1"] if name == "H5" else
                ["--rounds", "1", "--misfire-samples", "1", "--binary", str(root / "binary")])
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            code = module.main([*argv, "--out", str(output)])
        return code, json.loads(output.read_text())

    def test_import_does_not_construct_clients_or_import_beam_options(self):
        with isolated_hypotheses({"HTTPS_PROXY": "bad-proxy-scheme://invalid",
                                  "VERIFY_EXTRA_BODY": "{not-json"}) as (h5, h7, grounding, sdk):
            sdk.OpenAI.assert_not_called()
            self.assertNotIn("beam_adherence", sys.modules)
            self.assertIs(grounding.beam, h7.beam)
            self.assertEqual(h7.beam.REPO_ROOT, Path(h7.__file__).resolve().parents[2])
            for module in (h5, h7):
                self.assertIsNone(module.client)
                self.assertIsNone(module.judge_client)

    def test_proxy_constructor_failure_saves_invalid_configuration_artifact(self):
        for name in ("H5", "H7"):
            with self.subTest(name=name), isolated_hypotheses(
                {"HTTPS_PROXY": "bad-proxy-scheme://invalid"}
            ) as (h5, h7, grounding, sdk):
                sdk.OpenAI.side_effect = ValueError("Unknown scheme for proxy URL: bad-proxy-scheme")
                module = h5 if name == "H5" else h7
                with patch.object(h7, "MemVaultServer") as server:
                    code, artifact = self.invoke(module, name)
                self.assertEqual(code, 1)
                self.assertEqual(artifact["run_status"]["status"], "invalid")
                self.assertEqual(artifact["run_error"]["phase"], "configuration")
                self.assertEqual(artifact["run_error"]["type"], "ValueError")
                self.assertIn("proxy", artifact["run_error"]["message"])
                self.assertEqual(artifact["harness_spec"]["models"]["judge_max_tokens"], 2048)
                self.assertEqual(artifact["harness_spec"]["models"]["judge_retry_max_tokens"], 4096)
                self.assertEqual(sdk.OpenAI.call_count, 1)
                server.assert_not_called()
                self.assertIsNone(module.client)
                dependencies = artifact["harness_spec"]["dependency_sha256"]
                self.assertEqual(dependencies["benchmark_evidence.py"],
                                 grounding.beam.content_hash(grounding.beam.__file__))
                self.assertNotIn("beam_adherence.py", dependencies)

    def test_invalid_scheme_proxy_userinfo_is_redacted_from_failure_artifact(self):
        proxy = "invalid://user:private@proxy"
        for name in ("H5", "H7"):
            with self.subTest(name=name), isolated_hypotheses(
                {"HTTPS_PROXY": proxy}
            ) as (h5, h7, grounding, sdk):
                sdk.OpenAI.side_effect = ValueError(f"Unknown scheme for proxy URL: {proxy}")
                code, artifact = self.invoke(h5 if name == "H5" else h7, name)
                self.assertEqual(code, 1)
                self.assertEqual(artifact["run_error"]["phase"], "configuration")
                encoded = json.dumps(artifact)
                self.assertNotIn("user:private@", encoded)
                self.assertIn("invalid://[REDACTED]@proxy", artifact["run_error"]["message"])
                self.assertEqual(os.environ["HTTPS_PROXY"], proxy)
                sdk.OpenAI.assert_called_once()

    def test_separate_judge_constructor_failure_is_also_protected(self):
        for name in ("H5", "H7"):
            with self.subTest(name=name), isolated_hypotheses(
                {"VERIFY_JUDGE_BASE_URL": "http://judge.invalid"}
            ) as (h5, h7, grounding, sdk):
                answer = client_for()
                sdk.OpenAI.side_effect = [answer, ValueError("judge proxy construction failed")]
                code, artifact = self.invoke(h5 if name == "H5" else h7, name)
                self.assertEqual(code, 1)
                self.assertEqual(artifact["run_error"]["phase"], "configuration")
                self.assertEqual(sdk.OpenAI.call_count, 2)
                answer.chat.completions.create.assert_not_called()

    def test_invalid_active_budgets_import_then_fail_without_fabricated_values(self):
        for name in ("H5", "H7"):
            for option, value in (("VERIFY_JUDGE_MAX_TOKENS", "invalid"),
                                  ("VERIFY_JUDGE_MAX_TOKENS", "0"),
                                  ("VERIFY_JUDGE_RETRY_MAX_TOKENS", "invalid")):
                with self.subTest(name=name, option=option, value=value), isolated_hypotheses(
                    {option: value}
                ) as (h5, h7, grounding, sdk):
                    sdk.OpenAI.assert_not_called()
                    code, artifact = self.invoke(h5 if name == "H5" else h7, name)
                    self.assertEqual(code, 1)
                    self.assertEqual(artifact["run_status"]["status"], "invalid")
                    self.assertEqual(artifact["run_error"]["phase"], "configuration")
                    self.assertIn(option, artifact["run_error"]["message"])
                    field = "judge_max_tokens" if option == "VERIFY_JUDGE_MAX_TOKENS" else "judge_retry_max_tokens"
                    self.assertEqual(artifact["harness_spec"]["models"][field], value)
                    sdk.OpenAI.assert_not_called()

    def test_malformed_extra_body_blocks_protected_h5_h7_configuration(self):
        with isolated_hypotheses({"VERIFY_EXTRA_BODY": "{not-json"}) as (h5, h7, grounding, sdk):
            for module, name in ((h5, "H5"), (h7, "H7")):
                with self.subTest(name=name):
                    code, artifact = self.invoke(module, name)
                    self.assertEqual(code, 1)
                    self.assertEqual(artifact["run_status"]["status"], "invalid")
                    self.assertEqual(artifact["run_error"]["phase"], "configuration")
                    self.assertIn("VERIFY_EXTRA_BODY", artifact["run_error"]["message"])
                    self.assertEqual(artifact["harness_spec"]["models"]["extra_body"], "{not-json")
            sdk.OpenAI.assert_not_called()
            self.assertNotIn("beam_adherence", sys.modules)

    def test_direct_generators_keep_parameters_and_one_sdk_request(self):
        for name in ("H5", "H7"):
            with self.subTest(name=name), isolated_hypotheses() as (h5, h7, grounding, sdk):
                module = h5 if name == "H5" else h7
                fake = client_for(response("1. Fixture plan."), response("1. Fixture plan."))
                sdk.OpenAI.return_value = fake
                sdk.OpenAI.side_effect = None
                generate = module.generate_response if name == "H5" else module.generate_plan
                self.assertEqual(generate("fixture system", "fixture task"), "1. Fixture plan.")
                self.assertEqual(fake.chat.completions.create.call_count, 1)
                request = fake.chat.completions.create.call_args.kwargs
                self.assertEqual(request, {
                    "model": module.MODEL,
                    "messages": [{"role": "system", "content": "fixture system"},
                                 {"role": "user", "content": "fixture task" +
                                  ("" if name == "H5" else "\nReply with your step-by-step plan only.")}],
                    "max_tokens": 800 if name == "H5" else 500, "temperature": 0.3,
                })
                generate("fixture system", "fixture task")
                sdk.OpenAI.assert_called_once_with(base_url=module.BASE_URL, api_key="local")
                self.assertEqual(fake.chat.completions.create.call_count, 2)

    def test_h7_answer_budget_is_configurable_and_recorded(self):
        with isolated_hypotheses({"VERIFY_H7_MAX_TOKENS": "1500"}) as (h5, h7, grounding, sdk):
            self.assertEqual(h7.ANSWER_MAX_TOKENS, "1500")
            self.assertEqual(h7.configure_answer_budget(), 1500)
            fake = client_for(response("1. Fixture plan."))
            sdk.OpenAI.return_value, sdk.OpenAI.side_effect = fake, None
            self.assertEqual(h7.generate_plan("system", "task"), "1. Fixture plan.")
            self.assertEqual(fake.chat.completions.create.call_args.kwargs["max_tokens"], 1500)
            spec = h7.h7_harness_spec(
                SimpleNamespace(rounds=1, misfire_samples=1, invocation_argv=[]),
                None, [], [])
            self.assertEqual(spec["models"]["answer_max_tokens"], 1500)

    def test_invalid_h7_answer_budget_is_protected_configuration_error(self):
        for value in ("invalid", "0", "-1"):
            with self.subTest(value=value), isolated_hypotheses({"VERIFY_H7_MAX_TOKENS": value}) as (
                h5, h7, grounding, sdk
            ):
                code, artifact = self.invoke(h7, "H7")
                self.assertEqual(code, 1)
                self.assertEqual(artifact["run_status"]["status"], "invalid")
                self.assertEqual(artifact["run_error"]["phase"], "configuration")
                self.assertIn("VERIFY_H7_MAX_TOKENS", artifact["run_error"]["message"])
                self.assertEqual(artifact["harness_spec"]["models"]["answer_max_tokens"], value)
                sdk.OpenAI.assert_not_called()

    def test_extra_body_is_forwarded_to_h5_h7_generators_and_judges(self):
        body = {"enable_thinking": False}
        with isolated_hypotheses({"VERIFY_EXTRA_BODY": json.dumps(body)}) as (h5, h7, grounding, sdk):
            for module, name in ((h5, "H5"), (h7, "H7")):
                with self.subTest(name=name):
                    fake = client_for(response("1. Fixture plan."),
                                      response('{"step":null,"quote":null}'))
                    sdk.OpenAI.return_value, sdk.OpenAI.side_effect = fake, None
                    generate = module.generate_response if name == "H5" else module.generate_plan
                    judge = module.judge_pitfall_avoided if name == "H5" else module.judge_quote
                    self.assertEqual(generate("fixture system", "fixture task"), "1. Fixture plan.")
                    self.assertIs(judge("fixture precaution", "1. Fixture plan.")["avoided"], False)
                    requests = [call.kwargs for call in fake.chat.completions.create.call_args_list]
                    self.assertEqual([request["extra_body"] for request in requests], [body, body])
                    h5_spec = h5.h5_harness_spec(
                        SimpleNamespace(samples=1, invocation_argv=[]), h5.H5_SCENARIOS[:1])
                    h7_spec = h7.h7_harness_spec(
                        SimpleNamespace(rounds=1, misfire_samples=1, invocation_argv=[]),
                        None, [], [])
                    self.assertEqual(h5_spec["models"]["extra_body"], body)
                    self.assertEqual(h7_spec["models"]["extra_body"], body)

    def test_legacy_compliance_and_relevance_judges_ensure_client_on_demand(self):
        with isolated_hypotheses() as (h5, h7, grounding, sdk):
            fake = client_for(response('[{"status":"followed"}]'), response("yes"))
            sdk.OpenAI.return_value, sdk.OpenAI.side_effect = fake, None
            self.assertEqual(h5.judge_compliance(["fixture rule"], "fixture answer"),
                             [{"status": "followed"}])
            self.assertEqual(fake.chat.completions.create.call_count, 1)
            self.assertIs(h5.judge_relevance({"content": "fixture memory"}, "fixture context"), True)
            self.assertEqual(fake.chat.completions.create.call_count, 2)
            sdk.OpenAI.assert_called_once_with(base_url=h5.BASE_URL, api_key="local")
            requests = [call.kwargs for call in fake.chat.completions.create.call_args_list]
            self.assertEqual([request["model"] for request in requests], [h5.MODEL, h5.MODEL])
            self.assertEqual([request["max_tokens"] for request in requests], [500, 5])
            self.assertEqual([request["temperature"] for request in requests], [0.0, 0.0])
            self.assertTrue(all(len(request["messages"]) == 1 and
                                request["messages"][0]["role"] == "user" for request in requests))

    def test_direct_judges_share_or_split_clients_and_keep_protocol(self):
        for name in ("H5", "H7"):
            for split in (False, True):
                env = {"VERIFY_JUDGE_MODEL": "fixture-other-judge"} if split else {}
                with self.subTest(name=name, split=split), isolated_hypotheses(env) as (h5, h7, grounding, sdk):
                    module = h5 if name == "H5" else h7
                    fake = client_for(response('{"step":null,"quote":null}'))
                    sdk.OpenAI.return_value, sdk.OpenAI.side_effect = fake, None
                    judge = module.judge_pitfall_avoided if name == "H5" else module.judge_quote
                    result = judge("fixture precaution", "1. Fixture plan.")
                    self.assertIs(result["avoided"], False)
                    sdk.OpenAI.assert_called_once_with(base_url=module.JUDGE_BASE_URL, api_key="local")
                    self.assertIs(module.judge_client, fake)
                    self.assertIs(module.client, None if split else fake)
                    fake.chat.completions.create.assert_called_once()
                    request = fake.chat.completions.create.call_args.kwargs
                    self.assertEqual(set(request), {"model", "messages", "max_tokens", "temperature"})
                    self.assertEqual(request["model"], module.JUDGE_MODEL)
                    self.assertEqual(request["max_tokens"], 2048)
                    self.assertEqual(request["temperature"], 0.0)
                    self.assertEqual(len(request["messages"]), 1)

    def test_patched_clients_are_not_replaced_and_explicit_budgets_survive(self):
        with isolated_hypotheses({"VERIFY_JUDGE_MAX_TOKENS": "3072",
                                  "VERIFY_JUDGE_RETRY_MAX_TOKENS": "6144"}) as (h5, h7, grounding, sdk):
            for module, name in ((h5, "H5"), (h7, "H7")):
                answer = client_for(response("1. Fixture plan."))
                judge = client_for(response('{"step":null,"quote":null}', finish="length"),
                                   response('{"step":null,"quote":null}'))
                with patch.object(module, "client", answer), patch.object(module, "judge_client", judge):
                    generate = module.generate_response if name == "H5" else module.generate_plan
                    grade = module.judge_pitfall_avoided if name == "H5" else module.judge_quote
                    generate("system", "task")
                    result = grade("precaution", "1. Fixture plan.")
                    self.assertIs(result["avoided"], False)
                    requests = [call.kwargs for call in judge.chat.completions.create.call_args_list]
                    self.assertEqual([request["max_tokens"] for request in requests], [3072, 6144])
                    self.assertEqual(requests[0]["messages"], requests[1]["messages"])
                    self.assertEqual(requests[0]["temperature"], requests[1]["temperature"])
                    answer.chat.completions.create.assert_called_once()
            sdk.OpenAI.assert_not_called()

    def test_grounding_common_redaction_runs_once_for_nested_payload(self):
        with isolated_hypotheses() as (h5, h7, grounding, sdk):
            common = grounding.beam
            grounding.register_secret("registered-fixture-value")
            value = {"earlier_echo": "later-discovered-credential",
                     "nested": [{"echo": "registered-fixture-value", "max_tokens": 2048},
                                ("Bearer private-auth-value", {"password": "private-password"}),
                                {"api_key": "later-discovered-credential"}],
                     "url": "https://user:private-url-value@mock.invalid/v1?token=private-query-value"}
            with patch.object(common, "sanitize", wraps=common.sanitize) as redact:
                clean = grounding.sanitize(value)
                redact.assert_called_once_with(value)
            self.assertEqual(clean, {
                "earlier_echo": "[REDACTED]",
                "nested": [{"echo": "[REDACTED]", "max_tokens": 2048},
                           ["Bearer [REDACTED]", {"password": "[REDACTED]"}],
                           {"api_key": "[REDACTED]"}],
                "url": "https://[REDACTED]@mock.invalid/v1?token=[REDACTED]",
            })
            self.assertEqual(grounding.sanitize(clean), clean)
            self.assertEqual(grounding.sanitize("registered-fixture-value"), "[REDACTED]")
