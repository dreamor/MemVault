"""All tests are stdlib-only, with network/process/model calls and sleeps mocked."""
from __future__ import annotations

import contextlib
import copy
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import MagicMock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import beam_adherence as beam
import memory_agent_bench as mab
import offline_replay

FIXTURE = Path(__file__).parent / "fixtures/tiny_rows.json"


def completion(text):
    return SimpleNamespace(choices=[SimpleNamespace(message=SimpleNamespace(content=text))],
                           usage=SimpleNamespace(prompt_tokens=10, completion_tokens=3))


def client_for(*texts):
    create = MagicMock(side_effect=[completion(t) if isinstance(t, str) else t for t in texts])
    return SimpleNamespace(chat=SimpleNamespace(completions=SimpleNamespace(create=create)))


def grade(compliant=True, items=None, negative=False):
    items = [True, True] if items is None else items
    return {"parsed": True, "compliant": compliant, "rubric_met": items,
            "rubric_coverage": sum(items) / len(items) if items else None,
            "non_compliance_sign_present": negative, "reason": "fixture grade"}


def record(probe, c=True, e=True):
    return {"n": 1, "probe_id": probe["probe_id"], "probe": copy.deepcopy(probe),
            "category": probe["category"], "subtype": probe["subtype"],
            "rubric_items": len(probe["rubric"]), "seeded": True,
            "rule_in_injection": True, "instruction_layer_used": True,
            "control_response": "control text", "experiment_response": "experiment text",
            "control_judge": grade(c, [c] * len(probe["rubric"])),
            "experiment_judge": grade(e, [e] * len(probe["rubric"]))}


class FakeHTTP:
    def __init__(self, body=b"ok", status=200):
        self.body, self.status = body, status
        self.done = False

    def __enter__(self):
        return self

    def __exit__(self, *args):
        pass

    def read(self, size=-1):
        if self.done:
            return b""
        self.done = True
        return self.body


class FakeBeamServer:
    def __init__(self, *args):
        self.records, self.base = [], "http://mock.invalid"

    def seed_rule(self, probe):
        self.rule = probe["rule"]
        self.records.append({"request": {"content": self.rule}, "response": {"id": "memory-1"}})
        return "memory-1"

    def inject(self, question):
        self.records.append({"request": {"context_hint": question}, "response": {"formatted": self.rule}})
        return self.rule, 0.001, "MustRef"

    def clear(self, memory_id):
        self.records.append({"request": {"delete": memory_id}, "response": {"ok": True}})
        return True

    def stop(self):
        pass


class FakeMabServer:
    def __init__(self, binary, mode="keyword", provider="off", environment=None, session_agent="codex", session_project=None):
        self.records, self.mode = [], mode
        self.session_agent, self.session_project = session_agent, session_project
        self.facts = []

    def seed_fact(self, content, namespace):
        self.facts.append(content)
        memory_id = "memory-" + str(len(self.facts))
        self.records.append({"request": {"content": content, "force_insert": True},
                             "response": {"data": {"id": memory_id}}})
        return memory_id

    def search(self, query, top_k, namespace):
        results = [{"memory": {"content": self.facts[i], "id": "memory-" + str(i + 1),
                               "identity_verified": False}, "score": 0.5, "search_mode": "keyword"}
                   for i in (1, 2)]
        self.records.append({"request": {"mode": self.mode}, "response": {"data": results}})
        return results, 0.001

    def session(self, question):
        self.records.append({"request": {"agent_id": self.session_agent, "project": self.session_project},
                             "response": {"data": {"formatted": "", "agent_profile": "default", "skipped": []}}})
        return "", 0.001

    def stop(self):
        pass


def managed_row():
    return {"context": "1. goaltender is associated with the sport of ice hockey.\n"
                       "9. goaltender is associated with the sport of pesäpallo.\n"
                       "3. The author of Sidereus Nuncius is Galileo Galilei.\n"
                       "15. The author of Sidereus Nuncius is Samuel Beckett.\n"
                       "5. An unsupported event happened in 1901.",
            "questions": ["Which sport is goaltender associated with?"],
            "answers": [["pesäpallo"]], "metadata": {"qa_pair_ids": ["tiny_no1"]}}


class FakeManagedHTTPStore:
    """Offline protocol double, not an integration/core resolver verdict."""
    def __init__(self, verified=True, ignore_resolve=False, ignore_assertion=False):
        self.verified, self.ignore_resolve, self.ignore_assertion = verified, ignore_resolve, ignore_assertion
        self.memories, self.calls, self.startups = [], [], []
        self.process = MagicMock()

    def popen(self, args, **kwargs):
        self.startups.append((args, kwargs))
        db = Path(args[args.index("--db") + 1])
        self.registry = db.parent / "agents.yaml"
        self.profile = json.loads(self.registry.read_text())["agents"][0]
        return self.process

    def response(self, value, request, status=200):
        payload = json.dumps(value, ensure_ascii=False).encode()
        if status >= 400:
            raise mab.urllib.error.HTTPError(request.full_url, status, "fixture refusal", {}, io.BytesIO(payload))
        return FakeHTTP(payload, status)

    def urlopen(self, request, **kwargs):
        path = mab.urllib.parse.urlparse(request.full_url).path
        headers = {k.lower(): v for k, v in request.header_items()}
        self.calls.append({"path": path, "headers": headers,
                           "body": json.loads(request.data) if request.data else None})
        if path == "/health":
            return FakeHTTP()
        if headers.get("x-memvault-api-key") != self.profile["api_key"]:
            return self.response({"ok": False, "error": "unauthorized"}, request, 401)
        if path == "/api/memories" and request.get_method() == "GET":
            query = mab.urllib.parse.parse_qs(mab.urllib.parse.urlparse(request.full_url).query)
            namespace = query["namespace"][0]
            offset, limit = int(query["offset"][0]), int(query["limit"][0])
            rows = [m for m in self.memories if m["namespace"] == namespace]
            return self.response({"ok": True, "data": rows[offset:offset + limit]}, request)
        if path == "/api/search":
            rows = [m for m in self.memories if not m["superseded_by"]]
            return self.response({"ok": True, "data": [{"memory": m, "search_mode": "keyword"} for m in rows]}, request)
        if path == "/api/session/preview":
            body = json.loads(request.data)
            return self.response({"ok": True, "data": {
                "formatted": "", "agent_profile": body["agent_id"], "mode": "preview",
                "count": 0, "format": "MustRef", "results": [], "conflicts": [],
                "skipped": [], "degraded": [],
            }}, request)
        body = json.loads(request.data)
        assertion = None if self.ignore_assertion else body.get("fact_assertion")
        resolve = body.get("fact_write") == "resolve" and not body.get("force_insert") and not self.ignore_resolve
        current = next((m for m in self.memories if m["fact_management"] and not m["superseded_by"] and
                        m["fact_assertion"]["key"] == assertion["key"]), None) if resolve else None
        outcome, old_id = "inserted", None
        if current:
            order = assertion["source_order"] - current["fact_assertion"]["source_order"]
            if order < 0 or order == 0:
                outcome = "stale" if order < 0 else "idempotent"
                refused = order == 0 and assertion["value"] != current["fact_assertion"]["value"]
                if refused:
                    outcome = "conflict"
                return self.response({"ok": not refused, "data": {"id": current["id"], "action": "skipped",
                    "fact_outcome": outcome, "written": False, "memory": copy.deepcopy(current),
                    "reason": "fixture protected source order"}}, request, 409 if refused else 200)
            outcome, old_id = "superseded", current["id"]
        memory_id = "ephemeral-uuid-" + str(len(self.memories) + 1)
        memory = {"id": memory_id, "content": body["content"], "namespace": body["namespace"],
                  "identity_verified": self.verified, "source_agent": self.profile["id"],
                  "human_reviewed": False, "ai_generated": True, "superseded_by": None,
                  "archived_at": None, "fact_assertion": assertion, "fact_management": None}
        if resolve:
            memory["fact_management"] = {"state": "active", "record_id": memory_id,
                "producer_id": self.profile["id"], "namespace": body["namespace"],
                "source_namespace": assertion["source_namespace"], "key": assertion["key"],
                "source_order": assertion["source_order"]}
        if current:
            current["superseded_by"] = memory_id
        self.memories.append(memory)
        data = {"id": memory_id, "action": "saved"}
        if resolve:
            data.update({"fact_outcome": outcome, "written": True, "superseded_id": old_id,
                         "memory": copy.deepcopy(memory)})
        return self.response({"ok": True, "data": data}, request)


class OfflineCase(unittest.TestCase):
    def setUp(self):
        self.stack = contextlib.ExitStack()
        self.addCleanup(self.stack.close)
        self.network = self.stack.enter_context(patch("urllib.request.urlopen", side_effect=AssertionError("unexpected network")))
        self.popen = self.stack.enter_context(patch("subprocess.Popen", side_effect=AssertionError("unexpected subprocess")))
        self.stack.enter_context(patch("subprocess.run", return_value=SimpleNamespace(returncode=0, stdout="")))
        self.stack.enter_context(patch("time.sleep"))
        self.stack.enter_context(patch.dict("os.environ", {"VERIFY_BASE_URL": "http://mock.invalid"}, clear=True))
        self.rows = json.loads(FIXTURE.read_text())
        raw = beam.parse_probes(self.rows["beam"])["instruction_following"][0]
        self.probe = beam.normalise_probe("instruction_following", raw, beam.flatten_chat(self.rows["beam"]))


