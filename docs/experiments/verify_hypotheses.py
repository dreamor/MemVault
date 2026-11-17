#!/usr/bin/env python3
"""
MemVault Hypothesis Verification Experiments
=============================================

Validates key design hypotheses:
  H1: Pre-Prompt Injection improves compliance rate
  H2: Instructive format ([MUST]) > descriptive format
  H3: Optimal Token Budget is ~1500 (not too low, not too high)
  H4: Router mis-injection rate < 5%
  H5: Lesson injection reduces repeat-failure rate on similar tasks
      (episodic memory acceptance — three-memory plan Phase A; the shipped
      design now lives in docs/DESIGN.md)

Requirements:
  - OpenAI-compatible chat endpoint:
    - remote: OPENAI_API_KEY env var (uses gpt-4o-mini for cost-efficiency)
    - local:  VERIFY_BASE_URL (e.g. http://localhost:1234/v1 for LM Studio or
              http://localhost:11434/v1 for Ollama) + VERIFY_MODEL
  - Python 3.10+
  - pip install openai

Usage:
  python docs/experiments/verify_hypotheses.py [--hypothesis H1|H2|H3|H4|H5|all] [--samples N]
"""

import argparse
import copy
import json
import os
import re
import sys
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Optional

import judge_grounding as grounding
from benchmark_evidence import content_hash, execution_identity, json_hash
from judge_grounding import (
    aggregate_judgments, generation_status, judge_quote as grounded_judge_quote,
    recorded_completion, sanitize, summarize_generation_pairs, summarize_judgments,
)

try:
    from openai import OpenAI
except ImportError:
    print("ERROR: pip install openai")
    sys.exit(1)


# ─── Config ────────────────────────────────────────────────

MODEL = os.getenv("VERIFY_MODEL", "gpt-4o-mini")
# Local-first, same as the product: VERIFY_BASE_URL points at any
# OpenAI-compatible endpoint (LM Studio :1234/v1, Ollama :11434/v1, ...).
BASE_URL = os.getenv("VERIFY_BASE_URL") or os.getenv("OPENAI_BASE_URL") or None
client = None

# H5 splits the roles: a (possibly weak) AGENT generates the task plan, and a
# JUDGE evaluates whether the plan avoids the pitfall. Using a stronger judge
# than the agent is the standard LLM-as-judge setup and avoids the ceiling
# effect where a capable agent avoids the pitfall even without the lesson.
# Defaults fall back to the main model/endpoint when unset.
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


@dataclass
class ExperimentResult:
    hypothesis: str
    description: str
    samples: int
    compliant: int = 0
    violated: int = 0
    details: list = field(default_factory=list)

    @property
    def compliance_rate(self) -> float:
        if self.samples == 0:
            return 0.0
        return self.compliant / self.samples

    def summary(self) -> str:
        return (
            f"[{self.hypothesis}] {self.description}\n"
            f"  Samples: {self.samples}, Compliant: {self.compliant}, "
            f"Violated: {self.violated}\n"
            f"  Compliance Rate: {self.compliance_rate:.1%}\n"
        )


# ─── Test Scenarios ────────────────────────────────────────

MEMORIES = [
    {"content": "The user prefers Python, not Java", "priority": "MUST", "type": "preference"},
    {"content": "No code comments; functions stay under 10 lines", "priority": "MUST", "type": "preference"},
    {"content": "Use the FastAPI framework", "priority": "MUST", "type": "preference"},
    {"content": "Use snake_case for variable names", "priority": "MUST", "type": "preference"},
    {"content": "The project uses PostgreSQL", "priority": "REFERENCE", "type": "fact"},
]

USER_PROMPTS = [
    "Help me write a REST API endpoint that implements user login",
    "Write a function that queries the user list from the database and returns paginated results",
    "Implement a simple cache decorator",
    "Write a utility function that sends email",
    "Implement a file-upload API endpoint",
]

# Unrelated memories for mis-injection test
UNRELATED_MEMORIES = [
    {"content": "The user likes classical music", "priority": "REFERENCE", "type": "preference", "tags": ["music"]},
    {"content": "The user's cat is named Xiaohua", "priority": "REFERENCE", "type": "fact", "tags": ["personal"]},
    {"content": "The user likes running on weekends", "priority": "REFERENCE", "type": "fact", "tags": ["sports"]},
    {"content": "The user enjoys reading science-fiction novels", "priority": "REFERENCE", "type": "preference", "tags": ["reading"]},
    {"content": "The user's birthday is March 15", "priority": "REFERENCE", "type": "fact", "tags": ["personal"]},
]


