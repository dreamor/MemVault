#!/usr/bin/env python3
"""
H7 — Procedural memory acceptance experiment
============================================

Phase B acceptance for the removed MEMORY-EVOLUTION-PLAN.md design doc (§5.5; see git history):

  H7a: Skill injection improves first-attempt task success.
       A/B: agent plans a task WITH the matched skill injected (the real
       MemVault session_start output) vs with no injection. Each skill's
       steps carry NON-OBVIOUS, project-specific facts the agent cannot
       guess (same design principle as H5). Primary metric is objective:
       does the plan convey the specific facts (pre-registered keyword
       stems). Secondary metric: quote-grounded LLM judge.

  H7b: Trigger mis-injection rate < 5%.
       Skills live in the global namespace; the session runs in a project
       namespace filled with decoy memories, so the ONLY path by which a
       skill can enter an unrelated session is trigger matching (generic
       search is confined to the project namespace, and the cross-namespace
       fallback cannot fire). Feed many unrelated contexts; count how often
       a skill still gets injected.

The experiment drives a REAL memvault-mcp REST server (spawned as a
subprocess on a throwaway DB) so injection text, trigger matching and
quotas are all the production code paths.

Usage:
  VERIFY_BASE_URL=http://127.0.0.1:8123/v1 VERIFY_MODEL=qwen2.5-1.5b-instruct \
  VERIFY_JUDGE_BASE_URL=http://127.0.0.1:8124/v1 VERIFY_JUDGE_MODEL=qwen2.5-3b-instruct \
  python docs/experiments/verify_h7.py [--rounds 3] [--misfire-samples 40] [--binary path/to/memvault-mcp]
"""

import argparse
import json
import os
import re
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

JUDGE_MODEL = os.getenv("VERIFY_JUDGE_MODEL", MODEL)
JUDGE_BASE_URL = os.getenv("VERIFY_JUDGE_BASE_URL", BASE_URL)
judge_client = (
    OpenAI(base_url=JUDGE_BASE_URL, api_key=os.getenv("OPENAI_API_KEY") or "local")
    if JUDGE_BASE_URL != BASE_URL or JUDGE_MODEL != MODEL
    else client
)

BASE_SYSTEM = (
    "You are a careful engineering assistant. When asked to perform a task, "
    "respond ONLY with a concise numbered step-by-step plan."
)

# ─── Scenarios (pre-registered; steps carry unguessable project facts) ───

SCENARIOS = [
    {
        "task_type": "deploy",
        "task": "Deploy our web dashboard to the production environment and confirm it is live.",
        "skill": {
            "content": "deploy dashboard runbook",
            "trigger": "deploy",
            "steps": [
                "Set DASHBOARD_CDN to cdn-v2.memvault.io before building",
                "Run ./scripts/smoke-pages.sh after the push",
            ],
            "verification": "curl /healthz returns build=v2",
        },
        "keyword_stems": ["dashboard_cdn", "smoke-pages"],
    },
    {
        "task_type": "migrate",
        "task": "Migrate the users table to the new schema with the added profile columns.",
        "skill": {
            "content": "users migration runbook",
            "trigger": "migrate",
            "steps": [
                "Backfill users.profile_json with '{}' before adding the NOT NULL constraint",
                "Verify row counts match before and after the migration",
            ],
            "verification": "row counts identical, no NULL profile_json",
        },
        "keyword_stems": ["profile_json"],
    },
    {
        "task_type": "upgrade",
        "task": "Upgrade the payment SDK from v2 to v3 in our checkout service.",
        "skill": {
            "content": "payment sdk upgrade runbook",
            "trigger": "upgrade",
            "steps": [
                "Add an Idempotency-Key header to every POST call after moving to payment v3",
                "Run the refund flow in the sandbox before production rollout",
            ],
            "verification": "sandbox refund succeeds with idempotent retries",
        },
        "keyword_stems": ["idempotency"],
    },
]

# Unrelated contexts for H7b — none contains any scenario trigger word
# (deploy/migrate/upgrade) or a plausible paraphrase of them.
UNRELATED_CONTEXTS = [
    "write a haiku about the ocean",
    "summarize this meeting transcript for me",
    "plan a team lunch for eight people",
    "explain how coffee is roasted",
    "draft a friendly reminder about the office plants",
    "what books should I read about ancient history",
    "help me name my new cat",
    "write release notes jokes for april fools",
    "compose a lullaby for my nephew",
    "compare trains and planes for short trips",
    "give me a recipe for tomato soup",
    "outline a podcast episode about lighthouses",
    "translate this greeting into french",
    "suggest gifts for a woodworker",
    "write a poem about the first snowfall",
    "brainstorm names for a bakery",
    "describe how sourdough starter works",
    "plan a picnic menu for saturday",
    "recommend hiking trails near a lake",
    "write a thank you note to a mentor",
]


