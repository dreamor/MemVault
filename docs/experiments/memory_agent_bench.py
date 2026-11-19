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

The `/api/session/preview` rendered-injection channel is a separate read-only
diagnostic, not the primary arm. REFERENCE facts can be injected; an empty formatted block does not prove
they are excluded by design. Agent profile, project/namespace scope, channel,
and eligibility matter. The harness records the request and full session
response so those causes can be investigated. The default preserves the legacy
keyword/off/force-insert search baseline, now explicitly named raw_keyword.

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
import hashlib
import io
import json
import math
import os
import shutil
import re
import secrets
import socket
import statistics
import string
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

# Keep evidence serialization identical between the two standalone harnesses.
from beam_adherence import (EMBEDDING_ENV_KEYS, content_hash, execution_identity, final_completion,
                            isolate_server_environment, json_hash, paired_metrics,
                            register_secret, response_data, sanitize, server_environment)
from benchmark_evidence import EXPERIMENT_ENV_KEYS

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


def child_environment(provider: str = "off", api_base: str | None = None,
                      model: str | None = None, dimension: int | None = None,
                      api_key_env: str | None = None, native_cache_dir: str | None = None,
                      candidate_k: int | None = None, keyword_anchor_k: int | None = None) -> dict:
    """Isolate the server from parent model keys and MemVault tuning/config.

    Existing provider variables are documented in core embedding.rs. An opt-in
    key is forwarded only from the explicitly named environment variable.
    `local` retains the existing Ollama alias; `native` is the in-process provider.
    Resolve an explicit native model cache in the parent before HOME is isolated.
    """
    if native_cache_dir is not None and (not native_cache_dir.strip() or provider != "native"):
        raise ValueError("native-cache-dir must be nonempty and requires embedding-provider=native")
    env = server_environment()
    env["MEMVAULT_EMBEDDING_PROVIDER"] = provider
    if native_cache_dir is not None:
        env["MEMVAULT_EMBEDDING_CACHE_DIR"] = str(Path(native_cache_dir).expanduser().resolve())
    for key, value in (("MEMVAULT_EMBEDDING_API_BASE", api_base),
                       ("MEMVAULT_EMBEDDING_MODEL", model), ("MEMVAULT_EMBEDDING_DIM", dimension)):
        if value is not None:
            env[key] = str(value)
    if api_key_env:
        if not os.getenv(api_key_env):
            raise ValueError(f"embedding API key environment variable is unset: {api_key_env}")
        env["MEMVAULT_EMBEDDING_API_KEY"] = os.environ[api_key_env]
        register_secret(env["MEMVAULT_EMBEDDING_API_KEY"])
    if candidate_k is not None:
        env["MEMVAULT_HYBRID_CANDIDATE_K"] = str(candidate_k)
    if keyword_anchor_k is not None:
        env["MEMVAULT_HYBRID_KEYWORD_ANCHORS"] = str(keyword_anchor_k)
    return env


