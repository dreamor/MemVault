#!/usr/bin/env python3
"""
BEAM instruction/preference-adherence harness for MemVault.

Why this benchmark and this axis
--------------------------------
`docs/DESIGN.md` §2.6 records the load-bearing finding from the benchmark
survey: as context windows passed 1M tokens, the early retrieval-shaped
benchmarks (LoCoMo, LongMemEval) stopped discriminating — the naive "stuff the
entire history into context" baseline now scores competitively on them. So a
good LoCoMo number would prove nothing about memory architecture.

BEAM (arXiv 2510.27246, ICLR 2026) was built specifically to answer "can a 1M
context window replace a memory system", and two of its ten ability categories
are the ones MemVault is the only surveyed product built for:

    instruction_following   — did the agent honour a standing instruction?
    preference_following    — did the agent honour a stated preference?

Those are exactly MemVault's L3 (follow-through) and L6 (trusted memory) claims
(§11.1). Every other surveyed product can only *inject*; none measures whether
the injection was obeyed. This harness therefore measures the one public axis
where MemVault can win by a wide margin and win for an explainable reason.

The probes carry their own grading contract, which is what makes this tractable:

    instruction_being_tested / preference_being_tested   the standing rule
    expected_compliance                                  what compliance looks like
    compliance_indicators[]                              positive markers
    non_compliance_signs[]                               negative markers
    rubric[]                                             "LLM response should contain: …"
    source_chat_ids[]                                    which turns established the rule

`source_chat_ids` is provenance: it names the exact dialogue turns the rule came
from, so a seeded memory can carry its evidence rather than being asserted.

Design of the A/B
-----------------
Same shape as verify_h6.py / verify_h7.py (real `memvault-mcp` subprocess,
throwaway store, pre-registered scenarios), with one difference that matters:

    control     question -> LLM, no memory context
    experiment  question -> LLM, with MemVault's own injected block

The rule is seeded as a **MUST** memory with `human_reviewed=true` and
`ai_generated=false`, which is the path MemVault's trust model (§13.7, §8.3.4)
treats as human-authored — so it lands in the instruction block rather than as
passive context. That is the mechanism under test, not an incidental detail:
the claim is that instruction-layer injection changes behaviour, so the harness
must exercise instruction-layer injection.

Embeddings are OFF by default. That is deliberate and is itself a result worth
having: §6.1.2 says MUST resolves deterministically with zero embedding calls, so
the experiment arm must still inject correctly with no embedding provider
configured. `--embeddings` turns them on to measure the semantic path too.

On scoring honesty
------------------
BEAM's rubric items are *semantic* descriptions ("code blocks with syntax
highlighting"), not literal strings, so unlike H5's proper-noun stems they cannot
be scored by substring match. The primary metric is therefore an LLM judge —
which is also what BEAM itself uses, so it is the comparable choice.

Two things are done about the survey's warning that judge-prompt sensitivity
swings results by ~±10% (LongMemEval):

  1. `--judge-stability N` re-judges N probes with a reworded prompt and reports
     the self-agreement rate. A number published without it is not interpretable.
  2. Where an objective check *is* reliable it is recorded alongside the judge —
     for `format_instruction` probes, the presence of a fenced code block. The
     two are reported separately, never merged into one score.

The full harness specification (models, base URLs, MemVault commit, prompt
templates verbatim, scoring rules, split, n) is written into the results JSON.
§2.6 is explicit that a self-reported number without its harness spec is not
comparable and should not be cited.

Usage
-----
    pip install openai
    export OPENAI_API_KEY=...            # or VERIFY_BASE_URL for a local endpoint
    cargo build -p memvault-mcp

    # validate the harness end to end without spending any tokens
    python3 docs/experiments/beam_adherence.py --dry-run

    # a real run: 3 conversations from the 100K split
    python3 docs/experiments/beam_adherence.py --split 100K --conversations 3

    # with judge-stability measurement and engineering percentiles
    python3 docs/experiments/beam_adherence.py --split 100K --conversations 5 \
        --judge-stability 10 --out docs/experiments/beam_results.json

No pyarrow/pandas needed: rows are pulled from the HF datasets-server HTTP API
and cached on disk. `--from-file` replays a cached row file with no network.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import socket
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]

HF_ROWS_URL = "https://datasets-server.huggingface.co/rows"
HF_DATASET = "Mohammadta/BEAM"
CACHE_DIR = Path(tempfile.gettempdir()) / "memvault-beam-cache"

# The two BEAM ability categories that map onto MemVault's L3/L6 claims.
TARGET_CATEGORIES = ("instruction_following", "preference_following")

MODEL = os.getenv("VERIFY_MODEL", "gpt-4o-mini")
JUDGE_MODEL = os.getenv("VERIFY_JUDGE_MODEL", MODEL)
# Reasoning models spend completion tokens on hidden reasoning before emitting the
# JSON verdict. 400 was enough for GPT-4o-mini but can truncate a reasoning judge
# before it ever writes the JSON. Keep the old default for comparability and make
# the budget explicit in the persisted harness spec.
JUDGE_MAX_TOKENS = int(os.getenv("VERIFY_JUDGE_MAX_TOKENS", "400"))
# Optional provider-specific request body shared by both arms and the judge.
# Example: {"enable_thinking": false} for reasoning-model gateways that expose
# an explicit thinking switch. Empty means no extra fields.
_extra_body_raw = os.getenv("VERIFY_EXTRA_BODY", "").strip()
EXTRA_BODY = json.loads(_extra_body_raw) if _extra_body_raw else None
BASE_URL = os.getenv("VERIFY_BASE_URL") or os.getenv("OPENAI_BASE_URL") or None

# ─── Prompt templates (emitted verbatim into the results spec) ────────────
# Held as module constants on purpose: §2.6 requires the harness spec to carry
# the prompts, and a template buried inside a function is easy to change without
# noticing that every previously published number stopped being comparable.

BASE_SYSTEM = (
    "You are an engineering assistant. Answer the user's question helpfully and "
    "concisely. If a memory context block is provided, treat it as standing "
    "instructions from the user that apply to this conversation."
)

JUDGE_SYSTEM = (
    "You are a strict grader. You are given a standing instruction, a user "
    "question, and an assistant response. Decide whether the response complies "
    "with the instruction. Judge ONLY compliance with that instruction, not "
    "overall helpfulness or correctness of the answer."
)

JUDGE_TEMPLATE = """Standing instruction under test:
{instruction}

