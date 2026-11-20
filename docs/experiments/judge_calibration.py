#!/usr/bin/env python3
"""Replay the frozen H5/H7 judge-calibration cases.

This tool never regenerates a plan.  It sends the exact frozen
``judge_input.precaution`` / ``judge_input.plan`` /
``judge_input.ordered_conditions`` triple to the current strict judge and records
every attempt.  Human labels remain authoritative; when they are absent the
provisional assistant labels are reported only as provisional.

Before any model request, the manifest is checked against two separate
fingerprints: one for the immutable judge inputs and provenance, and one for the
editable human labels.  Source-artifact hashes declared by the manifest are also
re-verified.  The human-label gate is opt-in (``--require-human-labels``);
without it a replay is a diagnostic, not calibration acceptance.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

import judge_grounding as grounding

DEFAULT_CASES = Path(__file__).resolve().parent / "results/2026-11-19/post169/judge-calibration/cases.json"
COMPLETION_FAILURES = {
    "request_error",
    "empty_completion",
    "missing_finish_reason",
    "incomplete_completion",
}
JSON_FAILURES = {
    "malformed_json",
    "invalid_schema",
    "invalid_bindings",
    "invalid_binding_schema",
    "invalid_step",
    "invalid_quote",
}
GROUNDING_FAILURES = {"ungrounded_quote", "missing_or_ambiguous_step"}
ORDER_FAILURES = {
    "wrong_reference_order",
    "incomplete_ordered_bindings",
    "invalid_ordered_conditions",
}


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


REPO_ROOT = Path(__file__).resolve().parents[2]

# Fields that define what the judge is asked.  Labels, audit notes and observed
# outcomes are deliberately excluded: they are editable review state, not frozen
# model input.
IMMUTABLE_CASE_FIELDS = (
    "case_id",
    "benchmark",
    "source_artifact",
    "source_artifact_sha256",
    "pair_id",
    "task_type",
    "arm",
    "kind",
    "obligation_index",
    "obligation_source",
    "obligation",
    "claim_scope",
    "judge_input",
)


def canonical_sha256(value) -> str:
    payload = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def immutable_case(case: dict) -> dict:
    missing = [field for field in IMMUTABLE_CASE_FIELDS if field not in case]
    if missing:
        raise ValueError(
            f"{case.get('case_id', '<unknown>')}: missing immutable fields {missing}"
        )
    return {field: case[field] for field in IMMUTABLE_CASE_FIELDS}


def immutable_inputs_sha256(document: dict) -> str:
    return canonical_sha256([immutable_case(case) for case in document["cases"]])


def human_labels_sha256(document: dict) -> str:
    return canonical_sha256([
        {"case_id": case["case_id"], "human_label": case.get("human_label")}
        for case in document["cases"]
    ])


def repo_path(raw: str) -> Path:
    path = Path(raw)
    return path if path.is_absolute() else REPO_ROOT / path


def preflight(
    document: dict,
    *,
    require_human_labels: bool = False,
    expected_inputs_sha256: str | None = None,
) -> dict:
    """Verify frozen inputs and provenance before any network request."""
    errors: list[str] = []
    declared_inputs = document.get("immutable_inputs_sha256")
    inputs_sha256 = immutable_inputs_sha256(document)
    if declared_inputs is not None and declared_inputs != inputs_sha256:
        errors.append(
            "immutable_inputs_sha256 mismatch: "
            f"manifest={declared_inputs} computed={inputs_sha256}"
        )
    if expected_inputs_sha256 is not None and expected_inputs_sha256 != inputs_sha256:
        errors.append(
            "expected inputs fingerprint mismatch: "
            f"expected={expected_inputs_sha256} computed={inputs_sha256}"
        )
    labels_sha256 = human_labels_sha256(document)
    declared_labels = document.get("human_labels_sha256")
    if declared_labels is not None and declared_labels != labels_sha256:
        errors.append(
            "human_labels_sha256 mismatch: "
            f"manifest={declared_labels} computed={labels_sha256}"
        )

    source_artifacts: dict[str, str] = {}
    source_baseline = document.get("source_baseline")
    source_baseline_sha256 = document.get("source_baseline_sha256")
    if isinstance(source_baseline, str) and isinstance(source_baseline_sha256, str):
        path = repo_path(source_baseline)
        if not path.is_file():
            errors.append(f"source_baseline not found: {source_baseline}")
        else:
            actual = sha256_file(path)
            source_artifacts[source_baseline] = actual
            if actual != source_baseline_sha256:
                errors.append(
                    f"source_baseline hash mismatch: {source_baseline} "
                    f"declared={source_baseline_sha256} actual={actual}"
                )
    else:
        errors.append("manifest needs source_baseline and source_baseline_sha256")

    for case in document["cases"]:
        raw = case.get("source_artifact")
        expected = case.get("source_artifact_sha256")
        if not isinstance(raw, str) or not isinstance(expected, str):
            errors.append(f"{case['case_id']}: missing source_artifact provenance")
            continue
        if raw in source_artifacts:
            actual = source_artifacts[raw]
            if actual != expected:
                errors.append(
                    f"{case['case_id']}: source_artifact hash mismatch: {raw} "
                    f"declared={expected} actual={actual}"
                )
            continue
        path = repo_path(raw)
        if not path.is_file():
            errors.append(f"{case['case_id']}: source_artifact not found: {raw}")
            continue
        actual = sha256_file(path)
        source_artifacts[raw] = actual
        if actual != expected:
            errors.append(
                f"{case['case_id']}: source_artifact hash mismatch: {raw} "
                f"declared={expected} actual={actual}"
            )

    missing_human_labels = [
        case["case_id"] for case in document["cases"] if not case.get("human_label")
    ]
    if require_human_labels and missing_human_labels:
        errors.append(
            "human_label missing for "
            f"{len(missing_human_labels)} cases; calibration acceptance requires labels"
        )

    if errors:
        raise ValueError("preflight failed: " + "; ".join(errors))

    return {
        "immutable_inputs_sha256": inputs_sha256,
        "human_labels_sha256": labels_sha256,
        "source_baseline": {
            "path": source_baseline,
            "sha256": source_artifacts.get(source_baseline),
        },
        "source_artifacts": source_artifacts,
        "human_labels_complete": not missing_human_labels,
        "missing_human_labels": missing_human_labels,
        "human_label_gate_required": require_human_labels,
    }


def load_cases(path: Path) -> dict:
    document = json.loads(path.read_text())
    cases = document.get("cases")
    if not isinstance(cases, list) or not cases:
        raise ValueError("cases.json must contain a non-empty cases list")
    seen = set()
    for case in cases:
        if not isinstance(case, dict):
            raise ValueError("every case must be an object")
        case_id = case.get("case_id")
        if not isinstance(case_id, str) or not case_id.strip():
            raise ValueError("every case needs a non-empty case_id")
        if case_id in seen:
            raise ValueError(f"duplicate case_id: {case_id}")
        seen.add(case_id)
        judge_input = case.get("judge_input")
        if not isinstance(judge_input, dict):
            raise ValueError(f"{case_id}: missing judge_input")
        if not isinstance(judge_input.get("precaution"), str) or not judge_input["precaution"].strip():
            raise ValueError(f"{case_id}: judge_input.precaution must be a non-empty string")
        if not isinstance(judge_input.get("plan"), str) or not judge_input["plan"].strip():
            raise ValueError(f"{case_id}: judge_input.plan must be a non-empty string")
        ordered = judge_input.get("ordered_conditions")
        if ordered is not None and (
            not isinstance(ordered, list)
            or not ordered
            or any(not isinstance(item, str) or not item.strip() for item in ordered)
        ):
            raise ValueError(f"{case_id}: ordered_conditions must be null or a non-empty string list")
        label = case.get("human_label") or case.get("provisional_label")
        if label not in {"positive", "negative", "boundary"}:
            raise ValueError(f"{case_id}: unsupported label {label!r}")
    return document


def contract_flags(result: dict) -> dict:
    """Output-contract flags; semantic truth is reported separately."""
    reason = result.get("reason")
    status = result.get("status")
    positive = status == "positive"
    ordered = result.get("ordered_conditions") or []
    bindings = result.get("bindings") if isinstance(result.get("bindings"), list) else []
    completion_ok = reason not in COMPLETION_FAILURES
    json_ok = completion_ok and reason not in JSON_FAILURES
    quote_ok = None
    order_ok = None
    binding_count_ok = None
    if positive:
        quote_ok = all(
            isinstance(binding.get("grounding"), dict)
            and binding["grounding"].get("normalized_match") is True
            for binding in bindings
        ) if bindings else False
        order_ok = reason not in ORDER_FAILURES
        binding_count_ok = (not ordered) or len(bindings) in {1, len(ordered)}
    elif status == "negative":
        quote_ok = True
        order_ok = True
        binding_count_ok = True
    return {
        "completion_ok": completion_ok,
        "json_parse_ok": json_ok,
        "literal_quote_ok": quote_ok,
        "order_ok": order_ok,
        "binding_count_ok": binding_count_ok,
        "unknown": status == "unknown",
        "status": status,
        "reason": reason,
    }


def consensus(values: list[str]) -> str:
    if not values:
        return "no_repeats"
    return values[0] if all(value == values[0] for value in values) else "inconsistent"


def summarize(records: list[dict]) -> dict:
    attempts = [attempt for record in records for attempt in record["repeats"]]
    total = len(attempts)
    known = sum(1 for attempt in attempts if attempt["status"] in {"positive", "negative"})
    unknown = sum(1 for attempt in attempts if attempt["status"] == "unknown")
    positive = [attempt for attempt in attempts if attempt["status"] == "positive"]

    decisive_cases = [
        record for record in records
        if record["label_used"] in {"positive", "negative"}
        and record["consensus_status"] in {"positive", "negative"}
    ]
    false_positive = sum(
        1 for record in decisive_cases
        if record["label_used"] == "negative" and record["consensus_status"] == "positive"
    )
    false_negative = sum(
        1 for record in decisive_cases
        if record["label_used"] == "positive" and record["consensus_status"] == "negative"
    )
    true_positive = sum(
        1 for record in decisive_cases
        if record["label_used"] == "positive" and record["consensus_status"] == "positive"
    )
    true_negative = sum(
        1 for record in decisive_cases
        if record["label_used"] == "negative" and record["consensus_status"] == "negative"
    )

    return {
        "cases": len(records),
        "attempts": total,
        "known_attempts": known,
        "unknown_attempts": unknown,
        "unknown_rate": round(unknown / total, 4) if total else None,
        "json_parse_ok_rate": round(
            sum(1 for attempt in attempts if attempt["contract"]["json_parse_ok"]) / total, 4
        ) if total else None,
        "literal_quote_ok_rate_positive_claims": round(
            sum(1 for attempt in positive if attempt["contract"]["literal_quote_ok"]) / len(positive), 4
        ) if positive else None,
        "order_ok_rate_positive_claims": round(
            sum(1 for attempt in positive if attempt["contract"]["order_ok"]) / len(positive), 4
        ) if positive else None,
        "binding_count_ok_rate_positive_claims": round(
            sum(1 for attempt in positive if attempt["contract"]["binding_count_ok"]) / len(positive), 4
        ) if positive else None,
        "repeat_agreement_rate": round(
            sum(1 for record in records if record["repeat_agreement"]) / len(records), 4
        ) if records else None,
        "strict_repeat_agreement_rate": round(
            sum(1 for record in records if record.get("consensus_reason") != "inconsistent")
            / len(records), 4
        ) if records else None,
        "inconsistent_cases": [record["case_id"] for record in records if not record["repeat_agreement"]],
        "semantic_decisive_cases": len(decisive_cases),
        "true_positive": true_positive,
        "true_negative": true_negative,
        "false_positive": false_positive,
        "false_negative": false_negative,
        "boundary_cases": sum(1 for record in records if record["label_used"] == "boundary"),
        "label_sources": {
            "human": sum(1 for record in records if record["label_source"] == "human"),
            "provisional": sum(1 for record in records if record["label_source"] == "provisional"),
        },
    }


def replay(cases_doc: dict, *, model: str, base_url: str | None, repeats: int, only: set[str] | None) -> dict:
    from openai import OpenAI

    client = OpenAI(base_url=base_url, api_key=os.getenv("OPENAI_API_KEY") or "local")
    records = []
    selected = [
        case for case in cases_doc["cases"]
        if only is None or case["case_id"] in only
    ]
    if only is not None:
        missing = only - {case["case_id"] for case in selected}
        if missing:
            raise ValueError(f"unknown case ids: {sorted(missing)}")
    for case in selected:
        judge_input = case["judge_input"]
        attempts = []
        for _ in range(repeats):
            result = grounding.judge_quote(
                client,
                model,
                judge_input["precaution"],
                judge_input["plan"],
                base_url=base_url,
                ordered_conditions=judge_input.get("ordered_conditions"),
            )
            attempts.append({
                "status": result.get("status"),
                "valid": result.get("valid"),
                "avoided": result.get("avoided"),
                "reason": result.get("reason"),
                "step": result.get("step"),
                "quote": result.get("quote"),
                "bindings": result.get("bindings"),
                "reference_order": result.get("reference_order"),
                "raw": result.get("raw"),
                "metadata": result.get("metadata"),
                "contract": contract_flags(result),
            })
        statuses = [attempt["status"] for attempt in attempts]
        reasons = [attempt["reason"] for attempt in attempts]
        human_label = case.get("human_label")
        records.append({
            "case_id": case["case_id"],
            "benchmark": case.get("benchmark"),
            "pair_id": case.get("pair_id"),
            "kind": case.get("kind"),
            "provisional_label": case.get("provisional_label"),
            "human_label": human_label,
            "label_used": human_label or case.get("provisional_label"),
            "label_source": "human" if human_label else "provisional",
            "consensus_status": consensus(statuses),
            "consensus_reason": consensus(reasons),
            "repeat_agreement": consensus(statuses) != "inconsistent",
            "repeats": attempts,
        })
    return {
        "schema_version": 1,
        "created_at": datetime.now(timezone.utc).isoformat(),
        "cases_file_sha256": None,
        "judge": {
            "model": model,
            "endpoint": grounding.endpoint_record(base_url),
            "prompt_version": grounding.JUDGE_PROMPT_VERSION,
            "max_tokens": grounding.JUDGE_MAX_TOKENS,
            "retry_max_tokens": grounding.JUDGE_RETRY_MAX_TOKENS,
            "extra_body": grounding.configure_extra_body(),
        },
        "repeats": repeats,
        "label_status": "human_labels_required_before_final_semantic_metrics",
        "records": records,
        "summary": summarize(records),
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cases", type=Path, default=DEFAULT_CASES)
    parser.add_argument("--out", type=Path, required=False)
    parser.add_argument("--repeats", type=int, default=2)
    parser.add_argument("--model", default=os.getenv("VERIFY_JUDGE_MODEL", os.getenv("VERIFY_MODEL", "gpt-4o-mini")))
    parser.add_argument("--base-url", default=os.getenv("VERIFY_JUDGE_BASE_URL") or os.getenv("VERIFY_BASE_URL") or os.getenv("OPENAI_BASE_URL"))
    parser.add_argument("--only", default="", help="comma-separated case ids")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument(
        "--require-human-labels",
        action="store_true",
        help="refuse to replay until every case has a non-null human_label",
    )
    parser.add_argument(
        "--expect-inputs-sha256",
        default=None,
        help="external pin for immutable judge inputs (recommended for acceptance runs)",
    )
    args = parser.parse_args(argv)

    if args.repeats < 1:
        parser.error("--repeats must be positive")
    cases_path = args.cases.resolve()
    document = load_cases(cases_path)
    provenance = preflight(
        document,
        require_human_labels=args.require_human_labels,
        expected_inputs_sha256=args.expect_inputs_sha256,
    )
    only = {item.strip() for item in args.only.split(",") if item.strip()} or None

    if args.dry_run:
        selected = [case for case in document["cases"] if only is None or case["case_id"] in only]
        print(json.dumps({
            "cases": len(selected),
            "repeats": args.repeats,
            "model": args.model,
            "endpoint": grounding.endpoint_record(args.base_url),
            "prompt_version": grounding.JUDGE_PROMPT_VERSION,
            "extra_body": grounding.configure_extra_body(),
            "case_ids": [case["case_id"] for case in selected],
            "inputs_sha256": provenance["immutable_inputs_sha256"],
            "labels_sha256": provenance["human_labels_sha256"],
            "human_labels_complete": provenance["human_labels_complete"],
            "source_artifacts_verified": sorted(provenance["source_artifacts"]),
        }, indent=2))
        return 0

    grounding.configure_judge()
    artifact = replay(document, model=args.model, base_url=args.base_url,
                      repeats=args.repeats, only=only)
    artifact["inputs_sha256"] = provenance["immutable_inputs_sha256"]
    artifact["labels_sha256"] = provenance["human_labels_sha256"]
    artifact["provenance"] = provenance
    artifact["label_gate"] = {
        "required": provenance["human_label_gate_required"],
        "satisfied": provenance["human_labels_complete"],
        "missing": provenance["missing_human_labels"],
    }
    artifact["cases_file_sha256"] = sha256_file(cases_path)
    if args.out is None:
        print(json.dumps(artifact["summary"], indent=2))
    else:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(artifact, indent=2, ensure_ascii=False) + "\n")
        print(json.dumps(artifact["summary"], indent=2))
        print(f"wrote {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