# ─── LLM Helpers ───────────────────────────────────────────

def generate_response(system_prompt: str, user_prompt: str, *, trace=None) -> str:
    """Keep legacy text callers while optionally retaining generation evidence."""
    resp = recorded_completion(
        ensure_client(), trace=trace, model=MODEL,
        messages=[
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_prompt},
        ],
        max_tokens=800,
        temperature=0.3,
    )
    text = resp.choices[0].message.content
    if trace is not None and not isinstance(text, str):
        raise ValueError("generation content must be a string")
    return text


def judge_compliance(memory_rules: list[str], response: str) -> dict:
    """Use LLM-as-judge to check if response follows memory rules."""
    rules_text = "\n".join(f"- {r}" for r in memory_rules)
    judge_prompt = f"""You are a strict compliance judge. Given these rules and an AI response, 
determine if the response FOLLOWS or VIOLATES each rule.

RULES:
{rules_text}

AI RESPONSE:
{response}

For each rule, output a JSON object with:
- "rule": the rule text (abbreviated)
- "status": "followed" or "violated" or "not_applicable"
- "evidence": brief explanation

Output a JSON array. Only output valid JSON, no markdown."""

    resp = ensure_client().chat.completions.create(
        model=MODEL,
        messages=[{"role": "user", "content": judge_prompt}],
        max_tokens=500,
        temperature=0.0,
    )
    text = resp.choices[0].message.content.strip()
    # Clean markdown code fences if present
    if text.startswith("```"):
        text = text.split("\n", 1)[1].rsplit("```", 1)[0]
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return [{"rule": "parse_error", "status": "unknown", "evidence": text[:100]}]


def judge_relevance(memory: dict, context: str) -> bool:
    """Judge if a memory is relevant to a coding context."""
    prompt = f"""Is this memory relevant to a coding assistant helping with: "{context}"?

Memory: "{memory['content']}" (tags: {memory.get('tags', [])})

Answer only "yes" or "no"."""

    resp = ensure_client().chat.completions.create(
        model=MODEL,
        messages=[{"role": "user", "content": prompt}],
        max_tokens=5,
        temperature=0.0,
    )
    return "yes" in resp.choices[0].message.content.lower()


# ─── Experiments ───────────────────────────────────────────

def run_h1(samples: int) -> tuple[ExperimentResult, ExperimentResult]:
    """H1: Pre-Prompt Injection improves compliance rate."""
    print("\n═══ H1: Pre-Prompt Injection vs No Injection ═══")

    base_system = "You are a coding assistant. Write clean, concise code."
    injected_system = (
        "You are a coding assistant. Write clean, concise code.\n\n"
        "[MEMORY CONTEXT - MUST FOLLOW]:\n"
        "[MUST] Write the code in Python, not Java\n"
        "[MUST] No code comments; functions stay under 10 lines\n"
        "[MUST] Use the FastAPI framework\n"
        "[MUST] Use snake_case for variable names\n"
        "[REF] The project uses PostgreSQL\n"
    )

    rules = [m["content"] for m in MEMORIES if m["priority"] == "MUST"]

    control = ExperimentResult("H1-control", "No injection", samples)
    experiment = ExperimentResult("H1-experiment", "With injection", samples)

    for i in range(samples):
        prompt = USER_PROMPTS[i % len(USER_PROMPTS)]
        print(f"  Sample {i+1}/{samples}: {prompt[:30]}...", end=" ")

        # Control: no injection
        resp_ctrl = generate_response(base_system, prompt)
        judgments_ctrl = judge_compliance(rules, resp_ctrl)
        violated_ctrl = any(
            j.get("status") == "violated" for j in judgments_ctrl if isinstance(j, dict)
        )
        if violated_ctrl:
            control.violated += 1
        else:
            control.compliant += 1

        # Experiment: with injection
        resp_exp = generate_response(injected_system, prompt)
        judgments_exp = judge_compliance(rules, resp_exp)
        violated_exp = any(
            j.get("status") == "violated" for j in judgments_exp if isinstance(j, dict)
        )
        if violated_exp:
            experiment.violated += 1
        else:
            experiment.compliant += 1

        print(f"ctrl={'✗' if violated_ctrl else '✓'} exp={'✗' if violated_exp else '✓'}")

    return control, experiment


