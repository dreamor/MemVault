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
import copy
import datetime as dt
import hashlib
import json
import math
import os
import random
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
If the rubric is nonempty, compliant means ALL items are satisfied AND no
non-compliance sign is present. With no rubric, judge the standing instruction.
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
complied with the instruction. For a nonempty checklist, the overall label is
true only when every item passes and no negative sign occurs. If the checklist
is empty, evaluate the standing instruction itself. Answer in JSON only:
{{"rubric_met": [<bool per checklist item, in order>],
  "compliant": <bool>,
  "non_compliance_sign_present": <bool>,
  "reason": "<one sentence>"}}"""


# ─── Shared evidence helpers (also used by the MAB harness) ────────────────


def content_hash(path: str | Path | None) -> str | None:
    """Hash actual bytes, never substitute a URL, identifier or results file."""
    if path is None or not Path(path).is_file():
        return None
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def json_hash(value) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False,
                                     separators=(",", ":")).encode()).hexdigest()


_SECRET_KEY = re.compile(r"(?:api[_-]?key|secret|password|authorization|(?:^|[_-])(?:access[_-]?token|refresh[_-]?token|auth[_-]?token|token)$|^credentials?$)", re.I)


_REDACTION_VALUES = set()


def register_secret(value: str):
    if value:
        _REDACTION_VALUES.add(value)


_CREDENTIAL_HEADER = re.compile(
    r"\b(?:X-MemVault-Api-Key|X-Api-Key|api-key|Authorization)(?:\\?[\"'])?\s*:\s*"
    r"(?P<value>\\\"(?:\\.|[^\"\\])*?\\\"|\\'(?:\\.|[^'\\])*?\\'|"
    r"\"(?:\\.|[^\"\\])*\"|'(?:\\.|[^'\\])*'|"
    r"(?:(?:Basic|Bearer|Digest|Token)\s+)?[^\s\"'\\`,;}]+)", re.I)


def _redact_header(match):
    value = match.group("value")
    quote = (value[:2] if value.startswith(('\\"', "\\'")) else
             value[0] if value.startswith(('"', "'")) else "")
    body = value[len(quote):-len(quote)] if quote else value
    scheme = re.match(r"(?:Basic|Bearer|Digest|Token)\s+", body, re.I)
    replacement = quote + (scheme.group() if scheme else "") + "[REDACTED]" + quote
    return match.group()[:match.start("value") - match.start()] + replacement


def sanitize(value):
    """Preserve request/response structure except credentials, including in URLs.

    Apply at recording boundaries, not to the actual request or scoring input.
    No environment dump or HTTP authorization headers are ever recorded.
    """
    if isinstance(value, dict):
        for key, secret in value.items():
            if _SECRET_KEY.search(str(key)) and isinstance(secret, str):
                register_secret(secret)
        return {str(k): "[REDACTED]" if _SECRET_KEY.search(str(k)) else sanitize(v)
                for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [sanitize(v) for v in value]
    if not isinstance(value, str):
        return value
    value = _CREDENTIAL_HEADER.sub(_redact_header, value)
    for secret in _REDACTION_VALUES:
        value = value.replace(secret, "[REDACTED]")
    for key, secret in os.environ.items():
        if _SECRET_KEY.search(key) and secret:
            value = value.replace(secret, "[REDACTED]")
    value = re.sub(r"(https?://)[^/\s@]+@", r"\1[REDACTED]@", value)
    value = re.sub(r"([?&](?:api[_-]?key|key|access_token|refresh_token|token|secret|password)=)[^&\s]+",
                   r"\1[REDACTED]", value, flags=re.I)
    value = re.sub(r'("(?:api[_-]?key|access_token|refresh_token|token|secret|password|authorization)"\s*:\s*)"(?:\\.|[^"\\])*"',
                   r'\1"[REDACTED]"', value, flags=re.I)
    value = re.sub(r"\bBearer\s+[^\s\"']+", "Bearer [REDACTED]", value, flags=re.I)
    return re.sub(r"\bsk-[A-Za-z0-9_-]+", "[REDACTED]", value)


def response_data(response):
    if hasattr(response, "model_dump"):
        return response.model_dump(mode="json")
    if isinstance(response, dict):
        return {k: response_data(v) for k, v in response.items()}
    if isinstance(response, list):
        return [response_data(v) for v in response]
    if isinstance(response, (str, int, float, bool)) or response is None:
        return response
    if hasattr(response, "__dict__"):
        return {k: response_data(v) for k, v in vars(response).items() if not k.startswith("_")}
    return None  # explicit missing evidence for unsupported client objects


def checkout_identity(root: Path = REPO_ROOT) -> dict:
    def git(*args):
        try:
            result = subprocess.run(["git", "-C", str(root), *args],
                                    capture_output=True, text=True, check=False)
            return result.stdout.strip() if result.returncode == 0 else None
        except OSError:
            return None
    status = git("status", "--porcelain", "--untracked-files=all")
    diff = git("diff", "--binary", "HEAD")
    untracked = git("ls-files", "--others", "--exclude-standard")
    untracked_hashes = ({path: content_hash(root / path) for path in untracked.splitlines()}
                        if untracked is not None else None)
    return {"commit": git("rev-parse", "HEAD"), "branch": git("branch", "--show-current"),
            "version": git("describe", "--tags", "--always"),
            "dirty": bool(status) if status is not None else None,
            "dirty_status_sha256": json_hash(status) if status is not None else None,
            "tracked_diff_sha256": json_hash(diff) if diff is not None else None,
            "untracked_content_sha256": json_hash(untracked_hashes) if untracked_hashes is not None else None}


def execution_identity(harness: Path, binary: str | Path | None, dataset: dict) -> dict:
    """Snapshot before launch/model work; callers must retain, not recapture, it."""
    return {"schema_version": 2, "capture_phase": "before_execution",
            "captured_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "harness_path": str(harness.resolve()),
            "binary_path": str(Path(binary).resolve()) if binary is not None else None,
            "harness_sha256": content_hash(harness), "binary_sha256": content_hash(binary),
            "dataset": copy.deepcopy(dataset), "checkout": checkout_identity()}


_SERVER_ENV_KEYS = ("PATH", "LANG", "LC_ALL", "SYSTEMROOT", "SSL_CERT_FILE", "SSL_CERT_DIR",
                    "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "http_proxy", "https_proxy", "no_proxy")
EMBEDDING_ENV_KEYS = ("MEMVAULT_EMBEDDING_PROVIDER", "MEMVAULT_EMBEDDING_API_BASE",
                      "MEMVAULT_EMBEDDING_MODEL", "MEMVAULT_EMBEDDING_DIM",
                      "MEMVAULT_EMBEDDING_API_KEY")


def server_environment() -> dict:
    return {k: os.environ[k] for k in _SERVER_ENV_KEYS if k in os.environ}


def isolate_server_environment(run_dir: str, db_name: str, requested: dict) -> tuple[dict, Path, Path]:
    """Own every vault path and explicitly bypass the user's startup .env.

    Verified in memvault-mcp/src/main.rs (--env-file) and core storage/backend.rs:
    a Markdown backend ignores --db, so a temporary --db alone is NOT isolation.
    Only requested embedding options and basic transport settings may survive.
    """
    home = Path(run_dir).resolve()
    env_file, db = home / "empty.env", home / db_name
    env_file.write_bytes(b"")
    env = {k: v for k, v in requested.items() if k in _SERVER_ENV_KEYS or k in EMBEDDING_ENV_KEYS}
    env.update({"HOME": str(home), "TMPDIR": str(home), "TEMP": str(home), "TMP": str(home),
                "MEMVAULT_HOME": str(home), "MEMVAULT_ENV_FILE": str(env_file),
                "MEMVAULT_DB": str(db), "MEMVAULT_STORAGE_BACKEND": "sqlite"})
    return env, env_file, db


def paired_metrics(pairs: list[tuple[bool, bool]]) -> dict:
    """Paired binary accuracy and exact two-sided McNemar/binomial test."""
    n = len(pairs)
    improved = sum(not c and e for c, e in pairs)
    regressed = sum(c and not e for c, e in pairs)
    discordant = improved + regressed
    p = (min(1.0, 2 * sum(math.comb(discordant, i)
                         for i in range(min(improved, regressed) + 1)) / 2**discordant)
         if discordant else None)
    return {"n": n, "control_correct": sum(c for c, _ in pairs),
            "experiment_correct": sum(e for _, e in pairs),
            "control": sum(c for c, _ in pairs) / n if n else None,
            "experiment": sum(e for _, e in pairs) / n if n else None,
            "delta": (improved - regressed) / n if n else None,
            "both_correct": sum(c and e for c, e in pairs),
            "both_incorrect": sum(not c and not e for c, e in pairs),
            "improved": improved, "regressed": regressed,
            "mcnemar_exact_p": p}


# ─── Data acquisition ─────────────────────────────────────────────────────


def rows_cache_path(split: str, offset: int, length: int, revision: str = "main") -> Path:
    identity = {"dataset": HF_DATASET, "config": "default", "split": split,
                "offset": offset, "length": length, "revision": revision}
    return CACHE_DIR / f"beam__{json_hash(identity)}.json"


def fetch_rows(split: str, offset: int, length: int, revision: str = "main",
               evidence: dict | None = None) -> list[dict]:
    """Cache the actual datasets-server response bytes, keyed by requested revision.

    A mutable revision such as main is not an immutable dataset identity: the
    content hash is recorded separately. Old revisionless caches are not reused.
    """
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    cache = rows_cache_path(split, offset, length, revision)
    if not cache.exists():
        qs = urllib.parse.urlencode(
            {"dataset": HF_DATASET, "config": "default", "split": split,
             "offset": offset, "length": length, "revision": revision})
        req = urllib.request.Request(f"{HF_ROWS_URL}?{qs}",
                                     headers={"User-Agent": "memvault-beam/2.0"})
        with urllib.request.urlopen(req, timeout=300) as resp:
            payload = resp.read()
        json.loads(payload)  # reject malformed downloads before caching
        cache.write_bytes(payload)
    return load_rows_from_file(cache, evidence)


def load_rows_from_file(path: Path, evidence: dict | None = None) -> list[dict]:
    """Replay and hash the very bytes parsed, not the file after execution."""
    payload = path.read_bytes()
    if evidence is not None:
        evidence["source_sha256"] = hashlib.sha256(payload).hexdigest()
        evidence["hash_capture_phase"] = "read_bytes"
    data = json.loads(payload)
    if isinstance(data, dict) and "rows" in data:
        return [r["row"] for r in data["rows"]]
    if isinstance(data, dict):
        return [data]
    return data


def parse_probes(row: dict, errors: list | None = None) -> dict[str, list[dict]]:
    """Decode `probing_questions`.

    The field is a *Python literal* string (single-quoted), not JSON — the
    dataset was written from a Python pipeline and the column kept its repr.
    `json.loads` fails on it; `ast.literal_eval` is the correct reader. Imported
    lazily so `--help` stays cheap.
    """
    import ast

    def invalid(message):
        if errors is not None:
            errors.append(message)
        return {}

    raw = row.get("probing_questions")
    if not isinstance(raw, str) or not raw.strip():
        return invalid("probing_questions missing or not a nonempty string")
    try:
        parsed = ast.literal_eval(raw)
    except (ValueError, SyntaxError):
        # Fall back to JSON in case a future release normalises the column.
        try:
            parsed = json.loads(raw)
        except json.JSONDecodeError:
            return invalid("probing_questions unparseable")
    if not isinstance(parsed, dict):
        return invalid("probing_questions is not an object")
    for cat in TARGET_CATEGORIES:
        if cat in parsed and not isinstance(parsed[cat], list) and errors is not None:
            errors.append(f"{cat}: expected annotation list; slot count unknown")
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


def normalise_probe(category: str, raw: dict, chat: dict[int, dict],
                    locator: dict | None = None) -> dict | None:
    """Flatten the two categories' differing field names onto one shape.

    `instruction_following` says `instruction_being_tested`;
    `preference_following` says `preference_being_tested`. Everything else
    lines up. A malformed probe returns None; intake must retain its original
    slot, raw annotation and locator rather than backfilling a later valid probe.
    """
    if not isinstance(raw, dict):
        return None
    rule = raw.get("instruction_being_tested") or raw.get("preference_being_tested")
    question = raw.get("question")
    if not isinstance(rule, str) or not rule.strip() or not isinstance(question, str) or not question.strip():
        return None
    source_ids = [i for i in (raw.get("source_chat_ids") or []) if type(i) is int]
    source_turns = [chat[i] for i in source_ids if i in chat]
    anchor, anchor_origin = resolve_time_anchor(chat, source_ids)
    locator = locator or {"dataset": HF_DATASET, "row_index": None, "probe_index": None}
    return {
        "probe_id": f"beam:{category}:{json_hash({'locator': locator, 'annotation': raw})}",
        "source_locator": copy.deepcopy(locator),
        "raw_annotation": copy.deepcopy(raw),
        "source_turn_locators": [{"chat_id": i, "turn": copy.deepcopy(chat.get(i))}
                                  for i in source_ids],
        "category": category,
        "question": question.strip(),
        "rule": rule.strip(),
        "expected_compliance": (raw.get("expected_compliance") or "").strip(),
        "compliance_indicators": raw.get("compliance_indicators", []),
        "non_compliance_signs": raw.get("non_compliance_signs", []),
        "rubric": raw.get("rubric"),
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
        self.records = []
        requested = server_environment()
        if embeddings:
            requested.update({k: os.environ[k] for k in EMBEDDING_ENV_KEYS if k in os.environ})
            # Preserve the old embedding-key fallback only with explicit --embeddings.
            if "MEMVAULT_EMBEDDING_API_KEY" not in requested and os.getenv("OPENAI_API_KEY"):
                requested["MEMVAULT_EMBEDDING_API_KEY"] = os.environ["OPENAI_API_KEY"]
            register_secret(requested.get("MEMVAULT_EMBEDDING_API_KEY", ""))
        else:
            requested["MEMVAULT_EMBEDDING_PROVIDER"] = "off"
        env, self.env_file, db = isolate_server_environment(self.dbdir, "beam.db", requested)
        self.proc = subprocess.Popen(
            [str(binary), "--env-file", str(self.env_file), "--db", str(db),
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
        record = {"request": {"method": method, "path": path, "body": sanitize(body)},
                  "response": None, "error": None}
        self.records.append(record)
        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                raw = r.read().decode("utf-8")
                record.update({"status": r.status, "response_raw": sanitize(raw)})
                response = json.loads(raw)
                record["response"] = sanitize(response)
                record["response_raw"] = sanitize(raw)
                return response
        except Exception as exc:
            record["error"] = sanitize(str(exc))
            raise

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
            return True
        except Exception:
            return False  # the request trace preserves the failure; no silent cleanup


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


_MIN_CALL_INTERVAL = float(os.getenv("VERIFY_MIN_CALL_INTERVAL_S", "1.1"))
_last_call_at = 0.0


def _chat_with_retry(client, trace: list | None = None, **kwargs):
    """Call the chat endpoint, retrying the gateway's per-minute rate limit.

    The account-level limiter is 60 requests/minute and is shared by every
    concurrent harness run, so a fast gateway can cross it even when one run
    looks slow. A 429 here is a measurement interruption, not a verdict:
    back off and retry instead of aborting a half-finished run (there is no
    partial-result checkpoint to resume from).
    """
    # Proactive throttle: the gateway's account-level limit is 60 requests per
    # minute and a fast endpoint will blow through it in a burst. Retrying a
    # 429 is a repair, but staying under the limit in the first place is what
    # keeps a 200-call run from taking twice as long.
    global _last_call_at
    gap = _MIN_CALL_INTERVAL - (time.monotonic() - _last_call_at)
    if gap > 0:
        time.sleep(gap)
    _last_call_at = time.monotonic()
    delay = 20.0
    for attempt in range(7):
        record = {"attempt": attempt + 1, "request": sanitize(kwargs),
                  "response": None, "error": None}
        if trace is not None:
            trace.append(record)
        try:
            response = client.chat.completions.create(**kwargs)
            record["response"] = sanitize(response_data(response))
            return response
        except Exception as exc:  # noqa: BLE001 - provider-specific subclasses vary
            record["error"] = sanitize(str(exc))
            text = str(exc)
            retryable = ("429" in text or "RateLimit" in text or "rate limit" in text.lower()
                         or "timeout" in text.lower() or "502" in text or "503" in text
                         or "504" in text)
            if not retryable or attempt == 6:
                raise
            print(f"  [rate-limit retry {attempt + 1}/6 after {delay:.0f}s]",
                  file=sys.stderr)
            time.sleep(delay)
            delay = min(delay * 1.5, 90.0)


def answer(client, question: str, memory_block: str, max_tokens: int,
           trace: list | None = None) -> str:
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
        resp = _chat_with_retry(
            client, trace=trace, model=MODEL,
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


def validate_grade(grade: dict, rubric_count: int | None) -> dict:
    """Strict JSON types and length, independent of the historical label metric.

    Aggregation is all rubric items AND no negative sign. A binary disagreement
    is diagnostic, not permission to rewrite golds or drop the original label.
    Empty/missing rubrics have no computable rubric-derived verdict.
    """
    errors = []
    if grade.get("parsed") is not True:
        errors.append("unparsed")
    for key in ("compliant", "non_compliance_sign_present"):
        if type(grade.get(key)) is not bool:
            errors.append(f"{key}:expected_json_boolean")
    items = grade.get("rubric_met")
    if not isinstance(items, list) or any(type(v) is not bool for v in items):
        errors.append("rubric_met:expected_boolean_list")
    if type(rubric_count) is not int or rubric_count < 0:
        errors.append("rubric_length:unknown")
    elif isinstance(items, list) and len(items) != rubric_count:
        errors.append("rubric_length:mismatch")
    if not isinstance(grade.get("reason"), str):
        errors.append("reason:expected_string")
    aggregate = (all(items) and not grade["non_compliance_sign_present"]
                 if not errors and rubric_count else None)
    return {"valid": not errors, "errors": errors,
            "rubric_aggregate_compliant": aggregate,
            "label_aggregate_disagreement": (aggregate != grade["compliant"]
                                               if aggregate is not None else None)}


def judge(client, probe: dict, response: str, reworded: bool = False) -> dict:
    template = JUDGE_TEMPLATE_REWORDED if reworded else JUDGE_TEMPLATE
    for field in ("rubric", "compliance_indicators", "non_compliance_signs"):
        values = probe.get(field)
        if not isinstance(values, list) or any(not isinstance(v, str) for v in values):
            return {"parsed": False, "compliant": None, "rubric_met": None,
                    "rubric_coverage": None, "non_compliance_sign_present": None,
                    "reason": "invalid probe annotation", "records": [],
                    "validation": {"valid": False, "errors": [f"probe:{field}:expected_string_list"]}}
    prompt = template.format(
        instruction=probe["rule"],
        expected=probe["expected_compliance"] or "(not specified)",
        indicators="\n".join(f"- {i}" for i in probe["compliance_indicators"]) or "(none)",
        negatives="\n".join(f"- {n}" for n in probe["non_compliance_signs"]) or "(none)",
        rubric="\n".join(f"- {r}" for r in probe["rubric"]) or "(none)",
        question=probe["question"],
        response=response,
    )
    records = []
    try:
        resp = _chat_with_retry(
            client, trace=records, model=JUDGE_MODEL,
            messages=[{"role": "system", "content": JUDGE_SYSTEM},
                      {"role": "user", "content": prompt}],
            max_tokens=JUDGE_MAX_TOKENS, temperature=0.0,
            **(({"extra_body": EXTRA_BODY}) if EXTRA_BODY else {}),
        )
    except Exception as exc:
        return {"parsed": False, "compliant": None, "rubric_met": None,
                "rubric_coverage": None, "non_compliance_sign_present": None,
                "reason": "judge request failed", "records": records,
                "error": sanitize(str(exc)), "validation": {"valid": False, "errors": ["request_failed"]}}
    raw = resp.choices[0].message.content or ""
    verdict = _parse_json_loose(raw)
    grade = {"compliant": verdict.get("compliant"), "rubric_met": verdict.get("rubric_met"),
             "non_compliance_sign_present": verdict.get("non_compliance_sign_present"),
             "reason": verdict.get("reason"), "parsed": bool(verdict)}
    validation = validate_grade(grade, len(probe["rubric"]))
    items = grade["rubric_met"]
    grade["rubric_coverage"] = sum(items) / len(items) if validation["valid"] and items else None
    grade.update({"validation": validation, "records": records, "raw_verdict": sanitize(raw),
                  "parsed_verdict": sanitize(verdict), "prompt_variant": "reworded" if reworded else "original"})
    return grade


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


def _capture_execution_start(args):
    if hasattr(args, "execution_evidence"):
        return
    binary = None if args.dry_run and not args.with_server else find_binary(args.binary).resolve()
    args.execution_binary = str(binary) if binary is not None else None
    args.execution_evidence = execution_identity(Path(__file__), binary,
        getattr(args, "dataset_evidence", {"source_sha256": None}))


def run(args, probes: list[dict]) -> dict:
    _capture_execution_start(args)
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
        server = MemVaultServer(Path(args.execution_binary), args.embeddings)
        print(f"MemVault server: {server.base} (temp db, embeddings "
              f"{'on' if args.embeddings else 'off'})")

    run_error = None
    try:
        for n, probe in enumerate(probes[: args.probes], 1):
            rec: dict = {
                "n": n,
                "probe_id": probe.get("probe_id"),
                "probe": copy.deepcopy(probe),
                "rubric": copy.deepcopy(probe["rubric"]),
                "raw_annotation": copy.deepcopy(probe.get("raw_annotation")),
                "source_locator": copy.deepcopy(probe.get("source_locator")),
                "category": probe["category"],
                "difficulty": probe["difficulty"],
                "subtype": probe["subtype"],
                "question": probe["question"],
                "rule": probe["rule"],
                "rubric_items": len(probe["rubric"]) if isinstance(probe["rubric"], list) else None,
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

            results.append(rec)  # retain partial evidence if an endpoint fails
            if probe.get("intake_errors"):
                rec.update({"intake_errors": copy.deepcopy(probe["intake_errors"]),
                            "seeded": None, "execution_skipped": "invalid selected annotation slot"})
                continue
            if args.dry_run and server is None:
                # No server, no LLM: still exercise loading, normalisation,
                # provenance resolution and time-anchor parsing, which is most of
                # what can be wrong before spending tokens.
                # `seeded: None` means "not attempted" — distinct from `False`,
                # which means the store rejected the write. Conflating them would
                # report a dry run as four seed failures.
                rec.update({"dry_run": True, "seeded": None})
                continue

            request_start = len(server.records) if server else 0
            mem_id = server.seed_rule(probe) if server else ""
            rec["memory_id"] = mem_id
            rec["seeded"] = bool(mem_id)

            block, latency, fmt = ("", None, "")
            if server:
                block, latency, fmt = server.inject(probe["question"])
                inject_latencies.append(latency)
                injected_tokens.append(estimate_tokens(block))
            rec["injected_block"] = sanitize(block)
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
                rec["control_records"], rec["experiment_records"] = [], []
                control = answer(client, probe["question"], "", args.max_tokens, rec["control_records"])
                experiment = answer(client, probe["question"], block, args.max_tokens, rec["experiment_records"])
                rec["control_response"] = control
                rec["experiment_response"] = experiment
                rec["control_empty"] = not control.strip()
                rec["experiment_empty"] = not experiment.strip()
                rec["control_judge"] = judge(client, probe, control)
                rec["experiment_judge"] = judge(client, probe, experiment)
                rec["objective_format_control"] = objective_format_check(probe, control)
                rec["objective_format_experiment"] = objective_format_check(probe, experiment)

            if server and mem_id:
                rec["cleanup_succeeded"] = server.clear(mem_id)
            rec["server_records"] = copy.deepcopy(server.records[request_start:]) if server else None
            if rec.get("cleanup_succeeded") is False:
                raise RuntimeError("probe cleanup failed; refusing to contaminate the next probe")
            flag = "" if rec.get("rule_in_injection", True) else "  rule NOT in injection"
            print(f"  [{n}/{min(args.probes, len(probes))}] {probe['category']}"
                  f" ({probe['difficulty']}){flag}")
    except Exception as exc:
        run_error = sanitize(str(exc))
        if results:
            results[-1]["error"] = run_error
            results[-1]["server_records"] = copy.deepcopy(server.records[request_start:]) if server else None
    finally:
        if server:
            server.stop()

    stability = None
    if client is not None and args.judge_stability > 0:
        stability = regrade_pairs(client, results, args.judge_stability,
                                 getattr(args, "stability_seed", 0))

    return sanitize({
        "probes": results,
        "summary": summarise(results),
        "validity": validity_diagnostics(results),
        "run_error": run_error,
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
    })


def grade_for_record(record: dict, arm: str) -> dict:
    rubric = record.get("probe", {}).get("rubric", record.get("rubric"))
    count = len(rubric) if isinstance(rubric, list) else record.get("rubric_items")
    validation = validate_grade(record.get(f"{arm}_judge") or {}, count)
    if isinstance(rubric, list) and any(not isinstance(r, str) for r in rubric):
        validation["valid"] = False
        validation["errors"].append("probe:rubric:expected_string_list")
    return validation


def validity_diagnostics(results: list[dict]) -> dict:
    diagnostics, pairs, aggregate_pairs = [], [], []
    semantic_known = 0
    for r in results:
        grades = {arm: grade_for_record(r, arm) for arm in ("control", "experiment")}
        rubric = r.get("probe", {}).get("rubric", r.get("rubric"))
        semantic = isinstance(rubric, list) and all(isinstance(v, str) for v in rubric)
        semantic_known += semantic
        harness_errors = [key for key in ("seeded", "rule_in_injection", "instruction_layer_used", "cleanup_succeeded")
                          if r.get(key) is False]
        harness_errors += [key for key in ("control_empty", "experiment_empty") if r.get(key) is True]
        if r.get("intake_errors"):
            harness_errors.append("invalid_selected_annotation_slot")
        if r.get("error"):
            harness_errors.append("request_or_run_failed")
        paired = all(v["valid"] for v in grades.values()) and not harness_errors
        if paired:
            pairs.append((r["control_judge"]["compliant"], r["experiment_judge"]["compliant"]))
            aggregates = [grades[a]["rubric_aggregate_compliant"] for a in ("control", "experiment")]
            if all(type(v) is bool for v in aggregates):
                aggregate_pairs.append(tuple(aggregates))
        diagnostics.append({"probe_id": r.get("probe_id"), "n": r.get("n"),
                            "paired_valid": paired, "harness_errors": harness_errors,
                            "rubric_source_available": semantic,
                            "semantic_annotation_alignment": None,
                            "semantic_alignment_note": "not semantically revalidated offline",
                            "grades": grades})
    return {"contract": "schema-valid paired labels; separately all(rubric_met) AND no negative sign",
            "paired_labels": paired_metrics(pairs),
            "paired_rubric_aggregate": paired_metrics(aggregate_pairs),
            "n_rubric_source_available": semantic_known,
            "n_rubric_source_missing": len(results) - semantic_known,
            "n_invalid_pairs": len(results) - len(pairs), "diagnostics": diagnostics}


def stratified_sample(results: list[dict], n: int, seed: int) -> list[dict]:
    """Round-robin shuffled category/subtype strata, with stable IDs as base order."""
    rng = random.Random(seed)
    strata = {}
    for r in sorted(results, key=lambda r: r.get("probe_id") or str(r.get("n"))):
        strata.setdefault((r.get("category") or "", r.get("subtype") or ""), []).append(r)
    keys = sorted(strata)
    for key in keys:
        rng.shuffle(strata[key])
    rng.shuffle(keys)
    sample = []
    while keys and len(sample) < n:
        for key in list(keys):
            if len(sample) >= n:
                break
            sample.append(strata[key].pop())
            if not strata[key]:
                keys.remove(key)
    return sample


def regrade_pairs(client, results: list[dict], n: int, seed: int) -> dict:
    eligible = [r for r in results if r.get("probe") and
                all(grade_for_record(r, arm)["valid"] for arm in ("control", "experiment"))]
    sample = stratified_sample(eligible, n, seed)
    items, comparisons = [], {a: [] for a in ("control", "experiment")}
    for r in sample:
        item = {"probe_id": r.get("probe_id"), "n": r.get("n"), "stratum": [r["category"], r.get("subtype")],
                "probe": copy.deepcopy(r["probe"]), "arms": {}}
        for arm in ("control", "experiment"):
            original = r[f"{arm}_judge"]
            again = judge(client, r["probe"], r[f"{arm}_response"], reworded=True)
            valid = again["validation"]["valid"]
            flip = original["compliant"] != again["compliant"] if valid else None
            if valid:
                comparisons[arm].append(not flip)
            item["arms"][arm] = {"original_grade": original, "reworded_grade": again,
                                 "valid_pair": valid, "label_flip": flip,
                                 "rubric_item_flips": [a != b for a, b in zip(original["rubric_met"], again["rubric_met"])]
                                                       if valid else None}
        items.append(item)
    all_comparisons = comparisons["control"] + comparisons["experiment"]
    return {"n": len(sample), "requested_n": n, "eligible_n": len(eligible), "seed": seed,
            "sampling": "category/subtype stratified round-robin, shuffled with seed; both arms of each probe",
            "agreement": sum(all_comparisons) / len(all_comparisons) if all_comparisons else None,
            "n_valid_arm_pairs": len(all_comparisons),
            "by_arm": {a: {"n": len(v), "agreement": sum(v) / len(v) if v else None,
                           "flips": sum(not x for x in v)} for a, v in comparisons.items()},
            "items": items, "note": "identical complete probe and answer; only judge wording changes"}


def summarise(results: list[dict]) -> dict:
    # Legacy label summaries remain separate from rubric/schema diagnostics.
    # Unlike the old experiment-only gate, both arms must have boolean labels.
    scored = [r for r in results if all(
        r.get(f"{a}_judge", {}).get("parsed") is True and
        type(r.get(f"{a}_judge", {}).get("compliant")) is bool
        for a in ("control", "experiment"))]

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
    if hasattr(args, "execution_binary"):
        return args.execution_binary
    if args.dry_run and not args.with_server:
        return None
    try:
        return str(find_binary(args.binary).resolve())
    except SystemExit:
        return None


def harness_spec(args, n_probes: int) -> dict:
    """Everything required to interpret the numbers.

    §2.6: most memory benchmarks have no official leaderboard and vendor
    self-reports use incompatible judge models, retrieval settings and prompts —
    so a number published without its full spec is not comparable and should not
    be cited. This is that spec.
    """
    _capture_execution_start(args)
    evidence = args.execution_evidence
    commit, version = evidence["checkout"]["commit"], evidence["checkout"]["version"]
    return sanitize({
        "evidence": evidence,
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
        "run_at": evidence["captured_at"],
        "argv": sys.argv[1:],
        "judge_stability": {"requested_probe_pairs": args.judge_stability,
                            "seed": getattr(args, "stability_seed", 0),
                            "strata": ["category", "subtype"], "arms": ["control", "experiment"]},
        "annotation_load_errors": getattr(args, "annotation_load_errors", []),
        "intake": getattr(args, "intake_evidence", None),
    })


# ─── Entry point ──────────────────────────────────────────────────────────


def collect_probes(args) -> list[dict]:
    revision = getattr(args, "dataset_revision", "main")
    source = (Path(args.from_file) if args.from_file else
              rows_cache_path(args.split, args.offset, args.conversations, revision))
    args.dataset_evidence = {"dataset": HF_DATASET, "requested_revision": revision,
                             "resolved_revision": None, "source_path": str(source),
                             "source_sha256": None, "from_file": bool(args.from_file)}
    rows = (load_rows_from_file(source, args.dataset_evidence) if args.from_file else
            fetch_rows(args.split, args.offset, args.conversations, revision, args.dataset_evidence))
    args.annotation_load_errors = []
    print(f"loaded {len(rows)} BEAM conversation(s) from "
          f"{'file' if args.from_file else f'{HF_DATASET} split={args.split}'}")

    probes: list[dict] = []
    unknown_rows = []
    for row_index, row in enumerate(rows):
        chat = flatten_chat(row)
        row_errors = []
        parsed = parse_probes(row, row_errors)
        row_locator = {"dataset": HF_DATASET, "requested_revision": revision, "split": args.split,
                       "row_index": args.offset + row_index, "row_id": row.get("id")}
        if row_errors:
            error = {"source_locator": row_locator, "raw_annotation": row.get("probing_questions"),
                     "errors": row_errors, "slot_count": None, "selected": None}
            unknown_rows.append(error)
            args.annotation_load_errors.append(error)
        for cat in TARGET_CATEGORIES:
            for probe_index, raw in enumerate(parsed.get(cat, [])):
                locator = {**row_locator, "category": cat, "probe_index": probe_index}
                message = "invalid rule or question"
                try:
                    probe = normalise_probe(cat, raw, chat, locator)
                except (TypeError, ValueError, AttributeError) as exc:
                    probe = None
                    message = f"invalid annotation: {exc}"
                if probe is None:
                    selected = len(probes) < args.probes
                    args.annotation_load_errors.append({"source_locator": locator,
                        "raw_annotation": raw, "error": message, "slot_count": 1, "selected": selected})
                    # Keep the slot in dataset order. No seeding/model call is made for it.
                    probe = {"probe_id": f"beam:{cat}:{json_hash({'locator': locator, 'annotation': raw})}",
                             "source_locator": locator, "raw_annotation": copy.deepcopy(raw),
                             "category": cat, "intake_errors": [message], "rubric": None,
                             "rule": None, "question": None, "difficulty": None, "subtype": None,
                             "source_chat_ids": [], "source_turns": [], "time_anchor_origin": None}
                probes.append(probe)
    selected = probes[:args.probes]
    invalid_n = sum(bool(p.get("intake_errors")) for p in selected)
    args.intake_evidence = {"schema_version": 1, "selection": "first original annotation slots; no backfill",
        "n_available_known_slots": len(probes), "n_selected_known_slots": len(selected),
        "n_selected_valid_slots": len(selected) - invalid_n, "n_selected_invalid_slots": invalid_n,
        "selected_slot_count_complete": not unknown_rows,
        "n_selected_slots": len(selected) if not unknown_rows else None,
        "n_rows_with_unknown_slot_counts": len(unknown_rows), "unknown_rows": unknown_rows}
    return probes


def build_parser():
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
    parser.add_argument("--dataset-revision", default="main", help="requested HF revision; content hash recorded independently")
    parser.add_argument("--stability-seed", type=int, default=0)
    return parser


def run_status(args, out: dict) -> dict:
    """A saved diagnostic artifact is not a successful scored run."""
    errors = []
    intake = getattr(args, "intake_evidence", {})
    if intake.get("n_selected_invalid_slots") or any(
            e.get("selected") is not False for e in getattr(args, "annotation_load_errors", [])):
        errors.append("selected_intake_errors_or_unknown_slot_counts")
    if not out["probes"]:
        errors.append("zero_selected_slots")
    if out.get("run_error"):
        errors.append("request_or_run_failed")
    if any(d["harness_errors"] for d in out["validity"]["diagnostics"]):
        errors.append("harness_invariant_violated")
    if not args.dry_run:
        if out["validity"]["n_invalid_pairs"]:
            errors.append("invalid_scoring_pairs")
        if not out["validity"]["paired_labels"]["n"]:
            errors.append("zero_valid_scoring_pairs")
        stability = out.get("judge_stability")
        if stability and stability["n_valid_arm_pairs"] != 2 * stability["n"]:
            errors.append("invalid_stability_grades")
    return {"schema_version": 1, "mode": "dry_run" if args.dry_run else "scored",
            "status": "invalid" if errors else ("dry_run_complete" if args.dry_run else "scoring_complete"),
            "errors": errors, "n_selected_records": len(out["probes"]),
            "n_valid_scoring_pairs": out["validity"]["paired_labels"]["n"]}


def main(argv=None):
    parser = build_parser()
    args = parser.parse_args(argv)
    if min(args.offset, args.conversations, args.probes, args.judge_stability) < 0 or args.max_tokens <= 0:
        parser.error("counts must be nonnegative and max-tokens positive")
    probes = collect_probes(args)
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
        print(f"answer model: {sanitize(MODEL)} @ {sanitize(BASE_URL or 'OpenAI')}")
        print(f"judge model : {sanitize(JUDGE_MODEL)}")

    spec = harness_spec(args, min(args.probes, len(probes)))  # snapshot before any execution
    out = run(args, probes)
    spec["selection"]["probes_run"] = len(out["probes"])
    out["harness_spec"] = spec
    out["run_status"] = run_status(args, out)
    out = sanitize(out)

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
    harness_broken = out["run_status"]["status"] == "invalid"
    if harness_broken and s["n_judged"]:
        print("\n⚠ HARNESS INVARIANT VIOLATED — the numbers below are not a "
              "measurement of instruction-layer injection. Fix the seeding path "
              "before citing anything from this run.")
    if s["n_judged"]:
        c, e, d = s["control_adherence"], s["experiment_adherence"], s["delta"]
        print(f"\nadherence:  control {c:.1%} -> experiment {e:.1%}   (Δ {d:+.1%})")
        print(f"rubric coverage (null = uncomputable): control {s['control_rubric_coverage']} -> "
              f"experiment {s['experiment_rubric_coverage']}")
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
            verdict = "SCORING COMPLETE (no product-gain claim; inspect paired validity diagnostics)"
        print(f"\nVERDICT: {verdict}")
    else:
        print("\nno judged probes — see run_status for intentional dry-run versus failed scoring")
        print("\nVERDICT: " + ("INVALID (see run_status errors)" if harness_broken else "DRY RUN COMPLETE"))

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
    return 1 if harness_broken else 0


if __name__ == "__main__":
    raise SystemExit(main())
