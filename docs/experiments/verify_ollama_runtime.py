#!/usr/bin/env python3
"""
Ollama local runtime verification — end-to-end loop against a real
memvault-mcp REST server (2026-08-28)
================================================================================
Independent of model output quality (a plumbing regression): drives a real
memvault-mcp subprocess against a throwaway store.

  A) Skill injection  — save a Skill (type=skill, human_reviewed, trigger/steps);
                      a context_hint containing the trigger yields a
                      [SKILL:] block and count>=1
  B) Mis-injection rate — a project namespace filled with decoys + unrelated
                      contexts → 0 injections
  C) Outcome loop   — POST /api/outcome(failure, skill_id, task_type)
                      → GET /api/episodes → lesson_memory_id generated
                      (reflect_and_store goes through LLM extraction =
                      verifies the local Ollama extraction path)
  D) Skipped audit  — more candidates than max_memories(8) → the extras are
                      dropped and reported with a reason
  E) Model auto-detect — with MEMVAULT_LLM_EXTRACTION_MODEL unset, the
                      installed qwen2.5 model is picked automatically (fixes
                      the 404 problem); logs contain auto-detected

Run:
  python docs/experiments/verify_ollama_runtime.py [--binary path/to/memvault-mcp]
Exit codes: all pass=0, any failure=1
"""
import argparse, json, os, socket, subprocess, sys, tempfile, time, urllib.request, urllib.error
from pathlib import Path

UNRELATED_CONTEXTS = [
    "write a haiku about the ocean", "summarize this meeting transcript for me",
    "plan a team lunch for eight people", "explain how coffee is roasted",
    "draft a friendly reminder about the office plants", "what books should I read about ancient history",
    "help me name my new cat", "write release notes jokes for april fools",
    "compose a lullaby for my nephew", "compare trains and planes for short trips",
    "give me a recipe for tomato soup", "outline a podcast episode about lighthouses",
    "translate this greeting into french", "suggest gifts for a woodworker",
    "write a poem about the first snowfall", "brainstorm names for a bakery",
    "describe how sourdough starter works", "plan a picnic menu for saturday",
    "recommend hiking trails near a lake", "write a thank you note to a mentor",
]

SKILL = {
    "content": "deploy dashboard runbook",
    "trigger": "deploy dashboard",
    "steps": [
        "Set DASHBOARD_CDN to cdn-v2.memvault.io before building",
        "Run ./scripts/smoke-pages.sh after the push",
    ],
    "verification": "curl /healthz returns build=v2",
}

def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0)); return s.getsockname()[1]

def find_binary(explicit):
    if explicit: return Path(explicit)
    root = Path(__file__).resolve().parents[2]
    for rel in ["target/debug/memvault-mcp", "target/release/memvault-mcp"]:
        p = root / rel
        if p.exists(): return p
    sys.exit("memvault-mcp binary not found — build it first")