def run_h2(samples: int) -> tuple[ExperimentResult, ExperimentResult]:
    """H2: Instructive format ([MUST]) vs descriptive format."""
    print("\n═══ H2: Instructive [MUST] Format vs Descriptive Format ═══")

    descriptive_system = (
        "You are a coding assistant.\n\n"
        "Context about the user:\n"
        "The user prefers Python over Java for coding.\n"
        "The user likes code without comments and short functions under 10 lines.\n"
        "The user uses FastAPI framework.\n"
        "The user prefers snake_case naming.\n"
    )
    instructive_system = (
        "You are a coding assistant.\n\n"
        "[MEMORY CONTEXT - MUST FOLLOW]:\n"
        "[MUST] Write the code in Python, not Java\n"
        "[MUST] No code comments; functions stay under 10 lines\n"
        "[MUST] Use the FastAPI framework\n"
        "[MUST] Use snake_case for variable names\n"
    )

    rules = [m["content"] for m in MEMORIES if m["priority"] == "MUST"]

    descriptive = ExperimentResult("H2-descriptive", "Descriptive format", samples)
    instructive = ExperimentResult("H2-instructive", "Instructive [MUST] format", samples)

    for i in range(samples):
        prompt = USER_PROMPTS[i % len(USER_PROMPTS)]
        print(f"  Sample {i+1}/{samples}: {prompt[:30]}...", end=" ")

        resp_desc = generate_response(descriptive_system, prompt)
        judgments_desc = judge_compliance(rules, resp_desc)
        violated_desc = any(
            j.get("status") == "violated" for j in judgments_desc if isinstance(j, dict)
        )
        if violated_desc:
            descriptive.violated += 1
        else:
            descriptive.compliant += 1

        resp_inst = generate_response(instructive_system, prompt)
        judgments_inst = judge_compliance(rules, resp_inst)
        violated_inst = any(
            j.get("status") == "violated" for j in judgments_inst if isinstance(j, dict)
        )
        if violated_inst:
            instructive.violated += 1
        else:
            instructive.compliant += 1

        print(f"desc={'✗' if violated_desc else '✓'} inst={'✗' if violated_inst else '✓'}")

    return descriptive, instructive


def run_h3(samples: int) -> list[ExperimentResult]:
    """H3: Token Budget optimal value test."""
    print("\n═══ H3: Token Budget Threshold Test ═══")

    rules = [m["content"] for m in MEMORIES if m["priority"] == "MUST"]

    # Simulate different budget levels by varying how many memories are injected
    budgets = [
        ("500 tokens (~2 memories)", 2),
        ("1000 tokens (~4 memories)", 4),
        ("1500 tokens (~5 memories)", 5),
        ("2000 tokens (~8 memories)", 8),
    ]

    all_memories_text = [
        "[MUST] Write the code in Python, not Java",
        "[MUST] No code comments; functions stay under 10 lines",
        "[MUST] Use the FastAPI framework",
        "[MUST] Use snake_case for variable names",
        "[REF] The project uses PostgreSQL",
        "[REF] The user prefers concise error handling",
        "[REF] Use pydantic for data validation",
        "[REF] Use httpx for HTTP requests",
    ]

    results = []

    for budget_label, mem_count in budgets:
        result = ExperimentResult("H3", f"Budget: {budget_label}", samples)
        injected = "\n".join(all_memories_text[:mem_count])
        system = (
            f"You are a coding assistant.\n\n"
            f"[MEMORY CONTEXT - MUST FOLLOW]:\n{injected}\n"
        )

        for i in range(samples):
            prompt = USER_PROMPTS[i % len(USER_PROMPTS)]
            resp = generate_response(system, prompt)
            judgments = judge_compliance(rules, resp)
            violated = any(
                j.get("status") == "violated" for j in judgments if isinstance(j, dict)
            )
            if violated:
                result.violated += 1
            else:
                result.compliant += 1

        print(f"  {budget_label}: {result.compliance_rate:.0%} compliance")
        results.append(result)

    return results