Expected compliance:
{expected}

Compliance indicators (positive markers):
{indicators}

Non-compliance signs (negative markers):
{negatives}

Rubric:
{rubric}

User question:
{question}

Assistant response:
{response}

Grade each rubric item as satisfied or not, then give an overall verdict.
Respond with JSON only:
{{"rubric_met": [<bool per rubric item, in order>],
  "compliant": <bool>,
  "non_compliance_sign_present": <bool>,
  "reason": "<one sentence>"}}"""

# Reworded variant for the judge-stability measurement. Same semantics,
# different surface form: if the verdict flips under a rewording, the metric is
# judge-sensitive and the headline number needs that caveat attached.
JUDGE_TEMPLATE_REWORDED = """An instruction was in force for this conversation:
{instruction}

Compliance would look like: {expected}

Markers of compliance: {indicators}
Markers of non-compliance: {negatives}
Checklist: {rubric}

The user asked: {question}

The assistant replied: {response}

Work through the checklist item by item, then decide overall whether the reply
complied with the instruction. Answer in JSON only:
{{"rubric_met": [<bool per checklist item, in order>],
  "compliant": <bool>,
  "non_compliance_sign_present": <bool>,
  "reason": "<one sentence>"}}"""


# ─── Data acquisition ─────────────────────────────────────────────────────


def fetch_rows(split: str, offset: int, length: int) -> list[dict]:
    """Pull BEAM rows from the HF datasets-server HTTP API.

    Uses the rows endpoint rather than the parquet files so the harness needs no
    pyarrow/pandas — the parquet route also drags in a ~700KB-per-row download
    for the 1M split, which is a lot of bytes to move just to read two fields.
    """
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    cache = CACHE_DIR / f"{HF_DATASET.replace('/', '_')}__{split}__{offset}__{length}.json"
    if cache.exists():
        with cache.open(encoding="utf-8") as fh:
            return json.load(fh)

    qs = urllib.parse.urlencode(
        {"dataset": HF_DATASET, "config": "default", "split": split,
         "offset": offset, "length": length}
    )
    req = urllib.request.Request(f"{HF_ROWS_URL}?{qs}", headers={"User-Agent": "memvault-beam/1.0"})
    with urllib.request.urlopen(req, timeout=300) as resp:
        payload = json.loads(resp.read())
    rows = [r["row"] for r in payload.get("rows", [])]
    with cache.open("w", encoding="utf-8") as fh:
        json.dump(rows, fh)
    return rows


def load_rows_from_file(path: Path) -> list[dict]:
    """Replay a cached row file (or a single-row datasets-server response)."""
    with path.open(encoding="utf-8") as fh:
        data = json.load(fh)
    if isinstance(data, dict) and "rows" in data:
        return [r["row"] for r in data["rows"]]
    if isinstance(data, dict):
        return [data]
    return data


def parse_probes(row: dict) -> dict[str, list[dict]]:
    """Decode `probing_questions`.

    The field is a *Python literal* string (single-quoted), not JSON — the
    dataset was written from a Python pipeline and the column kept its repr.
    `json.loads` fails on it; `ast.literal_eval` is the correct reader. Imported
    lazily so `--help` stays cheap.
    """
    import ast

    raw = row.get("probing_questions") or ""
    if not raw.strip():
        return {}
    try:
        parsed = ast.literal_eval(raw)
    except (ValueError, SyntaxError):
        # Fall back to JSON in case a future release normalises the column.
        try:
            parsed = json.loads(raw)
        except json.JSONDecodeError:
            return {}
    if not isinstance(parsed, dict):
        return {}
    return {k: v for k, v in parsed.items() if isinstance(v, list)}


def flatten_chat(row: dict) -> dict[int, dict]:
    """Index `chat` turns by their numeric id.

    `chat` is a list of session groups, each a list of turns. Probes reference
    turns through `source_chat_ids`, so the ids have to be resolvable.
    """
    out: dict[int, dict] = {}
    for group in row.get("chat") or []:
        turns = group if isinstance(group, list) else [group]
        for turn in turns:
            if isinstance(turn, dict) and "id" in turn:
                out[int(turn["id"])] = turn
    return out


def resolve_time_anchor(chat: dict[int, dict], turn_ids: list[int]) -> tuple[str | None, str]:
    """Resolve the effective time anchor for a set of source turns.

    BEAM stamps the anchor on the **first turn of each session only** — a
    188-turn conversation with three sessions of 60/56/72 turns carries exactly
    three anchors, at ids 0, 60 and 116. So reading only the source turn's own
    field yields None for almost every probe, and a bounded backwards walk fails
    too: a turn deep in a 72-turn session is further from its anchor than any
    fixed window short of the session length.

    The walk is therefore unbounded, down to turn 0. That is correct rather than
    merely generous because the anchor is a session-level property: the nearest
    preceding anchored turn *is* the session this turn belongs to.

    The second return value says how the answer was obtained — `direct`,
    `inherited:<id>`, or `none` — because an inherited anchor is a sound reading
    of the data but still a reading, and a results file that did not distinguish
    the two would overstate what the dataset states.
    """
    for tid in turn_ids:
        own = (chat.get(tid) or {}).get("time_anchor")
        if own:
            return own, "direct"
    for tid in turn_ids:
        probe = tid - 1
        while probe >= 0:
            cand = (chat.get(probe) or {}).get("time_anchor")
            if cand:
                return cand, f"inherited:{probe}"
            probe -= 1
    return None, "none"


# ─── Probe normalisation ──────────────────────────────────────────────────


def normalise_probe(category: str, raw: dict, chat: dict[int, dict]) -> dict | None:
    """Flatten the two categories' differing field names onto one shape.

    `instruction_following` says `instruction_being_tested`;
    `preference_following` says `preference_being_tested`. Everything else
    lines up. Returning None for a malformed probe keeps a partial dataset from
    silently shrinking the denominator.
    """
    rule = raw.get("instruction_being_tested") or raw.get("preference_being_tested")
    question = raw.get("question")
    if not rule or not question:
        return None
    source_ids = [int(i) for i in (raw.get("source_chat_ids") or []) if isinstance(i, (int, float))]
    source_turns = [chat[i] for i in source_ids if i in chat]
    anchor, anchor_origin = resolve_time_anchor(chat, source_ids)
    return {
        "category": category,
        "question": question.strip(),
        "rule": rule.strip(),
        "expected_compliance": (raw.get("expected_compliance") or "").strip(),
        "compliance_indicators": raw.get("compliance_indicators") or [],
        "non_compliance_signs": raw.get("non_compliance_signs") or [],
        "rubric": raw.get("rubric") or [],
        "difficulty": raw.get("difficulty"),
        "subtype": raw.get("instruction_type") or raw.get("preference_type"),
        "source_chat_ids": source_ids,
        # Provenance: the actual turns that established the rule. Seeded into
        # the memory as evidence so the injected block is grounded rather than
        # asserted — the same principle as §13.7's trust gate.
        "source_turns": source_turns,
        "time_anchor": anchor,
        "time_anchor_origin": anchor_origin,
    }


# ─── MemVault server (same harness as verify_h6 / verify_h7) ──────────────


def find_binary(explicit: str | None) -> Path:
    if explicit:
        return Path(explicit)
    for rel in ["target/debug/memvault-mcp", "target/release/memvault-mcp"]:
        p = REPO_ROOT / rel
        if p.exists():
            return p
    raise SystemExit(
        "memvault-mcp binary not found — build it (cargo build -p memvault-mcp) or pass --binary"
    )


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class MemVaultServer:
    def __init__(self, binary: Path, embeddings: bool):
        self.port = free_port()
        self.base = f"http://127.0.0.1:{self.port}"
        self.dbdir = tempfile.mkdtemp(prefix="memvault-beam-")
        env = {**os.environ}
        if not embeddings:
            # Off by default: §6.1.2 says MUST resolves deterministically with
            # zero embedding calls, so the experiment arm must still inject with
            # no provider configured. Leaving embeddings on would hide whether
            # that claim actually holds.
            env["MEMVAULT_EMBEDDING_PROVIDER"] = "off"
        self.proc = subprocess.Popen(
            [str(binary), "--db", f"{self.dbdir}/beam.db",
             "--transport", "http", "--port", str(self.port)],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        for _ in range(60):
            time.sleep(0.5)
            try:
                with urllib.request.urlopen(f"{self.base}/health", timeout=2) as r:
                    if r.read().strip() == b"ok":
                        return
            except Exception:
                if self.proc.poll() is not None:
                    raise SystemExit("memvault-mcp exited during startup")
        raise SystemExit("memvault-mcp did not become healthy in time")

    def stop(self):
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()

    def _req(self, method: str, path: str, body=None, timeout: int = 60):
        req = urllib.request.Request(
            f"{self.base}{path}",
            data=json.dumps(body).encode() if body is not None else None,
            headers={"Content-Type": "application/json"},
            method=method,
        )
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return json.loads(r.read())

    def seed_rule(self, probe: dict) -> str:
        """Seed one probe's rule as a human-authored MUST memory.

        `human_reviewed=true` + `ai_generated=false` is not a convenience: it is
        the combination §13.7/§8.3.4 treat as human-authored, which is what
        gets the memory into the *instruction* block instead of passive context.
        The mechanism under test is instruction-layer injection, so the seed has
        to go through the trusted path or the experiment measures nothing.

        `instruction` carries the rule text because that is the field the Format
        Engine renders as a directive; `content` carries the same text so keyword
        retrieval can find it.
        """
        body = {
            "content": probe["rule"],
            "instruction": probe["rule"],
            "type": "preference" if probe["category"] == "preference_following" else "fact",
            "priority": "MUST",
            "namespace": "global",
            "agent_id": "beam-harness",
            "agent_type": "coding-assistant",
            "human_reviewed": True,
            "ai_generated": False,
        }
        # NOTE: `occurred_at` is deliberately NOT sent here. The event-time axis
        # is only settable through the *extraction* paths (`POST /api/extract`,
        # MCP `extract_memories`), not through a direct save — a coherent split,
        # since event-time describes the source conversation an extraction came
        # from. Sending it on this endpoint would be silently dropped, and a
        # request body carrying a field the server ignores would make the results
        # file claim provenance the memory does not actually have. The resolved
        # anchor is still recorded per-probe as dataset metadata
        # (`occurred_at_resolved` / `time_anchor_origin`), labelled as such.
        # The gap — a transcript-ingested memory saved directly loses its event
        # date — is tracked in DESIGN.md §15.
        resp = self._req("POST", "/api/memories", body)
        return (resp.get("data") or {}).get("id", "")

    def inject(self, context_hint: str) -> tuple[str, float, str]:
        """Return (formatted injection block, seconds spent, format engine used).

        Timed separately from the LLM call on purpose: §2.6 requires p50/p95
        latency next to accuracy, and the number that matters is MemVault's own
        injection latency, not the model's generation time. Mixing them would
        make the two-phase-injection advantage (§6.1.2) invisible.

        The format is returned because it is the thing actually under test. A
        MUST memory that reached the agent as ordinary REFERENCE context would
        still make `rule_in_injection` true while measuring a completely
        different mechanism — instruction-layer injection (§6.2) versus passive
        context. Asserting `MustRef` is what keeps the experiment honest about
        which claim it is testing.
        """
        t0 = time.perf_counter()
        resp = self._req("POST", "/api/session",
                         {"agent_id": "claude-desktop", "context_hint": context_hint})
        elapsed = time.perf_counter() - t0
        data = resp.get("data") or {}
        return data.get("formatted", ""), elapsed, data.get("format", "")

    def clear(self, memory_id: str):
        """Remove one memory so the next probe starts from an empty store.

        Per-probe isolation matters: MUST memories are always injected, so
        leaving probe N's rule in the store would leak it into probe N+1's
        context and inflate the experiment arm for the wrong reason.
        """
        try:
            self._req("DELETE", f"/api/memories/{memory_id}")
        except Exception:
            pass


def _parse_time_anchor(anchor: str | None) -> str | None:
    """BEAM's `time_anchor` looks like `March-15-2024`; normalise to RFC3339."""
    if not anchor:
        return None
    for fmt in ("%B-%d-%Y", "%B %d, %Y", "%Y-%m-%d"):
        try:
            return dt.datetime.strptime(anchor.strip(), fmt).replace(
                tzinfo=dt.timezone.utc
            ).isoformat()
        except ValueError:
            continue
    return None


