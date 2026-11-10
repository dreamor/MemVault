#!/usr/bin/env python3
"""
MemoryAgentBench (Conflict Resolution) harness for MemVault.

Why this benchmark
------------------
BEAM measures instruction/preference adherence — MemVault's L3/L6 claim. It
does not measure whether a *factual* corpus can be retrieved and reconciled.
MemoryAgentBench (ICLR 2026) fills that gap with a Conflict Resolution split
built specifically around stale facts: every context is a numbered fact list
where a larger serial number means a newer fact, and the model must answer from
the newest fact rather than the version it memorised from pre-training.

That is the cleanest public test of the §9.1.1 claim that MemVault can carry a
corpus across sessions and that supersession/delta-write semantics keep newer
truth reachable. It is also deliberately adversarial for a memory store: 455
facts (6K) up to 18,332 facts (262K), 100 questions per row, and the answers are
scored by the official substring-exact-match rule, not by an LLM judge.

Channel honesty
---------------
The official benchmark has two arms. `long_context_agent` stuffs the whole
context into the prompt; `agentic_memory_agent` retrieves from memory. MemVault
is measured as the second kind, with `/api/search` (keyword FTS by default,
embeddings off) standing in for the tool call the official agent makes.

The `/api/session` injection channel is *not* used as the primary arm. A fact
corpus is REFERENCE priority, and §6.1.2's deterministic MUST path deliberately
injects nothing for REFERENCE memories — verified on the shipped binary:
`formatted` is empty when only REFERENCE facts exist. Reporting that as a
retrieval failure would be dishonest, so the harness records it separately as
an insight about which channel is appropriate for facts. `--arm inject` exists
only to make that measurable; the headline number is `--arm search`.

Scoring
-------
Copied verbatim from the official repo (`utils/eval_other_utils.py`):
lowercase, strip punctuation, drop a/an/the, collapse whitespace, then check
whether any accepted ground truth is a substring of the prediction. No judge is
involved, so there is no judge-sensitivity caveat to report.

Usage
-----
    pip install pyarrow openai
    export VERIFY_BASE_URL=... OPENAI_API_KEY=... VERIFY_MODEL=...
    cargo build -p memvault-mcp

    # retrieval only, no LLM calls: seeds all facts and measures gold coverage
    python3 docs/experiments/memory_agent_bench.py --dry-run --questions 100

    # full search arm + control on the smallest conflict subset
    python3 docs/experiments/memory_agent_bench.py \
        --subsets factconsolidation_sh_6k --questions 100 \
        --arm both --top-k 20 \
        --binary target/debug/memvault-mcp \
        --out docs/experiments/mab_results.sh6k.search20.json
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import socket
import statistics
import string
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]

HF_DATASET = "ai-hyz/MemoryAgentBench"
HF_PARQUET_URL = (
    "https://huggingface.co/datasets/ai-hyz/MemoryAgentBench/resolve/main/"
    "data/Conflict_Resolution-00000-of-00001.parquet"
)
DEFAULT_PARQUET = Path(tempfile.gettempdir()) / "mab_conflict.parquet"

MODEL = os.getenv("VERIFY_MODEL", "gpt-4o-mini")
BASE_URL = os.getenv("VERIFY_BASE_URL") or os.getenv("OPENAI_BASE_URL") or None
_extra_body_raw = os.getenv("VERIFY_EXTRA_BODY", "").strip()
EXTRA_BODY = json.loads(_extra_body_raw) if _extra_body_raw else None

# ─── Official prompts, verbatim from MemoryAgentBench utils/templates.py ──
# Held as constants so the harness spec can carry them. If these drift from
# the benchmark, every previously published number stops being comparable.
SYSTEM_MESSAGE = (
    "You are a helpful assistant that can read the context and memorize it "
    "for future retrieval."
)

# `factconsolidation` / `agentic_memory_agent`, verbatim.
QUERY_TEMPLATE = (
    "Pretend you are a knowledge management system. Each fact in the  Archival "
    "Memory is provided with a serial number at the beginning, and the newer "
    "fact has larger serial number. \n You need to solve the conflicts of facts "
    "in the Archival Memory by finding the newest fact with larger serial "
    "number. You need to answer a question based on this rule. You should give "
    "a very concise answer without saying other words for the question **only** "
    "from the knowledge pool you have memorized rather than the real facts in "
    "real world. \n\nFor example:\n\n [Archival Memory] \n\n Question: Based on "
    "the Archival Memory, what is the name of the current president of Russia? "
    "\nAnswer: Donald Trump \n\n Now Answer the Question: Based on the  Archival "
    "Memory, {question} \nAnswer:"
)

# `factconsolidation` / `long_context_agent`, verbatim — used for the control
# arm only so both arms see the same answer instruction.
CONTROL_QUERY_TEMPLATE = (
    "Pretend you are a knowledge management system. Each fact in the knowledge "
    "pool is provided with a serial number at the beginning, and the newer fact "
    "has larger serial number. \n You need to solve the conflicts of facts in "
    "the knowledge pool by finding the newest fact with larger serial number. "
    "You need to answer a question based on this rule. You should give a very "
    "concise answer without saying other words for the question **only** from "
    "the knowledge pool you have memorized rather than the real facts in real "
    "world. \n\nFor example:\n\n [Knowledge Pool] \n\n Question: Based on the "
    "provided Knowledge Pool, what is the name of the current president of "
    "Russia? \nAnswer: Donald Trump \n\n Now Answer the Question: Based on the "
    "provided Knowledge Pool, {question} \nAnswer:"
)

MEMORY_HEADER = "Archival Memory (MemVault search results for this question):"


# ─── Official scoring, copied from utils/eval_other_utils.py ─────────────


def normalize_answer(answer_text: str) -> str:
    text = answer_text.lower()
    text = "".join(char for char in text if char not in string.punctuation)
    text = re.sub(r"\b(a|an|the)\b", " ", text)
    return " ".join(text.split())


def substring_exact_match(prediction: str, ground_truths: list[str]) -> bool:
    """True if any accepted answer is a substring of the prediction."""
    norm_pred = normalize_answer(prediction)
    return any(normalize_answer(gt) in norm_pred for gt in ground_truths)


# ─── Server / data plumbing ──────────────────────────────────────────────


def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


class MemVaultServer:
    def __init__(self, binary: Path):
        self.port = free_port()
        self.base = f"http://127.0.0.1:{self.port}"
        self.dbdir = tempfile.mkdtemp(prefix="memvault-mab-")
        env = {**os.environ, "MEMVAULT_EMBEDDING_PROVIDER": "off"}
        self.proc = subprocess.Popen(
            [str(binary), "--db", f"{self.dbdir}/mab.db",
             "--transport", "http", "--port", str(self.port)],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        for _ in range(120):
            time.sleep(0.25)
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

    def _req(self, method: str, path: str, body=None, timeout: int = 120):
        req = urllib.request.Request(
            f"{self.base}{path}",
            data=json.dumps(body).encode() if body is not None else None,
            headers={"Content-Type": "application/json"},
            method=method,
        )
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return json.loads(r.read())

    def seed_fact(self, content: str, namespace: str) -> str:
        """Insert one benchmark fact verbatim.

        `force_insert=true` is deliberate. Delta-write would merge two facts
        about the same subject into one memory, which resolves the benchmark's
        conflict *before* retrieval and would make the measurement about the
        writer rather than about retrieval + in-context conflict resolution.
        The benchmark wants both the old and the new fact reachable, with their
        serial numbers intact, so the agent under test has to apply the
        "larger serial wins" rule itself.
        """
        body = {
            "content": content,
            "type": "fact",
            "priority": "REFERENCE",
            "namespace": namespace,
            "agent_id": "mab-harness",
            "agent_type": "coding-assistant",
            "force_insert": True,
        }
        resp = self._req("POST", "/api/memories", body)
        return (resp.get("data") or {}).get("id", "")

    def search(self, query: str, top_k: int, namespace: str) -> tuple[list[dict], float]:
        t0 = time.perf_counter()
        resp = self._req("POST", "/api/search", {
            "query": query,
            "mode": "keyword",
            "top_k": top_k,
            "namespace": namespace,
        })
        elapsed = time.perf_counter() - t0
        return (resp.get("data") or []), elapsed

    def session(self, context_hint: str) -> tuple[str, float]:
        t0 = time.perf_counter()
        resp = self._req("POST", "/api/session",
                         {"agent_id": "codex", "context_hint": context_hint})
        elapsed = time.perf_counter() - t0
        data = resp.get("data") or {}
        return data.get("formatted", ""), elapsed


def fetch_parquet(path: Path) -> Path:
    if path.exists():
        return path
    print(f"downloading {HF_PARQUET_URL} -> {path}", file=sys.stderr)
    tmp = path.with_suffix(".part")
    with urllib.request.urlopen(HF_PARQUET_URL, timeout=600) as r, open(tmp, "wb") as f:
        while chunk := r.read(1 << 20):
            f.write(chunk)
    tmp.rename(path)
    return path


def load_rows(parquet: Path) -> list[dict]:
    try:
        import pyarrow.parquet as pq
    except ImportError:
        raise SystemExit("ERROR: pip install pyarrow")
    return pq.read_table(parquet).to_pylist()


FACT_RE = re.compile(r"^\s*(\d+)\.\s*(.+?)\s*$")


def parse_facts(context: str) -> list[tuple[int, str]]:
    facts = []
    for line in context.splitlines():
        m = FACT_RE.match(line)
        if m:
            facts.append((int(m.group(1)), line.strip()))
    return facts


def subset_of(row: dict) -> str:
    ids = ((row.get("metadata") or {}).get("qa_pair_ids") or [""])
    return ids[0].rsplit("_no", 1)[0]


# ─── LLM ─────────────────────────────────────────────────────────────────


def make_client():
    try:
        from openai import OpenAI
    except ImportError:
        raise SystemExit("ERROR: pip install openai")
    return OpenAI(base_url=BASE_URL, api_key=os.getenv("OPENAI_API_KEY") or "local")


_MIN_CALL_INTERVAL = float(os.getenv("VERIFY_MIN_CALL_INTERVAL_S", "1.1"))
_last_call_at = 0.0


def _chat_with_retry(client, **kwargs):
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
        try:
            return client.chat.completions.create(**kwargs)
        except Exception as exc:  # noqa: BLE001 - provider-specific subclasses vary
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


def answer(client, question: str, memory_block: str, max_tokens: int) -> str:
    system = SYSTEM_MESSAGE
    if memory_block:
        system = f"{system}\n\n{memory_block}"
    template = QUERY_TEMPLATE if memory_block else CONTROL_QUERY_TEMPLATE
    text = ""
    for attempt in range(3):
        resp = _chat_with_retry(
            client, model=MODEL,
            messages=[
                {"role": "system", "content": system},
                {"role": "user", "content": template.format(question=question)},
            ],
            max_tokens=max_tokens,
            temperature=0.0,
            **(({"extra_body": EXTRA_BODY}) if EXTRA_BODY else {}),
        )
        text = resp.choices[0].message.content or ""
        if text.strip():
            return text.strip()
        if attempt < 2:
            time.sleep(0.5 * (attempt + 1))
    return text.strip()


def percentile(values: list[float], p: float) -> float | None:
    if not values:
        return None
    values = sorted(values)
    k = (len(values) - 1) * p
    lo, hi = int(k), min(int(k) + 1, len(values) - 1)
    return values[lo] + (values[hi] - values[lo]) * (k - lo)


# ─── Main ────────────────────────────────────────────────────────────────


def main() -> int:
    ap = argparse.ArgumentParser(
        description="MemoryAgentBench Conflict Resolution harness for MemVault")
    ap.add_argument("--parquet", default=str(DEFAULT_PARQUET),
                    help="Conflict_Resolution parquet (downloaded if absent)")
    ap.add_argument("--subsets", default="factconsolidation_sh_6k",
                    help="comma-separated subset names (qa_pair_id prefix)")
    ap.add_argument("--questions", type=int, default=100,
                    help="max questions per subset (default 100 = whole row)")
    ap.add_argument("--top-k", type=int, default=20,
                    help="memories retrieved per question (default 20)")
    ap.add_argument("--max-tokens", type=int, default=300)
    ap.add_argument("--arm", choices=["search", "control", "both", "inject"],
                    default="search",
                    help="search = MemVault retrieval arm; control = no memory; "
                         "both = search + control; inject = /api/session diagnostic")
    ap.add_argument("--binary", default=str(REPO_ROOT / "target/debug/memvault-mcp"))
    ap.add_argument("--dry-run", action="store_true",
                    help="seed + retrieve + score coverage, no LLM calls")
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    parquet = fetch_parquet(Path(args.parquet))
    rows = load_rows(parquet)
    by_subset = {}
    for row in rows:
        by_subset[subset_of(row)] = row

    wanted = [s.strip() for s in args.subsets.split(",") if s.strip()]
    missing = [s for s in wanted if s not in by_subset]
    if missing:
        raise SystemExit(f"unknown subset(s): {missing}; available: {sorted(by_subset)}")

    client = None if args.dry_run else make_client()
    binary = Path(args.binary)
    if not binary.exists():
        raise SystemExit(f"binary not found: {binary} (cargo build -p memvault-mcp)")

    server = MemVaultServer(binary)
    started = dt.datetime.now(dt.timezone.utc)
    results = []
    try:
        for subset in wanted:
            row = by_subset[subset]
            facts = parse_facts(row["context"])
            namespace = f"mab-{subset}"
            print(f"[{subset}] seeding {len(facts)} facts ...", file=sys.stderr)
            seed_t0 = time.perf_counter()
            for _, text in facts:
                server.seed_fact(text, namespace)
            seed_s = time.perf_counter() - seed_t0
            print(f"[{subset}] seeded in {seed_s:.1f}s", file=sys.stderr)

            facts_serial = {serial: text for serial, text in facts}
            newest_serial = max(facts_serial) if facts_serial else -1
            questions = row["questions"][: args.questions]
            answers = row["answers"][: args.questions]
            qa_ids = ((row.get("metadata") or {}).get("qa_pair_ids") or [])[: args.questions]

            per_q = []
            for i, (question, gold_sets) in enumerate(zip(questions, answers)):
                # answers[i] is a list of acceptable strings (pyarrow:
                # list<list<string>> per row field). Flatten one level only;
                # flattening twice turns "pesäpallo" into its characters.
                golds = [g for g in gold_sets if isinstance(g, str)]
                retrieved, search_s = server.search(question, args.top_k, namespace)
                contents = [r.get("memory", {}).get("content", "") for r in retrieved]

                def contains_gold(text: str) -> bool:
                    return any(normalize_answer(g) in normalize_answer(text)
                               for g in golds)

                gold_hit = any(contains_gold(c) for c in contents)
                gold_top1 = bool(contents) and contains_gold(contents[0])
                gold_top5 = any(contains_gold(c) for c in contents[:5])
                retrieved_serials = []
                for c in contents:
                    m = FACT_RE.match(c)
                    if m:
                        retrieved_serials.append(int(m.group(1)))

                record = {
                    "question": question,
                    "golds": golds,
                    "qa_pair_id": qa_ids[i] if i < len(qa_ids) else None,
                    "retrieved_n": len(contents),
                    "retrieved_contents": contents,
                    "retrieved_serials": retrieved_serials,
                    "newest_serial_in_context": newest_serial,
                    "gold_in_retrieval": gold_hit,
                    "gold_in_top1": gold_top1,
                    "gold_in_top5": gold_top5,
                    "search_latency_s": search_s,
                    "retrieved_tokens_est": sum(len(c) for c in contents) / 4,
                }

                if not args.dry_run:
                    if args.arm in ("search", "both"):
                        block = ""
                        if contents:
                            block = MEMORY_HEADER + "\n" + "\n".join(contents)
                        pred = answer(client, question, block, args.max_tokens)
                        record["search_prediction"] = pred
                        record["search_correct"] = substring_exact_match(pred, golds)
                    if args.arm in ("control", "both"):
                        pred = answer(client, question, "", args.max_tokens)
                        record["control_prediction"] = pred
                        record["control_correct"] = substring_exact_match(pred, golds)
                    if args.arm == "inject":
                        block, inject_s = server.session(question)
                        record["injected"] = block
                        record["injected_tokens_est"] = len(block) / 4
                        record["injection_latency_s"] = inject_s
                        pred = answer(client, question, block, args.max_tokens)
                        record["inject_prediction"] = pred
                        record["inject_correct"] = substring_exact_match(pred, golds)
                per_q.append(record)
                if (i + 1) % 10 == 0:
                    print(f"[{subset}] {i + 1}/{len(questions)}", file=sys.stderr)

            def rate(key):
                vals = [q[key] for q in per_q if key in q]
                return (sum(bool(v) for v in vals) / len(vals)) if vals else None

            summary = {
                "subset": subset,
                "n_questions": len(per_q),
                "n_facts": len(facts),
                "seed_seconds": round(seed_s, 2),
                "retrieval_gold_coverage": rate("gold_in_retrieval"),
                "retrieval_top1_gold_coverage": rate("gold_in_top1"),
                "retrieval_top5_gold_coverage": rate("gold_in_top5"),
                "mean_retrieved_n": statistics.mean(
                    q["retrieved_n"] for q in per_q) if per_q else None,
                "mean_retrieved_tokens_est": statistics.mean(
                    q["retrieved_tokens_est"] for q in per_q) if per_q else None,
                "search_latency_p50_s": percentile(
                    [q["search_latency_s"] for q in per_q], 0.5),
                "search_latency_p95_s": percentile(
                    [q["search_latency_s"] for q in per_q], 0.95),
                "search_accuracy": rate("search_correct"),
                "control_accuracy": rate("control_correct"),
                "inject_accuracy": rate("inject_correct"),
                "mean_injected_tokens_est": (
                    statistics.mean(q["injected_tokens_est"] for q in per_q)
                    if per_q and "injected_tokens_est" in per_q[0] else None),
            }
            if summary["search_accuracy"] is not None and summary["control_accuracy"] is not None:
                summary["search_minus_control"] = (
                    summary["search_accuracy"] - summary["control_accuracy"])
            results.append({"summary": summary, "questions": per_q})
    finally:
        server.stop()

    def git(*cmd):
        try:
            return subprocess.run(["git", "-C", str(REPO_ROOT), *cmd],
                                  capture_output=True, text=True).stdout.strip()
        except Exception:
            return ""

    out = {
        "generated_at": started.isoformat(),
        "harness_spec": {
            "benchmark": "MemoryAgentBench / Conflict_Resolution",
            "dataset": HF_DATASET,
            "parquet": str(parquet),
            "subsets": wanted,
            "arms": {
                "search": "MemVault /api/search (keyword FTS, embeddings off) "
                          "results placed in the Archival Memory block",
                "control": "no memory context, same answer instruction",
                "inject": "MemVault /api/session formatted block (diagnostic; "
                          "REFERENCE facts are not injected by design)"
            },
            "models": {"answer": MODEL, "base_url": BASE_URL or "default"},
            "memvault": {
                "binary": str(binary),
                "commit": git("rev-parse", "HEAD"),
                "branch": git("rev-parse", "--abbrev-ref", "HEAD"),
            },
            "prompts": {
                "system": SYSTEM_MESSAGE,
                "query_search": QUERY_TEMPLATE,
                "query_control": CONTROL_QUERY_TEMPLATE,
                "memory_header": MEMORY_HEADER,
            },
            "scoring": {
                "rule": "official substring_exact_match: normalize (lowercase, "
                        "strip punctuation, drop a/an/the, collapse whitespace), "
                        "then accept if any ground truth is a substring of the "
                        "prediction",
                "source": "MemoryAgentBench utils/eval_other_utils.py",
            },
            "options": {
                "questions_per_subset": args.questions,
                "top_k": args.top_k,
                "max_tokens": args.max_tokens,
                "arm": args.arm,
                "dry_run": args.dry_run,
            },
            "argv": sys.argv,
        },
        "results": results,
    }

    if args.out:
        Path(args.out).write_text(json.dumps(out, indent=2, ensure_ascii=False))
        print(f"wrote {args.out}", file=sys.stderr)

    for r in results:
        s = r["summary"]
        print(json.dumps(s, indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
