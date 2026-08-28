#!/usr/bin/env python3
"""
Ollama 本地运行时实测 — 真实 memvault-mcp REST server 端到端闭环 (2026-08-28)
================================================================================
不依赖模型输出质量（plumbing 回归）：驱动真实 memvault-mcp 子进程 + 临时库。

  A) 技能触发注入   — 保存 Skill(type=skill, human_reviewed, trigger/steps)，
                      context_hint 含 trigger → 注入 [SKILL:] 块、count>=1
  B) 误注入率       — 项目命名空间铺满 decoy + 无关上下文 → 注入 0
  C) outcome 闭环   — POST /api/outcome(failure, skill_id, task_type)
                      → GET /api/episodes → lesson_memory_id 生成
                      （reflect_and_store 走 LLM 提取 = 验证本地 Ollama 提取通路）
  D) skipped 留痕   — 候选数 > max_memories(8) → 多出的被丢并 report reason
  E) 模型自动探测   — 不设 MEMVAULT_LLM_EXTRACTION_MODEL 时自动选用已安装
                      qwen2.5 模型（修复 404 问题）；日志含 auto-detected

运行：
  python docs/experiments/verify_ollama_runtime.py [--binary path/to/memvault-mcp]
退出码: 全部通过=0, 任一失败=1
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

    # ── A + E: skill 保存 & 触发注入（server 不开 LLM 屏蔽，退 rule-based，纯靠触发器） ──
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
        ok_type = check("A0 技能保存为 Skill/L2/human_reviewed",
            bool(rec and rec["type"] == "Skill" and rec["layer"] == "L2"
                 and rec["human_reviewed"] is True and rec["skill_meta"]))
        if not ok_type: sys.exit(1)

        hit = s.session("we are about to deploy the dashboard to production now")
        ok_a = check("A1 触发上下文注入技能",
            hit["count"] >= 1 and "[SKILL:" in hit["formatted"],
            f"count={hit['count']} skipped={len(hit['skipped'])}")
        if not ok_a:
            print("   formatted:", hit["formatted"][:300].replace("\n", " "))
        results["a_trigger"] = {"count": hit["count"], "skipped": hit["skipped"],
                                "has_skill_block": "[SKILL:" in hit["formatted"]}

        # misfire: project 命名空间 + decoys → 技能只能靠 trigger 进来
        for i in range(10):
            s.save(content=f"project note number {i} about internal trivia",
                   type="fact", namespace="project:rtlab", agent_id="rt")
        misfires = 0
        for i, ctx in enumerate(UNRELATED_CONTEXTS[: args.misfire_samples]):
            d = s.session(ctx, project="rtlab")
            if "[SKILL:" in d["formatted"]:
                misfires += 1
        ok_b = check("B 误注入率=0",
            misfires == 0, f"{misfires}/{args.misfire_samples}")
        results["b_misfire"] = {"rate": misfires / args.misfire_samples,
                                "misfires": misfires, "samples": args.misfire_samples}
    finally:
        s.stop()

    # ── C: outcome 闭环（reflect_and_store → LLM 提取 → episode/lesson） ──
    # 不设 LLM provider/model → 走 auto 路径；本机 Ollama 跑着 qwen2.5 时自动选中，
    # 顺带验证 E（不再 404）。若本机无 Ollama，C 的 LLM 步骤退 rule-based，仍应闭环。
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
        ok_c = check("C outcome → episode 闭环（lesson_memory_id）",
            first.get("id") and ep.get("count", 0) >= 1
            and (ep.get("episodes") or [{}])[0].get("lesson_memory_id"),
            f"episodes={ep.get('count')} lesson_source="
            f"{(first.get('lesson') or {}).get('source')}")
    finally:
        s.stop()
    lesson_src = (results.get("c_outcome", {}).get("lesson") or {}).get("source")
    results["e_auto_detect"] = {"lesson_source": lesson_src}
    ok_e = check("E 本地 Ollama 自动探测/LLM 提取", lesson_src == "llm",
                 f"lesson.source={lesson_src} (llm=经本地 Ollama 提取)")

    # ── D: skipped 留痕 ──
    s = Server(binary)
    try:
        s.save_skill()
        for i in range(12):  # 12 个 zebra 候选 > max_memories=8
            s.save(content=f"zebra workflow note number {i} about internal deployment",
                   type="fact", namespace="project:skippedlab", agent_id="rt")
        d = s.session("zebra deploy", project="skippedlab")
        reasons = [x["reason"] for x in d["skipped"]]
        results["d_skipped"] = {"count": d["count"], "skipped": reasons}
        ok_d = check("D 超额候选被丢并留痕 skipped",
            len(d["skipped"]) >= 1,
            f"injected={d['count']} skipped={reasons}")
        if not ok_d and d["count"] == 0:
            print("   (候选未命中 — FTS 未匹配 zebra,见下)")
    finally:
        s.stop()

    print("\n" + "=" * 60 + "\nRUN-TIME E2E (Ollama 本地)\n" + "=" * 60)
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
