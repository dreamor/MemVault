#!/usr/bin/env python3
"""
H7 — Procedural memory acceptance experiment
============================================

Phase B acceptance:

  H7a: Skill injection improves first-attempt task success.
       A/B: agent plans a task WITH the matched skill injected (the real
       MemVault session_start output) vs with no injection. Each skill's
       steps carry NON-OBVIOUS, project-specific facts the agent cannot
       guess (same design principle as H5). Primary metric is objective:
       does the plan convey the specific facts (pre-registered keyword
       stems). Secondary metric: quote-grounded LLM judge.

  H7b: Trigger mis-injection rate < 5%.
       Skills live in the global namespace; the session runs in a project
       namespace filled with decoy memories, so the ONLY path by which a
       skill can enter an unrelated session is trigger matching (generic
       search is confined to the project namespace, and the cross-namespace
       fallback cannot fire). Feed many unrelated contexts; count how often
       a skill still gets injected.

The experiment drives a REAL memvault-mcp REST server (spawned as a
subprocess on a throwaway DB) so injection text, trigger matching and
quotas are all the production code paths.

Usage:
  VERIFY_BASE_URL=http://127.0.0.1:8123/v1 VERIFY_MODEL=qwen2.5-1.5b-instruct \
  VERIFY_JUDGE_BASE_URL=http://127.0.0.1:8124/v1 VERIFY_JUDGE_MODEL=qwen2.5-3b-instruct \
  python docs/experiments/verify_h7.py [--rounds 3] [--misfire-samples 40] [--binary path/to/memvault-mcp]
"""

import argparse
import copy
import json
import math
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

import benchmark_evidence as beam
import judge_grounding as grounding
from judge_grounding import (
    aggregate_judgments, generation_status, judge_quote as grounded_judge_quote,
    recorded_completion, sanitize, summarize_generation_pairs, summarize_judgments,
)

try:
    from openai import OpenAI
except ImportError:
    print("ERROR: pip install openai")
    sys.exit(1)

MODEL = os.getenv("VERIFY_MODEL", "gpt-4o-mini")
BASE_URL = os.getenv("VERIFY_BASE_URL") or os.getenv("OPENAI_BASE_URL") or None
client = None

JUDGE_MODEL = os.getenv("VERIFY_JUDGE_MODEL", MODEL)
JUDGE_BASE_URL = os.getenv("VERIFY_JUDGE_BASE_URL", BASE_URL)
judge_client = None


def ensure_client():
    """Construct lazily so CLI configuration failures can be recorded."""
    global client
    if client is None:
        client = OpenAI(base_url=BASE_URL, api_key=os.getenv("OPENAI_API_KEY") or "local")
    return client


def ensure_judge_client():
    """Honor patched clients and keep the original shared/separate role policy."""
    global judge_client
    if judge_client is None:
        judge_client = (
            OpenAI(base_url=JUDGE_BASE_URL, api_key=os.getenv("OPENAI_API_KEY") or "local")
            if JUDGE_BASE_URL != BASE_URL or JUDGE_MODEL != MODEL
            else ensure_client()
        )
    return judge_client

BASE_SYSTEM = (
    "You are a careful engineering assistant. When asked to perform a task, "
    "respond ONLY with a concise numbered step-by-step plan."
)

# ─── Scenarios (pre-registered; steps carry unguessable project facts) ───

SCENARIOS = [
    {
        "task_type": "deploy",
        "task": "Deploy our web dashboard to the production environment and confirm it is live.",
        "skill": {
            "content": "deploy dashboard runbook",
            "trigger": "deploy",
            "steps": [
                "Set DASHBOARD_CDN to cdn-v2.memvault.io before building",
                "Run ./scripts/smoke-pages.sh after the push",
            ],
            "ordered_constraints": {
                "0": ["Set DASHBOARD_CDN to cdn-v2.memvault.io", "Build the dashboard"],
                "1": ["Push the dashboard", "Run ./scripts/smoke-pages.sh"],
            },
            "verification": "curl /healthz returns build=v2",
        },
        "keyword_stems": ["dashboard_cdn", "smoke-pages"],
    },
    {
        "task_type": "migrate",
        "task": "Migrate the users table to the new schema with the added profile columns.",
        "skill": {
            "content": "users migration runbook",
            "trigger": "migrate",
            "steps": [
                "Backfill users.profile_json with '{}' before adding the NOT NULL constraint",
                "Verify row counts match before and after the migration",
            ],
            "ordered_constraints": {
                "0": ["Backfill users.profile_json with '{}'", "Add the NOT NULL constraint"],
                "1": ["Record the row count before migration", "Check the post-migration count matches the pre-migration count"],
            },
            "verification": "row counts identical, no NULL profile_json",
        },
        "keyword_stems": ["profile_json"],
    },
    {
        "task_type": "upgrade",
        "task": "Upgrade the payment SDK from v2 to v3 in our checkout service.",
        "skill": {
            "content": "payment sdk upgrade runbook",
            "trigger": "upgrade",
            "steps": [
                "Add an Idempotency-Key header to every POST call after moving to payment v3",
                "Run the refund flow in the sandbox before production rollout",
            ],
            "ordered_constraints": {
                "0": ["Move to payment v3", "Add an Idempotency-Key header to every POST call"],
                "1": ["Run the refund flow in the sandbox", "Roll out to production"],
            },
            "verification": "sandbox refund succeeds with idempotent retries",
        },
        "keyword_stems": ["idempotency"],
    },
]