def run_h4(samples: int) -> ExperimentResult:
    """H4: Router mis-injection rate < 5%."""
    print("\n═══ H4: Router Mis-Injection Rate ═══")

    result = ExperimentResult("H4", "Mis-injection rate (should be <5%)", samples)

    coding_contexts = [
        "Help me write a REST API",
        "Implement a database query feature",
        "Write a file-processing function",
        "Implement JWT authentication middleware",
        "Write a data-validation module",
    ]

    for i in range(samples):
        memory = UNRELATED_MEMORIES[i % len(UNRELATED_MEMORIES)]
        context = coding_contexts[i % len(coding_contexts)]
        print(f"  Sample {i+1}/{samples}: '{memory['content'][:20]}...' vs '{context[:20]}...'", end=" ")

        is_relevant = judge_relevance(memory, context)
        if is_relevant:
            result.violated += 1  # mis-injection: irrelevant memory judged relevant
            print("✗ (mis-injection)")
        else:
            result.compliant += 1  # correctly filtered
            print("✓ (filtered)")

    return result


# ─── H5: Lesson injection reduces repeat-failure rate ──────
#
# Acceptance experiment for the episodic memory loop
# (three-memory plan Phase A, hypothesis H5; the shipped design now lives
# in docs/DESIGN.md).
#
# Each scenario is a task type with a hidden pitfall that naive plans miss.
# The lesson text uses the EXACT format MemVault produces end-to-end:
#   - reflection.rs rule path:  "Before '<task_type>' tasks, verify: <cause>"
#   - session_start injection:  "[REF] When working on '<task_type>' tasks: <lesson>"

# Scenario design (H5 calibration, 2026-08-26): pitfalls are deliberately
# NON-OBVIOUS, project-specific facts (a magic env var, an unannounced header,
# a policy commitment). Generic pitfalls ("take a backup", "check env vars")
# are common sense — agents avoid them even without the lesson, which
# ceilings the control group and hides the effect. Real episodic lessons
# carry exactly this kind of knowledge an agent cannot guess. The unique
# proper noun in each cause makes the secondary keyword signal objective.

H5_SCENARIOS = [
    {
        "task_type": "deploy",
        "task": "Deploy our web dashboard to the production server and confirm it is live.",
        "precautions": ["pointing the DASHBOARD_CDN environment variable at the new CDN domain before deploying"],
        "ordered_constraints": {"0": ["Point DASHBOARD_CDN at the new CDN domain", "Deploy the dashboard"]},
        "cause": "the DASHBOARD_CDN env var must point at the new CDN domain",
        # Pre-registered primary keyword signal; not tuned after results.
        "keyword_stems": ["dashboard_cdn"],
    },
    {
        "task_type": "migrate",
        "task": "Migrate the users table to the new schema with the added profile columns.",
        "precautions": ["backfilling users.profile_json before the NOT NULL constraint is added"],
        "ordered_constraints": {"0": ["Backfill users.profile_json", "Add the NOT NULL constraint"]},
        "cause": "users.profile_json must be backfilled before the NOT NULL constraint is added",
        "keyword_stems": ["profile_json"],
    },
    {
        "task_type": "upgrade",
        "task": "Upgrade the payment SDK from v2 to v3 in our checkout service.",
        "precautions": ["adding the Idempotency-Key header that payment v3 requires on every POST"],
        "cause": "payment v3 requires an Idempotency-Key header on every POST",
        "keyword_stems": ["idempotency"],
    },
    {
        "task_type": "refactor",
        "task": "Refactor the authentication module to split session handling from login.",
        "precautions": ["keeping legacy session cookies readable behind the AUTH_SPLIT feature flag"],
        "cause": "legacy session cookies must stay readable behind the AUTH_SPLIT feature flag",
        "keyword_stems": ["auth_split"],
    },
    {
        "task_type": "release",
        "task": "Release v2 of our public API to external partners.",
        "precautions": ["keeping legacy error codes on the v1 /billing endpoint for two more quarters"],
        "cause": "the v1 /billing endpoint must keep returning legacy error codes for two quarters",
        "keyword_stems": ["legacy error code", "/billing"],
    },
]

H5_BASE_SYSTEM = (
    "You are a careful engineering assistant. When asked to perform a task, "
    "respond ONLY with a concise numbered step-by-step plan."
)


def h5_injection_block(scenario: dict) -> str:
    """The injection in the same shape as MemVault's session_start output:
    [MUST]/[REF]-tagged lines under a MEMORY CONTEXT header. (The exact
    header wording produced by router/format.rs may differ across versions.)
    """
    lesson = f"Before '{scenario['task_type']}' tasks, verify: {scenario['cause']}"
    return (
        "[MEMORY CONTEXT - MUST FOLLOW]:\n"
        f"[REF] When working on '{scenario['task_type']}' tasks: {lesson}\n"
    )