class MemVaultServer:
    def __init__(self, binary: Path, mode: str = "keyword", provider: str = "off",
                 environment: dict | None = None, session_agent: str = "codex",
                 session_project: str | None = None, producer: dict | None = None,
                 readiness_timeout: float = 120.0):
        self.proc, self.dbdir, self.isolation = None, None, None
        self.records, self.headers = [], {}
        self.mode, self.producer, self.identity_verified = mode, producer, None
        self.session_agent, self.session_project = session_agent, session_project
        self.readiness = {"required": mode in ("semantic", "hybrid"), "status": "not_attempted",
                          "timeout_seconds": readiness_timeout, "records": [],
                          "note": "health is startup readiness only, never evidence of a query's effective mode"}
        try:
            if not math.isfinite(readiness_timeout) or readiness_timeout <= 0:
                raise ValueError("embedding-ready-timeout must be positive and finite")
            self.port = free_port()
            self.base = f"http://127.0.0.1:{self.port}"
            self.dbdir = str(Path(tempfile.mkdtemp(prefix="memvault-mab-")).resolve())
            self.isolation = {"data_dir": self.dbdir}
            requested = environment if environment is not None else child_environment(provider)
            env, self.env_file, db = isolate_server_environment(self.dbdir, "mab.db", requested)
            self.isolation.update({"db": str(db), "env_file": str(self.env_file),
                                   "env_file_sha256": content_hash(self.env_file), "child_environment": sanitize(env)})
            if producer is not None:
                # main.rs passes SQLite side_db_path.parent()/agents.yaml to the router.
                registry = db.parent / "agents.yaml"
                key = secrets.token_urlsafe(32)
                register_secret(key)  # before file/request/trace/exception recording
                profile = {**producer["profile"], "api_key": key}
                registry.write_text(json.dumps({"agents": [profile]}, indent=2), encoding="utf-8")  # JSON is YAML
                registry.chmod(0o600)
                self.headers = {"X-MemVault-Agent-Id": producer["agent_id"], "X-MemVault-Api-Key": key}
                self.isolation.update({"registry_path": str(registry), "registry_public": {"agents": [producer["profile"]]},
                                       "registry_path_source": "memvault-mcp/src/main.rs side_db_path.parent()/agents.yaml"})
            self.proc = subprocess.Popen(
                [str(binary), "--env-file", str(self.env_file), "--db", str(db),
                 "--transport", "http", "--port", str(self.port)],
                env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            deadline = time.monotonic() + 30.0
            while time.monotonic() < deadline:
                try:
                    if self._req("GET", "/health", timeout=min(2.0, max(0.001, deadline - time.monotonic()))).strip() == "ok":
                        break
                except Exception:
                    if self.proc.poll() is not None:
                        raise RuntimeError("memvault-mcp exited during startup")
                time.sleep(0.25)
            else:
                raise RuntimeError("memvault-mcp did not become healthy in time")
            self.wait_for_embeddings(readiness_timeout)
        except BaseException as exc:
            cleanup_errors = []
            try:
                self.stop()
            except Exception as cleanup_exc:
                cleanup_errors.append(sanitize(str(cleanup_exc)))
            finally:
                if self.dbdir is not None:
                    try:
                        shutil.rmtree(self.dbdir)
                    except OSError as cleanup_exc:
                        cleanup_errors.append(sanitize(str(cleanup_exc)))
            failure = RuntimeError(sanitize(str(exc)))
            failure.startup_evidence = sanitize({"records": self.records, "isolation": self.isolation,
                "readiness": self.readiness, "cleanup_errors": cleanup_errors})
            raise failure from None

    def wait_for_embeddings(self, timeout: float):
        if not self.readiness["required"]:
            self.readiness["status"] = "not_required"
            return
        self.readiness["status"] = "waiting"
        deadline = time.monotonic() + timeout
        before = len(self.records)
        try:
            while time.monotonic() < deadline:
                try:
                    health = self._req("GET", "/api/health", timeout=min(2.0, max(0.001, deadline - time.monotonic())))
                except Exception:
                    if self.proc.poll() is not None:
                        raise RuntimeError("memvault-mcp exited while waiting for embeddings")
                else:
                    if not isinstance(health, dict) or any(type(health.get(k)) is not bool for k in
                                                         ("embedding_expected", "embedding_ready")):
                        raise ValueError("/api/health must report embedding_expected and embedding_ready as real booleans")
                    if health["embedding_expected"] is not True:
                        raise ValueError("requested semantic/hybrid mode but /api/health embedding_expected=false")
                    if health["embedding_ready"] is True:
                        self.readiness["status"] = "ready"
                        return
                time.sleep(min(0.25, max(0.0, deadline - time.monotonic())))
            raise TimeoutError("embedding readiness timed out")
        except Exception as exc:
            self.readiness.update({"status": "failed", "error": sanitize(str(exc))})
            raise
        finally:
            self.readiness["records"] = self.records[before:]

    def stop(self):
        if self.proc is None:
            return
        proc, self.proc = self.proc, None
        try:
            proc.terminate()
            proc.wait(timeout=5)
        except (subprocess.TimeoutExpired, OSError):
            proc.kill()
            proc.wait(timeout=5)

    def _req(self, method: str, path: str, body=None, timeout: int = 120,
             accepted_statuses: tuple[int, ...] = ()):
        headers = {"Content-Type": "application/json", **getattr(self, "headers", {})}
        req = urllib.request.Request(
            f"{self.base}{path}",
            data=json.dumps(body).encode() if body is not None else None,
            headers=headers,
            method=method,
        )
        record = {"request": {"method": method, "path": path, "body": sanitize(body),
                              "headers": sanitize(headers)}, "response": None, "error": None}
        self.records.append(record)

        def capture(response):
            raw = response.read().decode("utf-8")
            record.update({"status": response.status, "response_raw": sanitize(raw)})
            value = raw if path == "/health" else json.loads(raw)
            record["response"] = sanitize(value)
            return value

        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                return capture(r)
        except urllib.error.HTTPError as exc:
            record["error"] = sanitize(str(exc))
            value = capture(exc)
            if exc.code in accepted_statuses:
                return value
            raise
        except Exception as exc:
            record["error"] = sanitize(str(exc))
            raise

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

    def snapshot(self, namespace: str) -> list[dict]:
        memories = []
        while True:
            query = urllib.parse.urlencode({"namespace": namespace, "limit": 100, "offset": len(memories)})
            response = self._req("GET", "/api/memories?" + query)
            page = response.get("data")
            if response.get("ok") is not True or not isinstance(page, list):
                raise ValueError("managed mode requires an auditable persisted MemoryDTO list")
            memories.extend(page)
            if len(page) < 100:
                return memories

    def verify_memory(self, memory: dict):
        if memory.get("identity_verified") is not True or memory.get("source_agent") != self.producer["agent_id"]:
            self.identity_verified = False
            raise ValueError("managed producer unverified: server must return identity_verified=true for the actual producer")
        if memory.get("human_reviewed") is not False or memory.get("ai_generated") is not True:
            raise ValueError("managed producer trust defaults changed: expected unreviewed AI fact")
        self.identity_verified = True

    def seed_managed_fact(self, content: str, namespace: str, assertion: dict | None, mode: str) -> dict:
        resolve = mode == "resolved" and assertion is not None
        body = {"content": content, "type": "fact", "priority": "REFERENCE", "namespace": namespace,
                "agent_id": self.producer["agent_id"], "agent_type": self.producer["agent_type"],
                "force_insert": not resolve, "fact_write": "resolve" if resolve else "preserve"}
        if assertion is not None:
            body["fact_assertion"] = assertion
        response = self._req("POST", "/api/memories", body, accepted_statuses=(400, 409))
        http_status = self.records[-1].get("status")
        data = response.get("data")
        if not isinstance(data, dict):
            raise ValueError("managed save API returned no structured decision")
        data = dict(data)
        outcome = data.get("fact_outcome")
        if resolve:
            allowed = {"inserted", "merged", "skipped", "superseded", "idempotent", "stale",
                       "ambiguity", "conflict", "review_required", "unsupported"}
            if outcome not in allowed or type(data.get("written")) is not bool:
                raise ValueError("requested fact_write=resolve API missing or ignored; raw fallback is forbidden")
            if outcome == "unsupported":
                raise ValueError("requested fact_write=resolve API is unsupported")
            refused = outcome in {"ambiguity", "conflict", "review_required"}
            if refused:
                if http_status != 409 or response.get("ok") is not False or data["written"]:
                    raise ValueError("refused resolve decision must report HTTP 409, ok=false and written=false")
                return data  # protected status tracked, never counted as an insert
            if http_status != 200 or response.get("ok") is not True or not data.get("id"):
                raise ValueError("resolve did not return HTTP 200 and a persisted current memory ID")
            if data["written"] != (outcome in {"inserted", "merged", "superseded"}):
                raise ValueError("resolve outcome/written disagree")
            if outcome == "superseded" and (not data.get("superseded_id") or data["superseded_id"] == data["id"]):
                raise ValueError("superseded resolution must return distinct old and new persisted IDs")
            memory = data.get("memory")
            if not isinstance(memory, dict) or memory.get("id") != data["id"]:
                raise ValueError("resolve must report the actual persisted MemoryDTO")
            self.verify_memory(memory)
            management = memory.get("fact_management") or {}
            current_assertion = memory.get("fact_assertion") or {}
            if (management.get("state") != "active" or management.get("producer_id") != self.producer["agent_id"]
                    or management.get("record_id") != memory["id"] or management.get("namespace") != namespace
                    or management.get("key") != assertion["key"] or management.get("source_namespace") != assertion["source_namespace"]
                    or current_assertion.get("key") != assertion["key"] or current_assertion.get("cardinality") != "single"
                    or current_assertion.get("source_namespace") != assertion["source_namespace"]
                    or management.get("source_order") != current_assertion.get("source_order")):
                raise ValueError("requested resolve API ignored: actual managed active state is absent or inconsistent")
            if outcome in {"inserted", "superseded", "idempotent"} and current_assertion != assertion:
                raise ValueError("resolve persisted assertion does not match requested source assertion")
            if outcome == "stale" and current_assertion.get("source_order", -1) <= assertion["source_order"]:
                raise ValueError("stale resolve must report a newer persisted source serial")
            if data["written"] and memory.get("content") != content:
                raise ValueError("resolve returned another input as a newly written fact")
        else:
            if response.get("ok") is not True or not data.get("id") or data.get("action") != "saved":
                raise ValueError("literal preserve force-insert failed")
            # Legacy force-insert result need not have a decision/DTO. Never
            # fabricate management. Verify the actual first row via read API;
            # a final snapshot verifies every forced row without O(n²) reads.
            if "written" not in data:
                data.update({"written": True, "written_evidence": "literal force-insert saved action; audited persisted snapshot"})
            if self.identity_verified is not True:
                memory = next((m for m in self.snapshot(namespace) if m.get("id") == data["id"]), None)
                if memory is None:
                    raise ValueError("forced input ID not found in persisted store")
                self.verify_memory(memory)
                if memory.get("fact_management") is not None or memory.get("content") != content or memory.get("fact_assertion") != assertion:
                    raise ValueError("force-insert must literally preserve assertion/content without management")
        return data

    def search(self, query: str, top_k: int, namespace: str) -> tuple[list[dict], float]:
        t0 = time.perf_counter()
        resp = self._req("POST", "/api/search", {
            "query": query,
            "mode": self.mode,
            "top_k": top_k,
            "namespace": namespace,
        })
        elapsed = time.perf_counter() - t0
        if not isinstance(resp, dict) or resp.get("ok") is False or not isinstance(resp.get("data"), list):
            raise ValueError("/api/search did not return a successful hit list")
        return resp["data"], elapsed

    def session(self, context_hint: str) -> tuple[str, float]:
        """Preview the requested profile without replacing the operational actor.

        No fallback to /api/session: that endpoint now runs as the header actor
        and would change this diagnostic's profile and session side effects.
        """
        t0 = time.perf_counter()
        body = {"agent_id": self.session_agent, "context_hint": context_hint}
        if self.session_project is not None:
            body["project"] = self.session_project
        resp = self._req("POST", "/api/session/preview", body)
        elapsed = time.perf_counter() - t0
        data = resp.get("data") or {}
        return data.get("formatted", ""), elapsed


def parquet_url(revision: str = "main") -> str:
    from urllib.parse import quote
    return (f"https://huggingface.co/datasets/{HF_DATASET}/resolve/{quote(revision, safe='')}/"
            "data/Conflict_Resolution-00000-of-00001.parquet")


def parquet_cache_path(revision: str = "main") -> Path:
    return Path(tempfile.gettempdir()) / f"mab_conflict__{json_hash({'dataset': HF_DATASET, 'revision': revision})}.parquet"


def fetch_parquet(path: Path, revision: str = "main") -> Path:
    if path.exists():
        return path
    url = parquet_url(revision)
    print(f"downloading {url} -> {path}", file=sys.stderr)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".part")
    with urllib.request.urlopen(url, timeout=600) as r, open(tmp, "wb") as f:
        while chunk := r.read(1 << 20):
            f.write(chunk)
    tmp.rename(path)
    return path


def load_rows(parquet: Path, evidence: dict | None = None) -> list[dict]:
    try:
        import pyarrow.parquet as pq
    except ImportError:
        raise SystemExit("ERROR: pip install pyarrow")
    payload = parquet.read_bytes()
    if evidence is not None:
        evidence["source_sha256"] = hashlib.sha256(payload).hexdigest()
        evidence["hash_capture_phase"] = "read_bytes"
    return pq.read_table(io.BytesIO(payload)).to_pylist()


FACT_RE = re.compile(r"^\s*(\d+)\.\s*(.+?)\s*$")


def parse_facts(context: str) -> list[tuple[int, str]]:
    facts = []
    for line in context.splitlines():
        m = FACT_RE.match(line)
        if m:
            facts.append((int(m.group(1)), line.strip()))
    return facts