# ─── LLM ──────────────────────────────────────────────────────────────────


def make_client():
    try:
        from openai import OpenAI
    except ImportError:
        raise SystemExit("ERROR: pip install openai")
    return OpenAI(base_url=BASE_URL, api_key=os.getenv("OPENAI_API_KEY") or "local")


def answer(client, question: str, memory_block: str, max_tokens: int) -> str:
    """One arm of the A/B. `memory_block` empty == control."""
    system = BASE_SYSTEM
    if memory_block:
        system = f"{BASE_SYSTEM}\n\n{memory_block}"
    # Reasoning-model gateways occasionally return an empty assistant message
    # when a request is truncated or the backend aborts generation. An empty
    # answer is not evidence about the memory, so retry it a couple of times
    # before recording the probe as an empty response.
    text = ""
    for attempt in range(3):
        resp = client.chat.completions.create(
            model=MODEL,
            messages=[{"role": "system", "content": system},
                      {"role": "user", "content": question}],
            max_tokens=max_tokens,
            temperature=0.0,
            **(({"extra_body": EXTRA_BODY}) if EXTRA_BODY else {}),
        )
        text = resp.choices[0].message.content or ""
        if text.strip():
            return text
        if attempt < 2:
            time.sleep(0.5 * (attempt + 1))
    return text


