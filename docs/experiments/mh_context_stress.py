#!/usr/bin/env python3
"""Build a deterministic long-context stress variant of a frozen MH context.

The input context is the real retrieval arm's output. This script keeps every
delivered fact and every confirmed support-chain fact unchanged, then inserts
unrelated corpus facts until each question has ``original_n + --distractors``
facts. The default takes the 15 held-out two-hop cases from 20-38 delivered
facts to 60-78 delivered facts.

Distractors are sampled deterministically per case from the same corpus, with
four exclusions so that the stress condition changes context length/competition
for attention, not the answer:

1. facts already delivered in that question's context;
2. support-chain facts;
3. facts in a support-chain subject/relation slot, or sharing any chain entity;
4. facts containing the gold answer text.

Original facts keep their relative order. Distractors are distributed among the
gaps with a per-case seeded RNG, so the chain does not simply move to the end.
No model is called here.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import random
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_SOURCE = (
    REPO_ROOT / "docs/experiments/results/2026-11-24/mh-heldout-2hop/retrieval-context.json"
)
DEFAULT_CHAINS = (
    REPO_ROOT / "docs/experiments/results/2026-11-24/mh-heldout-2hop/chains.json"
)
DEFAULT_OUT = (
    REPO_ROOT
    / "docs/experiments/results/2026-11-24/mh-heldout-2hop-stress/stress-context.json"
)


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def short_case(question_id: str) -> str:
    return question_id.rsplit("_", 1)[-1]


def eligible_distractors(
    *,
    question: dict,
    chain_question: dict,
    all_facts: list[dict],
    facts_by_serial: dict[int, dict],
    mh,
) -> list[dict]:
    """Return corpus facts that cannot change the question's answer."""
    support_serials = [int(step["serial"]) for step in chain_question["support_chain"]]
    support_facts = [facts_by_serial[serial] for serial in support_serials]
    protected_slots = {
        (mh.normalise(fact["subject"]), fact["relation"]) for fact in support_facts
    }
    protected_entities = {
        mh.normalise(entity)
        for fact in support_facts
        for entity in (fact["subject"], fact["object"])
    }
    golds = [mh.normalise(gold) for gold in question["golds"]]
    delivered = set(question["retrieved_serials"])
    support = set(support_serials)

    eligible = []
    for fact in all_facts:
        if fact["subject"] is None:
            continue
        if fact["serial"] in delivered or fact["serial"] in support:
            continue
        if (mh.normalise(fact["subject"]), fact["relation"]) in protected_slots:
            continue
        if (
            mh.normalise(fact["subject"]) in protected_entities
            or mh.normalise(fact["object"]) in protected_entities
        ):
            continue
        normalised_fact = mh.normalise(fact["fact"])
        if any(gold in normalised_fact for gold in golds):
            continue
        eligible.append(fact)
    return eligible


def distribute_distractors(
    original: list[str], distractors: list[str], *, rng: random.Random
) -> list[str]:
    """Insert distractors into random gaps while preserving original order."""
    gaps = [0] * (len(original) + 1)
    for _ in distractors:
        gaps[rng.randrange(len(gaps))] += 1

    merged: list[str] = []
    cursor = 0
    for index, gap_size in enumerate(gaps):
        merged.extend(distractors[cursor:cursor + gap_size])
        cursor += gap_size
        if index < len(original):
            merged.append(original[index])
    if cursor != len(distractors):
        raise AssertionError("not all distractors were placed")
    return merged


