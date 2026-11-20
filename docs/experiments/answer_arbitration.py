#!/usr/bin/env python3
"""Fixed-context answer-only A/B for MH full-chain cases.

This is deliberately *not* a retrieval experiment. It replays the delivered
context frozen by the two-hop keyword-priority arm, then changes only the answer
prompt:

A. the current Math/MAB answer prompt (`QUERY_TEMPLATE`).
B. a slot-wise arbitration prompt: find same-subject/same-relation candidates,
   choose the largest serial in each slot, traverse the selected branch, and
   never fill gaps with world knowledge.

Both arms use the same context, model, temperature, max_tokens and extra_body.
Six human/assistant corpus-checked full-chain cases are run three times each:
6 cases x 2 arms x 3 repeats = 36 generations.

The benchmark's deterministic substring-exact-match rule scores answers; no
LLM judge is involved.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import statistics
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_SOURCE = (
    REPO_ROOT
    / "docs/experiments/results/2026-11-19/post169/mab-mh-anchor-chain/"
    "rerun-anchor-k250-a5-twohop5-kwpriority.json"
)
DEFAULT_BASELINE = (
    REPO_ROOT
    / "docs/experiments/results/2026-11-19/post169/mab-mh-anchor-chain/baseline.json"
)
DEFAULT_CASES = ("no11", "no36", "no39", "no57", "no92", "no94")
DEFAULT_OUT = (
    REPO_ROOT
    / "docs/experiments/results/2026-11-20/answer-arbitration/results.json"
)

SLOT_PROMPT = (
    "Pretend you are a knowledge management system. The Archival Memory is the "
    "only admissible evidence; world knowledge is forbidden and missing links "
    "must not be filled.\n\n"
    "Resolve the question in two visible stages.\n\n"
    "STAGE 1 — SLOT TABLE:\n"
    "Decompose the question into ordered entity/relation slots. For each slot, "
    "enumerate ALL matching facts with that same subject and relation. Multiple "
    "facts for the same subject and relation are competing values: compare their "
    "serials and select the LARGEST serial, even when another value looks more "
    "plausible from world knowledge. A larger serial from an unrelated slot is "
    "never a candidate. Write one line per slot:\n"
    "SLOT <n>: <subject> | <relation> | candidates=<serial:value; serial:value> | "
    "selected serial=<serial> | value=<value>\n"
    "Then write:\n"
    "PATH: <entity in question> -> <value 1> -> <value 2> ... -> <final value>\n\n"
    "STAGE 2 — ANSWER:\n"
    "Copy the final value from PATH. Answer only after the slot table and PATH. "
    "If any required slot has no matching fact, set PATH: insufficient evidence "
    "and answer exactly that.\n\n"
    "Question: {question}\n"
)


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def short_case(question_id: str) -> str:
    return question_id.rsplit("_", 1)[-1]


def load_cases(source: Path, baseline: Path, wanted: tuple[str, ...]) -> list[dict]:
    src = json.loads(source.read_text())
    base = json.loads(baseline.read_text())
    questions = src["results"][0]["questions"]
    by_short = {short_case(q["question_id"]): q for q in questions}
    chain_by_short = {short_case(q["question_id"]): q for q in base["questions"]}
    cases = []
    for name in wanted:
        if name not in by_short:
            raise ValueError(f"case {name} missing from source context")
        q, b = by_short[name], chain_by_short[name]
        serials = [int(x["serial"]) if isinstance(x, dict) else int(x) for x in b["support_chain"]]
        found = set(q["retrieved_serials"])
        missing = [s for s in serials if s not in found]
        if missing:
            raise ValueError(f"{name}: full chain is not in frozen context; missing serials {missing}")
        if not q.get("retrieved_contents"):
            raise ValueError(f"{name}: frozen context is empty")
        cases.append(
            {
                "case": name,
                "question_id": q["question_id"],
                "question": q["question"],
                "golds": q["golds"],
                "support_chain": serials,
                "context_serials": q["retrieved_serials"],
                "context": q["retrieved_contents"],
            }
        )
    return cases


def normalise(text: str) -> str:
    import string

    text = text.lower()
    text = "".join(ch for ch in text if ch not in string.punctuation)
    text = re.sub(r"\b(a|an|the)\b", " ", text)
    return " ".join(text.split())


def correct(prediction: str, golds: list[str]) -> bool:
    pred = normalise(prediction)
    return any(normalise(g) in pred for g in golds)


def response_text(resp) -> str:
    try:
        return (resp.choices[0].message.content or "").strip()
    except Exception:  # noqa: BLE001 - provider response shape
        return ""


def extract_final_answer(text: str, arm: str) -> str:
    """Extract only the answer field, not gold values mentioned in a slot table.

    Arm B intentionally exposes its slot table; scoring the whole table would
    create false positives whenever an old and a new value both appear. Arm A
    normally returns a bare answer, but also handles an explicit final ``Answer:``.
    """
    text = text.strip()
    if not text:
        return ""
    if arm == "B":
        marker = "STAGE 2 — ANSWER:"
        if marker in text:
            text = text.split(marker, 1)[1].strip()
        else:
            marker = "STAGE 2 - ANSWER:"
            if marker in text:
                text = text.split(marker, 1)[1].strip()
    matches = list(re.finditer(r"(?im)^\s*answer\s*:\s*", text))
    if matches:
        text = text[matches[-1].end():].strip()
    elif arm == "B":
        lines = [line.strip() for line in text.splitlines() if line.strip()]
        text = lines[-1] if lines else ""
    return text.strip().strip('`').strip()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--source", type=Path, default=DEFAULT_SOURCE)
    ap.add_argument("--baseline", type=Path, default=DEFAULT_BASELINE)
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    ap.add_argument("--repeats", type=int, default=3)
    ap.add_argument("--max-tokens", type=int, default=3000)
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    if args.repeats <= 0:
        ap.error("--repeats must be positive")
    if args.max_tokens <= 0:
        ap.error("--max-tokens must be positive")

    # Imported only after environment validation; keeps --dry-run offline and
    # avoids importing the OpenAI client when it is not needed.
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    from memory_agent_bench import (SYSTEM_MESSAGE, QUERY_TEMPLATE, _chat_with_retry,
                                    make_client)  # noqa: E402

    cases = load_cases(args.source, args.baseline, DEFAULT_CASES)
    model = os.getenv("VERIFY_MODEL", "gpt-4o-mini")
    base_url = os.getenv("VERIFY_BASE_URL") or os.getenv("OPENAI_BASE_URL")
    extra_body_raw = os.getenv("VERIFY_EXTRA_BODY", "").strip()
    extra_body = json.loads(extra_body_raw) if extra_body_raw else None
    if not args.dry_run and (not base_url or not os.getenv("OPENAI_API_KEY")):
        raise SystemExit("missing VERIFY_BASE_URL/OPENAI_API_KEY; source ~/.config/memvault/bench.env")

    run = {
        "schema_version": 1,
        "created_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "purpose": "fixed-context answer-only A/B on six confirmed full-chain MH cases",
        "design": {
            "only_variable": "answer prompt",
            "arm_a": "current official MAB QUERY_TEMPLATE",
            "arm_b": "visible slot-table latest-fact branch traversal",
            "cases": list(DEFAULT_CASES),
            "repeats_per_case_per_arm": args.repeats,
            "expected_generations": len(cases) * 2 * args.repeats,
            "scoring": "official MAB substring_exact_match; no judge",
            "missing_evidence_rule": "insufficient evidence; no world-knowledge completion",
            "no3": "excluded: same-slot conflict (#403 vs newer #1777)",
        },
        "provenance": {
            "context_source": str(args.source.relative_to(REPO_ROOT)),
            "context_source_sha256": sha256(args.source),
            "chain_baseline": str(args.baseline.relative_to(REPO_ROOT)),
            "chain_baseline_sha256": sha256(args.baseline),
            "prompt_a_sha256": hashlib.sha256(QUERY_TEMPLATE.encode()).hexdigest(),
            "prompt_b_sha256": hashlib.sha256(SLOT_PROMPT.encode()).hexdigest(),
        },
        "models": {
            "answer_model": model,
            "base_url": base_url or "default",
            "temperature": 0.0,
            "max_tokens": args.max_tokens,
            "extra_body": extra_body,
            "answer_endpoint": "chat/completions",
        },
        "cases": cases,
        "runs": [],
        "summary": None,
        "dry_run": args.dry_run,
    }

    if args.dry_run:
        print(json.dumps({"cases": [c["case"] for c in cases],
                          "generations": len(cases) * 2 * args.repeats,
                          "provenance": run["provenance"]}, indent=2))
        return 0

    client = make_client()
    for case in cases:
        block = "Archival Memory (MemVault search results for this question):\n" + "\n".join(case["context"])
        system_a = f"{SYSTEM_MESSAGE}\n\n{block}"
        user_a = QUERY_TEMPLATE.format(question=case["question"])
        user_b = SLOT_PROMPT.format(question=case["question"])
        for arm in ("A", "B"):
            for repeat in range(1, args.repeats + 1):
                trace: list[dict] = []
                messages = [
                    {"role": "system", "content": system_a if arm == "A" else SYSTEM_MESSAGE + "\n\n" + block},
                    {"role": "user", "content": user_a if arm == "A" else user_b},
                ]
                resp = _chat_with_retry(
                    client,
                    trace=trace,
                    model=model,
                    messages=messages,
                    max_tokens=args.max_tokens,
                    temperature=0.0,
                    **({"extra_body": extra_body} if extra_body else {}),
                )
                raw_text = response_text(resp)
                final_answer = extract_final_answer(raw_text, arm)
                finish = None
                usage = None
                try:
                    finish = resp.choices[0].finish_reason
                    usage = resp.usage.model_dump() if resp.usage else None
                except Exception:  # noqa: BLE001
                    pass
                run["runs"].append(
                    {
                        "case": case["case"],
                        "question_id": case["question_id"],
                        "arm": arm,
                        "repeat": repeat,
                        "prediction": final_answer,
                        "raw_response": raw_text,
                        "normalised_prediction": normalise(final_answer),
                        "correct": correct(final_answer, case["golds"]),
                        "official_raw_substring_match": correct(raw_text, case["golds"]),
                        "finish_reason": finish,
                        "complete": finish == "stop" and bool(final_answer),
                        "usage": usage,
                        "trace": trace,
                    }
                )
                print(f"{case['case']} {arm} {repeat}/{args.repeats}: {final_answer[:80]!r}", file=sys.stderr)

    summary = {"arms": {}, "paired": {}}
    case_summary = {}
    for case in cases:
        name = case["case"]
        case_summary[name] = {}
        for arm in ("A", "B"):
            rows = [r for r in run["runs"] if r["case"] == name and r["arm"] == arm]
            answers = [r["normalised_prediction"] for r in rows]
            case_summary[name][arm] = {
                "n": len(rows),
                "correct_runs": sum(r["correct"] for r in rows),
                "complete_runs": sum(r["complete"] for r in rows),
                "unique_normalised_answers": len(set(answers)),
                "repeat_consistent": len(set(answers)) == 1,
                "majority_prediction": statistics.mode([r["prediction"] for r in rows]) if rows else None,
                "majority_correct": (sum(r["correct"] for r in rows) * 2 >= len(rows)) if rows else False,
                "predictions": [r["prediction"] for r in rows],
            }
    for arm in ("A", "B"):
        rows = [r for r in run["runs"] if r["arm"] == arm]
        summary["arms"][arm] = {
            "n": len(rows),
            "correct_runs": sum(r["correct"] for r in rows),
            "accuracy": (sum(r["correct"] for r in rows) / len(rows)) if rows else None,
            "complete_runs": sum(r["complete"] for r in rows),
            "consistency": sum(v[arm]["repeat_consistent"] for v in case_summary.values()),
            "consistent_cases": [k for k, v in case_summary.items() if v[arm]["repeat_consistent"]],
        }
    a_correct = sum(case_summary[c["case"]]["A"]["majority_correct"] for c in cases)
    b_correct = sum(case_summary[c["case"]]["B"]["majority_correct"] for c in cases)
    summary["paired"] = {
        "n_cases": len(cases),
        "a_majority_correct": a_correct,
        "b_majority_correct": b_correct,
        "a_accuracy": a_correct / len(cases),
        "b_accuracy": b_correct / len(cases),
        "b_improved": sum(case_summary[c["case"]]["B"]["majority_correct"] and not case_summary[c["case"]]["A"]["majority_correct"] for c in cases),
        "b_regressed": sum(case_summary[c["case"]]["A"]["majority_correct"] and not case_summary[c["case"]]["B"]["majority_correct"] for c in cases),
    }
    run["summary"] = summary
    run["case_summary"] = case_summary
    run["answer"] = (
        "In the fixed full-chain context, the slot-wise prompt changes whether the "
        "model follows the selected latest branch. Compare A/B accuracy and repeat "
        "consistency; do not infer retrieval gains from this answer-only test."
    )
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(run, indent=2, ensure_ascii=False) + "\n")
    print(json.dumps({"out": str(args.out), "summary": summary}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