class Server:
    def __init__(self, binary, extra_env=None, capture_log=False):
        self.port = free_port()
        self.base = f"http://127.0.0.1:{self.port}"
        self.dbdir = tempfile.mkdtemp(prefix="memvault-rt-")
        env = {**os.environ, "MEMVAULT_EMBEDDING_PROVIDER": "off"}
        if extra_env: env.update(extra_env)
        stderr = subprocess.PIPE if capture_log else subprocess.DEVNULL
        self.log = b""
        self.proc = subprocess.Popen(
            [str(binary), "--db", f"{self.dbdir}/rt.db", "--transport", "http",
             "--port", str(self.port)],
            env=env, stdout=subprocess.PIPE if capture_log else subprocess.DEVNULL,
            stderr=stderr,
        )
        for _ in range(60):
            time.sleep(0.5)
            try:
                with urllib.request.urlopen(f"{self.base}/health", timeout=2) as r:
                    if r.read().strip() == b"ok": return
            except Exception:
                if self.proc.poll() is not None:
                    sys.exit(f"memvault-mcp exited during startup: rc={self.proc.returncode}")
        sys.exit("memvault-mcp did not become healthy")
    def stop(self):
        # Terminate FIRST — reading a live pipe that never EOFs would block.
        self.proc.terminate()
        try: self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill(); self.proc.wait(timeout=5)
        try:
            if self.proc.stdout is not None:
                self.log += self.proc.stdout.read()
        except Exception: pass
        try:
            if self.proc.stderr is not None:
                self.log += self.proc.stderr.read()
        except Exception: pass
    def req(self, method, path, body=None, timeout=120):
        req = urllib.request.Request(f"{self.base}{path}",
            data=json.dumps(body).encode() if body is not None else None,
            headers={"Content-Type": "application/json"}, method=method)
        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                return json.loads(r.read())
        except urllib.error.HTTPError as e:
            body = e.read().decode(errors="replace")
            sys.exit(f"HTTP {e.code} {method} {path}: {body}")
    # ---- domain helpers ----
    def save(self, **kw):
        defaults = dict(content="note", type="fact", priority="REFERENCE",
                        namespace="global", agent_id="rt", agent_type="cli",
                        human_reviewed=True, ai_generated=False)
        defaults.update(kw)
        return self.req("POST", "/api/memories", defaults)["data"]["id"]
    def save_skill(self):
        return self.save(content=SKILL["content"], type="skill", priority="REFERENCE",
                         namespace="global", agent_id="rt",
                         human_reviewed=True, ai_generated=False,
                         skill_trigger=SKILL["trigger"], skill_steps=SKILL["steps"],
                         skill_verification=SKILL["verification"])
    def list_memories(self, namespace=None):
        q = f"?namespace={namespace}" if namespace else ""
        return self.req("GET", f"/api/memories{q}").get("data", [])
    def session(self, context, project=None):
        body = {"agent_id": "claude-desktop", "context_hint": context}
        if project: body["project"] = project
        return self.req("POST", "/api/session", body)["data"]
    def outcome(self, task, status, task_type=None, skill_id=None, namespace="global"):
        body = dict(task=task, status=status, task_type=task_type, skill_id=skill_id,
                    namespace=namespace, agent_id="rt")
        return self.req("POST", "/api/outcome", body)
    def episodes(self):
        return self.req("GET", "/api/episodes?limit=50").get("data", {})

