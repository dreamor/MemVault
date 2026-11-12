#!/usr/bin/env python3
"""Read-only stdlib score replay; no dataset fetch, server, client or model calls."""
from __future__ import annotations

import argparse
from collections import Counter
import json
import math
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import beam_adherence as beam
import memory_agent_bench as mab


def summary_differences(stored: dict, replayed: dict) -> dict:
    differences = {}
    for key, actual in replayed.items():
        expected = stored.get(key)
        equal = (math.isclose(expected, actual, rel_tol=1e-12, abs_tol=1e-12)
                 if type(expected) in (float, int) and type(actual) in (float, int)
                 else expected == actual)
        if not equal:
            differences[key] = {"stored": expected, "replayed": actual}
    return differences


def replay(path: Path) -> dict:
    with path.open(encoding="utf-8") as stream:
        original = json.load(stream)
    spec = original.get("harness_spec") or {}
    dataset = (spec.get("evidence") or {}).get("dataset") or {}
    source_path = dataset.get("source_path") or spec.get("parquet")
    result = {"file": str(path), "artifact_sha256": beam.content_hash(path),
              "dataset_source_path": source_path,
              "dataset_source_sha256": beam.content_hash(source_path),
              "note": "missing original dataset bytes are null; artifact hash is not a dataset hash"}
    if "probes" in original:
        records = original["probes"]
        summary = beam.summarise(records)
        validity = beam.validity_diagnostics(records)
        result.update({"benchmark": "BEAM", "summary": summary,
                       "summary_differences": summary_differences(original.get("summary") or {}, summary),
                       "paired_labels": validity["paired_labels"],
                       "paired_rubric_aggregate": validity["paired_rubric_aggregate"],
                       "n_invalid_pairs": validity["n_invalid_pairs"],
                       "n_rubric_source_missing": validity["n_rubric_source_missing"],
                       "semantic_annotation_alignment": None,
                       "schema_error_counts": dict(Counter(error for d in validity["diagnostics"]
                                                           for g in d["grades"].values() for error in g["errors"])),
                       "label_aggregate_disagreements": [{"n": d["n"], "probe_id": d["probe_id"], "arm": a}
                                                          for d in validity["diagnostics"]
                                                          for a, g in d["grades"].items()
                                                          if g.get("label_aggregate_disagreement") is True],
                       "legacy_judge_stability": original.get("judge_stability"),
                       "stability_note": "historical regrades cannot be repaired without complete original probes and fresh authorized judgments"})
    else:
        subsets = []
        for entry in original.get("results") or []:
            records = []
            for raw in entry["questions"]:
                q = dict(raw)
                for arm in ("search", "control", "inject"):
                    if isinstance(q.get(f"{arm}_prediction"), str):
                        q[f"{arm}_correct"] = mab.substring_exact_match(q[f"{arm}_prediction"], q["golds"])
                contents = q.get("retrieved_contents") or []
                for key, selected in (("gold_in_retrieval", contents), ("gold_in_top1", contents[:1]),
                                      ("gold_in_top5", contents[:5])):
                    q[key] = any(mab.substring_exact_match(c, q["golds"]) for c in selected)
                records.append(q)
            summary = mab.summarise_questions(records)
            subsets.append({"subset": entry.get("summary", {}).get("subset"), "summary": summary,
                            "summary_differences": summary_differences(entry.get("summary") or {}, summary),
                            "diagnostics": mab.mab_diagnostics(entry["questions"])})
        result.update({"benchmark": "MemoryAgentBench", "results": subsets})
    return beam.sanitize(result)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("files", nargs="+", type=Path, help="existing results JSON files (read only)")
    args = parser.parse_args(argv)
    # Stdout only: there is deliberately no --out or overwrite option.
    print(json.dumps([replay(path) for path in args.files], indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