def judge_pitfall_avoided(pitfall: str, plan: str, *, ordered_conditions=None, trace=None) -> dict:
    """Secondary semantic judgment with grounded evidence, not actual execution."""
    return grounded_judge_quote(ensure_judge_client(), JUDGE_MODEL, pitfall, plan,
                                base_url=JUDGE_BASE_URL, ordered_conditions=ordered_conditions, trace=trace)


def judge_h5_precautions(scenario: dict, plan: str, *, trace=None) -> dict:
    """Preserve each original obligation, including any declared ordering relation."""
    precautions = scenario["precautions"]
    constraints = scenario.get("ordered_constraints", {})
    if not isinstance(precautions, list) or not precautions or not isinstance(constraints, dict) or any(
        not isinstance(item, str) or not item.strip() for item in precautions
    ):
        return aggregate_judgments([])
    return aggregate_judgments([judge_pitfall_avoided(item, plan,
        ordered_conditions=constraints.get(str(index)), trace=trace) for index, item in enumerate(precautions)])


def h5_keyword_hit(scenario: dict, plan: str) -> bool:
    """Secondary pre-registered signal: any pitfall synonym stem in the plan."""
    low = plan.lower()
    return any(stem in low for stem in scenario.get("keyword_stems", []))


def unavailable_h5_judge(scenario: dict, reason: str) -> dict:
    items = [{"precaution": item, "valid": False, "avoided": None,
              "status": "unknown", "reason": reason, "raw": None,
              "metadata": {"attempts": [], "skipped_reason": reason}}
             for item in scenario["precautions"]]
    return aggregate_judgments(items)


def run_h5(samples: int, *, results=None, scenarios=None) -> tuple[ExperimentResult, ExperimentResult]:
    """Paired complete-generation knowledge conveyance; raw stems are diagnostic."""
    if type(samples) is not int or samples <= 0:
        raise ValueError("samples must be a positive integer")
    scenarios = copy.deepcopy(H5_SCENARIOS if scenarios is None else scenarios)
    if not scenarios:
        raise ValueError("H5 requires at least one scenario")
    control, experiment = results if results is not None else (
        ExperimentResult("H5-control", "No lesson injection", samples),
        ExperimentResult("H5-experiment", "With lesson injection", samples),
    )
    if (control.samples != samples or experiment.samples != samples or control.details or experiment.details
            or control.compliant or control.violated or experiment.compliant or experiment.violated):
        raise ValueError("H5 results must be fresh and match the requested samples")
    print("\n═══ H5: Lesson Injection vs No Injection (repeat-failure) ═══")
    print("    primary = complete paired knowledge conveyance; secondary = LLM judge")

    # Allocate both arms first so failures cannot erase a selected sample.
    for i in range(samples):
        scenario = scenarios[i % len(scenarios)]
        task_prompt = f"{scenario['task']}\nReply with your step-by-step plan only."
        injection = h5_injection_block(scenario)
        for result, block in ((control, ""), (experiment, injection)):
            result.details.append({
                "pair_id": f"h5:{i}:{scenario['task_type']}", "sample": i + 1,
                "scenario": scenario["task_type"], "scenario_spec": copy.deepcopy(scenario),
                "precautions": scenario["precautions"], "injected_block": block,
                "system_prompt": H5_BASE_SYSTEM + ("\n\n" + block if block else ""),
                "task_prompt": task_prompt, "plan": None, "records": [], "judge_records": [],
                "generation": generation_status(None, []), "keyword_hit": None,
                "judge": unavailable_h5_judge(scenario, "not_attempted"),
            })

    for i, (ctrl, exp) in enumerate(zip(control.details, experiment.details)):
        scenario = ctrl["scenario_spec"]
        print(f"  Sample {i+1}/{samples}: [{scenario['task_type']}] "
              f"{scenario['task'][:36]}...", end=" ")
        for result, detail in ((control, ctrl), (experiment, exp)):
            try:
                detail["plan"] = generate_response(detail["system_prompt"], detail["task_prompt"],
                                                   trace=detail["records"])
            finally:
                detail["generation"] = generation_status(detail["plan"], detail["records"])
            detail["keyword_hit"] = h5_keyword_hit(scenario, detail["plan"])
            # Preserve legacy counters as raw-stem diagnostics, never the headline.
            if detail["keyword_hit"]:
                result.compliant += 1
            else:
                result.violated += 1
            if detail["generation"]["valid"]:
                try:
                    detail["judge"] = judge_h5_precautions(scenario, detail["plan"], trace=detail["judge_records"])
                except BaseException:
                    detail["judge"] = unavailable_h5_judge(scenario, "judgment_incomplete")
                    raise
            else:
                detail["judge"] = unavailable_h5_judge(scenario, "incomplete_source_generation")
        print(f"ctrl={ctrl['keyword_hit']} exp={exp['keyword_hit']} "
              f"(judge ctrl={ctrl['judge']['status']} exp={exp['judge']['status']})")

    return control, experiment