def _parse_json_loose(text: str) -> dict:
    """Tolerant JSON extraction — models wrap verdicts in prose or fences.

    Same best-effort pattern as `bench.rs`'s judge parsing and
    `effectiveness.rs`; a judge that fails to parse must degrade to "no verdict"
    rather than crash the run.
    """
    if not text:
        return {}
    fenced = re.search(r"```(?:json)?\s*(\{.*?\})\s*```", text, re.DOTALL)
    candidate = fenced.group(1) if fenced else None
    if candidate is None:
        brace = re.search(r"\{.*\}", text, re.DOTALL)
        candidate = brace.group(0) if brace else None
    if candidate is None:
        return {}
    try:
        parsed = json.loads(candidate)
        return parsed if isinstance(parsed, dict) else {}
    except json.JSONDecodeError:
        return {}


def judge(client, probe: dict, response: str, reworded: bool = False) -> dict:
    template = JUDGE_TEMPLATE_REWORDED if reworded else JUDGE_TEMPLATE
    prompt = template.format(
        instruction=probe["rule"],
        expected=probe["expected_compliance"] or "(not specified)",
        indicators="\n".join(f"- {i}" for i in probe["compliance_indicators"]) or "(none)",
        negatives="\n".join(f"- {n}" for n in probe["non_compliance_signs"]) or "(none)",
        rubric="\n".join(f"- {r}" for r in probe["rubric"]) or "(none)",
        question=probe["question"],
        response=response,
    )
    resp = client.chat.completions.create(
        model=JUDGE_MODEL,
        messages=[{"role": "system", "content": JUDGE_SYSTEM},
                  {"role": "user", "content": prompt}],
        max_tokens=JUDGE_MAX_TOKENS,
        temperature=0.0,
        **(({"extra_body": EXTRA_BODY}) if EXTRA_BODY else {}),
    )
    verdict = _parse_json_loose(resp.choices[0].message.content or "")
    rubric_met = verdict.get("rubric_met")
    if not isinstance(rubric_met, list):
        rubric_met = []
    return {
        "compliant": bool(verdict.get("compliant")),
        "rubric_met": [bool(x) for x in rubric_met],
        "rubric_coverage": (sum(1 for x in rubric_met if x) / len(rubric_met)) if rubric_met else None,
        "non_compliance_sign_present": bool(verdict.get("non_compliance_sign_present")),
        "reason": str(verdict.get("reason") or ""),
        "parsed": bool(verdict),
    }


