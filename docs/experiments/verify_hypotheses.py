#!/usr/bin/env python3
"""
MemVault Hypothesis Verification Experiments
=============================================

Validates 4 key design hypotheses:
  H1: Pre-Prompt Injection improves compliance rate
  H2: Instructive format ([MUST]) > descriptive format
  H3: Optimal Token Budget is ~1500 (not too low, not too high)
  H4: Router mis-injection rate < 5%

Requirements:
  - OPENAI_API_KEY env var (uses gpt-4o-mini for cost-efficiency)
  - Python 3.10+
  - pip install openai

Usage:
  python docs/experiments/verify_hypotheses.py [--hypothesis H1|H2|H3|H4|all] [--samples N]
"""

import argparse
import json
import os
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
client = OpenAI()


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
    {"content": "用户偏好 Python，不用 Java", "priority": "MUST", "type": "preference"},
    {"content": "代码不加注释，函数不超过 10 行", "priority": "MUST", "type": "preference"},
    {"content": "使用 FastAPI 框架", "priority": "MUST", "type": "preference"},
    {"content": "变量命名用 snake_case", "priority": "MUST", "type": "preference"},
    {"content": "项目使用 PostgreSQL 数据库", "priority": "REFERENCE", "type": "fact"},
]

USER_PROMPTS = [
    "帮我写一个 REST API 端点，实现用户登录功能",
    "写一个函数，从数据库查询用户列表并返回分页结果",
    "实现一个简单的缓存装饰器",
    "写一个发送邮件的工具函数",
    "实现一个文件上传的 API 端点",
]

# Unrelated memories for mis-injection test
UNRELATED_MEMORIES = [
    {"content": "用户喜欢古典音乐", "priority": "REFERENCE", "type": "preference", "tags": ["music"]},
    {"content": "用户的猫叫小花", "priority": "REFERENCE", "type": "fact", "tags": ["personal"]},
    {"content": "用户周末喜欢跑步", "priority": "REFERENCE", "type": "fact", "tags": ["sports"]},
    {"content": "用户喜欢看科幻小说", "priority": "REFERENCE", "type": "preference", "tags": ["reading"]},
    {"content": "用户的生日是 3 月 15 日", "priority": "REFERENCE", "type": "fact", "tags": ["personal"]},
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
        "[MEMORY CONTEXT - 必须遵循]:\n"
        "[MUST] 代码使用 Python，不用 Java\n"
        "[MUST] 代码不加注释，函数不超过 10 行\n"
        "[MUST] 使用 FastAPI 框架\n"
        "[MUST] 变量命名用 snake_case\n"
        "[REF] 项目使用 PostgreSQL 数据库\n"
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
        "[MEMORY CONTEXT - 必须遵循]:\n"
        "[MUST] 代码使用 Python，不用 Java\n"
        "[MUST] 代码不加注释，函数不超过 10 行\n"
        "[MUST] 使用 FastAPI 框架\n"
        "[MUST] 变量命名用 snake_case\n"
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
        "[MUST] 代码使用 Python，不用 Java",
        "[MUST] 代码不加注释，函数不超过 10 行",
        "[MUST] 使用 FastAPI 框架",
        "[MUST] 变量命名用 snake_case",
        "[REF] 项目使用 PostgreSQL 数据库",
        "[REF] 用户偏好简洁的错误处理",
        "[REF] 使用 pydantic 做数据验证",
        "[REF] 用 httpx 做 HTTP 请求",
    ]

    results = []

    for budget_label, mem_count in budgets:
        result = ExperimentResult("H3", f"Budget: {budget_label}", samples)
        injected = "\n".join(all_memories_text[:mem_count])
        system = (
            f"You are a coding assistant.\n\n"
            f"[MEMORY CONTEXT - 必须遵循]:\n{injected}\n"
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
        "帮我写一个 REST API",
        "实现数据库查询功能",
        "写一个文件处理函数",
        "实现 JWT 认证中间件",
        "写一个数据验证模块",
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


# ─── Main ──────────────────────────────────────────────────

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
    if len(results) >= 1 and isinstance(results[0], tuple):
        ctrl, exp = results[0]
        diff = exp.compliance_rate - ctrl.compliance_rate
        print(f"H1: Injection improves compliance by {diff:+.0%}")
        print(f"    Control: {ctrl.compliance_rate:.0%}, Experiment: {exp.compliance_rate:.0%}")
        print(f"    VERDICT: {'CONFIRMED ✓' if diff > 0.1 else 'INCONCLUSIVE' if diff > 0 else 'REJECTED ✗'}")

    # H2
    if len(results) >= 2 and isinstance(results[1], tuple):
        desc, inst = results[1]
        diff = inst.compliance_rate - desc.compliance_rate
        print(f"\nH2: Instructive format improves compliance by {diff:+.0%}")
        print(f"    Descriptive: {desc.compliance_rate:.0%}, Instructive: {inst.compliance_rate:.0%}")
        print(f"    VERDICT: {'CONFIRMED ✓' if diff > 0.1 else 'INCONCLUSIVE' if diff > 0 else 'REJECTED ✗'}")

    # H3
    if len(results) >= 3 and isinstance(results[2], list):
        print(f"\nH3: Token Budget vs Compliance:")
        best = max(results[2], key=lambda r: r.compliance_rate)
        for r in results[2]:
            marker = " ← best" if r is best else ""
            print(f"    {r.description}: {r.compliance_rate:.0%}{marker}")
        print(f"    VERDICT: Optimal = {best.description}")

    # H4
    if len(results) >= 4 and isinstance(results[3], ExperimentResult):
        mis_rate = results[3].violated / results[3].samples if results[3].samples > 0 else 0
        print(f"\nH4: Mis-injection rate = {mis_rate:.0%}")
        print(f"    VERDICT: {'CONFIRMED ✓ (<5%)' if mis_rate < 0.05 else 'FAILED ✗ (>=5%)'}")


def main():
    parser = argparse.ArgumentParser(description="MemVault Hypothesis Verification")
    parser.add_argument("--hypothesis", choices=["H1", "H2", "H3", "H4", "all"], default="all")
    parser.add_argument("--samples", type=int, default=5, help="Samples per experiment (default: 5)")
    args = parser.parse_args()

    if not os.getenv("OPENAI_API_KEY"):
        print("ERROR: Set OPENAI_API_KEY environment variable")
        sys.exit(1)

    print(f"MemVault Hypothesis Verification")
    print(f"Model: {MODEL}, Samples: {args.samples}")
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

    print_report(results)


if __name__ == "__main__":
    main()
