#!/usr/bin/env python3
"""Summarise a fixed-context answer-arbitration results.json.

The harness already writes per-case majorities, but reporting also needs
cross-arm cost and a check for the failure mode this experiment is about:
did a wrong answer pick an older value from the same subject/relation slot?

This analyser is read-only. It never re-scores raw generations; it consumes the
harness's own ``correct``/``complete`` fields so the analysis cannot silently
change the scoring contract.

Usage:
    analyze_answer_arbitration.py results.json [--labels labels.json] [--out analysis.json]

``--labels`` is the optional case packet from the selector. It supplies
``older_competitors_in_context`` for the old-branch attribution; without it the
cost/accuracy sections still work and the attribution is reported as null.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import string
from collections import Counter
from pathlib import Path


def normalise(text: str) -> str:
    text = text.lower()
    text = "".join(ch for ch in text if ch not in string.punctuation)
    text = re.sub(r"\b(a|an|the)\b", " ", text)
    return " ".join(text.split())


def percentile(values: list[float], p: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    rank = (len(ordered) - 1) * p
    low = int(rank)
    high = min(low + 1, len(ordered) - 1)
    weight = rank - low
    return ordered[low] * (1 - weight) + ordered[high] * weight


def _mean(values: list[float]) -> float | None:
    return sum(values) / len(values) if values else None


def _rounded(value: float | None, digits: int = 1) -> float | None:
    return None if value is None else round(value, digits)


def _competitor_values(labels: dict | None, case: str) -> list[str]:
    if not labels:
        return []
    for item in labels.get("selected", []):
        if item.get("case") != case:
            continue
        return [
            value
            for row in item.get("older_competitors_in_context", [])
            for value in (row.get("object"), row.get("fact"))
            if value
        ]
    return []


def _is_old_branch(prediction: str, competitors: list[str]) -> bool:
    """Exact normalised equality with a listed older value.

    Deliberately not a substring test: a wrong answer that happens to mention an
    old value while answering something else is not evidence of selecting that
    branch.
    """
    pred = normalise(prediction)
    if not pred:
        return False
    return any(normalise(value) == pred for value in competitors)


def _strict_majority(values: list[str]) -> str | None:
    counts = Counter(values)
    highest = max(counts.values()) if counts else 0
    winners = [value for value, count in counts.items() if count == highest]
    if len(winners) != 1 or highest * 2 <= len(values):
        return None
    return winners[0]


def analyse(results: dict, labels: dict | None = None) -> dict:
    runs = results.get("runs", [])
    cases = results.get("design", {}).get("cases") or sorted({r["case"] for r in runs})
    model = results.get("models", {})

    arms: dict[str, dict] = {}
    for arm in ("A", "B"):
        rows = [r for r in runs if r.get("arm") == arm]
        complete = [r for r in rows if r.get("complete")]
        latencies = [r["latency_ms"] for r in rows if isinstance(r.get("latency_ms"), (int, float))]
        prompt_tokens = [
            r["usage"].get("prompt_tokens", 0)
            for r in rows
            if isinstance(r.get("usage"), dict)
        ]
        completion_tokens = [
            r["usage"].get("completion_tokens", 0)
            for r in rows
            if isinstance(r.get("usage"), dict)
        ]
        total_tokens = [
            r["usage"].get("total_tokens", 0)
            for r in rows
            if isinstance(r.get("usage"), dict)
        ]
        arms[arm] = {
            "n": len(rows),
            "complete_runs": len(complete),
            "incomplete_runs": len(rows) - len(complete),
            "completion_rate": (len(complete) / len(rows)) if rows else None,
            "correct_runs": sum(bool(r.get("correct")) for r in complete),
            "accuracy": (
                sum(bool(r.get("correct")) for r in complete) / len(complete)
                if complete
                else None
            ),
            "prompt_tokens_per_call": _rounded(_mean(prompt_tokens)),
            "completion_tokens_per_call": _rounded(_mean(completion_tokens)),
            "total_tokens": sum(total_tokens),
            "latency_samples": len(latencies),
            "latency_p50_ms": _rounded(percentile(latencies, 0.5)),
            "latency_mean_ms": _rounded(_mean(latencies)),
        }

    by_case = {c["case"]: c for c in results.get("cases", [])}
    per_case = []
    for case in cases:
        golds = by_case.get(case, {}).get("golds", [])
        entry: dict = {"case": case, "golds": golds}
        for arm in ("A", "B"):
            rows = [r for r in runs if r.get("case") == case and r.get("arm") == arm]
            complete = [r for r in rows if r.get("complete")]
            predictions = [r.get("prediction", "") for r in complete]
            majority = _strict_majority(predictions)
            entry[f"{arm.lower()}_predictions"] = predictions
            entry[f"{arm.lower()}_complete_runs"] = len(complete)
            entry[f"{arm.lower()}_correct_runs"] = sum(bool(r.get("correct")) for r in complete)
            entry[f"{arm.lower()}_majority_correct"] = (
                any(normalise(gold) in normalise(majority) for gold in golds)
                if majority is not None and golds
                else None
            )
            entry[f"{arm.lower()}_consistent"] = (
                len(set(normalise(p) for p in predictions)) == 1 if predictions else None
            )
        per_case.append(entry)

    paired = [
        entry
        for entry in per_case
        if entry["a_majority_correct"] is not None
        and entry["b_majority_correct"] is not None
    ]
    paired_cases = {entry["case"] for entry in paired}
    aggregate = {
        "arms": arms,
        "paired": {
            "n_cases": len(paired),
            "declared_cases": len(per_case),
            "excluded_incomplete_cases": [
                entry["case"] for entry in per_case if entry["case"] not in paired_cases
            ],
            "a_majority_correct": sum(bool(e["a_majority_correct"]) for e in paired),
            "b_majority_correct": sum(bool(e["b_majority_correct"]) for e in paired),
            "a_accuracy": (
                sum(bool(e["a_majority_correct"]) for e in paired) / len(paired)
                if paired
                else None
            ),
            "b_accuracy": (
                sum(bool(e["b_majority_correct"]) for e in paired) / len(paired)
                if paired
                else None
            ),
            "b_improved": sum(
                e["b_majority_correct"] and not e["a_majority_correct"] for e in paired
            ),
            "b_regressed": sum(
                e["a_majority_correct"] and not e["b_majority_correct"] for e in paired
            ),
            "a_consistent_cases": sum(e["a_consistent"] is True for e in per_case),
            "b_consistent_cases": sum(e["b_consistent"] is True for e in per_case),
        },
    }

    old_branch = {"available": labels is not None, "A": None, "B": None}
    if labels is not None:
        for arm in ("A", "B"):
            wrong = [
                r
                for r in runs
                if r.get("arm") == arm and r.get("complete") and not r.get("correct")
            ]
            hits = [
                r
                for r in wrong
                if _is_old_branch(
                    r.get("prediction", ""), _competitor_values(labels, r["case"])
                )
            ]
            old_branch[arm] = {
                "wrong_complete_runs": len(wrong),
                "wrong_run_equal_to_older_same_slot_value": len(hits),
                "cases": sorted({r["case"] for r in hits}),
            }

    return {
        "schema_version": 1,
        "created_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "source": {
            "purpose": results.get("purpose"),
            "created_at": results.get("created_at"),
            "model": model.get("answer_model"),
            "temperature": model.get("temperature"),
            "max_tokens": model.get("max_tokens"),
            "extra_body": model.get("extra_body"),
        },
        "aggregate": aggregate,
        "latency_note": (
            "latency_ms is wall-clock around the throttled request helper and "
            "includes VERIFY_MIN_CALL_INTERVAL_S (default 1.1 s) request spacing; "
            "compare the A/B difference rather than the absolute value"
        ),
        "old_branch_analysis": old_branch,
        "cases": per_case,
    }


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("results", type=Path)
    ap.add_argument("--labels", type=Path, default=None)
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args(argv)

    results = json.loads(args.results.read_text())
    labels = json.loads(args.labels.read_text()) if args.labels else None
    analysis = analyse(results, labels)
    text = json.dumps(analysis, indent=2, ensure_ascii=False) + "\n"
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(text)
        print(json.dumps(analysis["aggregate"], indent=2, ensure_ascii=False))
    else:
        print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