def objective_format_check(probe: dict, response: str) -> bool | None:
    """The one objective check that is actually reliable on this data.

    BEAM's `format_instruction` probes ask for fenced/highlighted code, and a
    markdown fence is a literal, unambiguous artifact — no judge needed. This is
    reported *alongside* the judge, never merged into it, so a disagreement
    between the two is visible instead of averaged away.

    Returns None for probes where no objective check applies.
    """
    if probe.get("subtype") != "format_instruction":
        return None
    # BEAM's format_instruction bucket is broader than code formatting: some
    # probes ask for status codes, step-by-step prose, or comparative analysis.
    # Only apply the literal markdown-fence check when the rule actually asks for
    # code/snippets; otherwise "no fence" is not evidence of non-compliance.
    rule = str(probe.get("rule") or "").lower()
    if not any(k in rule for k in ("code snippet", "code block", "syntax highlight", "fenced", "markdown")):
        return None
    return "```" in response


# ─── Metrics ──────────────────────────────────────────────────────────────


def percentile(values: list[float], pct: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    k = (len(ordered) - 1) * (pct / 100.0)
    lo, hi = int(k), min(int(k) + 1, len(ordered) - 1)
    return ordered[lo] + (ordered[hi] - ordered[lo]) * (k - lo)


def estimate_tokens(text: str) -> int:
    """~4 chars/token, matching `bench.rs`'s existing `injected_chars` heuristic.

    Deliberately the same estimator as the in-repo benchmark rather than a real
    tokenizer count: §2.6 asks for a reported token figure, and a figure
    computed differently from every other MemVault number is not comparable with
    them. It is an estimate and is labelled as one in the output.
    """
    return len(text) // 4


# ─── Run ──────────────────────────────────────────────────────────────────


def run(args, probes: list[dict]) -> dict:
    results: list[dict] = []
    inject_latencies: list[float] = []
    injected_tokens: list[int] = []

    client = None
    server = None
    if not args.dry_run:
        client = make_client()
        if not os.getenv("OPENAI_API_KEY") and BASE_URL is None:
            raise SystemExit("ERROR: set OPENAI_API_KEY or VERIFY_BASE_URL (local endpoint)")

    if not args.dry_run or args.with_server:
        server = MemVaultServer(find_binary(args.binary), args.embeddings)
        print(f"MemVault server: {server.base} (temp db, embeddings "
              f"{'on' if args.embeddings else 'off'})")

    try:
        for n, probe in enumerate(probes[: args.probes], 1):
            rec: dict = {
                "n": n,
                "category": probe["category"],
                "difficulty": probe["difficulty"],
                "subtype": probe["subtype"],
                "question": probe["question"],
                "rule": probe["rule"],
                "rubric_items": len(probe["rubric"]),
                "source_chat_ids": probe["source_chat_ids"],
                "has_provenance": bool(probe["source_turns"]),
                # Dataset metadata, NOT a claim about the seeded memory: the
                # direct-save path cannot set event-time (see seed_rule), so this
                # records what BEAM says the rule's session date was, not what
                # MemVault stored. Keeping the distinction is what stops the
                # results file from implying provenance the memory lacks.
                "occurred_at_resolved": _parse_time_anchor(probe.get("time_anchor")),
                # Recorded because an inherited anchor is an interpretation of the
                # data, not a fact stated by it — see resolve_time_anchor.
                "time_anchor_origin": probe.get("time_anchor_origin"),
            }

            if args.dry_run and server is None:
                # No server, no LLM: still exercise loading, normalisation,
                # provenance resolution and time-anchor parsing, which is most of
                # what can be wrong before spending tokens.
                # `seeded: None` means "not attempted" — distinct from `False`,
                # which means the store rejected the write. Conflating them would
                # report a dry run as four seed failures.
                rec.update({"dry_run": True, "seeded": None})
                results.append(rec)
                continue

            mem_id = server.seed_rule(probe) if server else ""
            rec["memory_id"] = mem_id
            rec["seeded"] = bool(mem_id)

            block, latency, fmt = ("", None, "")
            if server:
                block, latency, fmt = server.inject(probe["question"])
                inject_latencies.append(latency)
                injected_tokens.append(estimate_tokens(block))
            rec["injection_latency_s"] = latency
            rec["injection_format"] = fmt
            rec["injected_chars"] = len(block)
            rec["injected_tokens_est"] = estimate_tokens(block)
            # A MUST memory that did not reach the instruction block is a harness
            # failure, not a null result — record it loudly rather than letting it
            # look like "memory did not help". Both halves matter: the rule text
            # being present, and it arriving via the instruction-layer format
            # rather than as ordinary context.
            rec["rule_in_injection"] = bool(block) and (
                probe["rule"][:40].lower() in block.lower()
            )
            rec["instruction_layer_used"] = fmt == "MustRef"

            if client is not None:
                control = answer(client, probe["question"], "", args.max_tokens)
                experiment = answer(client, probe["question"], block, args.max_tokens)
                rec["control_response"] = control
                rec["experiment_response"] = experiment
                rec["control_empty"] = not control.strip()
                rec["experiment_empty"] = not experiment.strip()
                rec["control_judge"] = judge(client, probe, control)
                rec["experiment_judge"] = judge(client, probe, experiment)
                rec["objective_format_control"] = objective_format_check(probe, control)
                rec["objective_format_experiment"] = objective_format_check(probe, experiment)

            if server and mem_id:
                server.clear(mem_id)
            results.append(rec)
            flag = "" if rec.get("rule_in_injection", True) else "  ⚠ rule NOT in injection"
            print(f"  [{n}/{min(args.probes, len(probes))}] {probe['category']}"
                  f" ({probe['difficulty']}){flag}")
    finally:
        if server:
            server.stop()

    # Judge stability: re-judge a sample with the reworded prompt. Reported
    # because §2.6 flags ~±10% judge-prompt sensitivity on comparable benchmarks;
    # a headline number without this is not interpretable.
    stability = None
    if client is not None and args.judge_stability > 0:
        judged = [r for r in results if r.get("experiment_judge", {}).get("parsed")]
        sample = judged[: args.judge_stability]
        agree = 0
        for r in sample:
            again = judge(client, {"rule": r["rule"], "expected_compliance": "",
                                   "compliance_indicators": [], "non_compliance_signs": [],
                                   "rubric": [], "question": r["question"],
                                   "subtype": r["subtype"]},
                          r["experiment_response"], reworded=True)
            if again.get("parsed") and again["compliant"] == r["experiment_judge"]["compliant"]:
                agree += 1
        stability = {
            "n": len(sample),
            "agreement": (agree / len(sample)) if sample else None,
            "note": "re-judged with a reworded prompt; low agreement means the "
                    "headline rate is judge-sensitive and must be cited with that caveat",
        }

    return {
        "probes": results,
        "summary": summarise(results),
        "judge_stability": stability,
        "engineering": {
            "injection_latency_p50_s": percentile(inject_latencies, 50),
            "injection_latency_p95_s": percentile(inject_latencies, 95),
            "injection_latency_samples": len(inject_latencies),
            "injected_tokens_est_mean": (
                statistics.mean(injected_tokens) if injected_tokens else None
            ),
            "injected_tokens_est_p95": percentile([float(t) for t in injected_tokens], 95),
            "token_estimator": "chars/4 — same heuristic as bench.rs, an estimate not a tokenizer count",
            "note": "injection latency is measured around the /api/session call "
                    "only, never including model generation, so it is comparable "
                    "with mem0's published p50 frame (§2.6)",
        },
    }


def summarise(results: list[dict]) -> dict:
    scored = [r for r in results if r.get("experiment_judge", {}).get("parsed")]

    def rate(arm: str) -> float | None:
        vals = [r[f"{arm}_judge"]["compliant"] for r in scored]
        return (sum(1 for v in vals if v) / len(vals)) if vals else None

    def coverage(arm: str) -> float | None:
        vals = [r[f"{arm}_judge"]["rubric_coverage"] for r in scored
                if r[f"{arm}_judge"].get("rubric_coverage") is not None]
        return statistics.mean(vals) if vals else None

    control, experiment = rate("control"), rate("experiment")
    by_cat: dict[str, dict] = {}
    for cat in TARGET_CATEGORIES:
        sub = [r for r in scored if r["category"] == cat]
        if not sub:
            continue
        c = [r["control_judge"]["compliant"] for r in sub]
        e = [r["experiment_judge"]["compliant"] for r in sub]
        by_cat[cat] = {
            "n": len(sub),
            "control": (sum(c) / len(c)) if c else None,
            "experiment": (sum(e) / len(e)) if e else None,
        }

    return {
        "n_probes_loaded": len(results),
        "n_judged": len(scored),
        # `seeded is False` = the store rejected the write. `None` = not attempted
        # (dry run without a server). Counting the latter as a failure would make
        # a plumbing-only run look like four write errors.
        "n_seed_failed": sum(1 for r in results if r.get("seeded") is False),
        "n_seed_not_attempted": sum(1 for r in results if r.get("seeded") is None),
        # Reported separately from the adherence rates: a MUST that never reached
        # the instruction block invalidates the probe, and folding that into
        # "memory did not help" would hide a harness bug as a negative result.
        "n_rule_missing_from_injection": sum(
            1 for r in results if r.get("rule_in_injection") is False
        ),
        # Must be zero for the run to mean anything. If a seeded MUST arrived as
        # ordinary REFERENCE context, the experiment measured passive context
        # rather than instruction-layer injection (§6.2) — a different claim with
        # a different expected effect size.
        "n_not_instruction_layer": sum(
            1 for r in results if r.get("instruction_layer_used") is False
        ),
        "control_adherence": control,
        "experiment_adherence": experiment,
        "delta": (experiment - control) if (control is not None and experiment is not None) else None,
        "control_rubric_coverage": coverage("control"),
        "experiment_rubric_coverage": coverage("experiment"),
        "by_category": by_cat,
        "n_empty_control": sum(1 for r in results if r.get("control_empty") is True),
        "n_empty_experiment": sum(1 for r in results if r.get("experiment_empty") is True),
        "n_judge_unparsed": sum(
            1 for r in results
            if r.get("control_judge", {}).get("parsed") is False
            or r.get("experiment_judge", {}).get("parsed") is False
        ),
        "objective_format_agreement": _objective_agreement(scored),
    }


def _objective_agreement(scored: list[dict]) -> dict | None:
    """Where the objective fence check applies, does the judge agree with it?

    Kept separate from the headline rate on purpose. Agreement is a check on the
    judge, not a second metric to average in.
    """
    pairs = [
        (r["objective_format_experiment"], r["experiment_judge"]["compliant"])
        for r in scored
        if r.get("objective_format_experiment") is not None
    ]
    if not pairs:
        return None
    agree = sum(1 for obj, jd in pairs if obj == jd)
    return {
        "n": len(pairs),
        "agreement": agree / len(pairs),
        "objective_rate": sum(1 for o, _ in pairs if o) / len(pairs),
        "note": "format_instruction probes only; fence presence is literal, so a "
                "disagreement here is evidence about the judge, not the memory",
    }


def _safe_binary(args) -> str | None:
    """Report the binary actually used, or None if there was not one.

    Called unconditionally rather than only for real runs: `--dry-run
    --with-server` boots MemVault too, and a spec that said `binary: null` for a
    run that really did inject through a real server would misdescribe the
    evidence.
    """
    try:
        return str(find_binary(args.binary))
    except SystemExit:
        return None


def harness_spec(args, n_probes: int) -> dict:
    """Everything required to interpret the numbers.

    §2.6: most memory benchmarks have no official leaderboard and vendor
    self-reports use incompatible judge models, retrieval settings and prompts —
    so a number published without its full spec is not comparable and should not
    be cited. This is that spec.
    """
    commit = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=REPO_ROOT,
        capture_output=True, text=True, check=False,
    ).stdout.strip()
    version = subprocess.run(
        ["git", "describe", "--tags", "--always"], cwd=REPO_ROOT,
        capture_output=True, text=True, check=False,
    ).stdout.strip()
    return {
        "benchmark": {"name": "BEAM", "paper": "arXiv:2510.27246",
                      "dataset": HF_DATASET, "split": args.split,
                      "categories": list(TARGET_CATEGORIES),
                      "why": "the only public axis where instruction-layer "
                             "injection + follow-through tracking can be measured "
                             "(DESIGN.md §2.6)"},
        "selection": {"offset": args.offset, "conversations": args.conversations,
                      "probes_run": n_probes,
                      "note": "probes are taken in dataset order, never sampled — "
                              "a cherry-picked subset is not comparable"},
        "memvault": {"commit": commit, "version": version,
                     "binary": _safe_binary(args),
                     "embeddings": args.embeddings,
                     "seed_priority": "MUST",
                     "seed_trust": "human_reviewed=true, ai_generated=false",
                     "isolation": "one memory per probe, deleted before the next",
                     "store": "throwaway temp sqlite per run"},
        "models": {"answer_model": MODEL, "judge_model": JUDGE_MODEL,
                   "base_url": BASE_URL or "openai default",
                   "temperature": 0.0, "answer_max_tokens": args.max_tokens,
                   "judge_max_tokens": JUDGE_MAX_TOKENS,
                   "extra_body": EXTRA_BODY},
        "prompts": {"base_system": BASE_SYSTEM, "judge_system": JUDGE_SYSTEM,
                    "judge_template": JUDGE_TEMPLATE,
                    "judge_template_reworded": JUDGE_TEMPLATE_REWORDED},
        "scoring": {
            "primary": "LLM judge verdict on `compliant`, per BEAM's own rubric",
            "secondary": "rubric coverage = fraction of rubric items satisfied",
            "objective": "markdown fence presence for format_instruction probes, "
                         "reported separately and never merged",
            "known_limitation": "BEAM rubric items are semantic descriptions, not "
                                "literal strings, so unlike verify_h6's proper-noun "
                                "stems they cannot be scored by substring match — "
                                "the judge is load-bearing here, hence the "
                                "stability measurement",
        },
        "arms": {"control": "question only, no memory context",
                 "experiment": "question + MemVault's own /api/session injection block"},
        "run_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "argv": sys.argv[1:],
    }