# Unrelated contexts for H7b — none contains any scenario trigger word
# (deploy/migrate/upgrade) or a plausible paraphrase of them.
UNRELATED_CONTEXTS = [
    "write a haiku about the ocean",
    "summarize this meeting transcript for me",
    "plan a team lunch for eight people",
    "explain how coffee is roasted",
    "draft a friendly reminder about the office plants",
    "what books should I read about ancient history",
    "help me name my new cat",
    "write release notes jokes for april fools",
    "compose a lullaby for my nephew",
    "compare trains and planes for short trips",
    "give me a recipe for tomato soup",
    "outline a podcast episode about lighthouses",
    "translate this greeting into french",
    "suggest gifts for a woodworker",
    "write a poem about the first snowfall",
    "brainstorm names for a bakery",
    "describe how sourdough starter works",
    "plan a picnic menu for saturday",
    "recommend hiking trails near a lake",
    "write a thank you note to a mentor",
]


# ─── MemVault server management ───────────────────────────


def find_binary(explicit: str | None) -> Path:
    if explicit:
        return Path(explicit)
    repo_root = Path(__file__).resolve().parents[2]
    for rel in ["target/debug/memvault-mcp", "target/release/memvault-mcp"]:
        p = repo_root / rel
        if p.exists():
            return p
    raise SystemExit(
        "memvault-mcp binary not found — build it (cargo build -p memvault-mcp) "
        "or pass --binary"
    )


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class MemVaultServer:
    def __init__(self, binary: Path, *, evidence: dict | None = None,
                 readiness_timeout: float = 30.0):
        self._positive(readiness_timeout, "readiness_timeout")
        if evidence is not None and not isinstance(evidence, dict):
            raise ValueError("evidence must be a dictionary")
        self.evidence = evidence if evidence is not None else {}
        self.records = self.evidence.setdefault("records", [])
        if not isinstance(self.records, list):
            raise ValueError("evidence records must be a list")
        self.proc = None
        self.dbdir = None
        self._owned_tempdir = None
        self._tempdir_identity = None
        self._process_reaped = False
        startup = self.evidence["startup"] = {"status": "starting", "error": None, "records": []}
        self.evidence["cleanup"] = {"process_reaped": False, "tempdir_removed": False, "errors": []}
        try:
            self.port = free_port()
            self.base = f"http://127.0.0.1:{self.port}"
            startup.update({"port": self.port, "base": self.base})
            self.dbdir = tempfile.mkdtemp(prefix="memvault-h7-")
            self._owned_tempdir = Path(self.dbdir)
            startup["tempdir"] = sanitize(self.dbdir)
            owned_stat = self._owned_tempdir.lstat()
            self._tempdir_identity = (owned_stat.st_dev, owned_stat.st_ino)
            requested = beam.server_environment()
            requested["MEMVAULT_EMBEDDING_PROVIDER"] = "off"
            env, self.env_file, db = beam.isolate_server_environment(self.dbdir, "h7.db", requested)
            startup.update({"env_file": sanitize(str(self.env_file)), "db": sanitize(str(db))})
            self.proc = subprocess.Popen(
                [str(binary), "--env-file", str(self.env_file), "--db", str(db),
                 "--transport", "http", "--port", str(self.port)],
                env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
            startup["pid"] = self.proc.pid
            deadline = time.monotonic() + readiness_timeout
            while True:
                if self.proc.poll() is not None:
                    raise RuntimeError("memvault-mcp exited during startup")
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise RuntimeError("memvault-mcp did not become healthy in time")
                try:
                    self._request("GET", "/health", None, min(2.0, remaining), health=True)
                except Exception:
                    # The health record retains each failure, even if a later poll succeeds.
                    pass
                else:
                    if self.proc.poll() is not None:
                        raise RuntimeError("memvault-mcp exited during startup")
                    if time.monotonic() >= deadline:
                        raise RuntimeError("memvault-mcp did not become healthy in time")
                    startup["status"] = "ready"
                    return
                remaining = deadline - time.monotonic()
                if remaining > 0:
                    time.sleep(min(0.5, remaining))
        except BaseException as exc:
            message = str(exc) or type(exc).__name__
            startup.update({"status": "failed", "error": sanitize(message)})
            try:
                self.stop()
            except BaseException as cleanup_error:
                self.evidence["cleanup"]["errors"].append(sanitize(
                    str(cleanup_error) or type(cleanup_error).__name__))
            if isinstance(exc, (KeyboardInterrupt, SystemExit)):
                exc.startup_evidence = self.evidence
                raise
            error = RuntimeError(f"memvault-mcp startup failed: {sanitize(message)}")
            error.startup_evidence = self.evidence
            raise error from exc

    @staticmethod
    def _positive(value, name):
        if (isinstance(value, bool) or not isinstance(value, (int, float))
                or not math.isfinite(value) or value <= 0):
            raise ValueError(f"{name} must be positive and finite")

    def stop(self) -> dict:
        """Reap before deleting only this instance's tempdir; failed steps can be retried."""
        cleanup = self.evidence["cleanup"]

        def record_error(stage, exc):
            cleanup["errors"].append(sanitize(f"{stage}: {exc}"))

        if not self._process_reaped:
            if self.proc is None:
                self._process_reaped = True
            else:
                try:
                    if self.proc.poll() is None:
                        try:
                            self.proc.terminate()
                        except Exception as exc:
                            record_error("terminate", exc)
                    try:
                        self.proc.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        try:
                            self.proc.kill()
                        except Exception as exc:
                            record_error("kill", exc)
                        self.proc.wait(timeout=5)
                    self._process_reaped = True
                except Exception as exc:
                    record_error("reap", exc)
        cleanup["process_reaped"] = self._process_reaped
        if self._process_reaped and self._owned_tempdir is not None:
            try:
                try:
                    owned_stat = self._owned_tempdir.lstat()
                except FileNotFoundError:
                    owned_stat = None
                if owned_stat is not None:
                    identity = (owned_stat.st_dev, owned_stat.st_ino)
                    if self._owned_tempdir.is_symlink() or identity != self._tempdir_identity:
                        raise OSError("owned tempdir was replaced; refusing to delete it")
                    shutil.rmtree(self._owned_tempdir)
                self._owned_tempdir = None
            except Exception as exc:
                record_error("tempdir removal", exc)
        cleanup["tempdir_removed"] = self._owned_tempdir is None
        return cleanup

    def _request(self, method, path, body, timeout, *, health=False):
        record = {"request": {"method": sanitize(method), "path": sanitize(path), "body": sanitize(body)},
                  "status": None, "response_raw": None, "response": None, "error": None}
        self.records.append(record)
        if health:
            self.evidence["startup"]["records"].append(record)
        try:
            req = urllib.request.Request(
                f"{self.base}{path}",
                data=json.dumps(body).encode() if body is not None else None,
                headers={"Content-Type": "application/json"}, method=method,
            )
            with urllib.request.urlopen(req, timeout=timeout) as response:
                record["status"] = response.status
                raw = response.read().decode("utf-8", errors="replace")
                record["response_raw"] = sanitize(raw)
                if health:
                    if response.status != 200 or raw.strip() != "ok":
                        raise RuntimeError("health response must be HTTP 200 with plain 'ok'")
                    return
                parsed = json.loads(raw)
                record["response"] = sanitize(parsed)
                if not 200 <= response.status < 300:
                    raise RuntimeError("memvault REST request returned a non-success HTTP status")
                if (not isinstance(parsed, dict) or parsed.get("ok") is not True
                        or "error" not in parsed or parsed["error"] is not None
                        or not isinstance(parsed.get("data"), dict)):
                    raise RuntimeError("memvault REST response requires ok=true, error=null, and object data")
                return parsed
        except urllib.error.HTTPError as exc:
            record["status"] = exc.code
            try:
                raw = exc.read().decode("utf-8", errors="replace")
                record["response_raw"] = sanitize(raw)
                try:
                    record["response"] = sanitize(json.loads(raw))
                except (ValueError, TypeError):
                    pass
            except Exception as read_error:
                record["response_read_error"] = sanitize(str(read_error))
            record["error"] = sanitize(str(exc))
            raise
        except BaseException as exc:
            record["error"] = sanitize(str(exc) or type(exc).__name__)
            raise

    def _req(self, method: str, path: str, body=None, timeout=30):
        self._positive(timeout, "timeout")
        return self._request(method, path, body, timeout)

    def _response_error(self, message):
        self.records[-1]["error"] = sanitize(message)
        return RuntimeError(message)

    def _created_id(self, response) -> str:
        memory_id = response["data"].get("id")
        if not isinstance(memory_id, str) or not memory_id.strip():
            raise self._response_error("memory creation response requires a nonempty string id")
        return memory_id

    def create_skill(self, sc: dict) -> str:
        skill = sc["skill"]
        resp = self._req(
            "POST",
            "/api/memories",
            {
                "content": skill["content"],
                "type": "skill",
                "priority": "REFERENCE",
                "agent_id": "h7",
                "human_reviewed": True,
                "ai_generated": False,
                "skill_trigger": skill["trigger"],
                "skill_steps": skill["steps"],
                "skill_verification": skill["verification"],
            },
        )
        return self._created_id(resp)

    def create_decoys(self, namespace: str, count: int):
        self._positive(count, "count")
        if not isinstance(count, int):
            raise ValueError("count must be a positive integer")
        for i in range(count):
            resp = self._req(
                "POST",
                "/api/memories",
                {
                    "content": f"project note number {i} about internal trivia",
                    "type": "fact",
                    "priority": "REFERENCE",
                    "namespace": namespace,
                    "agent_id": "h7",
                    "human_reviewed": True,
                    "ai_generated": False,
                },
            )
            self._created_id(resp)

    def session_formatted(self, context: str, project: str | None = None, *, trace=None) -> str:
        start = len(self.records)
        try:
            body = {"agent_id": "claude-desktop", "context_hint": context}
            if project:
                body["project"] = project
            data = self._req("POST", "/api/session", body)["data"]
            if data.get("skipped_channel") is not None:
                raise self._response_error("session injection was skipped for the agent's canonical channel")
            if not isinstance(data.get("formatted"), str):
                raise self._response_error("session response requires an explicit string formatted field")
            return data["formatted"]
        finally:
            if trace is not None:
                trace.extend(self.records[start:])


# ─── LLM helpers ──────────────────────────────────────────


def generate_plan(system: str, task: str, *, trace=None) -> str:
    resp = recorded_completion(
        ensure_client(), trace=trace, model=MODEL,
        messages=[
            {"role": "system", "content": system},
            {"role": "user", "content": f"{task}\nReply with your step-by-step plan only."},
        ],
        max_tokens=500,
        temperature=0.3,
    )
    text = resp.choices[0].message.content
    if trace is not None and not isinstance(text, str):
        raise ValueError("generation content must be a string")
    return text


def judge_quote(pitfall: str, plan: str, *, ordered_conditions=None, trace=None) -> dict:
    """Shared strict evidence provenance/order validation for a semantic judge."""
    return grounded_judge_quote(ensure_judge_client(), JUDGE_MODEL, pitfall, plan,
                                base_url=JUDGE_BASE_URL, ordered_conditions=ordered_conditions, trace=trace)


def judge_skill(skill: dict, plan: str, *, trace=None) -> dict:
    """Each original skill step remains one obligation, even when its evidence spans steps."""
    steps = skill["steps"]
    constraints = skill.get("ordered_constraints", {})
    valid_steps = isinstance(steps, list) and bool(steps) and isinstance(constraints, dict) and all(
        isinstance(step, str) and step.strip() for step in steps
    )
    aggregate = aggregate_judgments([judge_quote(step, plan,
        ordered_conditions=constraints.get(str(index)), trace=trace) for index, step in enumerate(steps)] if valid_steps else [])
    verification = skill.get("verification")
    return {"steps": aggregate,
            "verification": judge_quote(verification, plan, trace=trace) if isinstance(verification, str) and verification.strip() else None,
            "verification_is_execution_evidence": False}


def stem_hit(sc: dict, plan: str) -> bool:
    low = plan.lower()
    return any(stem in low for stem in sc["keyword_stems"])


# ─── Experiments ──────────────────────────────────────────


def unavailable_skill_judgment(skill: dict, reason: str) -> dict:
    def unknown(precaution):
        return {"precaution": precaution, "valid": False, "avoided": None,
                "status": "unknown", "reason": reason, "raw": None,
                "metadata": {"attempts": [], "skipped_reason": reason}}
    return {"steps": aggregate_judgments([unknown(step) for step in skill["steps"]]),
            "verification": unknown(skill["verification"]) if skill.get("verification") else None,
            "verification_is_execution_evidence": False}


def session_status(formatted, records) -> dict:
    """A missing response or skipped channel is not evidence of a clean session."""
    status = {"attempted": bool(records), "valid": False, "reason": "missing_session_evidence"}
    if not records or not isinstance(records[-1], dict):
        return status
    record = records[-1]
    code, response = record.get("status"), record.get("response")
    if record.get("error") is not None:
        status["reason"] = "session_request_error"
    elif type(code) is not int or not 200 <= code < 300 or not isinstance(response, dict):
        status["reason"] = "invalid_session_response"
    else:
        data = response.get("data")
        if (response.get("ok") is not True or "error" not in response or response["error"] is not None
                or not isinstance(data, dict) or not isinstance(formatted, str)
                or not isinstance(data.get("formatted"), str)
                or data["formatted"] != sanitize(formatted)):
            status["reason"] = "invalid_session_response"
        elif data.get("skipped_channel") is not None:
            status["reason"] = "skipped_channel"
        else:
            status.update(valid=True, reason=None)
    return status


def matched_skill_status(expected_id, formatted, records) -> dict:
    status = session_status(formatted, records)
    if not status["valid"]:
        return status
    if "[SKILL:" not in formatted:
        status.update(valid=False, reason="missing_expected_skill")
        return status
    data = records[-1]["response"]["data"]
    results = data.get("results")
    if not isinstance(expected_id, str) or not expected_id or not isinstance(results, list):
        status.update(valid=False, reason="missing_skill_match_evidence")
        return status
    matching = [hit for hit in results if isinstance(hit, dict)
                and isinstance(hit.get("memory"), dict) and hit["memory"].get("id") == expected_id]
    if len(matching) != 1:
        status.update(valid=False, reason="unexpected_skill_identity")
        return status
    hit = matching[0]
    sources = hit.get("hit_sources")
    if (hit["memory"].get("type") != "skill" or hit.get("search_mode") != "session"
            or not isinstance(sources, list) or any(not isinstance(s, str) for s in sources)
            or "match" not in sources):
        status.update(valid=False, reason="expected_skill_not_explicitly_matched")
        return status
    status.update(matched_memory_id=expected_id, hit_sources=list(sources))
    return status


def h7_pair_summary(details: list, *, judge_field=None) -> dict:
    pairs = []
    for detail in details:
        pair = {}
        for arm in ("control", "experiment"):
            generation = detail.get(f"{arm}_generation", {})
            valid = generation.get("valid") is True
            if arm == "experiment":
                valid = valid and detail.get("injection_status", {}).get("valid") is True
            hit = detail.get(f"{arm}_kw")
            if judge_field:
                judge = detail.get(f"{arm}_{judge_field}") or {}
                valid = valid and judge.get("valid") is True
                hit = judge.get("avoided")
            pair[arm] = {"generation": {**generation, "valid": valid}, "keyword_hit": hit}
        pairs.append(pair)
    summary = summarize_generation_pairs(pairs)
    if judge_field:
        summary["policy"] = "paired_valid_judgments_and_complete_generation"
    return summary


def summarize_h7a(result: dict) -> dict:
    details = result["details"]
    primary = h7_pair_summary(details)
    output = {"primary_summary": primary, "control": primary["control"],
              "experiment": primary["experiment"], "control_hits": primary["control_correct"],
              "experiment_hits": primary["experiment_correct"], "total": primary["n"],
              "secondary_summary": h7_pair_summary(details, judge_field="judge"),
              "verification_summary": h7_pair_summary(details, judge_field="verification_judge")}
    item_details = []
    for detail in details:
        for index, step in enumerate(detail["skill"]["steps"]):
            item_detail = dict(detail)
            for arm in ("control", "experiment"):
                items = detail[f"{arm}_judge"].get("items", [])
                item_detail[f"{arm}_judge"] = items[index] if index < len(items) else {
                    "valid": False, "avoided": None, "precaution": step}
            item_details.append(item_detail)
    output["secondary_items_summary"] = h7_pair_summary(item_details, judge_field="judge")
    for arm in ("control", "experiment"):
        judgments = [d[f"{arm}_judge"] for d in details]
        summary = summarize_judgments(judgments)
        output[f"judge_{arm}"] = summary["rate"]
        output[f"judge_{arm}_summary"] = summary
        output[f"judge_{arm}_items"] = summarize_judgments(
            item for judgment in judgments for item in judgment.get("items", []))
        output[f"raw_{arm}_hits"] = sum(d.get(f"{arm}_kw") is True for d in details)
    return output


def initial_h7a(rounds: int, scenarios: list) -> dict:
    if type(rounds) is not int or rounds <= 0 or not scenarios:
        raise ValueError("H7a needs positive integer rounds and nonempty scenarios")
    details = []
    for rnd in range(rounds):
        for sc in scenarios:
            detail = {"pair_id": f"h7a:{rnd}:{sc['task_type']}", "round": rnd + 1,
                      "task_type": sc["task_type"], "scenario_spec": copy.deepcopy(sc),
                      "skill": copy.deepcopy(sc["skill"]), "injection": None,
                      "injection_records": [], "injection_status": session_status(None, []),
                      "verification_is_execution_evidence": False}
            for arm in ("control", "experiment"):
                unknown = unavailable_skill_judgment(sc["skill"], "not_attempted")
                detail.update({f"{arm}_plan": None, f"{arm}_kw": None, f"{arm}_records": [],
                               f"{arm}_generation": generation_status(None, []),
                               f"{arm}_judge": unknown["steps"], f"{arm}_judge_records": [],
                               f"{arm}_verification_judge": unknown["verification"],
                               f"{arm}_system_prompt": BASE_SYSTEM if arm == "control" else None})
            details.append(detail)
    result = {"details": details, "skill_ids": {}, "requested_total": len(details)}
    result.update(summarize_h7a(result))
    return result


def run_h7a(server: MemVaultServer, rounds: int, *, result=None, scenarios=None) -> dict:
    scenarios = copy.deepcopy(SCENARIOS if scenarios is None else scenarios)
    if type(rounds) is not int or rounds <= 0 or not scenarios:
        raise ValueError("H7a needs positive integer rounds and nonempty scenarios")
    result = initial_h7a(rounds, scenarios) if result is None else result
    if (len(result["details"]) != rounds * len(scenarios) or result.get("skill_ids")
            or any(d["scenario_spec"] != scenarios[i % len(scenarios)]
                   or d["round"] != i // len(scenarios) + 1
                   or any(d[f"{arm}_generation"].get("attempted") for arm in ("control", "experiment"))
                   for i, d in enumerate(result["details"]))):
        raise ValueError("H7a result slots must be fresh and match the selected scenarios")
    print("\n═══ H7a: Skill Injection vs No Injection (first-attempt success) ═══")
    print("    primary = complete paired fact conveyance; secondary = LLM judge")
    try:
        for sc in scenarios:
            skill_id = server.create_skill(sc)
            if not isinstance(skill_id, str) or not skill_id.strip():
                raise RuntimeError("skill creation did not return a valid id")
            result["skill_ids"][sc["task_type"]] = skill_id
        for detail in result["details"]:
            sc = detail["scenario_spec"]
            print(f"  round {detail['round']}/{rounds} [{sc['task_type']}] ...", end=" ")
            try:
                detail["control_plan"] = generate_plan(BASE_SYSTEM, sc["task"], trace=detail["control_records"])
            finally:
                detail["control_generation"] = generation_status(detail["control_plan"], detail["control_records"])
            detail["control_kw"] = stem_hit(sc, detail["control_plan"])
            detail["expected_skill_id"] = result["skill_ids"][sc["task_type"]]
            try:
                detail["injection"] = server.session_formatted(sc["task"], trace=detail["injection_records"])
            finally:
                detail["injection_status"] = matched_skill_status(
                    detail["expected_skill_id"], detail["injection"], detail["injection_records"])
            if not detail["injection_status"]["valid"]:
                raise RuntimeError(f"expected a matched skill for '{sc['task_type']}': "
                                   f"{detail['injection_status']['reason']}")
            detail["experiment_system_prompt"] = BASE_SYSTEM + "\n\n" + detail["injection"]
            try:
                detail["experiment_plan"] = generate_plan(detail["experiment_system_prompt"], sc["task"],
                                                          trace=detail["experiment_records"])
            finally:
                detail["experiment_generation"] = generation_status(detail["experiment_plan"], detail["experiment_records"])
            detail["experiment_kw"] = stem_hit(sc, detail["experiment_plan"])
            # Preserve experiment-then-control judgment order and independent obligations.
            for arm in ("experiment", "control"):
                try:
                    judgment = (judge_skill(sc["skill"], detail[f"{arm}_plan"], trace=detail[f"{arm}_judge_records"])
                                if detail[f"{arm}_generation"]["valid"] else
                                unavailable_skill_judgment(sc["skill"], "incomplete_source_generation"))
                except BaseException:
                    unavailable = unavailable_skill_judgment(sc["skill"], "judgment_incomplete")
                    detail[f"{arm}_judge"] = unavailable["steps"]
                    detail[f"{arm}_verification_judge"] = unavailable["verification"]
                    raise
                detail[f"{arm}_judge"] = judgment["steps"]
                detail[f"{arm}_verification_judge"] = judgment["verification"]
            print(f"ctrl={detail['control_kw']} exp={detail['experiment_kw']}")
    finally:
        result.update(summarize_h7a(result))
    return result


def summarize_h7b(result: dict) -> dict:
    details = result["details"]
    valid = [d for d in details if d["valid"] is True and type(d.get("injected_skill")) is bool]
    unique_valid = {d["context"] for d in valid}
    unique_misfires = {d["context"] for d in valid if d["injected_skill"] is True}
    misfires = sum(d["injected_skill"] is True for d in valid)
    return {"attempted_samples": len(details), "valid_samples": len(valid),
            "invalid_samples": len(details) - len(valid),
            "unattempted_samples": result["samples"] - len(details),
            "http_attempts": sum(len(d["records"]) for d in details),
            "unique_attempted_contexts": len({d["context"] for d in details}),
            "unique_valid_contexts": len(unique_valid), "unique_misfires": len(unique_misfires),
            "unique_any_misfire_rate": len(unique_misfires) / len(unique_valid) if unique_valid else None,
            "misfires": misfires, "rate": misfires / len(valid) if valid else None,
            "examples": [d["context"] for d in valid if d["injected_skill"] is True],
            "status": "complete" if len(valid) == result["samples"] else "invalid"}


def initial_h7b(samples: int, contexts: list) -> dict:
    if type(samples) is not int or samples <= 0 or not contexts or any(not isinstance(c, str) or not c.strip() for c in contexts):
        raise ValueError("H7b needs positive samples and nonempty string contexts")
    selected = [contexts[i % len(contexts)] for i in range(samples)]
    result = {"samples": samples, "requested_samples": samples, "details": [],
              "source_contexts": copy.deepcopy(contexts), "source_sha256": beam.json_hash(contexts),
              "unique_source_contexts": len(set(selected)),
              "repeated_samples": samples - len(set(selected)),
              "measurement_scope": "observed trigger path on predeclared contexts",
              "population_rate_bound": None}
    result.update(summarize_h7b(result))
    return result


def run_h7b(server: MemVaultServer, samples: int, *, result=None, contexts=None) -> dict:
    contexts = copy.deepcopy(UNRELATED_CONTEXTS if contexts is None else contexts)
    result = initial_h7b(samples, contexts) if result is None else result
    if (type(samples) is not int or samples <= 0 or not contexts
            or any(not isinstance(c, str) or not c.strip() for c in contexts)):
        raise ValueError("H7b needs positive integer samples and nonempty string contexts")
    if result["samples"] != samples or result["source_contexts"] != contexts or result["details"]:
        raise ValueError("H7b result slots must be fresh and match the selected contexts")
    print("\n═══ H7b: Trigger Mis-Injection Rate (observed target < 5%) ═══")
    print(f"    {samples} requests, {result['unique_source_contexts']} unique contexts; "
          "repetitions are not independent contexts")
    occurrences = {}
    try:
        # Preserve decoys and namespaces; no global-fallback shortcut is measured.
        server.create_decoys("project:h7lab", 10)
        for i in range(samples):
            ctx = contexts[i % len(contexts)]
            occurrences[ctx] = occurrences.get(ctx, 0) + 1
            detail = {"request_index": i + 1, "source_context_index": i % len(contexts),
                      "source_context_id": beam.json_hash(ctx), "repeat_index": occurrences[ctx],
                      "context": ctx, "formatted": None, "records": [], "valid": False,
                      "injected_skill": None, "error": None}
            result["details"].append(detail)
            try:
                detail["formatted"] = server.session_formatted(ctx, project="h7lab", trace=detail["records"])
            except BaseException as exc:
                detail["error"] = sanitize(str(exc) or type(exc).__name__)
                raise
            finally:
                detail["session_status"] = session_status(detail["formatted"], detail["records"])
            if not detail["session_status"]["valid"]:
                detail["error"] = detail["session_status"]["reason"]
                raise RuntimeError("H7b session lacks valid response evidence")
            detail["injected_skill"] = "[SKILL:" in detail["formatted"]
            detail["valid"] = True
            print(f"  {i+1}/{samples}: '{ctx[:38]}' -> "
                  f"{'SKILL INJECTED ✗' if detail['injected_skill'] else 'clean ✓'}")
    finally:
        result.update(summarize_h7b(result))
    return result


def h7_harness_spec(args, binary, scenarios: list, contexts: list) -> dict:
    path = Path(__file__).resolve()
    source = {"source": "inline_pre_registered_scenarios_and_contexts", "source_path": str(path),
              "source_sha256": beam.content_hash(path),
              "selection_sha256": beam.json_hash({"scenarios": scenarios, "contexts": contexts})}
    return sanitize({
        "schema_version": 2, "benchmark": "H7", "mechanism": "real_session_skill_injection",
        "evidence": beam.execution_identity(path, binary, source),
        "dependency_sha256": {name: beam.content_hash(Path(grounding.__file__).parent / name)
                              for name in ("judge_grounding.py", "benchmark_evidence.py")},
        "selection": {"rounds": args.rounds, "misfire_samples": args.misfire_samples,
                      "scenarios": scenarios, "contexts": contexts,
                      "order": "scenario order; original context cycle; no backfill"},
        "models": {"answer_model": MODEL, "answer_endpoint": grounding.endpoint_record(BASE_URL),
                   "answer_max_tokens": 500, "answer_temperature": 0.3,
                   "judge_model": JUDGE_MODEL, "judge_endpoint": grounding.endpoint_record(JUDGE_BASE_URL),
                   "judge_max_tokens": grounding.JUDGE_MAX_TOKENS,
                   "judge_retry_max_tokens": grounding.JUDGE_RETRY_MAX_TOKENS,
                   "judge_temperature": 0.0},
        "prompts": {"base_system": BASE_SYSTEM,
                    "judge_prompt_version": grounding.JUDGE_PROMPT_VERSION},
        "server_policy": {"provider": "off", "storage": "sqlite", "isolated_home": True,
                          "explicit_empty_env_file": True},
        "scoring": {"primary": "paired_complete_generation_stem_hits",
                    "secondary": "grounded judgments, unknown separate; not execution evidence",
                    "misinjection": "valid request-weighted observed trigger rate; not population proof"},
        "argv": list(args.invocation_argv),
    })


def h7_artifact(h7a: dict, h7b: dict, spec: dict, server_evidence: dict, run_error=None) -> dict:
    h7a.update(summarize_h7a(h7a))
    h7b.update(summarize_h7b(h7b))
    primary = h7a["primary_summary"]
    errors = ["execution_error"] if run_error else []
    if primary["n"] != primary["n_requested"]:
        errors.append("invalid_generation_pairs")
    if not primary["n"]:
        errors.append("no_valid_generation_pairs")
    if h7b["status"] != "complete":
        errors.append("invalid_h7b_sessions")
    cleanup = server_evidence.get("cleanup", {})
    if (cleanup.get("process_reaped") is not True or cleanup.get("tempdir_removed") is not True
            or cleanup.get("errors")):
        errors.append("cleanup_failed")
    return sanitize({"schema_version": 2, "harness_spec": spec, "run_error": run_error,
                     "server_evidence": server_evidence, "h7a": h7a, "h7b": h7b,
                     "run_status": {"schema_version": 1,
                                    "status": "invalid" if errors else "scoring_complete",
                                    "errors": errors, "n_selected_records": primary["n_requested"],
                                    "n_valid_scoring_pairs": primary["n"]}})


def print_h7_report(artifact: dict):
    h7a, h7b = artifact["h7a"], artifact["h7b"]
    primary = h7a["primary_summary"]
    valid_run = artifact["run_status"]["status"] == "scoring_complete"
    print("\n" + "=" * 60 + "\nRESULTS\n" + "=" * 60)
    print(f"H7a complete generation pairs: {primary['n']}/{primary['n_requested']}")
    for arm in ("control", "experiment"):
        rate = f"{primary[arm]:.0%}" if primary[arm] is not None else "null (no valid pair)"
        print(f"    {arm} knowledge conveyance: {rate}")
        summary = h7a[f"judge_{arm}_summary"]
        judge_rate = f"{summary['rate']:.0%}" if summary["rate"] is not None else "null (no valid judge)"
        print(f"    [secondary] LLM judge {arm}: {judge_rate}; "
              f"valid={summary['valid']}/{summary['total']}, unknown={summary['unknown']}")
    secondary = h7a["secondary_summary"]
    print(f"    [secondary paired] valid={secondary['n']}/{secondary['n_requested']}")
    if valid_run:
        diff = primary["delta"]
        print(f"    Knowledge-conveyance delta: {diff:+.0%}")
        print(f"    VERDICT: {'CONFIRMED ✓' if diff > 0.1 else 'INCONCLUSIVE' if diff > 0 else 'REJECTED ✗'}")
    else:
        print("    VERDICT: INVALID (generation, execution, session or cleanup)")
    rate = f"{h7b['rate']:.1%}" if h7b["rate"] is not None else "null (no valid session)"
    print(f"\nH7b observed mis-injection: {h7b['misfires']}/{h7b['valid_samples']} = {rate}; "
          f"requested={h7b['samples']}, unique={h7b['unique_source_contexts']}, "
          f"repeated={h7b['repeated_samples']}")
    if valid_run:
        print(f"    Observed threshold: {'PASS (<5%)' if h7b['rate'] < 0.05 else 'FAILED (>=5%)'}; "
              "not a population-rate bound")
    else:
        print("    VERDICT: INVALID; partial observations are diagnostic only")


def main(argv=None):
    parser = argparse.ArgumentParser(description="H7 procedural-memory acceptance")
    parser.add_argument("--rounds", type=int, default=3, help="H7a rounds over scenarios")
    parser.add_argument("--misfire-samples", type=int, default=40, help="H7b unrelated contexts")
    parser.add_argument("--binary", help="path to memvault-mcp binary")
    parser.add_argument("--out", type=Path, help="evidence JSON (default: h7_results.json)")
    args = parser.parse_args(argv)
    args.invocation_argv = list(sys.argv[1:] if argv is None else argv)
    if args.rounds <= 0 or args.misfire_samples <= 0:
        parser.error("--rounds and --misfire-samples must be positive")
    scenarios, contexts = copy.deepcopy(SCENARIOS), copy.deepcopy(UNRELATED_CONTEXTS)
    h7a, h7b = initial_h7a(args.rounds, scenarios), initial_h7b(args.misfire_samples, contexts)
    out = args.out if args.out is not None else Path(__file__).parent / "h7_results.json"
    server, spec, run_error = None, {}, None
    server_evidence = {"cleanup": {"process_reaped": True, "tempdir_removed": True,
                                   "errors": [], "not_started": True}}
    phase, exit_code = "configuration", 0
    try:
        spec = h7_harness_spec(args, Path(args.binary) if args.binary else None, scenarios, contexts)
        if not os.getenv("OPENAI_API_KEY") and BASE_URL is None:
            raise ValueError("set OPENAI_API_KEY or VERIFY_BASE_URL (local endpoint)")
        spec["models"].update(grounding.configure_judge())
        ensure_client()
        ensure_judge_client()
        binary = find_binary(args.binary)
        resolved_binary = str(binary.resolve())
        if (spec["evidence"]["binary_path"] != resolved_binary
                or spec["evidence"]["binary_sha256"] is None):
            spec["evidence"].update(binary_path=resolved_binary, binary_sha256=beam.content_hash(binary))
        print("MemVault H7 — procedural memory acceptance")
        print(sanitize(f"Agent model: {MODEL} @ {BASE_URL or 'OpenAI'}"))
        print(sanitize(f"Judge model: {JUDGE_MODEL} @ {JUDGE_BASE_URL or 'OpenAI'}"))
        print(sanitize(f"MemVault binary: {binary}"))
        phase = "startup"
        server = MemVaultServer(binary, evidence=server_evidence)
        print(f"MemVault server: {server.base} (isolated temp db)")
        phase = "H7a"
        run_h7a(server, args.rounds, result=h7a, scenarios=scenarios)
        phase = "H7b"
        run_h7b(server, args.misfire_samples, result=h7b, contexts=contexts)
    except KeyboardInterrupt:
        run_error = {"phase": phase, "type": "KeyboardInterrupt", "message": "interrupted"}
        exit_code = 130
    except (Exception, SystemExit) as exc:
        run_error = sanitize({"phase": phase, "type": type(exc).__name__, "message": str(exc)})
        exit_code = 1
    finally:
        if server is not None:
            try:
                server.stop()
            except BaseException as exc:
                message = str(exc) or type(exc).__name__
                server_evidence.setdefault("cleanup", {}).setdefault("errors", []).append(sanitize(message))
                if run_error is None:
                    run_error = sanitize({"phase": "cleanup", "type": type(exc).__name__, "message": message})
                    exit_code = 130 if isinstance(exc, KeyboardInterrupt) else 1
        artifact = h7_artifact(h7a, h7b, spec, server_evidence, run_error)
        try:
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_text(json.dumps(artifact, ensure_ascii=False, indent=1), encoding="utf-8")
            print(sanitize(f"Details written to {out}"))
        except Exception as exc:
            artifact["run_status"]["status"] = "invalid"
            artifact["run_status"]["errors"].append("artifact_write_failed")
            if run_error is None:
                run_error = sanitize({"phase": "write", "type": type(exc).__name__, "message": str(exc)})
                artifact["run_error"] = run_error
            print(sanitize(f"ERROR: cannot write H7 evidence to {out}: {exc}"), file=sys.stderr)
            if not exit_code:
                exit_code = 1
        if artifact["run_status"]["status"] != "scoring_complete" and not exit_code:
            exit_code = 1
    if run_error:
        print(sanitize(f"ERROR: {run_error['phase']}: {run_error['message']}"), file=sys.stderr)
    print_h7_report(artifact)
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