def h5_pair_summary(control: ExperimentResult, experiment: ExperimentResult, *, secondary=False) -> dict:
    pairs = []
    for i in range(max(control.samples, experiment.samples)):
        pair = {}
        for arm, result in (("control", control), ("experiment", experiment)):
            detail = result.details[i] if i < len(result.details) else {}
            generation = detail.get("generation", {})
            hit = detail.get("keyword_hit")
            if secondary:
                judge = detail.get("judge", {})
                generation = {**generation, "valid": generation.get("valid") is True
                              and judge.get("valid") is True}
                hit = judge.get("avoided")
            pair[arm] = {"generation": generation, "keyword_hit": hit}
        pairs.append(pair)
    summary = summarize_generation_pairs(pairs)
    if secondary:
        summary["policy"] = "paired_valid_judgments_and_complete_generation"
    return summary


def h5_harness_spec(args, scenarios: list) -> dict:
    path = Path(__file__).resolve()
    source = {"source": "inline_pre_registered_scenarios", "source_path": str(path),
              "source_sha256": content_hash(path), "selection_sha256": json_hash(scenarios)}
    return sanitize({
        "schema_version": 2, "benchmark": "H5", "mechanism": "manual_lesson_block",
        "evidence": execution_identity(path, None, source),
        "dependency_sha256": {name: content_hash(Path(grounding.__file__).parent / name)
                              for name in ("judge_grounding.py", "benchmark_evidence.py")},
        "selection": {"samples": args.samples, "scenarios": scenarios,
                      "order": "dataset order, cyclic; no backfill"},
        "models": {"answer_model": MODEL, "answer_endpoint": grounding.endpoint_record(BASE_URL),
                   "answer_max_tokens": 800, "answer_temperature": 0.3,
                   "judge_model": JUDGE_MODEL, "judge_endpoint": grounding.endpoint_record(JUDGE_BASE_URL),
                   "judge_max_tokens": grounding.JUDGE_MAX_TOKENS,
                   "judge_retry_max_tokens": grounding.JUDGE_RETRY_MAX_TOKENS,
                   "judge_temperature": 0.0},
        "prompts": {"base_system": H5_BASE_SYSTEM,
                    "judge_prompt_version": grounding.JUDGE_PROMPT_VERSION},
        "scoring": {"primary": "paired_complete_generation_stem_hits",
                    "legacy_arm_counters": "raw_stem_diagnostics_not_acceptance",
                    "secondary": "grounded judgments, unknown separate; not execution evidence"},
        "argv": list(args.invocation_argv),
    })


def h5_artifact(control: ExperimentResult, experiment: ExperimentResult, spec: dict, run_error=None) -> dict:
    primary = h5_pair_summary(control, experiment)
    errors = (["execution_error"] if run_error else [])
    if primary["n"] != primary["n_requested"]:
        errors.append("invalid_generation_pairs")
    if not primary["n"]:
        errors.append("no_valid_generation_pairs")
    artifact = {"schema_version": 2, "harness_spec": spec, "primary_summary": primary,
                "secondary_summary": h5_pair_summary(control, experiment, secondary=True),
                "run_error": run_error, "run_status": {
                    "schema_version": 1, "status": "invalid" if errors else "scoring_complete",
                    "errors": errors, "n_selected_records": primary["n_requested"],
                    "n_valid_scoring_pairs": primary["n"]}}
    for arm, result in (("control", control), ("experiment", experiment)):
        artifact[arm] = {**asdict(result), "counter_scope": "raw_stem_diagnostics_not_acceptance",
                         "judge_summary": summarize_judgments(d.get("judge", {}) for d in result.details),
                         "judge_items": summarize_judgments(item for d in result.details
                                        for item in d.get("judge", {}).get("items", []))}
    return sanitize(artifact)


