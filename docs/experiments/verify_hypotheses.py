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
import json
import os
import re
import sys
from dataclasses import dataclass, field
from typing import Optional

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
client = OpenAI(base_url=BASE_URL, api_key=os.getenv("OPENAI_API_KEY") or "local")

# H5 splits the roles: a (possibly weak) AGENT generates the task plan, and a
# JUDGE evaluates whether the plan avoids the pitfall. Using a stronger judge
# than the agent is the standard LLM-as-judge setup and avoids the ceiling
# effect where a capable agent avoids the pitfall even without the lesson.
# Defaults fall back to the main model/endpoint when unset.
JUDGE_MODEL = os.getenv("VERIFY_JUDGE_MODEL", MODEL)
JUDGE_BASE_URL = os.getenv("VERIFY_JUDGE_BASE_URL", BASE_URL)
judge_client = (
    OpenAI(base_url=JUDGE_BASE_URL, api_key=os.getenv("OPENAI_API_KEY") or "local")
    if JUDGE_BASE_URL != BASE_URL or JUDGE_MODEL != MODEL
    else client
)


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

def generate_response(system_prompt: str, user_prompt: str) -> str:
    """Simulate an Agent generating a response."""
    resp = client.chat.completions.create(
        model=MODEL,
        messages=[
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_prompt},
        ],
        max_tokens=800,
        temperature=0.3,
    )
    return resp.choices[0].message.content


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

    resp = client.chat.completions.create(
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

    resp = client.chat.completions.create(
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
        "pitfall": "pointing the DASHBOARD_CDN environment variable at the new CDN domain before deploying",
        "cause": "the DASHBOARD_CDN env var must point at the new CDN domain",
        # Secondary (non-judge) signal: the unique proper noun from the cause.
        # Pre-registered per scenario so it cannot be tuned after results.
        "keyword_stems": ["dashboard_cdn"],
    },
    {
        "task_type": "migrate",
        "task": "Migrate the users table to the new schema with the added profile columns.",
        "pitfall": "backfilling users.profile_json before the NOT NULL constraint is added",
        "cause": "users.profile_json must be backfilled before the NOT NULL constraint is added",
        "keyword_stems": ["profile_json"],
    },
    {
        "task_type": "upgrade",
        "task": "Upgrade the payment SDK from v2 to v3 in our checkout service.",
        "pitfall": "adding the Idempotency-Key header that payment v3 requires on every POST",
        "cause": "payment v3 requires an Idempotency-Key header on every POST",
        "keyword_stems": ["idempotency"],
    },
    {
        "task_type": "refactor",
        "task": "Refactor the authentication module to split session handling from login.",
        "pitfall": "keeping legacy session cookies readable behind the AUTH_SPLIT feature flag",
        "cause": "legacy session cookies must stay readable behind the AUTH_SPLIT feature flag",
        "keyword_stems": ["auth_split"],
    },
    {
        "task_type": "release",
        "task": "Release v2 of our public API to external partners.",
        "pitfall": "keeping legacy error codes on the v1 /billing endpoint for two more quarters",
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


def judge_pitfall_avoided(pitfall: str, plan: str) -> dict:
    """LLM-as-judge: does the plan explicitly include the precaution?

    Implementation note (H5 calibration, 2026-08-26): judge reliability
    depends heavily on framing, and the failure mode differs per model size:
    - normative yes/no ("does the plan include this?") → small models show a
      strong NEGATIVE bias (answer false even for verbatim matches);
    - neutral step lookup ("which step involves this?") → larger models show
      a POSITIVE bias (invent a step for unrelated plans).
    The calibrated framing requires the judge to QUOTE the exact words of the
    matching step and return null when none exists — grounding cancels both
    biases (7/8 on the calibration battery with qwen2.5-3b-instruct).
    """
    prompt = f"""PRECAUTION: {pitfall}

PLAN:
{plan}

Find the step that performs this precaution and quote its exact words (paraphrases of the precaution count, but the quote must be real text from the plan). If no step performs it, return null.
Answer ONLY JSON: {{"step": <number>, "quote": "<exact words from that step>"}} or {{"step": null, "quote": null}}"""

    resp = judge_client.chat.completions.create(
        model=JUDGE_MODEL,
        messages=[{"role": "user", "content": prompt}],
        max_tokens=100,
        temperature=0.0,
    )
    text = resp.choices[0].message.content.strip()
    if text.startswith("```"):
        text = text.split("\n", 1)[1].rsplit("```", 1)[0]

    step = None
    quote = None
    try:
        parsed = json.loads(text)
        step = parsed.get("step")
        quote = parsed.get("quote")
    except json.JSONDecodeError:
        # Small models sometimes skip strict JSON — recover the step number.
        m = re.search(r'"step"\s*:\s*(\d+)', text)
        if m:
            step = int(m.group(1))

    try:
        avoided = step is not None and int(step) >= 1
    except (TypeError, ValueError):
        avoided = False
    return {"avoided": avoided, "evidence": f"step={step}", "quote": quote, "raw": text[:150]}


def h5_keyword_hit(scenario: dict, plan: str) -> bool:
    """Secondary pre-registered signal: any pitfall synonym stem in the plan."""
    low = plan.lower()
    return any(stem in low for stem in scenario.get("keyword_stems", []))


def run_h5(samples: int) -> tuple[ExperimentResult, ExperimentResult]:
    """H5: Lesson injection reduces repeat-failure rate on similar tasks.

    Primary metric — knowledge conveyed: each scenario's pitfall is a
    NON-OBVIOUS, project-specific fact (a magic env var, an unannounced
    header, a policy commitment) identified by a unique proper noun. The
    agent "fails" iff its plan lacks that knowledge. Presence of the proper
    noun is objective and reproducible, which matters because small local
    LLM judges are unreliable at deciding whether a VAGUE plan covers a
    SPECIFIC fact (calibration found a strong positive bias: they mark
    generic "check the config" plans as covering the pitfall).

    Secondary metric — LLM judge: reported as a cross-check only.
    """
    print("\n═══ H5: Lesson Injection vs No Injection (repeat-failure) ═══")
    print("    primary = knowledge conveyed (objective); secondary = LLM judge")

    control = ExperimentResult("H5-control", "No lesson injection", samples)
    experiment = ExperimentResult("H5-experiment", "With lesson injection", samples)

    for i in range(samples):
        scenario = H5_SCENARIOS[i % len(H5_SCENARIOS)]
        task_prompt = f"{scenario['task']}\nReply with your step-by-step plan only."
        print(
            f"  Sample {i+1}/{samples}: [{scenario['task_type']}] "
            f"{scenario['task'][:36]}...",
            end=" ",
        )

        # Control: no memory injection — the specific knowledge cannot be guessed.
        plan_ctrl = generate_response(H5_BASE_SYSTEM, task_prompt)
        kw_ctrl = h5_keyword_hit(scenario, plan_ctrl)
        judge_ctrl = judge_pitfall_avoided(scenario["pitfall"], plan_ctrl)
        if kw_ctrl:
            control.compliant += 1
        else:
            control.violated += 1

        # Experiment: the lesson MemVault distilled from the earlier failure
        # is injected ahead of the task.
        injected_system = H5_BASE_SYSTEM + "\n\n" + h5_injection_block(scenario)
        plan_exp = generate_response(injected_system, task_prompt)
        kw_exp = h5_keyword_hit(scenario, plan_exp)
        judge_exp = judge_pitfall_avoided(scenario["pitfall"], plan_exp)
        if kw_exp:
            experiment.compliant += 1
        else:
            experiment.violated += 1

        control.details.append(
            {
                "scenario": scenario["task_type"],
                "plan": plan_ctrl,
                "keyword_hit": kw_ctrl,
                "judge": judge_ctrl,
            }
        )
        experiment.details.append(
            {
                "scenario": scenario["task_type"],
                "plan": plan_exp,
                "keyword_hit": kw_exp,
                "judge": judge_exp,
            }
        )

        print(
            f"ctrl={'✓' if kw_ctrl else '✗'} exp={'✓' if kw_exp else '✗'} "
            f"(judge ctrl={'✓' if judge_ctrl.get('avoided') else '✗'} "
            f"exp={'✓' if judge_exp.get('avoided') else '✗'})"
        )

    return control, experiment


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


def print_report(results: list):
    print("\n" + "=" * 60)
    print("EXPERIMENT REPORT")
    print("=" * 60)
    for r in results:
        if isinstance(r, tuple):
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

    # H5
    h5 = find_result(results, "H5")
    if isinstance(h5, tuple):
        ctrl, exp = h5
        diff = exp.compliance_rate - ctrl.compliance_rate
        print(f"\nH5: Lesson injection improves pitfall-avoidance by {diff:+.0%}")
        print(f"    Control (no lesson): {ctrl.compliance_rate:.0%} convey the specific knowledge")
        print(f"    Experiment (lesson): {exp.compliance_rate:.0%} convey the specific knowledge")
        # Secondary cross-check: LLM judge (known positive bias on vague plans,
        # reported for completeness — the objective metric above is primary).
        if ctrl.details and exp.details:
            c_j = sum(1 for d in ctrl.details if d.get("judge", {}).get("avoided")) / len(ctrl.details)
            e_j = sum(1 for d in exp.details if d.get("judge", {}).get("avoided")) / len(exp.details)
            print(f"    [secondary] LLM judge: control {c_j:.0%}, experiment {e_j:.0%} (positive-biased, informational)")
        print(f"    VERDICT: {'CONFIRMED ✓' if diff > 0.1 else 'INCONCLUSIVE' if diff > 0 else 'REJECTED ✗'}")


def main():
    parser = argparse.ArgumentParser(description="MemVault Hypothesis Verification")
    parser.add_argument(
        "--hypothesis", choices=["H1", "H2", "H3", "H4", "H5", "all"], default="all"
    )
    parser.add_argument("--samples", type=int, default=5, help="Samples per experiment (default: 5)")
    args = parser.parse_args()

    if not os.getenv("OPENAI_API_KEY") and BASE_URL is None:
        print(
            "ERROR: Set OPENAI_API_KEY, or VERIFY_BASE_URL for a local "
            "OpenAI-compatible endpoint (e.g. http://localhost:1234/v1)"
        )
        sys.exit(1)

    print(f"MemVault Hypothesis Verification")
    print(f"Model: {MODEL}, Samples: {args.samples}, Endpoint: {BASE_URL or 'OpenAI'}")
    if JUDGE_MODEL != MODEL or JUDGE_BASE_URL != BASE_URL:
        print(f"Judge: {JUDGE_MODEL} @ {JUDGE_BASE_URL or 'OpenAI'}")
    print(f"Running: {args.hypothesis}")

    results = []

    if args.hypothesis in ("H1", "all"):
        results.append(run_h1(args.samples))
    if args.hypothesis in ("H2", "all"):
        results.append(run_h2(args.samples))
    if args.hypothesis in ("H3", "all"):
        results.append(run_h3(args.samples))
    if args.hypothesis in ("H4", "all"):
        results.append(run_h4(args.samples))
    if args.hypothesis in ("H5", "all"):
        results.append(run_h5(args.samples))

    print_report(results)


if __name__ == "__main__":
    main()