# Bounded, auditable bridge extraction for the second-hop experiment. These
# rules only read retrieved corpus sentences; questions, golds and support
# annotations are never inputs.
BRIDGE_PATTERNS = (
    ("sport", re.compile(r" is associated with the sport of (.+?)\.?$", re.I)),
    ("country_of_origin", re.compile(r" (?:was|is) created in the country of (.+?)\.?$", re.I)),
    ("spoken_language", re.compile(r" (?:speaks|writes|signs) the language of (.+?)\.?$", re.I)),
    ("located_in", re.compile(r" is located in the (?:city|country|state|region) of (.+?)\.?$", re.I)),
    ("citizenship", re.compile(r" is a citizen of (.+?)\.?$", re.I)),
    ("field", re.compile(r" works in the field of (.+?)\.?$", re.I)),
    ("religion", re.compile(r" is affiliated with the religion of (.+?)\.?$", re.I)),
    ("generic_is", re.compile(r" (?:is|was|are|were) (.+?)\.?$", re.I)),
)


def extract_bridge_entities(contents: list[str], limit: int,
                            hit_sources: list | None = None) -> list[dict]:
    """Extract up to ``limit`` bridge strings from retrieved fact sentences.

    Facts recalled by the keyword channel are considered first, ordered by
    their keyword rank. A reserved anchor may sit at the bottom of the
    reranked context, but it is usually the query-relevant bridge; extracting
    in raw context order would instead pick unrelated top-ranked noise.
    """
    if limit <= 0:
        return []
    candidates = []
    for index, content in enumerate(contents):
        match = FACT_RE.match(content)
        if not match:
            continue
        sentence = match.group(2)
        keyword_rank = None
        if hit_sources is not None and index < len(hit_sources):
            for source in hit_sources[index] or []:
                found_rank = re.fullmatch(r"kw#(\d+)", str(source))
                if found_rank:
                    keyword_rank = int(found_rank.group(1))
                    break
        for rule, pattern in BRIDGE_PATTERNS:
            found = pattern.search(sentence)
            if not found:
                continue
            query = found.group(1).strip().strip("\"'")
            if len(query) >= 2:
                candidates.append(((0 if keyword_rank else 1,
                                    keyword_rank if keyword_rank else 10**9,
                                    index),
                                   {"query": query, "rule": rule,
                                    "source_serial": int(match.group(1)),
                                    "source_sentence": sentence,
                                    "source_keyword_rank": keyword_rank}))
            break
    candidates.sort(key=lambda item: item[0])
    bridges, seen = [], set()
    for _, bridge in candidates:
        if bridge["query"].lower() in seen:
            continue
        seen.add(bridge["query"].lower())
        bridges.append(bridge)
        if len(bridges) >= limit:
            break
    return bridges


# Deliberately finite, auditable templates from atomic input sentences. No QA,
# answer, annotation, date or model input is accepted by this parser. These two
# FactConsolidation templates describe single slots in this benchmark, not a
# general claim that authorship/sport relations are always single-valued.
ASSERTION_TEMPLATES = (
    ("sport_association", r"(?P<subject>.+?) is associated with the sport of (?P<value>.+)\."),
    ("book_author", r"The author of (?P<subject>.+?) is (?P<value>.+)\."),
)
ASSERTION_PARSER_VERSION = 1


def source_scope(subset: str, context: str) -> str:
    """Stable dataset/subset/input scope; independent of QA and annotations."""
    return "mab:" + json_hash({"dataset": HF_DATASET, "split": "Conflict_Resolution",
                               "subset": subset, "context": context})


def parse_assertion(serial: int, content: str, scope: str) -> dict:
    match = FACT_RE.fullmatch(content)
    if type(serial) is not int or not 0 <= serial <= (1 << 64) - 1:
        raise ValueError("fact source serial must fit u64")
    if not match or int(match.group(1)) != serial:
        raise ValueError("assertion source_order must be the original input serial")
    sentence = match.group(2)
    for predicate, pattern in ASSERTION_TEMPLATES:
        parsed = re.fullmatch(pattern, sentence)
        if parsed:
            subject, value = (parsed.group(k).strip() for k in ("subject", "value"))
            # Do not infer sentence boundaries from the case/length of the
            # token before a dot: "A. Bob left" and "AB. Bob left" are unsafe.
            # The only dotted-value exception is the finite full-name format
            # with one middle initial (e.g. Ursula K. Le Guin); leading initials,
            # abbreviations and dotted subjects otherwise remain ambiguous/raw.
            middle_initial_name = predicate == "book_author" and re.fullmatch(
                r"[A-Z][a-z]+(?: [A-Z][a-z]+)* [A-Z]\. [A-Z][a-z]+(?: [A-Z][a-z]+)*", value)
            if (re.search(r"[.?!\n\r\\]", subject)
                    or (re.search(r"[.?!\n\r\\]", value) and not middle_initial_name)):
                return {"assertion": None, "template": predicate,
                        "reason": "ambiguous_internal_sentence_boundary_or_abbreviation_preserved_raw"}
            # Lists/conjunctions are not silently coerced to single cardinality.
            # Event/unknown/multi templates are otherwise unsupported altogether.
            if (not subject or not value
                    or value.casefold() in {"unknown", "not known", "unspecified", "uncertain", "various", "multiple"}
                    or re.search(r"[,;\n!?]|\b(?:and|or|is|was|are|were)\b", value, re.I)):
                return {"assertion": None, "template": predicate,
                        "reason": "non_single_or_non_atomic_value_preserved_raw"}
            key = json.dumps(["mab-slot-v1", predicate, " ".join(subject.split())],
                             ensure_ascii=False, separators=(",", ":"))
            return {"assertion": {"key": key, "value": value, "source_namespace": scope,
                                   "source_order": serial, "cardinality": "single"},
                    "template": predicate, "reason": None}
    return {"assertion": None, "template": None, "reason": "unsupported_template_preserved_raw"}


def managed_producer(scopes: dict[str, str]) -> dict:
    """Same public actor/profile for either managed strategy on the same inputs."""
    identity = json_hash(scopes)
    actor = "mab-producer-" + identity
    namespaces = {subset: "project:mab-managed-" + json_hash(scope) for subset, scope in scopes.items()}
    profile = {"id": actor, "agent_type": "coding-assistant",
               "description": "Run-owned MemoryAgentBench producer",
               "inject_rules": {"max_memories": 8, "token_budget": 1500,
                                "priority_order": ["MUST", "REFERENCE"],
                                "namespace_filter": list(namespaces.values()), "exclude_types": []}}
    return {"agent_id": actor, "agent_type": "coding-assistant",
            "namespaces": namespaces, "profile": profile}


def retrieved_source_ids(retrieved: list[dict], facts: list[dict]) -> tuple[list[int], list[str]]:
    """Match actual persisted content + ID, never treat an ephemeral ID as serial.

    Stale/idempotent writes can return another input's existing ID. Only the
    exact original numbered content identifies which source was retrieved.
    """
    by_content = {f["content"]: f for f in facts}
    serials, source_ids = [], []
    for result in retrieved:
        memory = result.get("memory") or {}
        fact = by_content.get(memory.get("content"))
        if fact and fact.get("memory_id") == memory.get("id"):
            serials.append(fact["serial"])
            source_ids.append(fact["source_id"])
    return serials, source_ids