# ─── Main ──────────────────────────────────────────────────

def find_result(results: list, prefix: str):
    """Locate a hypothesis result by label, regardless of list position
    (robust when running a single hypothesis instead of 'all')."""
    for r in results:
        if isinstance(r, (tuple, list)) and r:
            first = r[0]
            if isinstance(first, ExperimentResult) and first.hypothesis.startswith(prefix):
                return r
        elif isinstance(r, ExperimentResult) and r.hypothesis.startswith(prefix):
            return r
    return None


def print_report(results: list, *, h5_evidence=None):
    print("\n" + "=" * 60)
    print("EXPERIMENT REPORT")
    print("=" * 60)
    for r in results:
        if isinstance(r, tuple):
            if r and r[0].hypothesis.startswith("H5"):
                print("Raw-stem diagnostics only (not acceptance scores):")
            for sub in r:
                print(sub.summary())
        elif isinstance(r, list):
            for sub in r:
                print(sub.summary())
        else:
            print(r.summary())

    print("=" * 60)
    print("CONCLUSIONS:")
    print("-" * 60)

    # H1
    h1 = find_result(results, "H1")
    if isinstance(h1, tuple):
        ctrl, exp = h1
        diff = exp.compliance_rate - ctrl.compliance_rate
        print(f"H1: Injection improves compliance by {diff:+.0%}")
        print(f"    Control: {ctrl.compliance_rate:.0%}, Experiment: {exp.compliance_rate:.0%}")
        print(f"    VERDICT: {'CONFIRMED ✓' if diff > 0.1 else 'INCONCLUSIVE' if diff > 0 else 'REJECTED ✗'}")

    # H2
    h2 = find_result(results, "H2")
    if isinstance(h2, tuple):
        desc, inst = h2
        diff = inst.compliance_rate - desc.compliance_rate
        print(f"\nH2: Instructive format improves compliance by {diff:+.0%}")
        print(f"    Descriptive: {desc.compliance_rate:.0%}, Instructive: {inst.compliance_rate:.0%}")
        print(f"    VERDICT: {'CONFIRMED ✓' if diff > 0.1 else 'INCONCLUSIVE' if diff > 0 else 'REJECTED ✗'}")

    # H3
    h3 = find_result(results, "H3")
    if isinstance(h3, list):
        print(f"\nH3: Token Budget vs Compliance:")
        best = max(h3, key=lambda r: r.compliance_rate)
        for r in h3:
            marker = " ← best" if r is best else ""
            print(f"    {r.description}: {r.compliance_rate:.0%}{marker}")
        print(f"    VERDICT: Optimal = {best.description}")

    # H4
    h4 = find_result(results, "H4")
    if isinstance(h4, ExperimentResult):
        mis_rate = h4.violated / h4.samples if h4.samples > 0 else 0
        print(f"\nH4: Mis-injection rate = {mis_rate:.0%}")
        print(f"    VERDICT: {'CONFIRMED ✓ (<5%)' if mis_rate < 0.05 else 'FAILED ✗ (>=5%)'}")

    # H5: legacy arm counters are never the complete-generation headline.
    h5 = find_result(results, "H5")
    if isinstance(h5, tuple):
        ctrl, exp = h5
        evidence = h5_evidence if h5_evidence is not None else h5_artifact(ctrl, exp, {})
        primary = evidence["primary_summary"]
        print(f"\nH5: complete generation pairs {primary['n']}/{primary['n_requested']}")
        for label in ("control", "experiment"):
            rate = f"{primary[label]:.0%}" if primary[label] is not None else "null (no valid pair)"
            print(f"    {label}: {rate} convey the specific knowledge")
        for label, result in (("control", ctrl), ("experiment", exp)):
            summary = summarize_judgments(d.get("judge", {}) for d in result.details)
            rate = f"{summary['rate']:.0%}" if summary["rate"] is not None else "null (no valid judge)"
            print(f"    [secondary] LLM judge {label}: {rate}; "
                  f"valid={summary['valid']}/{summary['total']}, unknown={summary['unknown']}")
        secondary = evidence["secondary_summary"]
        print(f"    [secondary paired] valid={secondary['n']}/{secondary['n_requested']}")
        if evidence["run_status"]["status"] != "scoring_complete":
            print("    VERDICT: INVALID (incomplete generation or execution)")
        else:
            diff = primary["delta"]
            print(f"    Complete-pair knowledge-conveyance delta: {diff:+.0%}")
            print(f"    VERDICT: {'CONFIRMED ✓' if diff > 0.1 else 'INCONCLUSIVE' if diff > 0 else 'REJECTED ✗'}")


