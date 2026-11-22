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
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

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


_SECRET_KEY = re.compile(
    r"(api[_-]?key|authorization|access[_-]?token|refresh[_-]?token|"
    r"client[_-]?secret|password|credential|^token$)",
    re.IGNORECASE,
)


def redact_text(text: str) -> str:
    """Remove provider credentials from strings persisted in the result file."""
    text = re.sub(r"(?i)(https?://)[^/\s@]+@", r"\1[REDACTED]@", text)
    text = re.sub(r"(?i)(bearer\s+)[A-Za-z0-9._\-]+", r"\1[REDACTED]", text)
    text = re.sub(
        r"(?i)(api[_-]?key|access[_-]?token|refresh[_-]?token|token|secret|password)=([^&\s]+)",
        r"\1=[REDACTED]",
        text,
    )
    text = re.sub(r"\bsk-[A-Za-z0-9._\-]{8,}", "[REDACTED]", text)
    for name in ("OPENAI_API_KEY", "VERIFY_API_KEY"):
        value = os.getenv(name)
        if value and len(value) >= 8:
            text = text.replace(value, "[REDACTED]")
    return text


def redact_url(url: str) -> str:
    """Keep the endpoint shape while dropping userinfo and secret query values."""
    try:
        parsed = urlsplit(url)
    except ValueError:
        return redact_text(url)
    host = parsed.hostname or ""
    if parsed.port:
        host = f"{host}:{parsed.port}"
    query = urlencode(
        [
            (key, "[REDACTED]" if _SECRET_KEY.search(key) else redact_text(value))
            for key, value in parse_qsl(parsed.query, keep_blank_values=True)
        ]
    )
    return urlunsplit((parsed.scheme, host, parsed.path, query, ""))


def redact_value(value, key: str | None = None):
    """Recursively redact credential-shaped keys/values before serialization."""
    if key is not None and _SECRET_KEY.search(key):
        return "[REDACTED]"
    if isinstance(value, dict):
        return {redact_text(str(k)): redact_value(v, str(k)) for k, v in value.items()}
    if isinstance(value, list):
        return [redact_value(item) for item in value]
    if isinstance(value, str):
        if value.startswith(("http://", "https://")):
            return redact_url(value)
        return redact_text(value)
    return value


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
    safe_extra_body = redact_value(extra_body)
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
            "accuracy_denominator": "complete generations only (finish_reason=stop and non-empty answer)",
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
            "base_url": redact_url(base_url) if base_url else "default",
            "temperature": 0.0,
            "max_tokens": args.max_tokens,
            "extra_body": safe_extra_body,
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
                        "trace": redact_value(trace),
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
            complete_rows = [r for r in rows if r["complete"]]
            answers = [r["normalised_prediction"] for r in complete_rows]
            correct_runs = sum(r["correct"] for r in complete_rows)
            case_summary[name][arm] = {
                "n": len(rows),
                "complete_runs": len(complete_rows),
                "incomplete_runs": len(rows) - len(complete_rows),
                "correct_runs": correct_runs,
                "unique_normalised_answers": len(set(answers)),
                "repeat_consistent": (len(set(answers)) == 1) if complete_rows else None,
                "majority_prediction": statistics.mode([r["prediction"] for r in complete_rows]) if complete_rows else None,
                "majority_correct": (correct_runs * 2 >= len(complete_rows)) if complete_rows else None,
                "predictions": [r["prediction"] for r in complete_rows],
                "incomplete_finish_reasons": sorted(
                    {str(r.get("finish_reason")) for r in rows if not r["complete"]}
                ),
            }
    for arm in ("A", "B"):
        rows = [r for r in run["runs"] if r["arm"] == arm]
        complete_rows = [r for r in rows if r["complete"]]
        correct_runs = sum(r["correct"] for r in complete_rows)
        summary["arms"][arm] = {
            "n": len(rows),
            "complete_runs": len(complete_rows),
            "incomplete_runs": len(rows) - len(complete_rows),
            "correct_runs": correct_runs,
            "accuracy": (correct_runs / len(complete_rows)) if complete_rows else None,
            "all_runs_accuracy": (
                sum(r["correct"] for r in rows) / len(rows) if rows else None
            ),
            "consistency": sum(
                1 for v in case_summary.values() if v[arm]["repeat_consistent"] is True
            ),
            "consistent_cases": [
                k
                for k, v in case_summary.items()
                if v[arm]["repeat_consistent"] is True
            ],
        }
    paired_cases = [
        c
        for c in cases
        if case_summary[c["case"]]["A"]["majority_correct"] is not None
        and case_summary[c["case"]]["B"]["majority_correct"] is not None
    ]
    paired_names = {c["case"] for c in paired_cases}
    excluded_incomplete = [c["case"] for c in cases if c["case"] not in paired_names]
    a_correct = sum(
        1 for c in paired_cases if case_summary[c["case"]]["A"]["majority_correct"]
    )
    b_correct = sum(
        1 for c in paired_cases if case_summary[c["case"]]["B"]["majority_correct"]
    )
    summary["paired"] = {
        "n_cases": len(paired_cases),
        "declared_cases": len(cases),
        "excluded_incomplete_cases": excluded_incomplete,
        "a_majority_correct": a_correct,
        "b_majority_correct": b_correct,
        "a_accuracy": (a_correct / len(paired_cases)) if paired_cases else None,
        "b_accuracy": (b_correct / len(paired_cases)) if paired_cases else None,
        "b_improved": sum(
            case_summary[c["case"]]["B"]["majority_correct"]
            and not case_summary[c["case"]]["A"]["majority_correct"]
            for c in paired_cases
        ),
        "b_regressed": sum(
            case_summary[c["case"]]["A"]["majority_correct"]
            and not case_summary[c["case"]]["B"]["majority_correct"]
            for c in paired_cases
        ),
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
