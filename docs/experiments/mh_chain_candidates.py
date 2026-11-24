#!/usr/bin/env python3
"""Build deterministic candidate support chains for MemoryAgentBench MH questions.

The MH subset provides no support annotations, so a chain is only a hypothesis
until a human signs it off. This script makes the hypothesis reproducible:

1. parse the numbered corpus facts into (subject, relation, object) triples;
2. link the entities the question mentions to those triples;
3. search forward at most three hops for a path that ends at the dataset gold;
4. recompute the same search over the *latest-serial* graph, so a candidate is
   only emitted when following each slot's newest fact reaches the gold;
5. report every competing same-slot value and whether the gold equals the final
   selected value.

Only candidates that need slot arbitration (at least one hop has competitors)
and whose latest-only traversal reproduces the gold are useful for the
answer-only A/B, so those are the defaults. Nothing here calls a model.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import defaultdict
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]

# (pattern, relation). Longest / most specific templates must come first.
FACT_TEMPLATES = (
    (r"^The headquarters of (?P<s>.+?) is located in the city of (?P<o>.+)$", "headquarters_city"),
    (r"^The univeristy where (?P<s>.+?) was educated is (?P<o>.+)$", "educated_at"),
    (r"^The chief executive officer of (?P<s>.+?) is (?P<o>.+)$", "ceo"),
    (r"^The director of (?P<s>.+?) is (?P<o>.+)$", "director"),
    (r"^The author of (?P<s>.+?) is (?P<o>.+)$", "author"),
    (r"^The chairperson of (?P<s>.+?) is (?P<o>.+)$", "chairperson"),
    (r"^The capital of (?P<s>.+?) is (?P<o>.+)$", "capital"),
    (r"^The official language of (?P<s>.+?) is (?P<o>.+)$", "official_language"),
    (r"^The company that produced (?P<s>.+?) is (?P<o>.+)$", "manufacturer"),
    (r"^The origianl broadcaster of (?P<s>.+?) is (?P<o>.+)$", "original_broadcaster"),
    (r"^The type of music that (?P<s>.+?) plays is (?P<o>.+)$", "music_genre"),
    (r"^The name of the current head of state in (?P<s>.+?) is (?P<o>.+)$", "head_of_state"),
    (r"^The name of the current head of the (?P<s>.+?) government is (?P<o>.+)$", "head_of_government"),
    (r"^The Prime Minister of (?P<s>.+?) is (?P<o>.+)$", "head_of_government"),
    (r"^(?P<s>.+?)'s child is (?P<o>.+)$", "child"),
    (r"^(?P<s>.+?) was born in the city of (?P<o>.+)$", "born_in_city"),
    (r"^(?P<s>.+?) died in the city of (?P<o>.+)$", "died_in_city"),
    (r"^(?P<s>.+?) plays the position of (?P<o>.+)$", "position"),
    (r"^(?P<s>.+?) is located in the continent of (?P<o>.+)$", "continent"),
    (r"^(?P<s>.+?) is located in the city of (?P<o>.+)$", "located_in_city"),
    (r"^(?P<s>.+?) is located in the country of (?P<o>.+)$", "located_in_country"),
    (r"^(?P<s>.+?) worked in the city of (?P<o>.+)$", "worked_in_city"),
    (r"^(?P<s>.+?) is married to (?P<o>.+)$", "spouse"),
    (r"^(?P<s>.+?) was founded by (?P<o>.+)$", "founded_by"),
    (r"^(?P<s>.+?) is associated with the sport of (?P<o>.+)$", "sport"),
    (r"^(?P<s>.+?) is a citizen of (?P<o>.+)$", "citizenship"),
    (r"^(?P<s>.+?) was performed by (?P<o>.+)$", "performer"),
    (r"^(?P<s>.+?) was founded in the city of (?P<o>.+)$", "founded_in_city"),
    (r"^(?P<s>.+?) speaks the language of (?P<o>.+)$", "languages"),
    (r"^(?P<s>.+?) is famous for (?P<o>.+)$", "notable_work"),
    (r"^(?P<s>.+?) is employed by (?P<o>.+)$", "employer"),
    (r"^(?P<s>.+?) is affiliated with the religion of (?P<o>.+)$", "religion"),
    (r"^(?P<s>.+?) was created in the country of (?P<o>.+)$", "country_of_origin"),
    (r"^(?P<s>.+?) works in the field of (?P<o>.+)$", "field_of_work"),
    (r"^(?P<s>.+?) was created by (?P<o>.+)$", "creator"),
    (r"^(?P<s>.+?) was developed by (?P<o>.+)$", "developer"),
    (r"^(?P<s>.+?) was written in the language of (?P<o>.+)$", "language_of_work"),
)
COMPILED_TEMPLATES = tuple((re.compile(p), r) for p, r in FACT_TEMPLATES)
SERIAL_LINE = re.compile(r"^(\d+)\.\s+(.*)$")
FROZEN = ("no3", "no5", "no8", "no11", "no16", "no35", "no36",
          "no39", "no40", "no43", "no57", "no92", "no94")


def normalise(text: str) -> str:
    """Case/punctuation-insensitive key for entity and answer comparison."""
    return re.sub(r"\s+", " ", text.strip().lower()).rstrip(".")


def parse_facts(context: str) -> list[dict]:
    """Parse ``<serial>. <subject> <relation> <object>.`` corpus lines."""
    facts = []
    for line in context.splitlines():
        line = line.strip()
        if not line:
            continue
        match = SERIAL_LINE.match(line)
        if not match:
            continue
        serial, body = int(match.group(1)), match.group(2)
        if body.endswith("."):
            body = body[:-1]
        for pattern, relation in COMPILED_TEMPLATES:
            found = pattern.match(body)
            if found:
                facts.append(
                    {
                        "serial": serial,
                        "subject": found.group("s").strip(),
                        "relation": relation,
                        "object": found.group("o").strip(),
                        "fact": line,
                    }
                )
                break
        else:
            facts.append(
                {
                    "serial": serial,
                    "subject": None,
                    "relation": None,
                    "object": None,
                    "fact": line,
                }
            )
    return facts


def slot_index(facts: list[dict]) -> dict[tuple[str, str], list[dict]]:
    index: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for fact in facts:
        if fact["subject"] is None:
            continue
        index[(normalise(fact["subject"]), fact["relation"])].append(fact)
    for rows in index.values():
        rows.sort(key=lambda row: row["serial"])
    return index


def forward_graph(index: dict[tuple[str, str], list[dict]]) -> dict[str, list[dict]]:
    graph: dict[str, list[dict]] = defaultdict(list)
    for (subject, relation), rows in index.items():
        for row in rows:
            graph[subject].append(
                {"relation": relation, "object": normalise(row["object"]), "serial": row["serial"]}
            )
    return graph


def latest_graph(index: dict[tuple[str, str], list[dict]]) -> dict[str, list[dict]]:
    graph: dict[str, list[dict]] = defaultdict(list)
    for (subject, relation), rows in index.items():
        newest = max(rows, key=lambda row: row["serial"])
        graph[subject].append(
            {
                "relation": relation,
                "object": normalise(newest["object"]),
                "serial": newest["serial"],
            }
        )
    return graph


def entity_vocabulary(facts: list[dict]) -> set[str]:
    vocabulary = set()
    for fact in facts:
        if fact["subject"] is None:
            continue
        vocabulary.add(normalise(fact["subject"]))
        vocabulary.add(normalise(fact["object"]))
    return vocabulary


def question_anchors(question: str, vocabulary: set[str], gold: str, limit: int = 12) -> list[str]:
    text = " " + re.sub(r"\s+", " ", question.lower()).strip() + " "
    gold_key = normalise(gold)
    hits = []
    for entity in vocabulary:
        if len(entity) < 3 or entity == gold_key:
            continue
        if re.search(r"(?<![a-z0-9])" + re.escape(entity) + r"(?![a-z0-9])", text):
            hits.append(entity)
    hits.sort(key=len, reverse=True)
    return hits[:limit]


def search_paths(graph: dict[str, list[dict]], anchors: list[str], gold: str,
                 max_depth: int = 3) -> list[list[dict]]:
    target = normalise(gold)
    paths: list[list[dict]] = []
    seen: set[tuple] = set()

    def walk(subject: str, trail: list[dict], visited: set[str]) -> None:
        if len(trail) >= max_depth:
            return
        for edge in graph.get(subject, []):
            step = {
                "subject": subject,
                "relation": edge["relation"],
                "object": edge["object"],
                "serial": edge["serial"],
            }
            if step["object"] == target:
                candidate = trail + [step]
                key = tuple((item["subject"], item["relation"], item["object"]) for item in candidate)
                if key not in seen:
                    seen.add(key)
                    paths.append(candidate)
                continue
            if step["object"] in visited:
                continue
            walk(step["object"], trail + [step], visited | {step["object"]})

    for anchor in anchors:
        walk(anchor, [], {anchor})
    paths.sort(key=len)
    return paths


def slot_table(index: dict[tuple[str, str], list[dict]], path: list[dict]) -> tuple[list[dict], bool]:
    """Return per-hop candidates plus whether the path follows the newest value."""
    slots = []
    latest_ok = True
    for step in path:
        rows = index.get((step["subject"], step["relation"]), [])
        selected = max(rows, key=lambda row: row["serial"]) if rows else None
        competitors = [row for row in rows if row is not selected]
        if selected is None or selected["serial"] != step["serial"]:
            latest_ok = False
        slots.append(
            {
                "subject": step["subject"],
                "relation": step["relation"],
                "selected_serial": step["serial"],
                "selected_value": step["object"],
                "selected_fact": next(
                    (row["fact"] for row in rows if row["serial"] == step["serial"]), None
                ),
                "selected_is_latest": selected is not None and selected["serial"] == step["serial"],
                "same_slot_competitors": [
                    {"serial": row["serial"], "value": normalise(row["object"]), "fact": row["fact"]}
                    for row in competitors
                ],
            }
        )
    return slots, latest_ok


def build_candidates(questions: list[str], answers: list[list[str]], facts: list[dict],
                     max_depth: int = 3) -> list[dict]:
    index = slot_index(facts)
    graph = forward_graph(index)
    resolved = latest_graph(index)
    vocabulary = entity_vocabulary(facts)
    out = []
    for number, (question, golds) in enumerate(zip(questions, answers)):
        case = f"no{number}"
        if case in FROZEN:
            continue
        gold = golds[0]
        anchors = question_anchors(question, vocabulary, gold)
        if not anchors:
            continue
        answer_paths = search_paths(graph, anchors, gold, max_depth)
        if not answer_paths:
            continue
        # A gold that only exists on an older branch is still recorded so the
        # packet can show why it was rejected; callers filter on the flag.
        latest_paths = search_paths(resolved, anchors, gold, max_depth)
        path = answer_paths[0]
        slots, latest_ok = slot_table(index, path)
        conflicting = [slot for slot in slots if slot["same_slot_competitors"]]
        out.append(
            {
                "case": case,
                "question": question,
                "gold": golds,
                "golds": golds,
                "anchors": anchors,
                "path": path,
                "slots": slots,
                "latest_path": latest_paths[0] if latest_paths else None,
                "latest_consistent": latest_ok and bool(latest_paths),
                "conflict_slots": len(conflicting),
                "conflict_signature": sorted(
                    f"{slot['subject']}|{slot['relation']}" for slot in conflicting
                ),
                "path_count": len(answer_paths),
                "expected_equals_final_selected_value": normalise(path[-1]["object"]) == normalise(gold),
            }
        )
    return out


def load_mh(path: Path | None, subset: str) -> tuple[list[str], list[list[str]], str, dict]:
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import memory_agent_bench as mab  # noqa: PLC0415

    parquet = path or mab.parquet_cache_path("main")
    rows = mab.load_rows(parquet)
    row = next(row for row in rows if mab.subset_of(row) == subset)
    metadata = row.get("metadata") or {}
    evidence = {
        "dataset": mab.HF_DATASET,
        "subset": subset,
        "source_path": str(parquet),
        "question_ids": (metadata.get("qa_pair_ids") or [])[: len(row["questions"])],
    }
    return list(row["questions"]), list(row["answers"]), row["context"], evidence


NOTES = {
    "no0": "author -> spouse -> citizenship; the two newest facts in every hop are the ones the gold follows.",
    "no15": "competition -> sport -> country of origin -> capital. The question does not name the sport, so confirm that this hop is intended.",
    "no33": "team -> sport -> country of origin -> continent.",
    "no50": "head of government -> spouse -> place of death; both offices resolve to the newest serial.",
    "no53": "spouse -> music genre -> country of origin.",
    "no75": "performer -> spouse -> religion.",
    "no85": "position -> sport -> country of origin.",
    "no87": "sport -> country of origin -> capital. Shares the association-football/Italy slot with no12 and no31.",
    "no99": "club -> sport -> country of origin -> official language.",
    "no9": "religion -> founder -> work location; third hop has no competitor.",
    "no12": "sport -> country of origin -> capital; the first hop has no competitor.",
    "no30": "performer -> education -> headquarters city; third hop has no competitor.",
    "no31": "sport -> country of origin -> head of government; third hop has no competitor.",
    "no48": "sport -> country of origin -> capital; the first hop (Galwegians RFC -> rugby union) has no competitor.",
    "no70": "spouse -> education -> headquarters city; the second hop has no competitor.",
}


def render_packet(payload: dict) -> str:
    """Render the human review packet for the machine-generated candidates."""
    lines = [
        "# MH held-out candidate chains — review packet",
        "",
        f"Status: **{payload['label_status']}**. Nothing here has been run through a model.",
        "",
        "## Provenance",
        "",
        f"- dataset: `{payload['dataset_evidence']['dataset']}` / `{payload['dataset_evidence']['subset']}`",
        f"- parquet: `{payload['dataset_evidence']['source_path']}`",
        f"- corpus facts parsed: {payload['corpus_facts_parsed']}/{payload['corpus_facts']}",
        f"- generator: `docs/experiments/mh_chain_candidates.py`",
        f"- frozen cases excluded: {', '.join(payload['frozen_cases'])}",
        "",
        "## Review rule",
        "",
        payload["review_rule"],
        "",
        "Every candidate below satisfies, mechanically:",
        "",
        "1. the chain starts at an entity named in the question;",
        "2. each hop is a fact whose subject is the previous hop's selected value;",
        "3. following the largest serial at every hop reaches the dataset gold;",
        "4. the gold is not mentioned in the question.",
        "",
        "What the machine cannot check is whether the hop sequence matches what the",
        "question actually asks. That is the part to sign off below.",
        "",
        "## Cases",
        "",
    ]
    for item in payload["selected"]:
        note = NOTES.get(item["case"], "")
        lines += [
            f"### {item['case']} — gold `{', '.join(item['golds'])}`",
            "",
            f"**Q:** {item['question']}",
            "",
            "| hop | subject | relation | selected | competitors |",
            "|---:|---|---|---|---|",
        ]
        for index, slot in enumerate(item["slots"], 1):
            competitors = "<br>".join(
                f"`{row['serial']}` {row['value']}" for row in slot["same_slot_competitors"]
            ) or "—"
            lines.append(
                f"| {index} | {slot['subject']} | {slot['relation']} | "
                f"`{slot['selected_serial']}` **{slot['selected_value']}** | {competitors} |"
            )
        lines += [
            "",
            f"- conflicting slots: {item['conflict_slots']} of {len(item['slots'])}",
            f"- reviewer note: {note}",
            f"- verdict: [ ] confirm  [ ] reject  [ ] needs correction",
            "",
        ]
    lines += [
        "## Correlated cases",
        "",
        "`no12`, `no31` and `no87` all depend on the same `association football |",
        "country_of_origin` slot resolving to Italy (serial 389 over 272). They are",
        "valid separate questions, but they are not independent observations of that",
        "mechanism; read their results together.",
        "",
        "## Sign-off",
        "",
        "- [ ] every hop sequence matches the question",
        "- [ ] every competitor list is complete for its slot",
        "- [ ] the gold equals the final selected value",
        "- [ ] cases that share a slot are accepted as correlated",
        "",
    ]
    return "\n".join(lines)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--subset", default="factconsolidation_mh_6k")
    ap.add_argument("--parquet", type=Path, default=None)
    ap.add_argument("--max-depth", type=int, default=3)
    ap.add_argument("--limit", type=int, default=15)
    ap.add_argument("--require-conflict", action="store_true", default=True)
    ap.add_argument("--include-nonconflict", dest="require_conflict", action="store_false")
    ap.add_argument("--out", type=Path, default=None)
    ap.add_argument("--packet", type=Path, default=None,
                    help="write the human review packet as Markdown")
    args = ap.parse_args(argv)

    if args.max_depth < 1:
        ap.error("--max-depth must be positive")
    questions, answers, context, evidence = load_mh(args.parquet, args.subset)
    facts = parse_facts(context)
    parsed = sum(1 for fact in facts if fact["subject"] is not None)
    candidates = build_candidates(questions, answers, facts, args.max_depth)
    selected = [
        candidate
        for candidate in candidates
        if candidate["latest_consistent"] and candidate["expected_equals_final_selected_value"]
        and (candidate["conflict_slots"] > 0 or not args.require_conflict)
    ]
    # Rank by evidence strength, then keep at most one case per identical
    # conflict-slot signature. Without this the packet fills up with near
    # duplicates that all turn on the same "association football -> Italy" slot.
    selected.sort(key=lambda item: (item["conflict_slots"], len(item["path"])), reverse=True)
    picked: list[dict] = []
    signatures: set[tuple] = set()
    for candidate in selected:
        signature = tuple(candidate["conflict_signature"])
        if signature in signatures:
            continue
        signatures.add(signature)
        picked.append(candidate)
        if len(picked) >= args.limit:
            break
    selected = picked
    payload = {
        "schema_version": 1,
        "label_status": "provisional_pending_human_confirmation",
        "dataset_evidence": evidence,
        "corpus_facts": len(facts),
        "corpus_facts_parsed": parsed,
        "frozen_cases": list(FROZEN),
        "candidates_considered": len(candidates),
        "selected": selected,
        "review_rule": (
            "For each hop, enumerate facts with the same subject and relation; the "
            "largest serial is the latest value. The chain must start at an entity "
            "named in the question and end at the dataset gold. A candidate is only "
            "usable if following the latest value at every hop still reaches the gold."
        ),
        "note": "Machine-generated candidate chains. They are not dataset support "
                "annotations and must be signed off by a human before any model run.",
    }
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(payload, indent=2, ensure_ascii=False) + "\n")
    if args.packet:
        args.packet.parent.mkdir(parents=True, exist_ok=True)
        args.packet.write_text(render_packet(payload))
    print(json.dumps(
        {
            "parsed": f"{parsed}/{len(facts)}",
            "candidates_considered": len(candidates),
            "selected": [item["case"] for item in selected],
        },
        indent=2,
    ))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