# ─── Entry point ──────────────────────────────────────────────────────────


def collect_probes(args) -> list[dict]:
    if args.from_file:
        rows = load_rows_from_file(Path(args.from_file))
    else:
        rows = fetch_rows(args.split, args.offset, args.conversations)
    print(f"loaded {len(rows)} BEAM conversation(s) from "
          f"{'file' if args.from_file else f'{HF_DATASET} split={args.split}'}")

    probes: list[dict] = []
    for row in rows:
        chat = flatten_chat(row)
        parsed = parse_probes(row)
        for cat in TARGET_CATEGORIES:
            for raw in parsed.get(cat, []):
                probe = normalise_probe(cat, raw, chat)
                if probe:
                    probes.append(probe)
    return probes


def main():
    parser = argparse.ArgumentParser(
        description="BEAM instruction/preference-adherence harness for MemVault"
    )
    parser.add_argument("--split", default="100K", choices=["100K", "500K", "1M"],
                        help="BEAM context-length split (default 100K = smallest)")
    parser.add_argument("--offset", type=int, default=0, help="first conversation index")
    parser.add_argument("--conversations", type=int, default=2,
                        help="how many conversations to load (default 2)")
    parser.add_argument("--probes", type=int, default=8,
                        help="max adherence probes to run (default 8)")
    parser.add_argument("--max-tokens", type=int, default=300,
                        help="answer max_tokens (default 300)")
    parser.add_argument("--judge-stability", type=int, default=0,
                        help="re-judge N probes with a reworded prompt and report "
                             "self-agreement (0 = off)")
    parser.add_argument("--binary", help="path to memvault-mcp binary")
    parser.add_argument("--embeddings", action="store_true",
                        help="enable an embedding provider (default off, so the "
                             "deterministic MUST path of §6.1.2 is what gets measured)")
    parser.add_argument("--from-file", help="replay a cached row file, no network")
    parser.add_argument("--with-server", action="store_true",
                        help="in --dry-run, still boot MemVault and exercise "
                             "seeding + injection (no LLM calls)")
    parser.add_argument("--dry-run", action="store_true",
                        help="no LLM calls: validate loading, provenance and "
                             "time-anchor resolution only")
    parser.add_argument("--out", help="write the full results + harness spec here as JSON")
    args = parser.parse_args()

    probes = collect_probes(args)
    if not probes:
        raise SystemExit(
            "no instruction_following / preference_following probes found — check "
            "the split, or pass --from-file with a cached row"
        )

    cats: dict[str, int] = {}
    for p in probes:
        cats[p["category"]] = cats.get(p["category"], 0) + 1
    with_prov = sum(1 for p in probes if p["source_turns"])
    print(f"extracted {len(probes)} adherence probes: {cats}")
    print(f"provenance resolvable for {with_prov}/{len(probes)} probes")
    origins: dict[str, int] = {}
    for p in probes:
        key = (p.get("time_anchor_origin") or "none").split(":")[0]
        origins[key] = origins.get(key, 0) + 1
    print(f"event-time anchors: {origins} (BEAM stamps the anchor on the first "
          f"turn of each session only, so nearly all are inherited backwards "
          f"from the referenced turn to its session start)")

    print("\n" + "=" * 64)
    print("BEAM instruction/preference adherence — MemVault")
    print("=" * 64)
    if args.dry_run:
        print("DRY RUN: no LLM calls" + ("" if args.with_server else ", no server"))
    else:
        print(f"answer model: {MODEL} @ {BASE_URL or 'OpenAI'}")
        print(f"judge model : {JUDGE_MODEL}")

    out = run(args, probes)
    out["harness_spec"] = harness_spec(args, min(args.probes, len(probes)))

    s = out["summary"]
    print("\n" + "=" * 64)
    print("RESULTS")
    print("=" * 64)
    print(f"probes loaded {s['n_probes_loaded']} · judged {s['n_judged']} · "
          f"seed failures {s['n_seed_failed']} · "
          f"seeding not attempted {s['n_seed_not_attempted']} · "
          f"rule missing from injection {s['n_rule_missing_from_injection']} · "
          f"not via instruction layer {s['n_not_instruction_layer']} · "
          f"empty answers control/experiment {s.get('n_empty_control', 0)}/"
          f"{s.get('n_empty_experiment', 0)} · "
          f"judge unparsed {s.get('n_judge_unparsed', 0)}")
    # Refuse to emit a headline verdict on an invalid run. A seeded MUST that
    # arrived as ordinary context measures a different mechanism (§6.2), so
    # printing CONFIRMED/REJECTED over it would be worse than printing nothing.
    harness_broken = (s["n_seed_failed"] or s["n_rule_missing_from_injection"]
                      or s["n_not_instruction_layer"]
                      or s.get("n_empty_control", 0)
                      or s.get("n_empty_experiment", 0))
    if harness_broken and s["n_judged"]:
        print("\n⚠ HARNESS INVARIANT VIOLATED — the numbers below are not a "
              "measurement of instruction-layer injection. Fix the seeding path "
              "before citing anything from this run.")
    if s["n_judged"]:
        c, e, d = s["control_adherence"], s["experiment_adherence"], s["delta"]
        print(f"\nadherence:  control {c:.1%} -> experiment {e:.1%}   (Δ {d:+.1%})")
        print(f"rubric coverage: control {s['control_rubric_coverage']:.1%} -> "
              f"experiment {s['experiment_rubric_coverage']:.1%}")
        for cat, v in s["by_category"].items():
            print(f"  {cat}: n={v['n']} control {v['control']:.1%} -> "
                  f"experiment {v['experiment']:.1%}")
        if s.get("objective_format_agreement"):
            o = s["objective_format_agreement"]
            print(f"\nobjective fence check (format_instruction only): n={o['n']}, "
                  f"objective rate {o['objective_rate']:.1%}, "
                  f"judge agreement {o['agreement']:.1%}")
        # Same verdict convention as verify_h6/verify_h7 so results are read the
        # same way across the experiments directory.
        if harness_broken:
            verdict = "INVALID (harness invariant violated — see above)"
        else:
            verdict = ("CONFIRMED ✓" if d > 0.1 else "INCONCLUSIVE" if d > 0 else "REJECTED ✗")
        print(f"\nVERDICT: {verdict}")
    else:
        print("\nno judged probes (dry run, or the judge could not parse) — "
              "loading/provenance/injection plumbing is what was exercised")

    eng = out["engineering"]
    if eng["injection_latency_samples"]:
        print(f"\ninjection latency: p50 {eng['injection_latency_p50_s']*1000:.1f} ms · "
              f"p95 {eng['injection_latency_p95_s']*1000:.1f} ms "
              f"(n={eng['injection_latency_samples']}, /api/session only)")
        print(f"injected tokens (est): mean {eng['injected_tokens_est_mean']:.0f} · "
              f"p95 {eng['injected_tokens_est_p95']:.0f}  [{eng['token_estimator']}]")

    if out.get("judge_stability"):
        js = out["judge_stability"]
        if js["agreement"] is not None:
            print(f"\njudge stability: {js['agreement']:.1%} self-agreement over n={js['n']} "
                  f"under a reworded prompt")
            if js["agreement"] < 0.8:
                print("  ⚠ below 80% — the headline rate is judge-sensitive; cite it "
                      "with that caveat (§2.6 notes ~±10% swing on comparable benchmarks)")

    if args.out:
        Path(args.out).write_text(json.dumps(out, indent=2, ensure_ascii=False), encoding="utf-8")
        print(f"\nwrote {args.out} (results + full harness spec)")
    else:
        print("\n(no --out given; the harness spec was not persisted. §2.6: a "
              "number without its spec is not comparable and should not be cited.)")


if __name__ == "__main__":
    main()