def main(argv=None):
    parser = argparse.ArgumentParser(description="MemVault Hypothesis Verification")
    parser.add_argument(
        "--hypothesis", choices=["H1", "H2", "H3", "H4", "H5", "all"], default="all"
    )
    parser.add_argument("--samples", type=int, default=5, help="Samples per experiment (default: 5)")
    parser.add_argument("--out", type=Path, help="H5 evidence JSON (default: h5_results.json)")
    args = parser.parse_args(argv)
    args.invocation_argv = list(sys.argv[1:] if argv is None else argv)
    if args.samples <= 0:
        parser.error("--samples must be positive")

    h5_selected = args.hypothesis in ("H5", "all")
    scenarios = copy.deepcopy(H5_SCENARIOS)
    h5_results = (ExperimentResult("H5-control", "No lesson injection", args.samples),
                  ExperimentResult("H5-experiment", "With lesson injection", args.samples))
    out = args.out if args.out is not None else Path(__file__).parent / "h5_results.json"
    results, spec, artifact = [], {}, None
    run_error, phase, exit_code = None, "configuration", 0
    try:
        if h5_selected:
            spec = h5_harness_spec(args, scenarios)
        if not os.getenv("OPENAI_API_KEY") and BASE_URL is None:
            message = "Set OPENAI_API_KEY, or VERIFY_BASE_URL for a local OpenAI-compatible endpoint"
            if not h5_selected:
                print("ERROR: " + message)
                sys.exit(1)
            raise ValueError(message)
        if h5_selected:
            spec["models"].update(grounding.configure_judge())
        ensure_client()
        if h5_selected:
            ensure_judge_client()

        print("MemVault Hypothesis Verification")
        print(sanitize(f"Model: {MODEL}, Samples: {args.samples}, Endpoint: {BASE_URL or 'OpenAI'}"))
        if JUDGE_MODEL != MODEL or JUDGE_BASE_URL != BASE_URL:
            print(sanitize(f"Judge: {JUDGE_MODEL} @ {JUDGE_BASE_URL or 'OpenAI'}"))
        print(f"Running: {args.hypothesis}")
        for name, runner in (("H1", run_h1), ("H2", run_h2), ("H3", run_h3), ("H4", run_h4)):
            if args.hypothesis in (name, "all"):
                phase = name
                results.append(runner(args.samples))
        if h5_selected:
            phase = "H5"
            results.append(run_h5(args.samples, results=h5_results, scenarios=scenarios))
    except KeyboardInterrupt:
        if not h5_selected:
            raise
        run_error = {"phase": phase, "type": "KeyboardInterrupt", "message": "interrupted"}
        exit_code = 130
    except (Exception, SystemExit) as exc:
        if not h5_selected:
            raise
        run_error = sanitize({"phase": phase, "type": type(exc).__name__, "message": str(exc)})
        exit_code = 1
    finally:
        if h5_selected:
            artifact = h5_artifact(*h5_results, spec, run_error)
            try:
                out.parent.mkdir(parents=True, exist_ok=True)
                out.write_text(json.dumps(artifact, ensure_ascii=False, indent=1), encoding="utf-8")
                print(sanitize(f"H5 details written to {out}"))
            except Exception as exc:
                artifact["run_status"]["status"] = "invalid"
                artifact["run_status"]["errors"].append("artifact_write_failed")
                if run_error is None:
                    run_error = sanitize({"phase": "write", "type": type(exc).__name__, "message": str(exc)})
                    artifact["run_error"] = run_error
                print(sanitize(f"ERROR: cannot write H5 evidence to {out}: {exc}"), file=sys.stderr)
                if not exit_code:
                    exit_code = 1
            if artifact["run_status"]["status"] != "scoring_complete" and not exit_code:
                exit_code = 1

    if run_error:
        print(sanitize(f"ERROR: {run_error['phase']}: {run_error['message']}"), file=sys.stderr)
    if h5_selected and find_result(results, "H5") is None:
        results.append(h5_results)
    print_report(results, h5_evidence=artifact)
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