def ingest_diagnostics(facts: list[dict], snapshot: list[dict] | None, mode: str) -> dict:
    outcomes = {}
    for fact in facts:
        decision = fact.get("seed_decision") or {}
        outcome = decision.get("fact_outcome") or decision.get("action") or "error"
        outcomes[outcome] = outcomes.get(outcome, 0) + 1
    live = [m for m in (snapshot or []) if not m.get("superseded_by") and not m.get("archived_at")]
    managed = [m for m in live if (m.get("fact_management") or {}).get("state") == "active"]
    return {"schema_version": 1, "ingest_mode": mode, "n_input_facts": len(facts),
            "n_corpus_facts": len(facts), "n_seed_attempted": len(facts), "n_seed_unattempted": 0,
            "n_run_errors": 0, "run_error": None,
            "n_parsed": sum(bool(f.get("fact_assertion")) for f in facts),
            "n_unparsed": sum(not f.get("fact_assertion") for f in facts),
            "n_unparsed_preserved_raw": sum(not f.get("fact_assertion") and
                                             (f.get("seed_decision") or {}).get("written") is True for f in facts),
            "outcomes": outcomes, "n_stale": outcomes.get("stale", 0),
            "n_conflicts": outcomes.get("conflict", 0),
            "n_refused": sum(outcomes.get(k, 0) for k in ("ambiguity", "conflict", "review_required", "unsupported")),
            "n_errors": sum(bool(f.get("seed_error")) for f in facts),
            "n_written": sum((f.get("seed_decision") or {}).get("written") is True for f in facts),
            "n_superseded": outcomes.get("superseded", 0),
            "snapshot_complete": snapshot is not None,
            "n_latest_active": len(live) if snapshot is not None else None,
            "n_managed_active": len(managed) if snapshot is not None else None,
            "n_latest_asserted_active": sum(m.get("fact_assertion") is not None for m in live) if snapshot is not None else None,
            "n_suspended": sum((m.get("fact_management") or {}).get("state") == "suspended" for m in snapshot) if snapshot is not None else None,
            "n_literal_unmanaged_active": sum(m.get("fact_management") is None for m in live) if snapshot is not None else None,
            "management_note": "managed_raw is authenticated/asserted but force-inserted heads remain literal unmanaged records",
            "as_of": {"older_indices_full_triple_chain_verified": False, "status": "future_only_not_tested"}}


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
        record = {"attempt": attempt + 1, "request": sanitize(kwargs), "response": None, "error": None}
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
    system = SYSTEM_MESSAGE
    if memory_block:
        system = f"{system}\n\n{memory_block}"
    template = QUERY_TEMPLATE if memory_block else CONTROL_QUERY_TEMPLATE
    text = ""
    for attempt in range(3):
        resp = _chat_with_retry(
            client, trace=trace, model=MODEL,
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


# ─── Optional offline support annotations and replayable metrics ─────────


def load_support_annotations(path: str | None, evidence: dict | None = None) -> dict:
    """Optional JSON keyed by qa_pair_id; never included in answer prompts.

    Each entry: support_sets=[[serial,...],...], with optional
    latest_serial_by_key={annotation_fact_key: serial}. Any one *complete* set
    is legitimate support; a union of fragments is not a complete alternative.
    """
    if path is None:
        return {}
    payload = Path(path).read_bytes()
    if evidence is not None:
        evidence.update({"source_sha256": hashlib.sha256(payload).hexdigest(),
                         "hash_capture_phase": "read_bytes"})
    annotations = json.loads(payload)
    if not isinstance(annotations, dict):
        raise ValueError("support annotations must be an object keyed by qa_pair_id")
    return annotations


def validate_support_annotation(annotation: dict | None, corpus_serials: set[int]) -> dict | None:
    if annotation is None:
        return None
    if not isinstance(annotation, dict):
        raise ValueError("support annotation must be an object")
    sets = annotation.get("support_sets")
    if not isinstance(sets, list) or not sets or any(
        not isinstance(s, list) or not s or any(type(i) is not int or i not in corpus_serials for i in s)
        for s in sets):
        raise ValueError("support_sets must contain nonempty complete serial sets from this corpus")
    latest = annotation.get("latest_serial_by_key")
    if latest is not None and (not isinstance(latest, dict) or any(
        not isinstance(k, str) or not k or type(v) is not int or v not in corpus_serials
        for k, v in latest.items())):
        raise ValueError("latest_serial_by_key must reference annotated corpus facts")
    return annotation


def support_coverage(annotation: dict | None, retrieved_serials: list[int]) -> bool | None:
    if annotation is None:
        return None
    found = set(retrieved_serials)
    return any(set(s) <= found for s in annotation["support_sets"])


def retrieval_modes(retrieved: list[dict], requested: str) -> dict:
    legal = ("keyword", "semantic", "hybrid")
    hits = retrieved if isinstance(retrieved, list) else []
    modes = [r.get("search_mode") if isinstance(r, dict) else None for r in hits]
    known = [m for m in modes if isinstance(m, str) and m in legal]
    reasons = [r["fallback_reason"] for r in hits if isinstance(r, dict) and r.get("fallback_reason")]
    explicit = bool(reasons) or any(isinstance(r, dict) and any(r.get(k) is True for k in
                                  ("fallback", "fallback_observed", "degraded")) for r in hits)
    mismatches = sum(m != requested for m in known)
    unknown = len(hits) - len(known)
    degraded = explicit or bool(mismatches)
    status = "degraded" if degraded else ("valid" if hits and not unknown and requested in legal else "unknown")
    return {"schema_version": 1, "requested_mode": requested, "actual_modes": sorted(set(known)) or None,
            "actual_mode_by_hit": modes, "status": status, "valid": status == "valid",
            "n_hits": len(hits), "n_unknown_hits": unknown, "n_mismatched_hits": mismatches,
            "fallback_observed": True if degraded else (False if status == "valid" else None),
            "fallback_reason": reasons or None,
            "note": "every hit must report a legal exact requested mode; empty/missing fields are unknown; health is not query proof"}


def seed_embedding_evidence(records: list[dict], memory_id: str | None, required: bool) -> dict:
    """Only the actual POST decision can prove this seed's vector is ready.

    Bare/current IDs, provider health and unrelated snapshot fields are not
    evidence of a vector for the returned ID. Do not invent a polling API.
    """
    index = next((i for i in range(len(records) - 1, -1, -1) if
                  records[i].get("request", {}).get("method") == "POST" and
                  records[i].get("request", {}).get("path") == "/api/memories"), None)
    response = records[index].get("response") if index is not None else None
    data = response.get("data") if isinstance(response, dict) else None
    data = data if isinstance(data, dict) else {}
    embedded, pending = data.get("embedded"), data.get("vector_pending")
    if not required:
        status = "not_required"
    elif (not memory_id or data.get("id") != memory_id or response.get("ok") is not True or
          type(embedded) is not bool or type(pending) is not bool):
        status = "unknown"
    else:
        status = "valid" if embedded is True and pending is False else "invalid"
    return {"schema_version": 1, "required": required, "status": status, "valid": status in ("valid", "not_required"),
            "record_index": index, "memory_id": memory_id, "response_memory_id": data.get("id"),
            "embedded": embedded, "vector_pending": pending,
            "note": "requires this POST's successful returned ID, embedded=true and vector_pending=false; health is not seed proof"}


def answer_completion(prediction: str | None, records: list[dict]) -> dict:
    metadata = final_completion(records)
    complete = metadata.get("complete") is True
    nonempty = isinstance(prediction, str) and bool(prediction.strip())
    return {**metadata, "schema_version": 1, "complete": complete, "nonempty": nonempty,
            "valid": complete and nonempty}


def score_admissible(question: dict, arm: str) -> bool:
    if type(question.get(f"{arm}_correct")) is not bool:
        return False
    mode = question.get("retrieval_mode")
    mode_contract = isinstance(mode, dict) and ("valid" in mode or "schema_version" in mode)
    if arm != "control" and mode_contract and mode.get("valid") is not True:
        return False
    completion = question.get(f"{arm}_completion")
    completion_contract = isinstance(completion, dict) and ("valid" in completion or "schema_version" in completion)
    return not completion_contract or completion.get("valid") is True


def run_status(results: list[dict], expected: int | None, requested_arm: str, dry_run: bool,
               execution_complete: bool, run_error: str | None, records: list[dict]) -> dict:
    questions = [q for entry in results for q in entry["questions"]]
    mode_counts = {s: sum((q.get("retrieval_mode") or {}).get("status", "unknown") == s for q in questions)
                   for s in ("valid", "degraded", "unknown")}
    actual_mode_counts = {m: sum((q.get("retrieval_mode") or {}).get("actual_mode_by_hit", []).count(m)
                                for q in questions) for m in ("keyword", "semantic", "hybrid")}
    arms = ("search", "control") if requested_arm == "both" else (requested_arm,)
    valid_scores = sum(q.get("execution_complete") is True and (q.get("retrieval_mode") or {}).get("valid") is True
                       and all(score_admissible(q, a) for a in arms) for q in questions)
    facts = [f for e in results for f in e["facts"]]
    seed_counts = {s: sum((f.get("seed_embedding") or {}).get("status", "unknown") == s for f in facts)
                   for s in ("valid", "invalid", "unknown", "not_required")}
    incomplete_answers = sum(q.get(f"{a}_completion", {}).get("valid") is False for q in questions for a in arms)
    completed = sum(q.get("execution_complete") is True for q in questions)
    errors = []
    if run_error is not None:
        errors.append("run_error")
    if expected == 0 or not questions:
        errors.append("zero_questions")
    if not execution_complete or expected is None or completed != expected:
        errors.append("partial_run")
    if mode_counts["degraded"]:
        errors.append("degraded_retrieval_modes")
    if mode_counts["unknown"]:
        errors.append("unknown_retrieval_modes")
    if seed_counts["invalid"] or seed_counts["unknown"]:
        errors.append("invalid_or_unknown_seed_embeddings")
    if incomplete_answers:
        errors.append("incomplete_answers")
    if not dry_run and (not valid_scores or valid_scores != expected):
        errors.append("invalid_or_missing_scoring_questions")
    answer_records = [r for q in questions for a in ("search", "control", "inject")
                      for r in q.get(f"{a}_answer_records", [])]
    return {"status": "invalid" if errors else ("dry_run_complete" if dry_run else "scoring_complete"),
            "valid": not errors, "errors": errors, "execution_complete": execution_complete,
            "n_expected_questions": expected, "n_questions": len(questions), "n_completed_questions": completed,
            "n_valid_scoring_questions": valid_scores, "n_valid_scoring_pairs": valid_scores if requested_arm == "both" else 0,
            "retrieval_mode_counts": mode_counts, "actual_mode_hit_counts": actual_mode_counts,
            "seed_embedding_counts": seed_counts, "n_incomplete_answers": incomplete_answers,
            "n_invalid_intake_questions": sum(q.get("intake_valid") is False for q in questions),
            "n_unknown_hits": sum((q.get("retrieval_mode") or {}).get("n_unknown_hits", 0) for q in questions),
            "n_empty_retrievals": sum(q.get("retrieved_objects") == [] for q in questions),
            "n_question_errors": sum(bool(q.get("error")) for q in questions),
            "n_seed_errors": sum(bool(f.get("seed_error")) for e in results for f in e["facts"]),
            "n_request_errors": sum(bool(r.get("error")) for r in records + answer_records),
            "n_run_errors": int(run_error is not None)}


def question_score_contract(per_q: list[dict], requested_arm: str | None = None) -> dict:
    unpaired = {}
    for arm in ("search", "control", "inject"):
        values = [q[f"{arm}_correct"] for q in per_q if score_admissible(q, arm)]
        unpaired[arm] = {"n_scored": len(values), "n_correct": sum(values),
                         "accuracy": sum(values) / len(values) if values else None}
    pairs = [(q["control_correct"], q["search_correct"]) for q in per_q
             if all(score_admissible(q, a) for a in ("control", "search"))]
    comparison = requested_arm == "both" or (requested_arm is None and
                  all(unpaired[a]["n_scored"] for a in ("search", "control")))
    return {"schema_version": 1, "requested_arm": requested_arm,
            "headline_denominator": "complete_search_control_pairs" if comparison else "requested_single_arm",
            "comparison_requested_or_observed": comparison,
            "paired_search_control": paired_metrics(pairs), "unpaired_by_arm": unpaired}


def summarise_questions(per_q: list[dict], requested_arm: str | None = None) -> dict:
    def rate(key):
        vals = [q[key] for q in per_q if type(q.get(key)) is bool]
        return sum(vals) / len(vals) if vals else None
    def numbers(key):
        return [q[key] for q in per_q if type(q.get(key)) in (int, float)]
    def mean(key):
        values = numbers(key)
        return statistics.mean(values) if values else None
    result = {
        "n_questions": len(per_q), "retrieval_gold_coverage": rate("gold_in_retrieval"),
        "retrieval_top1_gold_coverage": rate("gold_in_top1"),
        "retrieval_top5_gold_coverage": rate("gold_in_top5"),
        "mean_retrieved_n": mean("retrieved_n"),
        "mean_retrieved_tokens_est": mean("retrieved_tokens_est"),
        "search_latency_p50_s": percentile(numbers("search_latency_s"), 0.5),
        "search_latency_p95_s": percentile(numbers("search_latency_s"), 0.95),
        "search_accuracy": rate("search_correct"), "control_accuracy": rate("control_correct"),
        "inject_accuracy": rate("inject_correct"),
        "mean_injected_tokens_est": statistics.mean(q["injected_tokens_est"] for q in per_q
                                                     if "injected_tokens_est" in q)
                                    if any("injected_tokens_est" in q for q in per_q) else None,
    }
    scores = question_score_contract(per_q, requested_arm)
    for arm, values in scores["unpaired_by_arm"].items():
        result[f"{arm}_accuracy"] = values["accuracy"]
    if scores["comparison_requested_or_observed"]:
        paired = scores["paired_search_control"]
        result.update({"search_accuracy": paired["experiment"], "control_accuracy": paired["control"],
                       "search_minus_control": paired["delta"]})
    return result


def mab_diagnostics(per_q: list[dict], requested_arm: str | None = None) -> dict:
    pairs, mismatches = [], []
    for q in per_q:
        computed = {a: substring_exact_match(q[f"{a}_prediction"], q["golds"])
                    for a in ("control", "search") if isinstance(q.get(f"{a}_prediction"), str)}
        if len(computed) == 2:
            pairs.append((computed["control"], computed["search"]))
        for arm, correct in computed.items():
            if q.get(f"{arm}_correct") is not correct:
                mismatches.append({"qa_pair_id": q.get("qa_pair_id"), "arm": arm,
                                   "stored": q.get(f"{arm}_correct"), "replayed": correct})
    annotated = [q for q in per_q if type(q.get("support_in_retrieval")) is bool]
    return {"validated_scores": question_score_contract(per_q, requested_arm),
            "paired_prediction_scores": paired_metrics(pairs), "stored_score_mismatches": mismatches,
            "support_annotated_n": len(annotated), "support_unannotated_n": len(per_q) - len(annotated),
            "support_coverage": sum(q["support_in_retrieval"] for q in annotated) / len(annotated) if annotated else None,
            "support_top1_coverage": sum(q["support_in_top1"] for q in annotated) / len(annotated) if annotated else None,
            "support_top5_coverage": sum(q["support_in_top5"] for q in annotated) / len(annotated) if annotated else None,
            "note": "string answer coverage is not supporting-fact coverage; absent annotations are null"}


# ─── Main ────────────────────────────────────────────────────────────────


def build_parser():
    ap = argparse.ArgumentParser(
        description="MemoryAgentBench Conflict Resolution harness for MemVault")
    ap.add_argument("--parquet", default=None,
                    help="explicit local parquet; otherwise use a revision-keyed download cache")
    ap.add_argument("--subsets", default="factconsolidation_sh_6k",
                    help="comma-separated subset names (qa_pair_id prefix)")
    ap.add_argument("--questions", type=int, default=100,
                    help="max questions per subset (default 100 = whole row)")
    ap.add_argument("--top-k", type=int, default=20,
                    help="memories retrieved per question (default 20)")
    ap.add_argument("--candidate-k", type=int, default=None,
                    help="experimental hybrid candidate count fetched per channel before rerank; requires top-k or more")
    ap.add_argument("--keyword-anchor-k", type=int, default=0,
                    help="experimental keyword-only anchors appended before rerank (default 0)")
    ap.add_argument("--question-ids", default="",
                    help="comma-separated qa_pair_ids to run exactly, in the given order")
    ap.add_argument("--two-hop-bridges", type=int, default=0,
                    help="experimental bounded second hop: number of bridge entities to query (default 0 = off)")
    ap.add_argument("--two-hop-per-query", type=int, default=5,
                    help="results fetched per bridge query (default 5)")
    ap.add_argument("--max-tokens", type=int, default=300)
    ap.add_argument("--arm", choices=["search", "control", "both", "inject"],
                    default="search",
                    help="search = MemVault retrieval arm; control = no memory; "
                         "both = search + control; inject = /api/session/preview diagnostic")
    ap.add_argument("--ingest-mode", choices=["raw", "managed_raw", "resolved"], default="raw",
                    help="one strategy per fresh server/DB: raw legacy; managed_raw authenticated asserted literal preserve; "
                         "resolved authenticated single-slot resolution (unsupported sentences remain raw)")
    ap.add_argument("--binary", default=str(REPO_ROOT / "target/debug/memvault-mcp"))
    ap.add_argument("--dry-run", action="store_true",
                    help="seed + retrieve + score coverage, no LLM calls")
    ap.add_argument("--out", default=None)
    ap.add_argument("--search-mode", choices=["keyword", "semantic", "hybrid"], default="keyword")
    ap.add_argument("--embedding-provider", choices=["off", "ollama", "local", "native", "openai", "openai-compatible"], default="off",
                    help="local is the legacy Ollama alias; native is the in-process embedding provider")
    ap.add_argument("--native-cache-dir", help="explicit native model cache, resolved in parent HOME; requires native provider")
    ap.add_argument("--embedding-ready-timeout", type=float, default=120.0,
                    help="positive finite seconds to await semantic/hybrid /api/health readiness (default 120)")
    ap.add_argument("--embedding-api-base")
    ap.add_argument("--embedding-model")
    ap.add_argument("--embedding-dim", type=int)
    ap.add_argument("--embedding-api-key-env", help="name of environment variable to forward only to the child; never a literal key")
    ap.add_argument("--dataset-revision", default="main")
    ap.add_argument("--support-annotations", help="optional offline support-set JSON keyed by qa_pair_id")
    ap.add_argument("--session-agent", default="codex", help="legacy default; unknown agents may resolve to default profile")
    ap.add_argument("--session-project", default=None, help="explicit /api/session/preview project scope diagnostic")
    return ap


def selected_question_indices(row: dict, args) -> list[int]:
    """Return original row indices for --question-ids, else the first N."""
    if not args.question_ids.strip():
        return list(range(min(args.questions, len(row["questions"]))))
    qa_ids = ((row.get("metadata") or {}).get("qa_pair_ids") or [])
    positions = {qa: index for index, qa in enumerate(qa_ids) if isinstance(qa, str)}
    wanted = [item.strip() for item in args.question_ids.split(",") if item.strip()]
    if not wanted:
        raise ValueError("question-ids must contain at least one nonempty id")
    missing = [qa for qa in wanted if qa not in positions]
    if missing:
        raise ValueError(f"unknown question ids: {missing}")
    return [positions[qa] for qa in wanted]


def main(argv=None) -> int:
    ap = build_parser()
    args = ap.parse_args(argv)
    if args.questions < 0 or args.top_k <= 0 or args.max_tokens <= 0 or (args.embedding_dim is not None and args.embedding_dim <= 0):
        ap.error("questions must be nonnegative; top-k, max-tokens and embedding-dim must be positive")
    if args.candidate_k is not None and args.candidate_k < args.top_k:
        ap.error("candidate-k must be greater than or equal to top-k")
    if args.candidate_k is not None and args.candidate_k <= 0:
        ap.error("candidate-k must be positive")
    if args.keyword_anchor_k < 0:
        ap.error("keyword-anchor-k must be nonnegative")
    if args.two_hop_bridges < 0 or (args.two_hop_bridges > 0 and args.two_hop_per_query <= 0):
        ap.error("two-hop-bridges must be nonnegative; two-hop-per-query must be positive when enabled")
    if not math.isfinite(args.embedding_ready_timeout) or args.embedding_ready_timeout <= 0:
        ap.error("embedding-ready-timeout must be positive and finite")
    if args.native_cache_dir is not None and (not args.native_cache_dir.strip() or args.embedding_provider != "native"):
        ap.error("native-cache-dir must be nonempty and requires embedding-provider=native")
    environment = {}
    annotation_evidence = {"source_path": args.support_annotations, "source_sha256": None}
    parquet = Path(args.parquet) if args.parquet else parquet_cache_path(args.dataset_revision)
    dataset_evidence = {"dataset": HF_DATASET, "requested_revision": args.dataset_revision,
                        "resolved_revision": None, "source_path": str(parquet), "source_sha256": None,
                        "explicit_local_source": args.parquet is not None}
    wanted = [s.strip() for s in args.subsets.split(",") if s.strip()]
    binary = Path(args.binary).resolve()
    started = dt.datetime.now(dt.timezone.utc)
    evidence = execution_identity(Path(__file__), binary, dataset_evidence)
    evidence["shared_evidence_harness_sha256"] = content_hash(Path(__file__).with_name("beam_adherence.py"))
    evidence["support_annotations"] = annotation_evidence
    producer, server = None, None
    results, startup_evidence = [], None
    run_error, execution_complete, expected_questions = None, False, None
    try:
        environment = child_environment(args.embedding_provider, args.embedding_api_base,
                                        args.embedding_model, args.embedding_dim, args.embedding_api_key_env,
                                        args.native_cache_dir, args.candidate_k, args.keyword_anchor_k)
        annotations = load_support_annotations(args.support_annotations, annotation_evidence)
        parquet = fetch_parquet(parquet, args.dataset_revision)
        rows = load_rows(parquet, dataset_evidence)
        by_subset = {subset_of(row): row for row in rows}
        missing = [s for s in wanted if s not in by_subset]
        if missing or not wanted:
            raise ValueError(f"unknown or empty subset selection: {missing}; available: {sorted(by_subset)}")
        selection = {subset: selected_question_indices(by_subset[subset], args) for subset in wanted}
        expected_questions = sum(len(indices) for indices in selection.values())
        if expected_questions == 0:
            raise ValueError("zero selected questions")
        for subset in wanted:
            row = by_subset[subset]
            if len(row["questions"]) != len(row["answers"]):
                raise ValueError(f"question/answer count mismatch for {subset}")
        if not binary.exists():
            raise ValueError(f"binary not found: {binary} (cargo build -p memvault-mcp)")
        scopes = {s: source_scope(s, by_subset[s]["context"]) for s in wanted} if args.ingest_mode != "raw" else {}
        producer = managed_producer(scopes) if scopes else None
        client = None if args.dry_run else make_client()
        server_args = (binary, args.search_mode, args.embedding_provider, environment,
                       args.session_agent, args.session_project)
        server_kwargs = {"producer": producer} if producer else {}
        if args.embedding_ready_timeout != 120.0:
            server_kwargs["readiness_timeout"] = args.embedding_ready_timeout
        # Exactly one chosen ingestion strategy. Constructor failures are inside the artifact boundary.
        server = MemVaultServer(*server_args, **server_kwargs)
        for subset in wanted:
            entry = {"summary": None, "questions": [], "facts": [],
                     "raw_mode": f"raw_{args.search_mode}" if producer is None else None,
                     "ingest_mode": args.ingest_mode}
            results.append(entry)
            # Initialize every subset local used by failure handling before parsing.
            row, facts, namespace = None, [], None
            fact_records, per_q = entry["facts"], entry["questions"]
            seed_t0, seed_s, corpus_max_serial = None, None, None
            facts_serial, questions, answers, qa_ids = {}, [], [], []
            entry["context_parse_complete"] = False
            row = by_subset[subset]
            facts = parse_facts(row["context"])
            entry["context_parse_complete"] = True
            namespace = producer["namespaces"][subset] if producer else f"mab-{subset}"
            if producer:
                if len({serial for serial, _ in facts}) != len(facts):
                    raise ValueError("duplicate corpus serials cannot be used as unambiguous source IDs")
                server.session_project = namespace  # actual isolated scope, same in both strategies
                entry["source_scope"] = scopes[subset]
            print(f"[{subset}] seeding {len(facts)} facts ...", file=sys.stderr)
            seed_t0 = time.perf_counter()
            fact_records = entry["facts"]
            for serial, text in facts:
                before = len(server.records)
                fact = {"fact_id": f"mab:{subset}:{serial}:{json_hash(text)}", "serial": serial,
                        "content": text, "memory_id": None,
                        "source_locator": {"dataset": HF_DATASET, "subset": subset, "serial": serial},
                        "seed_records": []}
                fact_records.append(fact)  # failed inputs retain their original immutable mapping slot
                try:
                    if producer:
                        parsed = parse_assertion(serial, text, scopes[subset])
                        fact.update({"source_id": fact["fact_id"], "source_serial": serial, "input_fact": text,
                                     "fact_assertion": parsed["assertion"], "assertion_parse": parsed})
                        decision = server.seed_managed_fact(text, namespace, parsed["assertion"], args.ingest_mode)
                        fact.update({"seed_decision": decision, "memory_id": decision.get("id")})
                    else:
                        fact["memory_id"] = server.seed_fact(text, namespace) or None
                        if fact["memory_id"] is None:
                            raise ValueError("seed returned no persisted memory ID")
                    fact["seed_embedding"] = seed_embedding_evidence(server.records[before:], fact["memory_id"],
                                                                     args.search_mode in ("semantic", "hybrid"))
                    if fact["seed_embedding"]["valid"] is not True:
                        raise ValueError("seed embedding evidence is " + fact["seed_embedding"]["status"] +
                                         ": require embedded=true and vector_pending=false for the returned ID")
                except Exception as exc:
                    fact["seed_error"] = sanitize(str(exc))
                    if producer:
                        seed_record = next((r for r in server.records[before:] if
                                            r.get("request", {}).get("method") == "POST" and
                                            r.get("request", {}).get("path") == "/api/memories"), {})
                        response = seed_record.get("response") or {}
                        fact["seed_decision"] = response.get("data") if isinstance(response.get("data"), dict) else None
                        fact["memory_id"] = (fact["seed_decision"] or {}).get("id")
                    raise
                finally:
                    fact["seed_records"] = server.records[before:]
                    if "seed_embedding" not in fact:
                        fact["seed_embedding"] = seed_embedding_evidence(fact["seed_records"], fact["memory_id"],
                                                                         args.search_mode in ("semantic", "hybrid"))
            if producer:
                before = len(server.records)
                snapshot = server.snapshot(namespace)
                entry["store_snapshot"], entry["snapshot_records"] = snapshot, server.records[before:]
                for memory in snapshot:
                    server.verify_memory(memory)
                    if memory.get("namespace") != namespace:
                        raise ValueError("persisted snapshot escaped the isolated source namespace")
                if server.identity_verified is not True:
                    raise ValueError("managed producer identity could not be verified from persisted responses")
                if args.ingest_mode == "resolved" and not any(f["fact_assertion"] for f in fact_records):
                    raise ValueError("resolved requested but no supported single-slot facts: resolve API cannot be verified")
                by_id = {m["id"]: m for m in snapshot}
                for fact in fact_records:
                    memory = by_id.get(fact["memory_id"])
                    fact["stored_as_input"] = bool(memory and memory.get("content") == fact["input_fact"])
                    if (fact.get("seed_decision") or {}).get("written") is True and not fact["stored_as_input"]:
                        raise ValueError("written input not present literally under its returned persisted ID")
                    if fact["stored_as_input"] and (args.ingest_mode == "managed_raw" or fact["fact_assertion"] is None):
                        if memory.get("fact_management") is not None or memory.get("fact_assertion") != fact["fact_assertion"]:
                            raise ValueError("force_insert literal preservation invariant violated")
                entry["ingest_diagnostics"] = ingest_diagnostics(fact_records, snapshot, args.ingest_mode)
            seed_s = time.perf_counter() - seed_t0
            print(f"[{subset}] seeded in {seed_s:.1f}s", file=sys.stderr)

            facts_serial = {serial: text for serial, text in facts}
            corpus_max_serial = max(facts_serial) if facts_serial else None
            if len(facts_serial) != len(facts):
                raise ValueError("duplicate corpus serials cannot be used as unambiguous support IDs")
            all_qa_ids = ((row.get("metadata") or {}).get("qa_pair_ids") or [])
            selected = selection[subset]
            questions = [row["questions"][index] for index in selected]
            answers = [row["answers"][index] for index in selected]
            qa_ids = [all_qa_ids[index] if index < len(all_qa_ids) else None for index in selected]

            per_q = entry["questions"]
            for position, (source_index, question, gold_sets) in enumerate(
                    zip(selected, questions, answers)):
                # answers[i] is a list of acceptable strings (pyarrow:
                # list<list<string>> per row field). Flatten one level only;
                # flattening twice turns "pesäpallo" into its characters.
                qa_id = qa_ids[position]
                record = {"question": question, "golds": gold_sets, "qa_pair_id": qa_id,
                          "question_id": qa_id or f"mab:{subset}:question:{source_index}:{json_hash(question)}",
                          "source_locator": {"dataset": HF_DATASET, "subset": subset, "question_index": source_index},
                          "execution_complete": False, "intake_valid": False, "search_records": [],
                          "retrieval_mode": retrieval_modes([], args.search_mode)}
                per_q.append(record)  # retain failed annotation/network slots, not only successful searches
                if not isinstance(question, str) or not question.strip() or not isinstance(gold_sets, list) or not gold_sets or any(
                        not isinstance(g, str) for g in gold_sets):
                    raise ValueError("question must be a nonempty string with a nonempty list of string golds")
                if any(not normalize_answer(g) for g in gold_sets):
                    record["intake_valid"] = False
                    raise ValueError("every accepted gold must remain nonempty after official normalization")
                record["intake_valid"] = True
                golds = list(gold_sets)
                annotation = validate_support_annotation(annotations.get(qa_id), set(facts_serial))
                before = len(server.records)
                try:
                    first_retrieved, search_s = server.search(question, args.top_k, namespace)
                finally:
                    record["search_records"] = server.records[before:]
                search_records = record["search_records"]
                first_contents = [r.get("memory", {}).get("content", "") for r in first_retrieved]
                retrieved = first_retrieved
                two_hop = {
                    "enabled": args.two_hop_bridges > 0,
                    "query_construction": "retrieved bridge entity only; questions/golds/support annotations are never query inputs",
                    "bridges": [], "records": [], "latency_s": 0.0,
                    "first_hop_n": len(first_retrieved), "added_n": 0,
                }
                if args.two_hop_bridges > 0:
                    bridges = extract_bridge_entities(
                        first_contents, args.two_hop_bridges,
                        [item.get("hit_sources", []) for item in first_retrieved])
                    seen_ids = {r.get("memory", {}).get("id") for r in first_retrieved}
                    union = list(first_retrieved)
                    total_hop_latency = 0.0
                    for bridge in bridges:
                        hop_before = len(server.records)
                        try:
                            extra, hop_latency = server.search(
                                bridge["query"], args.two_hop_per_query, namespace)
                        finally:
                            hop_records = server.records[hop_before:]
                        total_hop_latency += hop_latency
                        added = []
                        for item in extra:
                            memory_id = item.get("memory", {}).get("id")
                            if memory_id and memory_id not in seen_ids:
                                seen_ids.add(memory_id)
                                union.append(item)
                                added.append(item)
                        added_serials = []
                        for item in added:
                            found = FACT_RE.match(item.get("memory", {}).get("content", ""))
                            if found:
                                added_serials.append(int(found.group(1)))
                        two_hop["bridges"].append({
                            **bridge,
                            "latency_s": hop_latency,
                            "returned_n": len(extra),
                            "added_n": len(added),
                            "added_serials": added_serials,
                            "records": hop_records,
                        })
                        two_hop["records"].extend(hop_records)
                    two_hop["latency_s"] = total_hop_latency
                    two_hop["added_n"] = len(union) - len(first_retrieved)
                    two_hop["context_n"] = len(union)
                    retrieved = union
                record["two_hop"] = two_hop
                record["first_hop_objects"] = first_retrieved
                record["retrieved_objects"] = retrieved
                record["retrieval_mode"] = retrieval_modes(retrieved, args.search_mode)
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
                retrieved_ids = None
                if producer:
                    retrieved_serials, retrieved_ids = retrieved_source_ids(retrieved, fact_records)

                record.update({
                    "question": question,
                    "golds": golds,
                    "qa_pair_id": qa_id,
                    "question_id": qa_id or f"mab:{subset}:question:{source_index}:{json_hash(question)}",
                    "source_locator": {"dataset": HF_DATASET, "subset": subset, "question_index": source_index},
                    "retrieved_objects": retrieved,
                    "search_records": search_records,
                    "retrieval_mode": retrieval_modes(retrieved, args.search_mode),
                    "raw_mode": f"raw_{args.search_mode}" if producer is None else None,
                    "ingest_mode": args.ingest_mode,
                    "support_annotation": annotation,
                    "support_in_retrieval": support_coverage(annotation, retrieved_serials),
                    "support_in_top1": support_coverage(annotation, retrieved_source_ids(retrieved[:1], fact_records)[0] if producer else
                                                        [int(m.group(1)) for c in contents[:1] if (m := FACT_RE.match(c))]),
                    "support_in_top5": support_coverage(annotation, retrieved_source_ids(retrieved[:5], fact_records)[0] if producer else
                                                        [int(m.group(1)) for c in contents[:5] if (m := FACT_RE.match(c))]),
                    "question_latest_serial_by_key": (annotation or {}).get("latest_serial_by_key"),
                    "retrieved_n": len(contents),
                    "retrieved_contents": contents,
                    "retrieved_serials": retrieved_serials,
                    "corpus_max_serial": corpus_max_serial,
                    "retrieved_max_serial": max(retrieved_serials) if retrieved_serials else None,
                    # Deprecated compatibility alias: still means whole-corpus max.
                    "newest_serial_in_context": corpus_max_serial,
                    "gold_in_retrieval": gold_hit,
                    "gold_in_top1": gold_top1,
                    "gold_in_top5": gold_top5,
                    "search_latency_s": search_s,
                    "retrieved_tokens_est": sum(len(c) for c in contents) / 4,
                })

                if producer:
                    record.update({"retrieved_source_ids": retrieved_ids,
                                   "source_identity_rule": "original input content + returned persisted ID; never ID-as-serial",
                                   "retrieved_unmapped_n": len(retrieved) - len(retrieved_serials)})
                if not args.dry_run:
                    if args.arm in ("search", "both"):
                        block = ""
                        if contents:
                            block = MEMORY_HEADER + "\n" + "\n".join(contents)
                        record["search_answer_records"] = []
                        pred = None
                        try:
                            pred = answer(client, question, block, args.max_tokens, record["search_answer_records"])
                        finally:
                            record["search_completion"] = answer_completion(pred, record["search_answer_records"])
                        record["search_prediction"] = pred
                        record["search_correct"] = substring_exact_match(pred, golds)
                    if args.arm in ("control", "both"):
                        record["control_answer_records"] = []
                        pred = None
                        try:
                            pred = answer(client, question, "", args.max_tokens, record["control_answer_records"])
                        finally:
                            record["control_completion"] = answer_completion(pred, record["control_answer_records"])
                        record["control_prediction"] = pred
                        record["control_correct"] = substring_exact_match(pred, golds)
                    if args.arm == "inject":
                        before = len(server.records)
                        try:
                            block, inject_s = server.session(question)
                        finally:
                            record["session_records"] = server.records[before:]
                        record["session_scope_diagnostic"] = {
                            "agent_id_requested": server.session_agent, "project_requested": server.session_project,
                            "endpoint": "/api/session/preview", "requested_mode": "preview",
                            "operational_actor": producer["agent_id"] if producer else None,
                            "seed_namespace": namespace,
                            "scope_note": ("verified producer credentials authorize preview of the separately requested target profile; explicit isolated seed project scope" if producer else
                                           "legacy mab-* namespace is not implicitly the session's global/project scope; inspect full response profile/results/skipped"),
                            "empty_block_cause": None}
                        record["injected"] = block
                        record["injected_tokens_est"] = len(block) / 4
                        record["injection_latency_s"] = inject_s
                        record["inject_answer_records"] = []
                        pred = None
                        try:
                            pred = answer(client, question, block, args.max_tokens, record["inject_answer_records"])
                        finally:
                            record["inject_completion"] = answer_completion(pred, record["inject_answer_records"])
                        record["inject_prediction"] = pred
                        record["inject_correct"] = substring_exact_match(pred, golds)
                record["execution_complete"] = True
                if (position + 1) % 10 == 0:
                    print(f"[{subset}] {position + 1}/{len(questions)}", file=sys.stderr)

            summary = {"subset": subset, "n_facts": len(facts), "seed_seconds": round(seed_s, 2),
                       **summarise_questions(per_q, args.arm)}
            entry.update({"summary": summary, "diagnostics": mab_diagnostics(per_q, args.arm),
                          "n_seed_failed": sum(f["memory_id"] is None for f in fact_records)})
        execution_complete = True
    except (Exception, SystemExit) as exc:
        run_error = sanitize(str(exc))
        startup_evidence = getattr(exc, "startup_evidence", None)
        if results:
            entry["error"] = run_error
            if entry["questions"] and entry["questions"][-1].get("execution_complete") is not True:
                entry["questions"][-1]["error"] = run_error
            entry["summary"] = {"subset": subset, **summarise_questions(entry["questions"], args.arm)}
            entry["diagnostics"] = mab_diagnostics(entry["questions"], args.arm)
            if entry["context_parse_complete"] is not True:
                entry["context_parse_error_input"] = row.get("context") if isinstance(row, dict) else row
            if producer:
                entry["ingest_diagnostics"] = ingest_diagnostics(entry["facts"], entry.get("store_snapshot"), args.ingest_mode)
                entry["ingest_diagnostics"]["n_corpus_facts"] = len(facts) if entry["context_parse_complete"] else None
                entry["ingest_diagnostics"]["n_seed_unattempted"] = len(facts) - len(entry["facts"]) if entry["context_parse_complete"] else None
                entry["ingest_diagnostics"].update({"run_error": run_error, "n_run_errors": 1})
                entry["ingest_diagnostics"]["n_errors"] = max(1, entry["ingest_diagnostics"]["n_errors"])
    finally:
        if server is not None:
            try:
                server.stop()
            except Exception as exc:
                run_error = run_error or sanitize(str(exc))
        evidence["dataset"] = dataset_evidence
        evidence["support_annotations"] = annotation_evidence

    server_records = server.records if server is not None else (startup_evidence or {}).get("records", [])
    status = run_status(results, expected_questions, args.arm, args.dry_run, execution_complete, run_error, server_records)
    out = {
        "generated_at": started.isoformat(),
        "harness_spec": {
            "benchmark": "MemoryAgentBench / Conflict_Resolution",
            "evidence": evidence,
            "raw_mode": f"raw_{args.search_mode}" if producer is None else None,
            "ingest_mode": args.ingest_mode,
            "managed_arms": {"status": "implemented; selected strategy only; no fresh LLM score claimed",
                             "enabled": producer is not None, "selected": args.ingest_mode,
                             "identity_verified": getattr(server, "identity_verified", None),
                             "producer": producer, "isolation": getattr(server, "isolation", (startup_evidence or {}).get("isolation")),
                             "assertion_parser": {"version": ASSERTION_PARSER_VERSION,
                                                  "templates": ASSERTION_TEMPLATES,
                                                  "input": "original atomic numbered fact sentences only; no questions/golds/support annotations",
                                                  "source_scope": "dataset/split/subset/context stable hash",
                                                  "source_order": "original source serial, not date",
                                                  "unsupported": "force_insert=true/preserve without assertion; explicitly counted"},
                             "comparison": "managed_raw/resolved have identical parsed inputs, producer/profile/trust/search/provider/model/prompts; only seed force/write mode differs",
                             "fresh_store_per_run": True},
            "dataset": HF_DATASET,
            "parquet": str(parquet),
            "subsets": wanted,
            "arms": {
                "search": f"MemVault /api/search requested {args.search_mode}, provider {args.embedding_provider}; "
                          "results placed in the Archival Memory block; legacy search_* fields retained",
                "control": "no memory context, same answer instruction",
                "inject": "MemVault /api/session/preview formatted block (read-only target-profile/project/eligibility diagnostic; "
                          "REFERENCE is not categorically excluded)"
            },
            "models": {"answer": MODEL, "base_url": BASE_URL or "default", "temperature": 0.0,
                       "max_tokens": args.max_tokens, "extra_body": EXTRA_BODY},
            "memvault": {
                "binary": str(binary),
                "commit": evidence["checkout"]["commit"],
                "branch": evidence["checkout"]["branch"],
                "child_environment": {k: v for k, v in environment.items()
                                      if (k in EMBEDDING_ENV_KEYS or k in EXPERIMENT_ENV_KEYS)
                                      and not k.endswith("API_KEY")},
                "retrieval_experiment": {
                    "candidate_k": args.candidate_k,
                    "keyword_anchor_k": args.keyword_anchor_k,
                    "two_hop_bridges": args.two_hop_bridges,
                    "two_hop_per_query": args.two_hop_per_query,
                    "first_hop_query_construction": "original question only; golds and support annotations are never query inputs",
                    "second_hop_query_construction": "bridge entity extracted from retrieved facts only; no gold/support input",
                    "final_truncation": "post-rerank top_k for the first hop; bounded union for the optional second hop",
                },
                "embedding_credentials_configured": "MEMVAULT_EMBEDDING_API_KEY" in environment,
                "readiness": getattr(server, "readiness", (startup_evidence or {}).get("readiness")),
                "seed_strategy": ("raw force_insert=true, fact/REFERENCE; no resolve API used" if producer is None else
                                  "verified producer fact/REFERENCE with default AI/unreviewed trust; " +
                                  ("force_insert=true/preserve, actual unmanaged heads" if args.ingest_mode == "managed_raw" else
                                   "force_insert=false/resolve for parsed single slots; unparsed force_insert=true/preserve")),
                "seed_credentials_configured": producer is not None,
                "trust_inputs": "no human_reviewed, ai_generated or management authority fields sent; inspect actual DTO defaults",
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
                "support_annotation_path": args.support_annotations,
                "support_annotation_sha256": annotation_evidence["source_sha256"],
                "support_rule": "any complete annotated support set; null when annotations absent",
                "deprecated_field": "newest_serial_in_context retains whole-corpus maximum meaning; use corpus_max_serial or retrieved_max_serial",
            },
            "options": {
                "questions_per_subset": args.questions,
                "top_k": args.top_k,
                "max_tokens": args.max_tokens,
                "arm": args.arm,
                "ingest_mode": args.ingest_mode,
                "dry_run": args.dry_run,
                "search_mode": args.search_mode,
                "embedding_provider": args.embedding_provider,
                "native_cache_dir": environment.get("MEMVAULT_EMBEDDING_CACHE_DIR"),
                "embedding_ready_timeout": args.embedding_ready_timeout,
                "session_agent": args.session_agent, "session_project": args.session_project,
            },
            "argv": sys.argv,
        },
        "results": results,
        "run_error": run_error,
        "run_status": status,
        "startup_evidence": startup_evidence,
        "server_records": server_records,
    }

    out = sanitize(out)
    if args.out:
        Path(args.out).write_text(json.dumps(out, indent=2, ensure_ascii=False), encoding="utf-8")
        print(f"wrote {args.out}", file=sys.stderr)

    for r in results:
        s = r["summary"]
        print(json.dumps(s, indent=2, ensure_ascii=False))
    print("SCORING COMPLETE" if status["status"] == "scoring_complete" else status["status"].upper(), file=sys.stderr)
    return 0 if status["valid"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