class BeamTests(OfflineCase):
    def run_main(self, client=None, raw_probes=None, limit=8, dry=False, rows=None,
                 server=FakeBeamServer, extra=()):
        directory = self.stack.enter_context(tempfile.TemporaryDirectory())
        source, binary, output = (Path(directory) / name for name in ("rows.json", "binary", "result.json"))
        row = copy.deepcopy(self.rows["beam"])
        if raw_probes is not None:
            row["probing_questions"] = repr({"instruction_following": raw_probes})
        source.write_text(json.dumps(rows if rows is not None else row))
        binary.write_bytes(b"fixture binary start")
        argv = ["--from-file", str(source), "--binary", str(binary), "--out", str(output), "--probes", str(limit), *extra]
        if dry:
            argv.append("--dry-run")
        stdout = io.StringIO()
        with patch.object(beam, "make_client", return_value=client) as make_client, patch.object(beam, "MemVaultServer", server), patch.object(beam, "BASE_URL", "http://mock.invalid"), contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(io.StringIO()):
            code = beam.main(argv)
            if dry:
                make_client.assert_not_called()
        return code, json.loads(output.read_text()), stdout.getvalue(), source, binary

    def valid_judge_text(self):
        return json.dumps({"rubric_met": [True, True], "compliant": True,
                           "non_compliance_sign_present": False, "reason": "fixture valid"})

    def test_main_401_saves_failed_judge_artifact_and_exits_nonzero(self):
        client = client_for("control", "experiment", RuntimeError("401 fixture unauthorized"), RuntimeError("401 fixture unauthorized"))
        code, output, text, _, _ = self.run_main(client, [self.probe["raw_annotation"]])
        self.assertEqual(code, 1)
        self.assertIsNone(output["run_error"])
        self.assertEqual(output["run_status"]["status"], "invalid")
        self.assertEqual(output["validity"]["paired_labels"]["n"], 0)
        self.assertIn("401", output["probes"][0]["control_judge"]["records"][0]["error"])
        self.assertNotIn("SCORING COMPLETE", text)

    def test_main_exhausted_judge_retries_are_failure_not_score(self):
        client = client_for("control", "experiment", *[RuntimeError("429 fixture rate limit") for _ in range(7)], self.valid_judge_text())
        code, output, _, _, _ = self.run_main(client, [self.probe["raw_annotation"]])
        self.assertEqual(code, 1)
        self.assertEqual(len(output["probes"][0]["control_judge"]["records"]), 7)
        self.assertEqual(output["run_status"]["n_valid_scoring_pairs"], 0)

    def test_main_malformed_vectors_and_boolean_labels_fail(self):
        for changes in ({"rubric_met": [True]}, {"rubric_met": [True, "false"]}, {"compliant": "false"}):
            with self.subTest(changes=changes):
                verdict = json.loads(self.valid_judge_text())
                verdict.update(changes)
                client = client_for("control", "experiment", json.dumps(verdict), self.valid_judge_text())
                code, output, text, _, _ = self.run_main(client, [self.probe["raw_annotation"]])
                self.assertEqual(code, 1)
                self.assertEqual(output["run_status"]["status"], "invalid")
                self.assertNotIn("SCORING COMPLETE", text)

    def test_main_partial_invalid_pair_exits_nonzero(self):
        good = self.valid_judge_text()
        client = client_for("control", "experiment", good, good,
                            "control", "experiment", "malformed fixture", good)
        code, output, text, _, _ = self.run_main(client, [self.probe["raw_annotation"]] * 2)
        self.assertEqual(code, 1)
        self.assertEqual(output["run_status"]["n_valid_scoring_pairs"], 1)
        self.assertNotIn("SCORING COMPLETE", text)

    def test_main_zero_valid_pairs_without_judge_attempt_is_failure(self):
        client = client_for()
        code, output, text, _, _ = self.run_main(client, limit=0)
        self.assertEqual(code, 1)
        self.assertIn("zero_valid_scoring_pairs", output["run_status"]["errors"])
        client.chat.completions.create.assert_not_called()
        self.assertNotIn("SCORING COMPLETE", text)

    def test_main_valid_scored_and_intentional_dry_runs_exit_zero(self):
        good = self.valid_judge_text()
        code, output, text, _, _ = self.run_main(client_for("control", "experiment", good, good), [self.probe["raw_annotation"]])
        self.assertEqual(code, 0)
        self.assertEqual(output["run_status"]["status"], "scoring_complete")
        self.assertIn("SCORING COMPLETE", text)
        code, output, _, _, _ = self.run_main(dry=True)
        self.assertEqual(code, 0)
        self.assertEqual(output["run_status"]["status"], "dry_run_complete")
        code, output, _, _, _ = self.run_main(dry=True, extra=("--with-server",))
        self.assertEqual(code, 0)
        self.assertEqual(output["run_status"]["status"], "dry_run_complete")
        self.assertTrue(output["probes"][0]["seeded"])

    def test_main_invalid_requested_stability_regrade_exits_nonzero(self):
        good = self.valid_judge_text()
        client = client_for("control", "experiment", good, good, good, RuntimeError("401 fixture regrade denied"))
        code, output, text, _, _ = self.run_main(client, [self.probe["raw_annotation"]], extra=("--judge-stability", "1"))
        self.assertEqual(code, 1)
        self.assertEqual(output["run_status"]["n_valid_scoring_pairs"], 1)
        self.assertIn("invalid_stability_grades", output["run_status"]["errors"])
        self.assertEqual(output["judge_stability"]["n_valid_arm_pairs"], 1)
        self.assertNotIn("SCORING COMPLETE", text)

    def test_selected_invalid_annotations_retain_slots_and_locators(self):
        raw = self.probe["raw_annotation"]
        for invalid in (dict(raw, instruction_being_tested=""), dict(raw, question=42)):
            with self.subTest(invalid=invalid):
                good = self.valid_judge_text()
                code, output, text, _, _ = self.run_main(client_for("control", "experiment", good, good), [raw, invalid], limit=2)
                self.assertEqual(code, 1)
                intake = output["harness_spec"]["intake"]
                self.assertEqual(intake["n_selected_slots"], 2)
                self.assertEqual(intake["n_selected_valid_slots"], 1)
                self.assertEqual(intake["n_selected_invalid_slots"], 1)
                bad = output["probes"][1]
                self.assertEqual(bad["raw_annotation"], invalid)
                self.assertEqual(bad["source_locator"]["probe_index"], 1)
                self.assertIn("execution_skipped", bad)
                self.assertNotIn("SCORING COMPLETE", text)

    def test_probe_limit_never_backfills_invalid_first_slot(self):
        raw = self.probe["raw_annotation"]
        client = client_for()
        code, output, _, _, _ = self.run_main(client, [dict(raw, question=42), raw], limit=1)
        self.assertEqual(code, 1)
        self.assertEqual(len(output["probes"]), 1)
        self.assertEqual(output["probes"][0]["source_locator"]["probe_index"], 0)
        self.assertEqual(output["harness_spec"]["intake"]["n_selected_invalid_slots"], 1)
        client.chat.completions.create.assert_not_called()

    def test_invalid_annotation_outside_selected_limit_not_in_denominator(self):
        raw, good = self.probe["raw_annotation"], self.valid_judge_text()
        code, output, _, _, _ = self.run_main(client_for("control", "experiment", good, good), [raw, dict(raw, question=42)], limit=1)
        self.assertEqual(code, 0)
        self.assertFalse(output["harness_spec"]["annotation_load_errors"][0]["selected"])
        self.assertEqual(output["harness_spec"]["intake"]["n_selected_slots"], 1)

    def test_unparseable_row_slot_count_unknown_artifact_retained(self):
        row = dict(self.rows["beam"], probing_questions="unparseable fixture")
        code, output, text, _, _ = self.run_main(dry=True, rows=[row])
        self.assertEqual(code, 1)
        intake = output["harness_spec"]["intake"]
        self.assertIsNone(intake["n_selected_slots"])
        self.assertFalse(intake["selected_slot_count_complete"])
        self.assertEqual(intake["n_rows_with_unknown_slot_counts"], 1)
        self.assertIsNone(intake["unknown_rows"][0]["slot_count"])
        self.assertEqual(intake["unknown_rows"][0]["raw_annotation"], row["probing_questions"])
        self.assertNotIn("SCORING COMPLETE", text)

    def test_invalid_intake_is_not_a_successful_dry_run(self):
        code, output, _, _, _ = self.run_main(dry=True, raw_probes=[dict(self.probe["raw_annotation"], question=42)])
        self.assertEqual(code, 1)
        self.assertEqual(output["run_status"]["status"], "invalid")

    def test_main_execution_identity_retains_start_hashes_and_checkout(self):
        directory = self.stack.enter_context(tempfile.TemporaryDirectory())
        harness = Path(directory) / "beam_adherence.py"
        harness.write_bytes(b"fixture harness start")
        hashes = {"harness": beam.content_hash(harness)}
        class ChangingServer(FakeBeamServer):
            def __init__(server, binary, *args):
                super().__init__(binary, *args)
                hashes["binary"] = beam.content_hash(binary)
                hashes["dataset"] = beam.content_hash(binary.with_name("rows.json"))
                binary.write_bytes(b"fixture executed path changed afterwards")
                binary.with_name("rows.json").write_bytes(b"fixture dataset changed afterwards")
                harness.write_bytes(b"fixture harness end")
        with patch.object(beam, "__file__", str(harness)), patch.object(beam, "checkout_identity", return_value={"commit": "fixture-start", "version": "fixture-start", "branch": "fixture-start"}) as checkout:
            good = self.valid_judge_text()
            code, output, _, source, binary = self.run_main(client_for("control", "experiment", good, good), [self.probe["raw_annotation"]], server=ChangingServer)
        self.assertEqual(code, 0)
        checkout.assert_called_once()
        evidence = output["harness_spec"]["evidence"]
        self.assertEqual(evidence["capture_phase"], "before_execution")
        self.assertEqual(evidence["binary_path"], str(binary.resolve()))
        self.assertEqual(evidence["binary_sha256"], hashes["binary"])
        self.assertEqual(evidence["harness_sha256"], hashes["harness"])
        self.assertEqual(evidence["dataset"]["source_sha256"], hashes["dataset"])
        self.assertEqual(evidence["checkout"]["commit"], "fixture-start")
        self.assertNotEqual(evidence["binary_sha256"], beam.content_hash(binary))
        self.assertNotEqual(evidence["harness_sha256"], beam.content_hash(harness))
        self.assertNotEqual(evidence["dataset"]["source_sha256"], beam.content_hash(source))

    def test_complete_probe_and_missing_source_locator(self):
        self.assertEqual(self.probe["raw_annotation"]["annotation_extra"], "retain me")
        self.assertEqual(self.probe["rubric"], ["No code fence", "Explain the loop"])
        self.assertIsNone(self.probe["source_turn_locators"][1]["turn"])
        self.assertEqual(self.probe["time_anchor_origin"], "inherited:0")

    def test_stable_ids_and_distinct_locators(self):
        raw = self.probe["raw_annotation"]
        a = beam.normalise_probe("instruction_following", raw, {}, {"row_index": 1})
        b = beam.normalise_probe("instruction_following", copy.deepcopy(raw), {}, {"row_index": 1})
        c = beam.normalise_probe("instruction_following", raw, {}, {"row_index": 2})
        self.assertEqual(a["probe_id"], b["probe_id"])
        self.assertNotEqual(a["probe_id"], c["probe_id"])

    def test_string_false_rejected(self):
        g = grade()
        g["compliant"] = "false"
        self.assertFalse(beam.validate_grade(g, 2)["valid"])

    def test_non_boolean_rubric_rejected(self):
        for value in ("false", 0, 1, None):
            g = grade()
            g["rubric_met"] = [True, value]
            self.assertFalse(beam.validate_grade(g, 2)["valid"])

    def test_length_and_reason_types_rejected(self):
        g = grade(items=[True])
        self.assertIn("rubric_length:mismatch", beam.validate_grade(g, 2)["errors"])
        g = grade()
        g["reason"] = 4
        self.assertFalse(beam.validate_grade(g, 2)["valid"])

    def test_negative_boolean_strict(self):
        g = grade()
        g["non_compliance_sign_present"] = "false"
        self.assertFalse(beam.validate_grade(g, 2)["valid"])

    def test_aggregate_disagreement_not_rewritten(self):
        g = grade(True, [False, True])
        v = beam.validate_grade(g, 2)
        self.assertTrue(v["valid"])
        self.assertTrue(v["label_aggregate_disagreement"])
        self.assertTrue(g["compliant"])
        self.assertFalse(v["rubric_aggregate_compliant"])

    def test_empty_rubric_not_fabricated(self):
        self.assertIsNone(beam.validate_grade(grade(items=[]), 0)["rubric_aggregate_compliant"])
        self.assertFalse(beam.validate_grade(grade(), None)["valid"])

    def test_unparsed_judge(self):
        client = client_for("not JSON")
        result = beam.judge(client, self.probe, "answer")
        self.assertFalse(result["parsed"])
        self.assertIsNone(result["compliant"])
        self.assertFalse(result["validation"]["valid"])

    def test_judge_keeps_raw_and_full_messages(self):
        text = json.dumps({"rubric_met": [True, True], "compliant": True,
                           "non_compliance_sign_present": False, "reason": "okay"})
        client = client_for(text)
        result = beam.judge(client, self.probe, "answer")
        self.assertEqual(result["raw_verdict"], text)
        self.assertEqual(result["records"][0]["request"]["max_tokens"], beam.JUDGE_MAX_TOKENS)
        prompt = result["records"][0]["request"]["messages"][1]["content"]
        for value in ("Prose only.", "plain text", "code fence", "No code fence", "Explain the loop"):
            self.assertIn(value, prompt)
        self.assertTrue(result["validation"]["valid"])

    def test_judge_failure_keeps_trace(self):
        result = beam.judge(client_for(RuntimeError("denied")), self.probe, "answer")
        self.assertFalse(result["validation"]["valid"])
        self.assertEqual(result["records"][0]["error"], "denied")

    def test_probe_rubric_type_rejected_without_call(self):
        self.probe["rubric"] = "not a list"
        client = client_for()
        result = beam.judge(client, self.probe, "answer")
        self.assertFalse(result["validation"]["valid"])
        client.chat.completions.create.assert_not_called()

    def test_legacy_summary_both_arms_required(self):
        r = record(self.probe, False, True)
        self.assertEqual(beam.summarise([r])["experiment_adherence"], 1)
        r["control_judge"]["parsed"] = False
        self.assertEqual(beam.summarise([r])["n_judged"], 0)
        self.assertEqual(beam.validity_diagnostics([r])["n_invalid_pairs"], 1)

    def test_legacy_missing_rubric_is_explicit(self):
        r = record(self.probe)
        del r["probe"]
        validity = beam.validity_diagnostics([r])
        self.assertEqual(validity["n_rubric_source_missing"], 1)
        self.assertIsNone(validity["diagnostics"][0]["semantic_annotation_alignment"])
        self.assertEqual(validity["paired_labels"]["n"], 1)

    def test_regrades_keep_complete_probe_and_both_arms(self):
        r = record(self.probe)
        replies = [json.dumps({"rubric_met": [True, True], "compliant": True,
                               "non_compliance_sign_present": False, "reason": "same"})] * 2
        client = client_for(*replies)
        result = beam.regrade_pairs(client, [r], 1, 7)
        self.assertEqual(client.chat.completions.create.call_count, 2)
        self.assertEqual(result["n_valid_arm_pairs"], 2)
        for arm in ("control", "experiment"):
            item = result["items"][0]["arms"][arm]
            self.assertFalse(item["label_flip"])
            self.assertEqual(item["rubric_item_flips"], [False, False])
            prompt = item["reworded_grade"]["records"][0]["request"]["messages"][1]["content"]
            self.assertIn("Prose only.", prompt)
            self.assertIn("No code fence", prompt)

    def test_stratified_sampling_reproducible(self):
        records = [dict(record(self.probe), probe_id=f"id-{i}", category=f"category-{i % 2}", subtype="same")
                   for i in range(12)]
        a = beam.stratified_sample(records, 6, 42)
        b = beam.stratified_sample(list(reversed(records)), 6, 42)
        self.assertEqual([r["probe_id"] for r in a], [r["probe_id"] for r in b])
        self.assertEqual(sum(r["category"] == "category-0" for r in a), 3)

    def test_dry_run_without_server_or_client(self):
        args = beam.build_parser().parse_args(["--dry-run"])
        with patch.object(beam, "make_client", side_effect=AssertionError("unexpected client")), patch.object(beam, "MemVaultServer", side_effect=AssertionError("unexpected server")):
            result = beam.run(args, [self.probe])
        self.assertIsNone(result["probes"][0]["seeded"])
        self.assertEqual(result["summary"]["n_seed_not_attempted"], 1)
        self.assertIsNone(beam._safe_binary(args))

    def test_fake_end_to_end_request_records(self):
        args = beam.build_parser().parse_args(["--probes", "1"])
        judge_text = json.dumps({"rubric_met": [True, True], "compliant": True,
                                "non_compliance_sign_present": False, "reason": "okay"})
        with patch.object(beam, "make_client", return_value=client_for("control", "experiment", judge_text, judge_text)), patch.object(beam, "MemVaultServer", FakeBeamServer), patch.object(beam, "find_binary", return_value=Path("/mock/binary")), patch.object(beam, "BASE_URL", "http://mock.invalid"):
            result = beam.run(args, [self.probe])
        r = result["probes"][0]
        self.assertEqual(len(r["server_records"]), 3)
        self.assertEqual(r["control_records"][0]["request"]["messages"][1]["content"], self.probe["question"])
        self.assertEqual(r["injected_block"], self.probe["rule"])
        self.assertEqual(result["validity"]["paired_labels"]["n"], 1)


    def test_missing_or_false_rubric_not_normalized_away(self):
        for rubric in (None, False, "false"):
            raw = dict(self.probe["raw_annotation"], rubric=rubric)
            probe = beam.normalise_probe("instruction_following", raw, {})
            self.assertEqual(probe["rubric"], rubric)
            self.assertFalse(beam.judge(client_for(), probe, "answer")["validation"]["valid"])

    def test_invalid_regrade_denominator_and_flip_retained(self):
        r = record(self.probe)
        valid = json.dumps({"rubric_met": [False, False], "compliant": False,
                            "non_compliance_sign_present": False, "reason": "flip"})
        result = beam.regrade_pairs(client_for(valid, "bad JSON"), [r], 1, 0)
        self.assertEqual(result["n_valid_arm_pairs"], 1)
        self.assertTrue(result["items"][0]["arms"]["control"]["label_flip"])
        self.assertIsNone(result["items"][0]["arms"]["experiment"]["label_flip"])

    def test_dry_run_with_mock_server_still_no_client(self):
        args = beam.build_parser().parse_args(["--dry-run", "--with-server"])
        with patch.object(beam, "make_client", side_effect=AssertionError("unexpected client")), patch.object(beam, "MemVaultServer", FakeBeamServer), patch.object(beam, "find_binary", return_value=Path("/mock/binary")):
            result = beam.run(args, [self.probe])
        self.assertEqual(len(result["probes"][0]["server_records"]), 3)
        self.assertEqual(result["summary"]["n_judged"], 0)

    def test_run_failure_retains_partial_evidence(self):
        class FailingServer(FakeBeamServer):
            def inject(self, question):
                self.records.append({"request": {"context_hint": question}, "error": "failed"})
                raise RuntimeError("failed endpoint")
        args = beam.build_parser().parse_args(["--dry-run", "--with-server"])
        with patch.object(beam, "MemVaultServer", FailingServer), patch.object(beam, "find_binary", return_value=Path("/mock/binary")):
            result = beam.run(args, [self.probe])
        self.assertEqual(result["run_error"], "failed endpoint")
        self.assertEqual(result["probes"][0]["server_records"][-1]["error"], "failed")


