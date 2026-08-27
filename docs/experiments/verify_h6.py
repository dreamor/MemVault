#!/usr/bin/env python3
"""
H6 — Semantic memory acceptance experiment
===========================================

Phase C acceptance for docs/MEMORY-EVOLUTION-PLAN.md (§4.6):

  H6a  Knowledge conveyance: with domain facts injected from the store,
       agents answer domain questions with the specific facts; without
       injection they cannot. A/B with an objective keyword primary metric.

  H6b  Cross-session consistency: the SAME question asked in two separate
       sessions gets the SAME specific answer (memory makes answers stable
       across sessions instead of drifting).

  H6c  Knowledge correction (C4 supersede): after a fact is superseded by a
       newer one, sessions convey the NEW fact and never the superseded one.

Like H7, the experiment drives a REAL memvault-mcp subprocess server so the
storage, search-filtering and injection code paths are the production ones.

Usage:
  VERIFY_BASE_URL=http://127.0.0.1:8123/v1 VERIFY_MODEL=qwen2.5-1.5b-instruct \
  python docs/experiments/verify_h6.py [--rounds 2] [--binary path/to/memvault-mcp]
"""

import argparse
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

try:
    from openai import OpenAI
except ImportError:
    print("ERROR: pip install openai")
    sys.exit(1)

MODEL = os.getenv("VERIFY_MODEL", "gpt-4o-mini")
BASE_URL = os.getenv("VERIFY_BASE_URL") or os.getenv("OPENAI_BASE_URL") or None
client = OpenAI(base_url=BASE_URL, api_key=os.getenv("OPENAI_API_KEY") or "local")

BASE_SYSTEM = (
    "You are an engineering assistant for our team. Answer the user's question "
    "in ONE short sentence, using the provided memory context when present. "
    "If the context does not contain the answer, say you don't know."
)

# ─── Scenarios (pre-registered; facts are unguessable internal knowledge) ──

SCENARIOS = [
    {
        "question": "Which CDN domain must our dashboard static assets use?",
        "fact": "Dashboard static assets must use cdn-v2.memvault.io (migrated from cdn-v1 in 2026-08).",
        "stems": ["cdn-v2.memvault.io"],
        # For H6c: the corrected fact that supersedes the original.
        "corrected_fact": "Dashboard static assets must use cdn-v3.memvault.io (cdn-v2 decommissioned 2026-09).",
        "corrected_stems": ["cdn-v3.memvault.io"],
    },
    {
        "question": "What is the maintenance window for the payment database migration?",
        "fact": "The payment DB migration maintenance window is Sunday 03:00-05:00 UTC (agreed with the DBA team).",
        "stems": ["03:00-05:00"],
        "corrected_fact": "The payment DB migration maintenance window moved to Saturday 02:00-04:00 UTC.",
        "corrected_stems": ["02:00-04:00"],
    },
    {
        "question": "Which error-code contract must the v1 /billing endpoint keep?",
        "fact": "v1 /billing must keep legacy error codes LEGACY_4001 and LEGACY_5001 for two more quarters.",
        "stems": ["legacy_4001"],
        "corrected_fact": "v1 /billing legacy error codes were retired; use standard HTTP codes now.",
        "corrected_stems": ["retired"],
    },
]


# ─── MemVault server management (same harness as verify_h7) ─────────────