def check(name, ok, detail=""):
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
    return ok

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--binary")
    ap.add_argument("--misfire-samples", type=int, default=20)
    args = ap.parse_args()
    binary = find_binary(args.binary)
    results = {}
    print(f"binary: {binary}\n")

    # ── A + E: skill storage & trigger injection (LLM gating off on the server;
    #    injection falls back to rule-based, trigger-only) ──
    s = Server(binary)
    try:
        sid = s.save_skill()
        mems = s.list_memories("global")
        rec = next((m for m in mems if m["id"] == sid), None)
        results["skill_record"] = {
            "type": rec["type"] if rec else None, "layer": rec["layer"] if rec else None,
            "human_reviewed": rec["human_reviewed"] if rec else None,
            "skill_meta": rec["skill_meta"] if rec else None,
        }
        ok_type = check("A0 skill stored as Skill/L2/human_reviewed",
            bool(rec and rec["type"] == "Skill" and rec["layer"] == "L2"
                 and rec["human_reviewed"] is True and rec["skill_meta"]))
        if not ok_type: sys.exit(1)

        hit = s.session("we are about to deploy the dashboard to production now")
        ok_a = check("A1 trigger context injects the skill",
            hit["count"] >= 1 and "[SKILL:" in hit["formatted"],
            f"count={hit['count']} skipped={len(hit['skipped'])}")
        if not ok_a:
            print("   formatted:", hit["formatted"][:300].replace("\n", " "))
        results["a_trigger"] = {"count": hit["count"], "skipped": hit["skipped"],
                                "has_skill_block": "[SKILL:" in hit["formatted"]}

        # misfire: project namespace + decoys → the skill can only enter via trigger
        for i in range(10):
            s.save(content=f"project note number {i} about internal trivia",
                   type="fact", namespace="project:rtlab", agent_id="rt")
        misfires = 0
        for i, ctx in enumerate(UNRELATED_CONTEXTS[: args.misfire_samples]):
            d = s.session(ctx, project="rtlab")
            if "[SKILL:" in d["formatted"]:
                misfires += 1
        ok_b = check("B mis-injection rate = 0",
            misfires == 0, f"{misfires}/{args.misfire_samples}")
        results["b_misfire"] = {"rate": misfires / args.misfire_samples,
                                "misfires": misfires, "samples": args.misfire_samples}
    finally:
        s.stop()

    # ── C: outcome loop (reflect_and_store → LLM extraction → episode/lesson) ──
    # No LLM provider/model is set → the auto path is used; with a local Ollama
    # running qwen2.5 it is selected automatically, which also verifies E
    # (no more 404). Without local Ollama, C's LLM step degrades to rule-based
    # and should still close the loop.
    s = Server(binary, capture_log=True, extra_env={"RUST_LOG": "memvault_core=info,memvault_mcp=info"})
    try:
        sid = s.save_skill()
        first = s.outcome(
            task="Deploy the dashboard but the smoke test failed",
            status="failure", task_type="deploy", skill_id=sid,
        )["data"]
        ep = s.episodes()
        results["c_outcome"] = {
            "outcome_id": first.get("id"), "lesson": first.get("lesson"),
            "episode_count": ep.get("count", 0),
            "episode_lesson_memory_id": (ep.get("episodes") or [{}])[0].get("lesson_memory_id")
                                        if ep.get("count") else None,
        }
        ok_c = check("C outcome → episode loop (lesson_memory_id)",
            first.get("id") and ep.get("count", 0) >= 1
            and (ep.get("episodes") or [{}])[0].get("lesson_memory_id"),
            f"episodes={ep.get('count')} lesson_source="
            f"{(first.get('lesson') or {}).get('source')}")
    finally:
        s.stop()
    lesson_src = (results.get("c_outcome", {}).get("lesson") or {}).get("source")
    results["e_auto_detect"] = {"lesson_source": lesson_src}
    ok_e = check("E local Ollama auto-detected / LLM extraction", lesson_src == "llm",
                 f"lesson.source={lesson_src} (llm=extracted via local Ollama)")

    # ── D: skipped audit-trail ──
    s = Server(binary)
    try:
        s.save_skill()
        for i in range(12):  # 12 zebra candidates > max_memories=8
            s.save(content=f"zebra workflow note number {i} about internal deployment",
                   type="fact", namespace="project:skippedlab", agent_id="rt")
        d = s.session("zebra deploy", project="skippedlab")
        reasons = [x["reason"] for x in d["skipped"]]
        results["d_skipped"] = {"count": d["count"], "skipped": reasons}
        ok_d = check("D over-quota candidates dropped and reported as skipped",
            len(d["skipped"]) >= 1,
            f"injected={d['count']} skipped={reasons}")
        if not ok_d and d["count"] == 0:
            print("   (candidates missed — FTS did not match zebra, see below)")
    finally:
        s.stop()

    print("\n" + "=" * 60 + "\nRUN-TIME E2E (local Ollama)\n" + "=" * 60)
    ok = all([ok_a, ok_b, ok_c, ok_d, ok_e])
    for k, v in results.items():
        print(f"  {k}: {json.dumps(v, ensure_ascii=False)}")
    out = Path(__file__).parent / "ollama_runtime_results.json"
    out.write_text(json.dumps(results, ensure_ascii=False, indent=1))
    print(f"\nJSON -> {out}")
    print("VERDICT:", "ALL PASS ✓" if ok else "FAILED ✗")
    sys.exit(0 if ok else 1)

if __name__ == "__main__":
    main()