def build_stress_context(
    *,
    source: dict,
    chains: dict,
    all_facts: list[dict],
    distractor_count: int,
    seed: int,
    mh,
) -> dict:
    if distractor_count <= 0:
        raise ValueError("distractor_count must be positive")
    facts_by_serial = {fact["serial"]: fact for fact in all_facts}
    chain_by_id = {item["question_id"]: item for item in chains["questions"]}
    result = json.loads(json.dumps(source))
    questions = result["results"][0]["questions"]
    per_case = []

    for question in questions:
        case = short_case(question["question_id"])
        chain_question = chain_by_id.get(question["question_id"])
        if chain_question is None:
            raise ValueError(f"{case}: no confirmed chain found")
        missing = [
            int(step["serial"])
            for step in chain_question["support_chain"]
            if int(step["serial"]) not in set(question["retrieved_serials"])
        ]
        if missing:
            raise ValueError(f"{case}: support chain missing from source context: {missing}")

        eligible = eligible_distractors(
            question=question,
            chain_question=chain_question,
            all_facts=all_facts,
            facts_by_serial=facts_by_serial,
            mh=mh,
        )
        if len(eligible) < distractor_count:
            raise ValueError(
                f"{case}: only {len(eligible)} eligible distractors, "
                f"need {distractor_count}"
            )
        rng = random.Random(f"{seed}:{case}")
        # Stable candidate order before sampling keeps the draw reproducible.
        pool = sorted(eligible, key=lambda fact: fact["serial"])
        picked = rng.sample(pool, distractor_count)
        merged = distribute_distractors(
            list(question["retrieved_contents"]),
            [fact["fact"] for fact in picked],
            rng=rng,
        )
        serials = [int(line.split(".", 1)[0]) for line in merged]
        if len(serials) != len(merged):
            raise AssertionError("serial/content mismatch")
        if not set(question["retrieved_serials"]).issubset(serials):
            raise AssertionError(f"{case}: original facts were lost")
        if len(set(serials)) != len(serials):
            raise AssertionError(f"{case}: duplicate serial in stressed context")

        original_n = len(question["retrieved_contents"])
        question["retrieved_contents"] = merged
        question["retrieved_serials"] = serials
        question["retrieved_n"] = len(merged)
        question["stress_variant"] = {
            "kind": "unrelated_distractor_injection",
            "source_retrieved_n": original_n,
            "distractors_added": len(picked),
            "final_retrieved_n": len(merged),
            "distractor_serials": [fact["serial"] for fact in picked],
            "support_chain_serials_preserved": [
                int(step["serial"]) for step in chain_question["support_chain"]
            ],
        }
        per_case.append(question["stress_variant"])

    result["generated_at"] = dt.datetime.now(dt.timezone.utc).isoformat()
    result["stress_spec"] = {
        "schema_version": 1,
        "kind": "unrelated_distractor_injection",
        "source_context_canonical_sha256": hashlib.sha256(
            json.dumps(source, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()
        ).hexdigest(),
        "chains_canonical_sha256": hashlib.sha256(
            json.dumps(chains, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()
        ).hexdigest(),
        "distractors_per_case": distractor_count,
        "seed": seed,
        "sampling": "runtime seed 'seed:case', sorted eligible serials, random.sample",
        "placement": "random gaps, original relative order preserved",
        "constraints": [
            "no already-delivered fact is removed or changed",
            "no support-chain fact is removed or changed",
            "no distractor shares a support-chain subject/relation slot",
            "no distractor shares a support-chain entity",
            "no distractor contains a gold answer",
        ],
        "per_case": per_case,
    }
    return result


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--source", type=Path, default=DEFAULT_SOURCE)
    ap.add_argument("--chains", type=Path, default=DEFAULT_CHAINS)
    ap.add_argument("--parquet", type=Path, default=None)
    ap.add_argument("--subset", default="factconsolidation_mh_6k")
    ap.add_argument("--distractors", type=int, default=40)
    ap.add_argument("--seed", type=int, default=20261124)
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    args = ap.parse_args(argv)

    if args.distractors <= 0:
        ap.error("--distractors must be positive")
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import mh_chain_candidates as mh  # noqa: PLC0415

    source = json.loads(args.source.read_text())
    chains = json.loads(args.chains.read_text())
    _, _, corpus_context, evidence = mh.load_mh(args.parquet, args.subset)
    all_facts = mh.parse_facts(corpus_context)
    parsed = sum(1 for fact in all_facts if fact["subject"] is not None)
    if parsed != len(all_facts):
        raise SystemExit(f"unparsed corpus facts: {parsed}/{len(all_facts)}")

    stress = build_stress_context(
        source=source,
        chains=chains,
        all_facts=all_facts,
        distractor_count=args.distractors,
        seed=args.seed,
        mh=mh,
    )
    stress["stress_spec"]["dataset_evidence"] = evidence
    stress["stress_spec"]["corpus_facts"] = len(all_facts)
    stress["stress_spec"]["source_context_file_sha256"] = sha256(args.source)
    stress["stress_spec"]["chains_file_sha256"] = sha256(args.chains)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(stress, indent=2, ensure_ascii=False) + "\n")

    lengths = [item["final_retrieved_n"] for item in stress["stress_spec"]["per_case"]]
    print(json.dumps(
        {
            "out": str(args.out),
            "cases": len(lengths),
            "context_lengths": {"min": min(lengths), "max": max(lengths)},
            "distractors_per_case": args.distractors,
            "seed": args.seed,
        },
        indent=2,
    ))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