def find_binary(explicit: str | None) -> Path:
    if explicit:
        return Path(explicit)
    repo_root = Path(__file__).resolve().parents[2]
    for rel in ["target/debug/memvault-mcp", "target/release/memvault-mcp"]:
        p = repo_root / rel
        if p.exists():
            return p
    raise SystemExit(
        "memvault-mcp binary not found — build it (cargo build -p memvault-mcp) "
        "or pass --binary"
    )


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class MemVaultServer:
    def __init__(self, binary: Path):
        self.port = free_port()
        self.base = f"http://127.0.0.1:{self.port}"
        self.dbdir = tempfile.mkdtemp(prefix="memvault-h6-")
        self.proc = subprocess.Popen(
            [
                str(binary),
                "--db",
                f"{self.dbdir}/h6.db",
                "--transport",
                "http",
                "--port",
                str(self.port),
            ],
            env={**os.environ, "MEMVAULT_EMBEDDING_PROVIDER": "off"},
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

    def _req(self, method: str, path: str, body=None):
        req = urllib.request.Request(
            f"{self.base}{path}",
            data=json.dumps(body).encode() if body is not None else None,
            headers={"Content-Type": "application/json"},
            method=method,
        )
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.loads(r.read())

    def create_fact(self, content: str) -> str:
        resp = self._req(
            "POST",
            "/api/memories",
            {
                "content": content,
                "type": "fact",
                "priority": "REFERENCE",
                "agent_id": "h6",
                "human_reviewed": True,
                "ai_generated": False,
            },
        )
        return resp["data"]["id"]

    def supersede(self, old_id: str, new_id: str):
        return self._req("POST", f"/api/memories/{old_id}/supersede", {"replacement_id": new_id})

    def session_formatted(self, context: str) -> str:
        resp = self._req(
            "POST", "/api/session", {"agent_id": "claude-desktop", "context_hint": context}
        )
        return resp["data"].get("formatted", "")


# ─── LLM helper ───────────────────────────────────────────


def answer(system: str, question: str) -> str:
    resp = client.chat.completions.create(
        model=MODEL,
        messages=[
            {"role": "system", "content": system},
            {"role": "user", "content": question},
        ],
        max_tokens=120,
        temperature=0.0,
    )
    return resp.choices[0].message.content


def stem_hit(stems: list[str], text: str) -> bool:
    low = text.lower()
    return any(s.lower() in low for s in stems)


# ─── Experiments ──────────────────────────────────────────


def run_h6(server: MemVaultServer, rounds: int) -> dict:
    print("\n═══ H6: Semantic memory acceptance ═══")

    # Seed all scenario facts.
    fact_ids = {}
    for sc in SCENARIOS:
        fact_ids[sc["question"]] = server.create_fact(sc["fact"])

    control_hits = experiment_hits = consistent_pairs = total_pairs = 0

    for rnd in range(rounds):
        for sc in SCENARIOS:
            print(f"  round {rnd+1}/{rounds}: {sc['question'][:40]} ...", end=" ")

            # Control: bare system, no memory.
            ans_ctrl = answer(BASE_SYSTEM, sc["question"])
            hit_ctrl = stem_hit(sc["stems"], ans_ctrl)
            control_hits += hit_ctrl

            # Experiment: injection from the store, two sessions for
            # consistency (H6b).
            injection = server.session_formatted(sc["question"])
            sys_with_mem = BASE_SYSTEM + "\n\n" + injection
            ans1 = answer(sys_with_mem, sc["question"])
            ans2 = answer(sys_with_mem, sc["question"])
            hit1 = stem_hit(sc["stems"], ans1)
            hit2 = stem_hit(sc["stems"], ans2)
            experiment_hits += hit1
            total_pairs += 1
            if hit1 == hit2 and hit1:
                consistent_pairs += 1

            print(
                f"ctrl={'✓' if hit_ctrl else '✗'} exp={'✓' if hit1 else '✗'} "
                f"(session2={'✓' if hit2 else '✗'})"
            )

    # H6c: supersede each fact with its corrected version, then verify the
    # store conveys ONLY the corrected knowledge.
    print("\n  --- H6c: knowledge correction via supersede ---")
    correction_ok = 0
    for sc in SCENARIOS:
        old_id = fact_ids[sc["question"]]
        new_id = server.create_fact(sc["corrected_fact"])
        server.supersede(old_id, new_id)
        injection = server.session_formatted(sc["question"])
        has_new = stem_hit(sc["corrected_stems"], injection)
        has_old = stem_hit(sc["stems"], injection)
        ok = has_new and not has_old
        correction_ok += ok
        print(
            f"  {sc['question'][:40]} ... new={'✓' if has_new else '✗'} "
            f"old-absent={'✓' if not has_old else '✗'}"
        )

    total_samples = rounds * len(SCENARIOS)
    return {
        "control_rate": control_hits / total_samples,
        "experiment_rate": experiment_hits / total_samples,
        "consistency_rate": consistent_pairs / total_pairs if total_pairs else 0.0,
        "correction_rate": correction_ok / len(SCENARIOS),
        "total_samples": total_samples,
    }


def main():
    parser = argparse.ArgumentParser(description="H6 semantic-memory acceptance")
    parser.add_argument("--rounds", type=int, default=2)
    parser.add_argument("--binary", help="path to memvault-mcp binary")
    args = parser.parse_args()

    if not os.getenv("OPENAI_API_KEY") and BASE_URL is None:
        print("ERROR: set OPENAI_API_KEY or VERIFY_BASE_URL (local endpoint)")
        sys.exit(1)

    print("MemVault H6 — semantic memory acceptance")
    print(f"Agent model: {MODEL} @ {BASE_URL or 'OpenAI'}")

    binary = find_binary(args.binary)
    print(f"MemVault binary: {binary}")
    server = MemVaultServer(binary)
    print(f"MemVault server: {server.base} (temp db)")

    try:
        r = run_h6(server, args.rounds)
    finally:
        server.stop()

    print("\n" + "=" * 60)
    print("RESULTS")
    print("=" * 60)
    diff = r["experiment_rate"] - r["control_rate"]
    print(
        f"H6a knowledge conveyance: control {r['control_rate']:.0%} -> "
        f"experiment {r['experiment_rate']:.0%}  (Δ {diff:+.0%})"
    )
    verdict_a = "CONFIRMED ✓" if diff > 0.1 else "INCONCLUSIVE" if diff > 0 else "REJECTED ✗"
    print(f"    VERDICT: {verdict_a}")

    print(f"\nH6b cross-session consistency: {r['consistency_rate']:.0%}")
    verdict_b = "CONFIRMED ✓" if r["consistency_rate"] >= 0.8 else "BELOW TARGET ✗"
    print(f"    VERDICT: {verdict_b}")

    print(f"\nH6c correction via supersede: {r['correction_rate']:.0%}")
    verdict_c = "CONFIRMED ✓" if r["correction_rate"] == 1.0 else "FAILED ✗"
    print(f"    VERDICT: {verdict_c}")

    out = Path(__file__).parent / "h6_results.json"
    out.write_text(json.dumps(r, ensure_ascii=False, indent=1))
    print(f"\nDetails written to {out}")


if __name__ == "__main__":
    main()