# ─── MemVault server management ───────────────────────────


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
        self.dbdir = tempfile.mkdtemp(prefix="memvault-h7-")
        self.proc = subprocess.Popen(
            [
                str(binary),
                "--db",
                f"{self.dbdir}/h7.db",
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

    def create_skill(self, sc: dict) -> str:
        skill = sc["skill"]
        resp = self._req(
            "POST",
            "/api/memories",
            {
                "content": skill["content"],
                "type": "skill",
                "priority": "REFERENCE",
                "agent_id": "h7",
                "human_reviewed": True,
                "ai_generated": False,
                "skill_trigger": skill["trigger"],
                "skill_steps": skill["steps"],
                "skill_verification": skill["verification"],
            },
        )
        return resp["data"]["id"]

    def create_decoys(self, namespace: str, count: int):
        for i in range(count):
            self._req(
                "POST",
                "/api/memories",
                {
                    "content": f"project note number {i} about internal trivia",
                    "type": "fact",
                    "priority": "REFERENCE",
                    "namespace": namespace,
                    "agent_id": "h7",
                    "human_reviewed": True,
                    "ai_generated": False,
                },
            )

    def session_formatted(self, context: str, project: str | None = None) -> str:
        body = {"agent_id": "claude-desktop", "context_hint": context}
        if project:
            body["project"] = project
        resp = self._req("POST", "/api/session", body)
        return resp["data"].get("formatted", "")


# ─── LLM helpers ──────────────────────────────────────────


def generate_plan(system: str, task: str) -> str:
    resp = client.chat.completions.create(
        model=MODEL,
        messages=[
            {"role": "system", "content": system},
            {"role": "user", "content": f"{task}\nReply with your step-by-step plan only."},
        ],
        max_tokens=500,
        temperature=0.3,
    )
    return resp.choices[0].message.content


def judge_quote(pitfall: str, plan: str) -> dict:
    """Quote-grounded judge (H5-calibrated): neutral framing, must quote."""
    prompt = f"""PRECAUTION: {pitfall}

PLAN:
{plan}

Find the step that performs this precaution and quote its exact words (paraphrases of the precaution count, but the quote must be real text from the plan). If no step performs it, return null.
Answer ONLY JSON: {{"step": <number>, "quote": "<exact words from that step>"}} or {{"step": null, "quote": null}}"""
    resp = judge_client.chat.completions.create(
        model=JUDGE_MODEL,
        messages=[{"role": "user", "content": prompt}],
        max_tokens=100,
        temperature=0.0,
    )
    text = resp.choices[0].message.content.strip()
    if text.startswith("```"):
        text = text.split("\n", 1)[1].rsplit("```", 1)[0]
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        m = re.search(r'"step"\s*:\s*(\d+)', text)
        return {"step": int(m.group(1)) if m else None, "quote": None}


def stem_hit(sc: dict, plan: str) -> bool:
    low = plan.lower()
    return any(stem in low for stem in sc["keyword_stems"])


# ─── Experiments ──────────────────────────────────────────


def run_h7a(server: MemVaultServer, rounds: int) -> dict:
    print("\n═══ H7a: Skill Injection vs No Injection (first-attempt success) ═══")
    print("    primary = specific facts conveyed (objective); secondary = LLM judge")

    skill_ids = {sc["task_type"]: server.create_skill(sc) for sc in SCENARIOS}

    control_hits = control_total = 0
    experiment_hits = experiment_total = 0
    judge_ctrl = judge_exp = judge_n = 0
    details = []

    for rnd in range(rounds):
        for sc in SCENARIOS:
            print(f"  round {rnd+1}/{rounds} [{sc['task_type']}] ...", end=" ")

            plan_ctrl = generate_plan(BASE_SYSTEM, sc["task"])
            hit_ctrl = stem_hit(sc, plan_ctrl)
            control_hits += hit_ctrl
            control_total += 1

            injection = server.session_formatted(sc["task"])
            assert "[SKILL:" in injection, (
                f"expected the matched skill to be injected for '{sc['task_type']}'"
            )
            plan_exp = generate_plan(BASE_SYSTEM + "\n\n" + injection, sc["task"])
            hit_exp = stem_hit(sc, plan_exp)
            experiment_hits += hit_exp
            experiment_total += 1

            # Secondary judge on the experiment plan (one per scenario-round).
            jv = judge_quote(
                "; ".join(sc["skill"]["steps"]), plan_exp
            )
            judge_exp += 1 if (jv.get("step") or 0) >= 1 else 0
            jvc = judge_quote("; ".join(sc["skill"]["steps"]), plan_ctrl)
            judge_ctrl += 1 if (jvc.get("step") or 0) >= 1 else 0
            judge_n += 1

            details.append(
                {
                    "round": rnd + 1,
                    "task_type": sc["task_type"],
                    "control_kw": hit_ctrl,
                    "experiment_kw": hit_exp,
                    "experiment_plan": plan_exp,
                }
            )
            print(f"ctrl={'✓' if hit_ctrl else '✗'} exp={'✓' if hit_exp else '✗'}")

    ctrl_rate = control_hits / control_total
    exp_rate = experiment_hits / experiment_total
    return {
        "control": ctrl_rate,
        "experiment": exp_rate,
        "control_hits": control_hits,
        "experiment_hits": experiment_hits,
        "total": control_total,
        "judge_control": judge_ctrl / judge_n if judge_n else None,
        "judge_experiment": judge_exp / judge_n if judge_n else None,
        "details": details,
        "skill_ids": skill_ids,
    }


def run_h7b(server: MemVaultServer, samples: int) -> dict:
    print("\n═══ H7b: Trigger Mis-Injection Rate (target < 5%) ═══")

    # Decoys fill the project namespace so generic search never reaches the
    # global skills; skills can only enter via trigger matching.
    server.create_decoys("project:h7lab", 10)

    contexts = (UNRELATED_CONTEXTS * ((samples // len(UNRELATED_CONTEXTS)) + 1))[:samples]
    misfires = 0
    examples = []
    for i, ctx in enumerate(contexts):
        formatted = server.session_formatted(ctx, project="h7lab")
        injected_skill = "[SKILL:" in formatted
        if injected_skill:
            misfires += 1
            examples.append(ctx)
        print(
            f"  {i+1}/{samples}: '{ctx[:38]}' -> {'SKILL INJECTED ✗' if injected_skill else 'clean ✓'}"
        )

    rate = misfires / samples
    return {"rate": rate, "misfires": misfires, "samples": samples, "examples": examples}


def main():
    parser = argparse.ArgumentParser(description="H7 procedural-memory acceptance")
    parser.add_argument("--rounds", type=int, default=3, help="H7a rounds over scenarios")
    parser.add_argument("--misfire-samples", type=int, default=40, help="H7b unrelated contexts")
    parser.add_argument("--binary", help="path to memvault-mcp binary")
    args = parser.parse_args()

    if not os.getenv("OPENAI_API_KEY") and BASE_URL is None:
        print("ERROR: set OPENAI_API_KEY or VERIFY_BASE_URL (local endpoint)")
        sys.exit(1)

    print("MemVault H7 — procedural memory acceptance")
    print(f"Agent model: {MODEL} @ {BASE_URL or 'OpenAI'}")
    print(f"Judge model: {JUDGE_MODEL} @ {JUDGE_BASE_URL or 'OpenAI'}")

    binary = find_binary(args.binary)
    print(f"MemVault binary: {binary}")
    server = MemVaultServer(binary)
    print(f"MemVault server: {server.base} (temp db)")

    try:
        h7a = run_h7a(server, args.rounds)
        h7b = run_h7b(server, args.misfire_samples)
    finally:
        server.stop()

    print("\n" + "=" * 60)
    print("RESULTS")
    print("=" * 60)

    diff = h7a["experiment"] - h7a["control"]
    print(
        f"H7a success conveyance: control {h7a['control']:.0%} "
        f"({h7a['control_hits']}/{h7a['total']}) -> "
        f"experiment {h7a['experiment']:.0%} "
        f"({h7a['experiment_hits']}/{h7a['total']})  Δ {diff:+.0%}"
    )
    if h7a["judge_control"] is not None:
        print(
            f"    [secondary] LLM judge: control {h7a['judge_control']:.0%}, "
            f"experiment {h7a['judge_experiment']:.0%}"
        )
    verdict_a = "CONFIRMED ✓" if diff > 0.1 else "INCONCLUSIVE" if diff > 0 else "REJECTED ✗"
    print(f"    VERDICT: {verdict_a}")

    print(
        f"\nH7b mis-injection: {h7b['misfires']}/{h7b['samples']} = {h7b['rate']:.1%}"
    )
    verdict_b = "CONFIRMED ✓ (<5%)" if h7b["rate"] < 0.05 else "FAILED ✗ (>=5%)"
    print(f"    VERDICT: {verdict_b}")

    out = Path(__file__).parent / "h7_results.json"
    out.write_text(json.dumps({"h7a": h7a, "h7b": h7b}, ensure_ascii=False, indent=1))
    print(f"\nDetails written to {out}")


if __name__ == "__main__":
    main()
