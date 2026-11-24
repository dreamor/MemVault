#!/usr/bin/env python3
"""Freeze the deterministic SH single-hop latest-slot case set.

The SH split already supplies official gold answers. This tool selects only
questions where the frozen retrieval arm delivered:

1. the gold fact, whose subject is named in the question;
2. exactly one same-subject/same-relation slot;
3. the gold fact is the largest serial in that slot across the whole corpus; and
4. at least one older same-slot competitor is also in the delivered context.

No chain is inferred, no answer is generated, and selection never reads a model
prediction or outcome. The output is a filtered context, a one-hop baseline and
a review table for the answer-only A/B.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import sys
from collections import Counter
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_SOURCE = (
    REPO_ROOT
    / "docs/experiments/results/2026-11-19/post169/mab/memvault-mab-post169.sh6k.raw.json"
)
DEFAULT_OUT_DIR = REPO_ROOT / "docs/experiments/results/2026-11-24/sh-latest-slot"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def short_case(question_id: str) -> str:
    return question_id.rsplit("_", 1)[-1]


def _gold_facts(question: dict, facts_by_serial: dict[int, dict], mh) -> list[dict]:
    text = " " + mh.normalise(question["question"]) + " "
    golds = [mh.normalise(gold) for gold in question["golds"]]
    candidates = []
    for serial in question["retrieved_serials"]:
        fact = facts_by_serial.get(int(serial))
        if fact is None or fact["subject"] is None:
            continue
        fact_text = mh.normalise(fact["fact"])
        if not any(gold in fact_text for gold in golds):
            continue
        if mh.normalise(fact["subject"]) not in text:
            continue
        candidates.append(fact)
    return candidates


def select_cases(source: dict, corpus_context: str, mh) -> tuple[list[dict], list[dict], dict]:
    """Return selected cases, exclusions and aggregate counts."""
    all_facts = mh.parse_facts(corpus_context)
    facts_by_serial = {fact["serial"]: fact for fact in all_facts}
    index = mh.slot_index(all_facts)
    questions = source["results"][0]["questions"]
    selected: list[dict] = []
    excluded: list[dict] = []

    for question in questions:
        case = short_case(question["qa_pair_id"])
        candidates = _gold_facts(question, facts_by_serial, mh)
        if not candidates:
            excluded.append({
                "case": case,
                "reason_class": "no_question_named_gold_fact",
                "reason": "no retrieved fact contains a gold and has a subject named in the question",
            })
            continue

        slots = {(mh.normalise(fact["subject"]), fact["relation"]) for fact in candidates}
        if len(slots) != 1:
            excluded.append({
                "case": case,
                "reason_class": "ambiguous_gold_slot",
                "reason": f"gold-bearing facts span {len(slots)} slots",
            })
            continue

        slot = next(iter(slots))
        slot_rows = index[slot]
        newest = max(slot_rows, key=lambda fact: fact["serial"])
        # A gold value can be a substring of an older value too; the relevant
        # candidate is the newest gold-bearing fact, not whichever came first in
        # retrieval order.
        gold_fact = max(candidates, key=lambda fact: fact["serial"])
        if newest["serial"] != gold_fact["serial"]:
            excluded.append({
                "case": case,
                "reason_class": "gold_is_not_latest",
                "reason": (
                    f"gold fact {gold_fact['serial']} is older than "
                    f"same-slot fact {newest['serial']}"
                ),
            })
            continue

        context_rows = [
            facts_by_serial[serial]
            for serial in question["retrieved_serials"]
            if int(serial) in facts_by_serial
            and facts_by_serial[int(serial)]["subject"] is not None
            and (
                mh.normalise(facts_by_serial[int(serial)]["subject"]),
                facts_by_serial[int(serial)]["relation"],
            ) == slot
        ]
        if len(context_rows) < 2:
            excluded.append({
                "case": case,
                "reason_class": "no_older_competitor_in_context",
                "reason": f"only {len(context_rows)} same-slot fact(s) delivered",
            })
            continue

        competitors = [row for row in context_rows if row["serial"] != gold_fact["serial"]]
        selected.append({
            "case": case,
            "question_id": question["qa_pair_id"],
            "question": question["question"],
            "golds": question["golds"],
            "subject": gold_fact["subject"],
            "relation": gold_fact["relation"],
            "selected_serial": gold_fact["serial"],
            "selected_fact": gold_fact["fact"],
            "same_slot_context_serials": [row["serial"] for row in context_rows],
            "older_competitors_in_context": [
                {"serial": row["serial"], "object": row["object"], "fact": row["fact"]}
                for row in competitors
            ],
            "all_slot_serials_in_corpus": [row["serial"] for row in slot_rows],
            "retrieved_n": question["retrieved_n"],
            "retrieved_serials": question["retrieved_serials"],
            "source_search_correct": question.get("search_correct"),
        })

    reasons = Counter(item["reason_class"] for item in excluded)
    counts = {
        "source_questions": len(questions),
        "selected": len(selected),
        "excluded": len(excluded),
        "excluded_by_reason": dict(sorted(reasons.items())),
        "selected_retrieved_min": (
            min(item["retrieved_n"] for item in selected) if selected else None
        ),
        "selected_retrieved_max": (
            max(item["retrieved_n"] for item in selected) if selected else None
        ),
        "selected_source_search_correct": sum(
            bool(item["source_search_correct"]) for item in selected
        ),
    }
    return selected, excluded, counts


def build_artifacts(source: dict, selected: list[dict], source_path: Path, mh) -> tuple[dict, dict]:
    """Build the harness context and one-hop baseline from selected questions."""
    selected_ids = {item["question_id"] for item in selected}
    by_id = {
        question["qa_pair_id"]: question
        for question in source["results"][0]["questions"]
    }
    context = {
        "schema_version": 1,
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "purpose": "frozen SH as-retrieved context for single-hop latest-slot A/B",
        "source_result": str(source_path),
        "source_result_sha256": sha256(source_path),
        "selection_rule": (
            "official gold is the latest serial in its single retrieved "
            "subject/relation slot, and at least one older same-slot fact is "
            "also delivered"
        ),
        "results": [{
            "questions": [
                by_id[question_id]
                for question_id in sorted(selected_ids)
            ]
        }],
        "run_status": "derived_from_existing_retrieval_run",
    }
    baseline = {
        "schema_version": 1,
        "source": "official SH gold; one-hop deterministic slot",
        "questions": [
            {
                "question_id": item["question_id"],
                "support_chain": [{"serial": item["selected_serial"]}],
                "golds": item["golds"],
            }
            for item in selected
        ],
    }
    return context, baseline


def render_packet(labels: dict) -> str:
    lines = [
        "# SH latest-slot case packet — official gold",
        "",
        f"Status: **{labels['label_status']}**.",
        "",
        "## Selection rule",
        "",
        labels["selection_rule"],
        "",
        "No model was called and no answer was generated while building this list.",
        "",
        f"Selected **{labels['counts']['selected']}/{labels['counts']['source_questions']}** questions.",
        "",
        "| Case | Question | Gold | Slot | Gold serial | Older competitors in context |",
        "|---|---|---|---|---:|---|",
    ]
    for item in labels["selected"]:
        competitors = "<br>".join(
            f"`{row['serial']}` {row['object']}"
            for row in item["older_competitors_in_context"]
        )
        lines.append(
            f"| {item['case']} | {item['question']} | {', '.join(item['golds'])} | "
            f"{item['subject']} | {item['relation']} | {item['selected_serial']} | {competitors} |"
        )
    lines += [
        "",
        "## Exclusions",
        "",
        "| Reason | Count |",
        "|---|---:|",
    ]
    for reason, count in labels["counts"]["excluded_by_reason"].items():
        lines.append(f"| {reason} | {count} |")
    lines += [
        "",
        "The dataset's official gold is authoritative here; the selection does not",
        "infer a multi-hop chain or retype an answer.",
        "",
    ]
    return "\n".join(lines)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--source", type=Path, default=DEFAULT_SOURCE)
    ap.add_argument("--parquet", type=Path, default=None)
    ap.add_argument("--subset", default="factconsolidation_sh_6k")
    ap.add_argument("--out-dir", type=Path, default=DEFAULT_OUT_DIR)
    args = ap.parse_args(argv)

    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import mh_chain_candidates as mh  # noqa: PLC0415
    import memory_agent_bench as mab  # noqa: PLC0415

    source = json.loads(args.source.read_text())
    rows = mab.load_rows(args.parquet or mab.parquet_cache_path("main"))
    row = next(row for row in rows if mab.subset_of(row) == args.subset)
    selected, excluded, counts = select_cases(source, row["context"], mh)
    context, baseline = build_artifacts(source, selected, args.source, mh)
    labels = {
        "schema_version": 1,
        "created_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "label_status": "official_gold_deterministic_single_slot",
        "purpose": "Select SH questions whose official gold is the latest fact in one delivered conflicting slot.",
        "source_result": str(args.source),
        "source_result_sha256": sha256(args.source),
        "dataset_evidence": {
            "dataset": mab.HF_DATASET,
            "subset": args.subset,
            "context_source_path": str(args.parquet or mab.parquet_cache_path("main")),
        },
        "selection_rule": (
            "The gold-bearing retrieved fact must have a subject named in the "
            "question; all gold-bearing facts must share one subject/relation slot; "
            "the gold serial must be the maximum in that slot across the full "
            "corpus; and at least one older same-slot fact must also be in the "
            "delivered context."
        ),
        "invariants": [
            "The gold is the dataset's official answer, not a model or human guess.",
            "No multi-hop chain is inferred.",
            "Selection does not use answer correctness from any model run.",
            "The original retrieval result is copied unchanged into the filtered context.",
        ],
        "counts": counts,
        "excluded": excluded,
        "selected": selected,
    }

    args.out_dir.mkdir(parents=True, exist_ok=True)
    (args.out_dir / "context.json").write_text(
        json.dumps(context, indent=2, ensure_ascii=False) + "\n"
    )
    (args.out_dir / "baseline.json").write_text(
        json.dumps(baseline, indent=2, ensure_ascii=False) + "\n"
    )
    (args.out_dir / "labels.json").write_text(
        json.dumps(labels, indent=2, ensure_ascii=False) + "\n"
    )
    (args.out_dir / "SH_LATEST_SLOT_PACKET.md").write_text(render_packet(labels))
    print(json.dumps({
        "out_dir": str(args.out_dir),
        "counts": counts,
        "cases": [item["case"] for item in selected],
    }, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