class EvidenceTests(OfflineCase):
    def test_inline_dynamic_credential_headers_nested_strings(self):
        values = {
            "source": 'curl -H "X-MemVault-Api-Key: fixture-opaque-one" --data harmless',
            "model": "gateway 'x-api-key: fixture-opaque-two' model-small",
            "errors": ['API-KEY: "fixture-opaque-three"; timeout',
                       "Authorization: Basic Zml4dHVyZS1vbmx5; 401",
                       "aUtHoRiZaTiOn: bearer fixture-opaque-four"],
            "max_tokens": 300, "harmless": "ordinary answer text"}
        clean = beam.sanitize(values)
        encoded = json.dumps(clean)
        for value in ("fixture-opaque-one", "fixture-opaque-two", "fixture-opaque-three",
                      "Zml4dHVyZS1vbmx5", "fixture-opaque-four"):
            self.assertNotIn(value, encoded)
        self.assertEqual(clean["source"], 'curl -H "X-MemVault-Api-Key: [REDACTED]" --data harmless')
        self.assertEqual(clean["model"], "gateway 'x-api-key: [REDACTED]' model-small")
        self.assertEqual(clean["errors"][0], 'API-KEY: "[REDACTED]"; timeout')
        self.assertEqual(clean["errors"][1], "Authorization: Basic [REDACTED]; 401")
        self.assertEqual(clean["max_tokens"], 300)
        self.assertEqual(clean["harmless"], values["harmless"])
        self.assertEqual(mab.sanitize(values), clean)
        self.assertEqual(beam.sanitize(clean), clean)

    def test_quoted_header_values_and_existing_dictionary_redaction(self):
        value = {"model": '''{"x-MEMVAULT-api-KEY": "fixture opaque quoted", "Authorization": "Basic Zml4dHVyZQ=="}''',
                 "source": "curl -H 'api-key: fixture-unregistered'",
                 "credentials": "fixture-dictionary-secret", "response": "fixture-dictionary-secret",
                 "usage": {"prompt_tokens": 10, "completion_tokens": 4}}
        encoded = json.dumps(beam.sanitize(value))
        for secret in ("fixture opaque quoted", "Zml4dHVyZQ==", "fixture-unregistered", "fixture-dictionary-secret"):
            self.assertNotIn(secret, encoded)
        self.assertEqual(beam.sanitize(value)["usage"], value["usage"])

    def test_escaped_inline_headers_preserve_serialized_trace_structure(self):
        raw = json.dumps({"source": 'curl -H "X-MemVault-Api-Key: fixture-escaped-one" --data harmless',
                          "model": 'gateway "Authorization: Basic Zml4dHVyZS1lc2NhcGVk" model-small',
                          "max_tokens": 300})
        clean = beam.sanitize(raw)
        parsed = json.loads(clean)
        self.assertEqual(parsed["source"], 'curl -H "X-MemVault-Api-Key: [REDACTED]" --data harmless')
        self.assertEqual(parsed["model"], 'gateway "Authorization: Basic [REDACTED]" model-small')
        self.assertEqual(parsed["max_tokens"], 300)
        escaped = r'{\"x-api-key\": \"fixture spaced credential\", \"harmless\": 300}'
        self.assertEqual(beam.sanitize(escaped), r'{\"x-api-key\": \"[REDACTED]\", \"harmless\": 300}')
        self.assertEqual(beam.sanitize(clean), clean)

    def test_header_redaction_only_at_recording_boundary(self):
        question, reply = "Explain curl -H 'X-Api-Key: fixture-input-token'", "Authorization: Basic Zml4dHVyZS1yZXBseQ=="
        client = client_for(reply)
        trace = []
        self.assertEqual(beam.answer(client, question, "", 300, trace), reply)
        self.assertEqual(client.chat.completions.create.call_args.kwargs["messages"][1]["content"], question)
        self.assertNotIn("fixture-input-token", json.dumps(trace))
        self.assertNotIn("Zml4dHVyZS1yZXBseQ==", json.dumps(trace))
        self.assertTrue(mab.substring_exact_match(reply, ["Zml4dHVyZS1yZXBseQ=="]))

    def test_beam_hashes_exact_bytes_parsed_even_if_source_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "rows.json"
            path.write_text(json.dumps(self.rows["beam"]))
            original_hash = beam.content_hash(path)
            loads = json.loads
            def changed(payload):
                path.write_bytes(b"changed fixture after read")
                return loads(payload)
            evidence = {}
            with patch.object(beam.json, "loads", side_effect=changed):
                rows = beam.load_rows_from_file(path, evidence)
            self.assertEqual(rows[0]["id"], self.rows["beam"]["id"])
            self.assertEqual(evidence["source_sha256"], original_hash)
            self.assertNotEqual(evidence["source_sha256"], beam.content_hash(path))

    def test_mab_hashes_same_parquet_buffer_passed_to_reader(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "source.parquet"
            payload = b"fixture parquet before parsing"
            path.write_bytes(payload)
            original_hash = beam.content_hash(path)
            def read_table(buffer):
                self.assertEqual(buffer.read(), payload)
                path.write_bytes(b"fixture changed parquet")
                return SimpleNamespace(to_pylist=lambda: [self.rows["mab"]])
            pq = SimpleNamespace(read_table=MagicMock(side_effect=read_table))
            with patch.dict(sys.modules, {"pyarrow": SimpleNamespace(parquet=pq), "pyarrow.parquet": pq}):
                evidence = {}
                rows = mab.load_rows(path, evidence)
            self.assertEqual(rows, [self.rows["mab"]])
            self.assertEqual(evidence["source_sha256"], original_hash)
            self.assertNotEqual(evidence["source_sha256"], beam.content_hash(path))

    def test_annotations_hash_bytes_at_read_not_after_parse(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "support.json"
            path.write_text(json.dumps(self.rows["support"]))
            original_hash = beam.content_hash(path)
            loads = json.loads
            def changed(payload):
                path.write_text("{}")
                return loads(payload)
            evidence = {}
            with patch.object(mab.json, "loads", side_effect=changed):
                annotation = mab.load_support_annotations(str(path), evidence)
            self.assertEqual(annotation, self.rows["support"])
            self.assertEqual(evidence["source_sha256"], original_hash)
            self.assertNotEqual(evidence["source_sha256"], beam.content_hash(path))

    def test_both_servers_override_vault_configuration_before_popen(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            user = root / "user"
            vault = user / ".memvault"
            vault.mkdir(parents=True)
            user_env = vault / ".env"
            user_env.write_text("MEMVAULT_STORAGE_BACKEND=markdown\nMEMVAULT_MARKDOWN_DIR=/fixture/user-vault\n")
            original_env = user_env.read_bytes()
            polluted = {"HOME": str(user), "MEMVAULT_HOME": str(vault), "MEMVAULT_ENV_FILE": str(user_env),
                        "MEMVAULT_STORAGE_BACKEND": "markdown", "MEMVAULT_MARKDOWN_DIR": "/fixture/user-vault",
                        "MEMVAULT_DB": "/fixture/user-db", "MEMVAULT_AGENTS_CONFIG": "/fixture/user-agents",
                        "MEMVAULT_LLM_PROVIDER": "fixture-parent", "OPENAI_API_KEY": "fixture-parent-model-key"}
            for module in (beam, mab):
                with self.subTest(module=module.__name__):
                    owned = root / module.__name__
                    owned.mkdir()
                    process = MagicMock()
                    def popen(args, **kwargs):
                        env = kwargs["env"]
                        empty = Path(args[args.index("--env-file") + 1])
                        self.assertEqual(empty.parent, owned)
                        self.assertTrue(empty.is_file())
                        self.assertEqual(empty.read_bytes(), b"")
                        self.assertEqual(env["MEMVAULT_ENV_FILE"], str(empty))
                        self.assertEqual(env["MEMVAULT_STORAGE_BACKEND"], "sqlite")
                        self.assertEqual(env["MEMVAULT_HOME"], str(owned))
                        self.assertEqual(env["HOME"], str(owned))
                        self.assertEqual(Path(args[args.index("--db") + 1]).parent, owned)
                        self.assertEqual(env["MEMVAULT_EMBEDDING_PROVIDER"], "off")
                        for key in ("OPENAI_API_KEY", "MEMVAULT_MARKDOWN_DIR", "MEMVAULT_AGENTS_CONFIG", "MEMVAULT_LLM_PROVIDER"):
                            self.assertNotIn(key, env)
                        return process
                    with patch.dict("os.environ", polluted), patch("tempfile.mkdtemp", return_value=str(owned)), patch("subprocess.Popen", side_effect=popen) as started, patch("urllib.request.urlopen", return_value=FakeHTTP()):
                        server = (beam.MemVaultServer(Path("/fixture/binary"), False) if module is beam else
                                  mab.MemVaultServer(Path("/fixture/binary"), environment={**polluted, "MEMVAULT_EMBEDDING_PROVIDER": "off"}))
                        started.assert_called_once()
                        server.stop()
            self.assertEqual(user_env.read_bytes(), original_env)

    def test_beam_opt_in_preserves_only_requested_embedding_options(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict("os.environ", {
                "MEMVAULT_EMBEDDING_PROVIDER": "openai-compatible", "MEMVAULT_EMBEDDING_API_BASE": "http://mock.invalid/v1",
                "MEMVAULT_EMBEDDING_MODEL": "fixture-model", "MEMVAULT_EMBEDDING_DIM": "8",
                "MEMVAULT_EMBEDDING_API_KEY": "fixture-embedding-opt-in", "MEMVAULT_STORAGE_BACKEND": "markdown",
                "MEMVAULT_MARKDOWN_DIR": "/fixture/user-vault"}), patch("tempfile.mkdtemp", return_value=directory), patch("subprocess.Popen", return_value=MagicMock()) as popen, patch("urllib.request.urlopen", return_value=FakeHTTP()):
            server = beam.MemVaultServer(Path("/fixture/binary"), True)
            env = popen.call_args.kwargs["env"]
            self.assertEqual(env["MEMVAULT_STORAGE_BACKEND"], "sqlite")
            self.assertEqual(env["MEMVAULT_EMBEDDING_API_KEY"], "fixture-embedding-opt-in")
            self.assertEqual(env["MEMVAULT_EMBEDDING_API_BASE"], "http://mock.invalid/v1")
            self.assertEqual(env["MEMVAULT_EMBEDDING_MODEL"], "fixture-model")
            self.assertEqual(env["MEMVAULT_EMBEDDING_DIM"], "8")
            self.assertNotIn("MEMVAULT_MARKDOWN_DIR", env)
            server.stop()

    def test_sanitize_credentials_not_token_budgets(self):
        with patch.dict("os.environ", {"OPENAI_API_KEY": "opaque-key-value"}):
            clean = beam.sanitize({"api_key": "opaque-key-value", "max_tokens": 400,
                                   "url": "https://user:pass@example.test/v1?token=opaque-key-value",
                                   "message": "opaque-key-value"})
        self.assertNotIn("opaque-key-value", json.dumps(clean))
        self.assertNotIn("user:pass", clean["url"])
        self.assertEqual(clean["max_tokens"], 400)

    def test_missing_hash_and_actual_content_hash(self):
        self.assertIsNone(beam.content_hash("/missing/original/dataset"))
        self.assertEqual(len(beam.content_hash(FIXTURE)), 64)
        evidence = beam.execution_identity(Path(beam.__file__), None, {"source_sha256": None})
        self.assertIsNone(evidence["binary_sha256"])
        self.assertIsNone(evidence["dataset"]["source_sha256"])

    def test_dirty_checkout_identity(self):
        values = [" M docs/experiments/beam_adherence.py\n?? untracked.py\n", "actual diff bytes", "untracked.py", "commit", "branch", "tag"]
        with patch("subprocess.run", side_effect=[SimpleNamespace(returncode=0, stdout=s) for s in values]):
            identity = beam.checkout_identity()
        self.assertTrue(identity["dirty"])
        self.assertEqual(identity["commit"], "commit")
        self.assertEqual(len(identity["dirty_status_sha256"]), 64)

    def test_revision_cache_identity(self):
        self.assertNotEqual(beam.rows_cache_path("100K", 0, 2, "a"), beam.rows_cache_path("100K", 0, 2, "b"))
        self.assertNotEqual(mab.parquet_cache_path("a"), mab.parquet_cache_path("b"))
        self.assertIn("revision%2Fone", mab.parquet_url("revision/one"))

    def test_beam_download_cache_hashes_actual_response(self):
        payload = json.dumps({"rows": [{"row": self.rows["beam"]}]}).encode()
        with tempfile.TemporaryDirectory() as directory, patch.object(beam, "CACHE_DIR", Path(directory)), patch("urllib.request.urlopen", return_value=FakeHTTP(payload)) as urlopen:
            rows = beam.fetch_rows("100K", 0, 1, "fixed")
            source = beam.rows_cache_path("100K", 0, 1, "fixed")
            self.assertEqual(source.read_bytes(), payload)
            self.assertEqual(rows[0]["id"], "conversation-tiny")
            self.assertIn("revision=fixed", urlopen.call_args.args[0].full_url)
            args = beam.build_parser().parse_args(["--conversations", "1", "--dataset-revision", "fixed"])
            beam.collect_probes(args)
            self.assertEqual(args.dataset_evidence["source_sha256"], beam.content_hash(source))
            self.assertEqual(urlopen.call_count, 1)

    def test_http_records_request_response_failure(self):
        server = beam.MemVaultServer.__new__(beam.MemVaultServer)
        server.records, server.base = [], "http://mock.invalid"
        with patch("urllib.request.urlopen", return_value=FakeHTTP(b'{"data":{"id":"one"}}')):
            self.assertEqual(server._req("POST", "/api/memories", {"content": "rule"})["data"]["id"], "one")
        self.assertEqual(server.records[0]["request"]["body"], {"content": "rule"})
        self.assertEqual(server.records[0]["response_raw"], '{"data":{"id":"one"}}')
        with patch("urllib.request.urlopen", side_effect=RuntimeError("failed")):
            self.assertFalse(server.clear("one"))
        self.assertEqual(server.records[-1]["error"], "failed")

    def test_retry_records_each_attempt(self):
        client = client_for(RuntimeError("429 rate limit"), "answer")
        trace = []
        mab.answer(client, "question", "memory", 300, trace)
        self.assertEqual(len(trace), 2)
        self.assertIn("429", trace[0]["error"])
        self.assertEqual(trace[1]["response"]["choices"][0]["message"]["content"], "answer")

    def test_exact_paired_metrics(self):
        result = beam.paired_metrics([(False, True), (True, False), (True, True)])
        self.assertEqual(result["improved"], 1)
        self.assertEqual(result["regressed"], 1)
        self.assertEqual(result["delta"], 0)
        self.assertEqual(result["mcnemar_exact_p"], 1)
        self.assertIsNone(beam.paired_metrics([])["control"])


class MabTests(OfflineCase):
    def test_defaults_preserve_raw_keyword_off(self):
        args = mab.build_parser().parse_args([])
        self.assertEqual(args.search_mode, "keyword")
        self.assertEqual(args.embedding_provider, "off")
        self.assertEqual(args.arm, "search")
        server = mab.MemVaultServer.__new__(mab.MemVaultServer)
        server._req = MagicMock(return_value={"data": {"id": "one"}})
        server.seed_fact("1. original text", "mab-tiny")
        body = server._req.call_args.args[2]
        self.assertTrue(body["force_insert"])
        self.assertEqual(body["priority"], "REFERENCE")
        self.assertNotIn("fact_write", body)

    def test_child_environment_isolated_and_explicit_key(self):
        with patch.dict("os.environ", {"OPENAI_API_KEY": "parent-model-key", "MEMVAULT_EMBEDDING_PROVIDER": "openai",
                                       "MEMVAULT_AGENTS_CONFIG": "parent.yaml", "ODD_NAME": "child-opaque-value"}):
            default = mab.child_environment()
            configured = mab.child_environment("openai-compatible", "http://mock.invalid/v1", "tiny", 8, "ODD_NAME")
        self.assertEqual(default["MEMVAULT_EMBEDDING_PROVIDER"], "off")
        self.assertNotIn("OPENAI_API_KEY", configured)
        self.assertNotIn("MEMVAULT_AGENTS_CONFIG", configured)
        self.assertEqual(configured["MEMVAULT_EMBEDDING_API_KEY"], "child-opaque-value")
        self.assertEqual(beam.sanitize("child-opaque-value"), "[REDACTED]")

    def test_startup_process_environment_mocked(self):
        process = MagicMock()
        with tempfile.TemporaryDirectory() as directory, patch("subprocess.Popen", return_value=process) as popen, patch("urllib.request.urlopen", return_value=FakeHTTP()), patch.object(mab, "free_port", return_value=1234), patch("tempfile.mkdtemp", return_value=directory):
            server = mab.MemVaultServer(Path("/mock/binary"), "hybrid", "off")
            self.assertEqual(popen.call_args.kwargs["env"]["MEMVAULT_EMBEDDING_PROVIDER"], "off")
            self.assertEqual(server.mode, "hybrid")
            server.stop()
        process.terminate.assert_called_once()

    def test_requested_actual_mode_and_missing_reason(self):
        self.assertTrue(mab.retrieval_modes([{"search_mode": "keyword"}], "hybrid")["fallback_observed"])
        self.assertIsNone(mab.retrieval_modes([], "hybrid")["actual_modes"])
        self.assertIsNone(mab.retrieval_modes([{"search_mode": "keyword"}], "hybrid")["fallback_reason"])

    def test_support_any_complete_set_not_union_fragments(self):
        annotation = self.rows["support"]["tiny_no1"]
        self.assertTrue(mab.support_coverage(annotation, [4, 5]))
        self.assertTrue(mab.support_coverage(annotation, [2, 3]))
        self.assertFalse(mab.support_coverage(annotation, [2, 5]))
        self.assertIsNone(mab.support_coverage(None, [2, 3]))

    def test_invalid_support_references_and_boolean_serials(self):
        for annotation in ({"support_sets": [[True]]}, {"support_sets": [[12345]]}, {"support_sets": []}):
            with self.assertRaises(ValueError):
                mab.validate_support_annotation(annotation, {1, 2, 3})

    def test_normalization_and_substring_scorer_unchanged(self):
        self.assertEqual(mab.normalize_answer("The A, test!"), "test")
        self.assertTrue(mab.substring_exact_match("The Current City.", ["Current City"]))
        self.assertTrue(mab.substring_exact_match("not-current", ["Current"]))
        self.assertTrue(mab.substring_exact_match("anything", ["the"]))  # official empty-normalized-gold behavior

    def run_fake_mab(self, extra=(), dry=False, annotated=False, questions=1, texts=None,
                     row=None, server=FakeMabServer, expected_code=0):
        directory = self.stack.enter_context(tempfile.TemporaryDirectory())
        source, output, annotations = Path(directory) / "source.parquet", Path(directory) / "result.json", Path(directory) / "support.json"
        source.write_bytes(b"actual selected cache bytes")
        annotations.write_text(json.dumps(self.rows["support"]))
        argv = ["--parquet", str(source), "--binary", str(source), "--subsets", "tiny", "--questions", str(questions), "--arm", "both", "--out", str(output), *extra]
        if annotated:
            argv.extend(["--support-annotations", str(annotations)])
        if dry:
            argv.append("--dry-run")
        def loaded(path, evidence):
            evidence.update({"source_sha256": beam.content_hash(path), "hash_capture_phase": "read_bytes"})
            return [row if row is not None else self.rows["mab"]]
        client = client_for(*(texts if texts is not None else ("Current City", "wrong")))
        with patch.object(mab, "fetch_parquet", return_value=source), patch.object(mab, "load_rows", side_effect=loaded), patch.object(mab, "MemVaultServer", server), patch.object(mab, "make_client", return_value=client) as make_client, contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            code = mab.main(argv)
            if dry:
                make_client.assert_not_called()
        self.assertEqual(code, expected_code)
        return json.loads(output.read_text()), source, annotations

    def test_partial_both_arm_headline_uses_only_complete_pairs(self):
        row = copy.deepcopy(self.rows["mab"])
        row["questions"] = row["questions"] * 2
        row["answers"] = row["answers"] * 2
        row["metadata"]["qa_pair_ids"] = ["tiny_no1", "tiny_no2"]
        output, _, _ = self.run_fake_mab(questions=2, row=row,
            texts=("Current City", "Current City", "wrong", RuntimeError("401 fixture failed control")), expected_code=1)
        entry = output["results"][0]
        self.assertEqual(entry["questions"][1]["search_prediction"], "wrong")
        self.assertNotIn("control_prediction", entry["questions"][1])
        self.assertEqual(entry["summary"]["search_accuracy"], 1)
        self.assertEqual(entry["summary"]["control_accuracy"], 1)
        self.assertEqual(entry["summary"]["search_minus_control"], 0)
        scores = entry["diagnostics"]["validated_scores"]
        self.assertEqual(scores["schema_version"], 1)
        self.assertEqual(scores["paired_search_control"]["n"], 1)
        self.assertEqual(scores["unpaired_by_arm"]["search"], {"n_scored": 2, "n_correct": 1, "accuracy": 0.5})
        self.assertEqual(scores["unpaired_by_arm"]["control"], {"n_scored": 1, "n_correct": 1, "accuracy": 1})

    def test_requested_both_no_complete_pairs_has_null_headline(self):
        output, _, _ = self.run_fake_mab(texts=("Current City", RuntimeError("401 fixture missing control")), expected_code=1)
        entry = output["results"][0]
        for key in ("search_accuracy", "control_accuracy", "search_minus_control"):
            self.assertIsNone(entry["summary"][key])
        self.assertEqual(entry["diagnostics"]["validated_scores"]["unpaired_by_arm"]["search"]["accuracy"], 1)
        self.assertEqual(entry["diagnostics"]["validated_scores"]["paired_search_control"]["n"], 0)

    def test_single_arm_requests_keep_their_own_denominator(self):
        for arm, texts in (("search", ("Current City",)), ("control", ("Current City",)), ("inject", ("Current City",))):
            with self.subTest(arm=arm):
                output, _, _ = self.run_fake_mab(extra=("--arm", arm), texts=texts)
                entry = output["results"][0]
                self.assertEqual(entry["summary"][f"{arm}_accuracy"], 1)
                self.assertNotIn("search_minus_control", entry["summary"])
                self.assertEqual(entry["diagnostics"]["validated_scores"]["headline_denominator"], "requested_single_arm")

    def test_mab_execution_and_annotation_identity_retains_start_values(self):
        directory = self.stack.enter_context(tempfile.TemporaryDirectory())
        harness, shared = Path(directory) / "memory_agent_bench.py", Path(directory) / "beam_adherence.py"
        harness.write_bytes(b"fixture MAB harness start")
        shared.write_bytes(b"fixture shared helper start")
        hashes = {"harness": beam.content_hash(harness), "shared": beam.content_hash(shared)}
        class ChangingServer(FakeMabServer):
            def __init__(server, binary, *args):
                super().__init__(binary, *args)
                hashes["binary"] = beam.content_hash(binary)
                hashes["annotations"] = beam.content_hash(binary.with_name("support.json"))
                binary.write_bytes(b"fixture MAB binary and source changed")
                binary.with_name("support.json").write_text("{}")
                harness.write_bytes(b"fixture MAB harness end")
                shared.write_bytes(b"fixture shared helper end")
        with patch.object(mab, "__file__", str(harness)), patch.object(beam, "checkout_identity", return_value={"commit": "fixture-start", "branch": "fixture-start"}) as checkout:
            output, source, annotations = self.run_fake_mab(annotated=True, server=ChangingServer)
        checkout.assert_called_once()
        evidence = output["harness_spec"]["evidence"]
        self.assertEqual(evidence["capture_phase"], "before_execution")
        self.assertEqual(evidence["binary_path"], str(source.resolve()))
        self.assertEqual(evidence["binary_sha256"], hashes["binary"])
        self.assertEqual(evidence["dataset"]["source_sha256"], hashes["binary"])
        self.assertEqual(evidence["harness_sha256"], hashes["harness"])
        self.assertEqual(evidence["shared_evidence_harness_sha256"], hashes["shared"])
        self.assertEqual(evidence["checkout"]["commit"], "fixture-start")
        self.assertEqual(output["harness_spec"]["scoring"]["support_annotation_sha256"], hashes["annotations"])
        self.assertEqual(evidence["support_annotations"]["source_sha256"], hashes["annotations"])
        self.assertNotEqual(evidence["binary_sha256"], beam.content_hash(source))
        self.assertNotEqual(evidence["harness_sha256"], beam.content_hash(harness))
        self.assertNotEqual(evidence["support_annotations"]["source_sha256"], beam.content_hash(annotations))
        self.assertTrue(output["results"][0]["questions"][0]["support_in_retrieval"])

    def test_fake_main_fact_ids_objects_messages_and_serials(self):
        output, source, _ = self.run_fake_mab(["--search-mode", "hybrid"])
        entry = output["results"][0]
        q = entry["questions"][0]
        self.assertEqual(q["corpus_max_serial"], 99)
        self.assertEqual(q["newest_serial_in_context"], 99)
        self.assertEqual(q["retrieved_max_serial"], 3)
        self.assertEqual(entry["facts"][2]["memory_id"], "memory-3")
        self.assertEqual(q["retrieved_objects"][1]["memory"]["identity_verified"], False)
        self.assertEqual(q["raw_mode"], "raw_hybrid")
        self.assertEqual(output["harness_spec"]["evidence"]["dataset"]["source_sha256"], beam.content_hash(source))
        self.assertFalse(output["harness_spec"]["managed_arms"]["enabled"])
        self.assertIsNone(q["support_in_retrieval"])
        self.assertEqual(len(q["search_answer_records"][0]["request"]["messages"]), 2)
        self.assertEqual(q["search_answer_records"][0]["response"]["choices"][0]["message"]["content"], "Current City")

    def test_fake_dry_run_has_no_client_not_an_offline_cli_claim(self):
        output, _, _ = self.run_fake_mab(dry=True)
        q = output["results"][0]["questions"][0]
        self.assertNotIn("search_prediction", q)
        self.assertIsNone(output["results"][0]["summary"]["search_accuracy"])
        self.assertIsNone(output["results"][0]["diagnostics"]["support_coverage"])

    def test_support_annotations_never_in_prompt(self):
        annotation = mab.validate_support_annotation(self.rows["support"]["tiny_no1"], {1, 2, 3, 4, 5, 99})
        client = client_for("answer")
        trace = []
        mab.answer(client, self.rows["mab"]["questions"][0], "retrieved numbered facts", 300, trace)
        prompt = json.dumps(trace[0]["request"])
        self.assertNotIn("support_sets", prompt)
        self.assertNotIn("alice/residence", prompt)
        self.assertTrue(mab.support_coverage(annotation, [2, 3]))

    def test_main_support_annotations_scored_but_not_prompted(self):
        output, _, annotations = self.run_fake_mab(annotated=True)
        entry = output["results"][0]
        q = entry["questions"][0]
        self.assertTrue(q["support_in_retrieval"])
        self.assertFalse(q["support_in_top1"])
        self.assertEqual(entry["diagnostics"]["support_coverage"], 1)
        self.assertEqual(output["harness_spec"]["scoring"]["support_annotation_sha256"], beam.content_hash(annotations))
        prompt = json.dumps(q["search_answer_records"][0]["request"])
        self.assertNotIn("support_sets", prompt)
        self.assertNotIn("alice/residence", prompt)

    def test_session_scope_uses_existing_project_field(self):
        server = mab.MemVaultServer.__new__(mab.MemVaultServer)
        server.session_agent, server.session_project = "claude-code", "tiny"
        server._req = MagicMock(return_value={"data": {"formatted": "REFERENCE fact"}})
        text, _ = server.session("question")
        self.assertEqual(text, "REFERENCE fact")
        self.assertEqual(server._req.call_args.args, ("POST", "/api/session/preview", {
            "agent_id": "claude-code", "context_hint": "question", "project": "tiny",
        }))


    def run_managed_mab(self, mode="resolved", row=None, engine=None, extra=(), dry=True, **kwargs):
        engine = engine or FakeManagedHTTPStore()
        directory = self.stack.enter_context(tempfile.TemporaryDirectory())
        with patch("subprocess.Popen", side_effect=engine.popen), patch("urllib.request.urlopen", side_effect=engine.urlopen), patch.object(mab, "free_port", return_value=1234), patch("tempfile.mkdtemp", return_value=directory):
            result = self.run_fake_mab(extra=("--ingest-mode", mode, *extra), dry=dry,
                                       row=managed_row() if row is None else row, server=mab.MemVaultServer, **kwargs)
        return result[0], engine

    def test_ingest_mode_default_and_search_arm_are_independent(self):
        self.assertEqual(mab.build_parser().parse_args([]).ingest_mode, "raw")
        for mode in ("raw", "managed_raw", "resolved"):
            for arm in ("search", "control", "both", "inject"):
                args = mab.build_parser().parse_args(["--ingest-mode", mode, "--arm", arm])
                self.assertEqual((args.ingest_mode, args.arm), (mode, arm))
        output, _, _ = self.run_fake_mab(extra=("--ingest-mode", "raw"))
        self.assertEqual(output["harness_spec"]["raw_mode"], "raw_keyword")
        self.assertEqual(output["harness_spec"]["memvault"]["seed_strategy"], "raw force_insert=true, fact/REFERENCE; no resolve API used")
        self.assertFalse(output["harness_spec"]["memvault"]["seed_credentials_configured"])
        self.assertNotIn("fact_assertion", output["results"][0]["facts"][0])

    def test_atomic_sport_and_author_values_and_stable_keys(self):
        scope = mab.source_scope("tiny", managed_row()["context"])
        for old, new, values in (("1. goaltender is associated with the sport of ice hockey.",
                                  "310. goaltender is associated with the sport of pesäpallo.", ("ice hockey", "pesäpallo")),
                                 ("1. The author of A Wizard of Earthsea is Ursula K. Le Guin.",
                                  "310. The author of A Wizard of Earthsea is Pius XII.", ("Ursula K. Le Guin", "Pius XII"))):
            a, b = mab.parse_assertion(1, old, scope)["assertion"], mab.parse_assertion(310, new, scope)["assertion"]
            self.assertEqual(a["key"], b["key"])
            self.assertEqual((a["value"], b["value"]), values)
            self.assertEqual((a["source_order"], b["source_order"]), (1, 310))
            self.assertEqual(a["cardinality"], "single")
            self.assertNotIn(values[0], a["key"])
        author = mab.parse_assertion(3, "3. The author of goaltender is A Person.", scope)["assertion"]
        sport = mab.parse_assertion(3, "3. goaltender is associated with the sport of hockey.", scope)["assertion"]
        self.assertNotEqual(author["key"], sport["key"])

    def test_parser_scope_and_assertions_do_not_consult_questions_golds_or_support(self):
        row = managed_row()
        scope = mab.source_scope("tiny", row["context"])
        assertion = mab.parse_assertion(1, row["context"].splitlines()[0], scope)
        row.update({"questions": ["Different question with new author"], "answers": [["invented gold"]],
                    "support_sets": [[999]], "metadata": {"qa_pair_ids": ["changed_qa"]}})
        self.assertEqual(scope, mab.source_scope("tiny", row["context"]))
        self.assertEqual(assertion, mab.parse_assertion(1, row["context"].splitlines()[0], scope))
        self.assertNotEqual(scope, mab.source_scope("different-subset", row["context"]))
        self.assertNotEqual(scope, mab.source_scope("tiny", row["context"] + "\n6. Extra input."))
        with self.assertRaises(TypeError):
            mab.parse_assertion(1, row["context"].splitlines()[0], scope, row["answers"])

    def test_unrecognized_multi_event_unknown_sentences_remain_unparsed(self):
        for sentence in ("An event happened on 2024-01-01.", "Alice lives in Paris.",
                         "The authors of Book are Alice and Bob.", "The author of Book is Alice and Bob.",
                         "The author of Book is Alice, Bob.", "Player is associated with the sport of hockey or tennis.",
                         "The current author of Book is unknown.", "The author of Book is unknown.",
                         "The author of Book is Jane Doe. Another assertion."):
            parsed = mab.parse_assertion(1, "1. " + sentence, "scope")
            self.assertIsNone(parsed["assertion"], sentence)
            self.assertIn("preserved_raw", parsed["reason"])

    def test_single_letter_internal_sentence_boundary_is_preserved_raw(self):
        for value in ("A. Bob left", "a. Bob left", "A. Bob Left", "A. B", "A.. Bob left",
                      r"A.\nBob left", r"A.\rBob left", "A.\tBob left"):
            with self.subTest(value=value):
                parsed = mab.parse_assertion(5, "5. The author of Book is " + value + ".", "scope")
                self.assertIsNone(parsed["assertion"])
                self.assertEqual(parsed["reason"], "ambiguous_internal_sentence_boundary_or_abbreviation_preserved_raw")

    def test_uppercase_abbreviation_and_leading_initials_are_preserved_raw(self):
        for value in ("AB. Bob left", "AB. Bob Left", "ab. Bob left", "aB. Bob left",
                      "J. Smith", "J. R. R. Tolkien", "T. S. Eliot", "Alice Ph.D. Smith",
                      "Alice B. C. Smith", "Alice B. Bob left", r"J.\nSmith", r"Jane\nBob left"):
            with self.subTest(value=value):
                parsed = mab.parse_assertion(5, "5. The author of Book is " + value + ".", "scope")
                self.assertIsNone(parsed["assertion"])
                self.assertIn("abbreviation_preserved_raw", parsed["reason"])

    def test_dotted_multisentence_or_abbreviated_subjects_are_preserved_raw(self):
        for sentence in ("The author of Book. Bob left is Jane Doe.",
                         "The author of A. Bob Left is Jane Doe.",
                         "The author of AB. Bob left is Jane Doe.",
                         "The author of J. R. Book is Jane Doe.",
                         "Player. Bob left is associated with the sport of hockey.",
                         "Player A. Smith is associated with the sport of hockey.",
                         r"The author of Book.\nBob left is Jane Doe.",
                         r"Player AB.\rBob left is associated with the sport of hockey."):
            with self.subTest(sentence=sentence):
                parsed = mab.parse_assertion(5, "5. " + sentence, "scope")
                self.assertIsNone(parsed["assertion"])
                self.assertIn("boundary_or_abbreviation_preserved_raw", parsed["reason"])

    def test_single_middle_initial_full_name_format_and_single_templates_remain_compatible(self):
        self.assertEqual(mab.ASSERTION_PARSER_VERSION, 1)  # v1 format bugfix, not a new schema/template
        self.assertEqual(len(mab.ASSERTION_TEMPLATES), 2)
        for sentence, value in (("The author of A Wizard of Earthsea is Ursula K. Le Guin.", "Ursula K. Le Guin"),
                                ("The author of Starship Troopers is Robert A. Heinlein.", "Robert A. Heinlein"),
                                ("The author of Book is Jane Doe.", "Jane Doe"),
                                ("goaltender is associated with the sport of pesäpallo.", "pesäpallo")):
            with self.subTest(sentence=sentence):
                parsed = mab.parse_assertion(5, "5. " + sentence, "scope")["assertion"]
                self.assertEqual((parsed["value"], parsed["source_order"], parsed["cardinality"]), (value, 5, "single"))
        # A name-like dotted string is not a sport, and appended second text
        # must not inherit the finite middle-initial exception.
        for sentence in ("Player is associated with the sport of Alice B. Smith.",
                         "The author of Book is Ursula K. Le Guin. Bob left.",
                         "The author of Book is Ursula K. Le Guin. Bob Left.",
                         r"The author of Book is Ursula K. Le Guin.\nBob left."):
            self.assertIsNone(mab.parse_assertion(5, "5. " + sentence, "scope")["assertion"])

    def test_ambiguous_sentence_boundary_counts_as_unparsed_literal_input(self):
        row = managed_row()
        row["context"] += "\n55. The author of Book is A. Bob left."
        for mode in ("managed_raw", "resolved"):
            with self.subTest(mode=mode):
                output, engine = self.run_managed_mab(mode, row=row)
                fact = output["results"][0]["facts"][-1]
                self.assertEqual(fact["source_serial"], 55)
                self.assertIsNone(fact["fact_assertion"])
                self.assertTrue(fact["stored_as_input"])
                self.assertIn("boundary_or_abbreviation_preserved_raw", fact["assertion_parse"]["reason"])
                self.assertEqual(output["results"][0]["ingest_diagnostics"]["n_unparsed_preserved_raw"], 2)
                request = next(c["body"] for c in engine.calls if c["body"] and c["body"].get("content") == fact["content"])
                self.assertEqual((request["force_insert"], request["fact_write"]), (True, "preserve"))
                self.assertNotIn("fact_assertion", request)

    def test_parser_order_uses_original_serial_not_date_and_rejects_mismatch(self):
        parsed = mab.parse_assertion(2, "2. The author of 2099: A Novel is Jane Doe.", "scope")
        self.assertEqual(parsed["assertion"]["source_order"], 2)
        for serial in (True, -1, 1 << 64, 3):
            with self.assertRaises(ValueError):
                mab.parse_assertion(serial, "2. The author of Book is Jane Doe.", "scope")

    def test_managed_raw_and_resolved_same_auth_profile_and_nonwrite_configuration(self):
        outputs, engines = [], []
        for mode in ("managed_raw", "resolved"):
            output, engine = self.run_managed_mab(mode, extra=("--search-mode", "hybrid", "--embedding-provider", "off"))
            outputs.append(output)
            engines.append(engine)
        a, b = (output["harness_spec"] for output in outputs)
        for key in ("producer", "assertion_parser"):
            self.assertEqual(a["managed_arms"][key], b["managed_arms"][key])
        for key in ("models", "prompts", "scoring"):
            self.assertEqual(a[key], b[key])
        self.assertEqual(a["memvault"]["child_environment"], b["memvault"]["child_environment"])
        self.assertEqual([f["fact_assertion"] for f in outputs[0]["results"][0]["facts"]],
                         [f["fact_assertion"] for f in outputs[1]["results"][0]["facts"]])
        self.assertNotEqual(a["managed_arms"]["isolation"]["db"], b["managed_arms"]["isolation"]["db"])
        for engine in engines:
            self.assertEqual(len(engine.startups), 1)
            self.assertTrue(all(c["headers"].get("x-memvault-agent-id") == engine.profile["id"] for c in engine.calls))
            self.assertTrue(all(c["headers"].get("x-memvault-api-key") == engine.profile["api_key"] for c in engine.calls))
            for call in engine.calls:
                if call["path"] == "/api/memories" and call["body"]:
                    self.assertEqual(call["body"]["agent_type"], "coding-assistant")
                    for forbidden in ("human_reviewed", "ai_generated", "fact_management", "producer_id", "managed_owner"):
                        self.assertNotIn(forbidden, call["body"])

    def test_managed_force_flags_actual_unmanaged_state_and_resolved_new_ids(self):
        raw, raw_engine = self.run_managed_mab("managed_raw")
        resolved, resolved_engine = self.run_managed_mab("resolved")
        raw_seed = [c["body"] for c in raw_engine.calls if c["path"] == "/api/memories" and c["body"]]
        resolved_seed = [c["body"] for c in resolved_engine.calls if c["path"] == "/api/memories" and c["body"]]
        self.assertTrue(all(c["force_insert"] and c["fact_write"] == "preserve" for c in raw_seed))
        self.assertTrue(all(not c["force_insert"] and c["fact_write"] == "resolve" for c in resolved_seed[:4]))
        self.assertTrue(resolved_seed[4]["force_insert"])
        self.assertNotIn("fact_assertion", resolved_seed[4])
        a, b = raw["results"][0]["ingest_diagnostics"], resolved["results"][0]["ingest_diagnostics"]
        self.assertEqual((a["n_latest_active"], a["n_managed_active"]), (5, 0))
        self.assertEqual((b["n_latest_active"], b["n_managed_active"], b["n_superseded"]), (3, 2, 2))
        self.assertEqual((b["n_parsed"], b["n_unparsed_preserved_raw"]), (4, 1))
        facts = resolved["results"][0]["facts"]
        self.assertEqual(facts[1]["seed_decision"]["superseded_id"], facts[0]["memory_id"])
        self.assertNotEqual(facts[1]["memory_id"], facts[0]["memory_id"])
        self.assertTrue(all(f["stored_as_input"] for f in facts))
        self.assertFalse(b["as_of"]["older_indices_full_triple_chain_verified"])
        self.assertEqual(b["as_of"]["status"], "future_only_not_tested")

    def test_managed_unverified_auth_ignored_aborts_both_modes(self):
        for mode in ("managed_raw", "resolved"):
            with self.subTest(mode=mode):
                output, engine = self.run_managed_mab(mode, engine=FakeManagedHTTPStore(verified=False), expected_code=1)
                self.assertIn("producer unverified", output["run_error"])
                self.assertIs(output["harness_spec"]["managed_arms"]["identity_verified"], False)
                self.assertEqual(len(engine.memories), 1)
                self.assertEqual(output["results"][0]["facts"][0]["memory_id"], engine.memories[0]["id"])
                self.assertFalse(output["results"][0]["ingest_diagnostics"]["snapshot_complete"])
                self.assertIsNone(output["results"][0]["ingest_diagnostics"]["n_latest_active"])

    def test_resolve_api_missing_or_ignored_aborts_without_raw_claim(self):
        output, engine = self.run_managed_mab(engine=FakeManagedHTTPStore(ignore_resolve=True), expected_code=1)
        self.assertIn("resolve API missing or ignored", output["run_error"])
        self.assertEqual(len(engine.memories), 1)
        fact = output["results"][0]["facts"][0]
        self.assertIn("seed_error", fact)
        self.assertEqual(fact["seed_records"][0]["request"]["body"]["fact_write"], "resolve")
        self.assertEqual(output["harness_spec"]["ingest_mode"], "resolved")
        self.assertIsNone(output["harness_spec"]["raw_mode"])

    def test_managed_raw_assertions_ignored_are_explicit_failure(self):
        output, _ = self.run_managed_mab("managed_raw", engine=FakeManagedHTTPStore(ignore_assertion=True), expected_code=1)
        self.assertIn("literally preserve assertion", output["run_error"])

    def test_requested_resolved_without_supported_templates_aborts(self):
        row = managed_row()
        row["context"] = "1. Unsupported event in 2024."
        output, _ = self.run_managed_mab(row=row, expected_code=1)
        self.assertIn("no supported single-slot facts", output["run_error"])
        self.assertEqual(output["results"][0]["ingest_diagnostics"]["n_unparsed_preserved_raw"], 1)

    def test_http_409_refusal_tracked_and_not_counted_as_new_insert(self):
        server = mab.MemVaultServer.__new__(mab.MemVaultServer)
        server.records, server.base, server.headers = [], "http://mock.invalid", {}
        server.producer, server.identity_verified = {"agent_id": "fixture-producer", "agent_type": "coding-assistant"}, True
        response = {"ok": False, "data": {"id": "persisted-current", "action": "skipped", "fact_outcome": "conflict",
                    "written": False, "reason": "fixture protected status"}}
        refused = mab.urllib.error.HTTPError(server.base, 409, "Conflict", {}, io.BytesIO(json.dumps(response).encode()))
        with patch("urllib.request.urlopen", side_effect=refused):
            decision = server.seed_managed_fact("2. The author of Book is Bob.", "scope",
                       mab.parse_assertion(2, "2. The author of Book is Bob.", "scope")["assertion"], "resolved")
        self.assertEqual(decision["id"], "persisted-current")
        self.assertEqual(server.records[0]["status"], 409)
        self.assertEqual(server.records[0]["response"], response)
        diagnostic = mab.ingest_diagnostics([{"seed_decision": decision, "fact_assertion": {"key": "book"}}], [], "resolved")
        self.assertEqual((diagnostic["n_refused"], diagnostic["n_conflicts"], diagnostic["n_written"]), (1, 1, 0))

    def test_http_unsupported_400_is_failure_with_preserved_decision(self):
        server = mab.MemVaultServer.__new__(mab.MemVaultServer)
        server.records, server.base, server.headers = [], "http://mock.invalid", {}
        server.producer = {"agent_id": "fixture-producer", "agent_type": "coding-assistant"}
        value = {"ok": False, "data": {"fact_outcome": "unsupported", "written": False, "reason": "fixture backend"}}
        error = mab.urllib.error.HTTPError(server.base, 400, "Bad Request", {}, io.BytesIO(json.dumps(value).encode()))
        with patch("urllib.request.urlopen", side_effect=error), self.assertRaisesRegex(ValueError, "unsupported"):
            server.seed_managed_fact("1. The author of Book is Alice.", "scope",
                       mab.parse_assertion(1, "1. The author of Book is Alice.", "scope")["assertion"], "resolved")
        self.assertEqual(server.records[0]["status"], 400)
        self.assertEqual(server.records[0]["response"]["data"]["fact_outcome"], "unsupported")

    def test_stale_returns_current_id_without_changing_source_serial_mapping(self):
        row = managed_row()
        lines = row["context"].splitlines()
        row["context"] = "\n".join([lines[1], lines[0], *lines[2:]])
        output, _ = self.run_managed_mab(row=row)
        entry = output["results"][0]
        newer, stale = entry["facts"][:2]
        self.assertEqual(stale["seed_decision"]["fact_outcome"], "stale")
        self.assertIs(stale["seed_decision"]["written"], False)
        self.assertEqual(newer["memory_id"], stale["memory_id"])
        self.assertEqual((newer["source_serial"], stale["source_serial"]), (9, 1))
        self.assertTrue(newer["stored_as_input"])
        self.assertFalse(stale["stored_as_input"])
        self.assertIn(9, entry["questions"][0]["retrieved_serials"])
        self.assertNotIn(1, entry["questions"][0]["retrieved_serials"])
        self.assertEqual(entry["ingest_diagnostics"]["n_stale"], 1)

    def test_support_ids_map_replacement_sources_not_ephemeral_memory_ids(self):
        self.rows["support"] = {"tiny_no1": {"support_sets": [[9, 15]], "latest_serial_by_key": {"sport/goaltender": 9}}}
        output, _ = self.run_managed_mab(annotated=True)
        entry = output["results"][0]
        q = entry["questions"][0]
        self.assertTrue(q["support_in_retrieval"])
        self.assertFalse(q["support_in_top1"])
        self.assertTrue(q["support_in_top5"])
        self.assertEqual(q["retrieved_serials"], [9, 15, 5])
        self.assertEqual(q["retrieved_source_ids"], [entry["facts"][i]["source_id"] for i in (1, 3, 4)])
        unknown = [{"memory": {"id": entry["facts"][1]["memory_id"], "content": "100. forged unknown input."}}]
        self.assertEqual(mab.retrieved_source_ids(unknown, entry["facts"]), ([], []))

    def test_ephemeral_registered_key_never_leaks_full_sanitized_evidence(self):
        with patch.object(mab.secrets, "token_urlsafe", return_value="fixture-ephemeral-producer-secret"):
            output, engine = self.run_managed_mab()
        self.assertNotIn(engine.profile["api_key"], json.dumps(output))
        self.assertEqual(output["server_records"][0]["request"]["headers"]["X-MemVault-Api-Key"], "[REDACTED]")
        self.assertNotIn("api_key", output["harness_spec"]["managed_arms"]["producer"]["profile"])
        self.assertEqual(mab.sanitize("echo fixture-ephemeral-producer-secret"), "echo [REDACTED]")
        self.assertIs(output["harness_spec"]["managed_arms"]["identity_verified"], True)

    def test_managed_registry_actual_path_owned_empty_env_and_parent_unchanged(self):
        polluted = {"HOME": "/fixture/global-home", "MEMVAULT_HOME": "/fixture/global-vault",
                    "MEMVAULT_AGENTS_CONFIG": "/fixture/global-agents.yaml", "MEMVAULT_STORAGE_BACKEND": "markdown",
                    "MEMVAULT_MARKDOWN_DIR": "/fixture/real-vault", "OPENAI_API_KEY": "fixture-parent-secret"}
        with patch.dict("os.environ", polluted):
            output, engine = self.run_managed_mab()
            for key, value in polluted.items():
                self.assertEqual(mab.os.environ[key], value)
        args, kwargs = engine.startups[0]
        env = kwargs["env"]
        db, empty = Path(args[args.index("--db") + 1]), Path(args[args.index("--env-file") + 1])
        self.assertEqual(engine.registry, db.parent / "agents.yaml")
        self.assertEqual(empty.parent, db.parent)
        self.assertEqual(env["HOME"], env["MEMVAULT_HOME"])
        self.assertEqual(env["MEMVAULT_STORAGE_BACKEND"], "sqlite")
        for key in ("MEMVAULT_AGENTS_CONFIG", "MEMVAULT_MARKDOWN_DIR", "OPENAI_API_KEY"):
            self.assertNotIn(key, env)
        isolation = output["harness_spec"]["managed_arms"]["isolation"]
        self.assertEqual(isolation["registry_path"], str(engine.registry))
        self.assertEqual(isolation["env_file_sha256"], mab.hashlib.sha256(b"").hexdigest())
        self.assertEqual(isolation["data_dir"], str(db.parent))

    def test_selected_managed_strategy_never_triples_model_calls(self):
        for mode in ("managed_raw", "resolved"):
            with self.subTest(mode=mode):
                output, engine = self.run_managed_mab(mode, extra=("--arm", "search"), dry=False, texts=("pesäpallo",))
                entry = output["results"][0]
                self.assertEqual(len(engine.startups), 1)
                self.assertEqual(len(entry["questions"][0]["search_answer_records"]), 1)
                self.assertNotIn("control_answer_records", entry["questions"][0])
                self.assertEqual(entry["summary"]["search_accuracy"], 1)

    def test_idempotent_response_keeps_current_id_and_written_false(self):
        output, engine = self.run_managed_mab()
        server = mab.MemVaultServer.__new__(mab.MemVaultServer)
        producer = output["harness_spec"]["managed_arms"]["producer"]
        server.base, server.records, server.producer = "http://mock.invalid", [], producer
        server.headers = {"X-MemVault-Agent-Id": producer["agent_id"], "X-MemVault-Api-Key": engine.profile["api_key"]}
        server.identity_verified = True
        fact = output["results"][0]["facts"][1]
        with patch("urllib.request.urlopen", side_effect=engine.urlopen):
            decision = server.seed_managed_fact(fact["content"], producer["namespaces"]["tiny"], fact["fact_assertion"], "resolved")
        self.assertEqual(decision["fact_outcome"], "idempotent")
        self.assertIs(decision["written"], False)
        self.assertEqual(decision["id"], fact["memory_id"])
        self.assertEqual(server.records[0]["status"], 200)
        self.assertEqual(len(engine.memories), 5)

    def test_inconsistent_resolve_ids_or_management_abort(self):
        mutations = (
            lambda data: data.update(written=False),
            lambda data: data["memory"].update(fact_management=None),
            lambda data: data["memory"]["fact_management"].update(producer_id="wrong-producer"),
            lambda data: data["memory"]["fact_management"].update(record_id="unpersisted-id"),
            lambda data: data["memory"].update(id="wrong-memory-id"),
            lambda data: data["memory"].update(human_reviewed=True),
            lambda data: data["memory"]["fact_assertion"].update(source_order=900),
        )
        for mutate in mutations:
            with self.subTest(mutate=mutate):
                engine = FakeManagedHTTPStore()
                response = engine.response
                def corrupted(value, request, status=200):
                    if isinstance(value.get("data"), dict) and value["data"].get("fact_outcome") == "inserted":
                        mutate(value["data"])
                    return response(value, request, status)
                engine.response = corrupted
                output, _ = self.run_managed_mab(engine=engine, expected_code=1)
                self.assertIsNotNone(output["run_error"])
                self.assertEqual(len(engine.memories), 1)
                self.assertEqual(output["results"][0]["ingest_diagnostics"]["n_errors"], 1)

    def test_duplicate_source_serials_fail_before_managed_seed(self):
        row = managed_row()
        row["context"] = row["context"].replace("9. goaltender", "1. goaltender")
        output, engine = self.run_managed_mab(row=row, expected_code=1)
        self.assertIn("duplicate corpus serials", output["run_error"])
        self.assertEqual(engine.memories, [])

    def test_managed_preview_keeps_requested_target_distinct_from_authorizing_producer(self):
        output, engine = self.run_managed_mab(extra=("--arm", "inject", "--session-agent", "fixture-target"),
                                              dry=False, texts=("pesäpallo",))
        session = next(c for c in engine.calls if c["path"] == "/api/session/preview")
        producer = output["harness_spec"]["managed_arms"]["producer"]
        self.assertEqual(session["body"], {
            "agent_id": "fixture-target", "context_hint": managed_row()["questions"][0],
            "project": producer["namespaces"]["tiny"],
        })
        self.assertNotEqual(session["body"]["agent_id"], producer["agent_id"])
        self.assertEqual(session["headers"]["x-memvault-agent-id"], producer["agent_id"])
        self.assertEqual(session["headers"]["x-memvault-api-key"], engine.profile["api_key"])
        self.assertFalse(any(c["path"] == "/api/session" for c in engine.calls))
        question = output["results"][0]["questions"][0]
        scope = question["session_scope_diagnostic"]
        self.assertEqual(scope["seed_namespace"], session["body"]["project"])
        self.assertEqual(scope["agent_id_requested"], "fixture-target")
        self.assertEqual(scope["operational_actor"], producer["agent_id"])
        self.assertEqual(scope["endpoint"], "/api/session/preview")
        self.assertEqual(question["session_records"][0]["response"]["data"]["mode"], "preview")
        self.assertNotIn("inject_session_id", question["session_records"][0]["response"]["data"])
        self.assertNotIn(engine.profile["api_key"], json.dumps(output))

    def test_managed_preview_legacy_default_target_is_not_silently_replaced(self):
        output, engine = self.run_managed_mab(extra=("--arm", "inject"), dry=False, texts=("pesäpallo",))
        preview = next(c for c in engine.calls if c["path"] == "/api/session/preview")
        self.assertEqual(preview["body"]["agent_id"], "codex")
        self.assertNotEqual(preview["headers"]["x-memvault-agent-id"], "codex")
        self.assertEqual(output["harness_spec"]["options"]["session_agent"], "codex")

    def test_preview_failure_has_no_operational_fallback(self):
        server = mab.MemVaultServer.__new__(mab.MemVaultServer)
        server.session_agent, server.session_project = "fixture-target", None
        server._req = MagicMock(side_effect=ValueError("preview unsupported"))
        with self.assertRaisesRegex(ValueError, "preview unsupported"):
            server.session("question")
        server._req.assert_called_once_with("POST", "/api/session/preview", {
            "agent_id": "fixture-target", "context_hint": "question",
        })


class ReplayTests(OfflineCase):
    def test_small_legacy_beam_replay_preserves_scores(self):
        records = [record(self.probe, False, True), record(self.probe, True, True)]
        for i, r in enumerate(records):
            r["n"] = i + 1
            del r["probe"]
        payload = {"probes": records, "summary": beam.summarise(records)}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "old-beam.json"
            path.write_text(json.dumps(payload))
            before = beam.content_hash(path)
            result = offline_replay.replay(path)
            self.assertEqual(result["paired_labels"]["experiment_correct"], 2)
            self.assertEqual(result["summary_differences"], {})
            self.assertEqual(result["n_rubric_source_missing"], 2)
            self.assertIsNone(result["dataset_source_sha256"])
            self.assertEqual(before, beam.content_hash(path))
        self.network.assert_not_called()
        self.popen.assert_not_called()

    def test_small_mab_replay_predictions_and_top_coverage(self):
        q = {"question": "q", "qa_pair_id": "q1", "golds": ["Current City"],
             "retrieved_contents": ["1. Old City", "3. Current City"], "retrieved_n": 2,
             "retrieved_tokens_est": 10, "search_latency_s": 0.1,
             "gold_in_retrieval": True, "gold_in_top1": False, "gold_in_top5": True,
             "search_prediction": "Current City", "search_correct": True,
             "control_prediction": "Old City", "control_correct": False}
        payload = {"harness_spec": {"parquet": "/missing/original.parquet"},
                   "results": [{"questions": [q], "summary": mab.summarise_questions([q])}]}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "old-mab.json"
            path.write_text(json.dumps(payload))
            before = beam.content_hash(path)
            result = offline_replay.replay(path)
            self.assertEqual(result["results"][0]["summary_differences"], {})
            self.assertEqual(result["results"][0]["diagnostics"]["paired_prediction_scores"]["improved"], 1)
            self.assertIsNone(result["dataset_source_sha256"])
            self.assertEqual(before, beam.content_hash(path))
        self.network.assert_not_called()
        self.popen.assert_not_called()


if __name__ == "__main__":
    unittest.main()
