# MemVault Hypothesis Verification

> ⚠️ **Historical verification (2026-08-11)**: This directory holds MemVault's early verification experiments for the four core design hypotheses (H1–H4) in `../DESIGN.md` Appendix C; all conclusions were CONFIRMED and archived. Kept as historical evidence for the design decisions — scripts are retained for reproducibility; re-run them as described below to re-verify. Full results in `REPORT.md`.
>
> **H5 (2026-08-26, CONFIRMED ✓)**: Phase A acceptance hypothesis of the three-memory-type evolution (see `../DESIGN.md` §15) — "lesson injection reduces the repeat-failure rate on similar tasks". Measured with local open-source models: knowledge conveyance 0% → 90% (+90%). Design and results in the H5 section of `REPORT.md`.
>
> **H7 (2026-08-27, CONFIRMED ✓)**: Phase B procedural-memory acceptance — "skill injection improves first-attempt success" + "trigger mis-fire rate < 5%". Measured against a real `memvault-mcp` server: specific-step conveyance 0% → 78% (+78%), zero mis-injections across 40 unrelated contexts. See the H7 section of `REPORT.md` (including a quota defect exposed and fixed by the experiment).
>
> **H6 (2026-08-27, CONFIRMED ✓ ×3)**: Phase C semantic-memory acceptance — knowledge conveyance 0%→100%, cross-session consistency 100%, supersede correction propagation 100%. See the H6 section of `REPORT.md`.
>
> **Runtime regression (2026-08-28)**: Injection / outcome-loop / audit-trail plumbing regression — drives a real `memvault-mcp --transport http` subprocess + throwaway store (skill injection / mis-injection 0/20 / outcome loop / skipped audit trail / model auto-detection) 6/6 PASS; also includes the 2026-08-28 deepseek-v4-flash cross-validation. Script `verify_ollama_runtime.py`, results in the "Runtime regression" section of `REPORT.md`.


Automated experiments to validate the core design assumptions in `../DESIGN.md` Appendix C (H1–H4) and the episodic/procedural/semantic-memory acceptance hypotheses from `../DESIGN.md` §15 (H5, H7, H6).

## Hypotheses

| ID | Hypothesis | Method |
|----|-----------|--------|
| H1 | Pre-Prompt Injection improves compliance rate | A/B: no injection vs injected, LLM-as-judge |
| H2 | Instructive `[MUST]` format > descriptive format | A/B: two format styles, same memories |
| H3 | Optimal Token Budget ≈ 1500 tokens | Vary budget (500/1000/1500/2000), measure compliance |
| H4 | Router mis-injection rate < 5% | Feed unrelated memories + coding context, judge relevance |
| H5 | Lesson injection reduces repeat-failure rate | A/B: no lesson vs injected lesson; primary = objective knowledge-conveyance check |
| H7 | Skill injection improves first-attempt success; trigger mis-fire < 5% | Real `memvault-mcp` subprocess: A/B with real injection blocks (objective stems) + 40 unrelated contexts against trigger-matched skills (`verify_h7.py`) |
| H6 | Semantic knowledge conveyed, consistent across sessions, corrections propagate | Real `memvault-mcp` subprocess: fact injection A/B + two-session consistency + supersede correction propagation (`verify_h6.py`) |

### H5 notes (episodic-memory acceptance, 2026-08-26 CONFIRMED ✓)

H5 simulates the real episodic-memory loop: each task type (deploy / migrate / upgrade / refactor / release) carries a **non-obvious, project-specific fact** as its pitfall (e.g. `DASHBOARD_CDN` must point to the new CDN domain — a model cannot guess this from common sense). Lesson text uses the same format as production:
- Reflection output (`reflection.rs` rule path): `Before '<task_type>' tasks, verify: <cause>`
- Injection format (`session_start`): `[REF] When working on '<task_type>' tasks: <lesson>`

The control group runs without lessons and the experiment group with lessons injected; the model produces a task plan in each. **The primary metric is objective**: whether the scenario's unique proper noun appears in the plan (i.e. whether the knowledge was conveyed); the LLM judge is reference-only (calibration found that small-model judges exhibit positive/negative bias on vague plans — see the H5 section of `REPORT.md`). Result: control 0% vs experiment 90%, **CONFIRMED**.

## Usage

```bash
# Prerequisites
pip install openai

# Remote endpoint (OpenAI)
export OPENAI_API_KEY=sk-...

# — OR — local OpenAI-compatible endpoint (LM Studio / Ollama), no key needed
export VERIFY_BASE_URL=http://localhost:1234/v1   # LM Studio
# export VERIFY_BASE_URL=http://localhost:11434/v1  # Ollama
export VERIFY_MODEL=qwen2.5-1.5b-instruct

# Run all experiments (5 samples each, ~2 min)
python docs/experiments/verify_hypotheses.py --samples 5

# Run single hypothesis
python docs/experiments/verify_hypotheses.py --hypothesis H5 --samples 10

# High-confidence run (more samples, ~10 min)
python docs/experiments/verify_hypotheses.py --samples 20
```

## Configuration

| Env Var | Default | Description |
|---------|---------|-------------|
| `OPENAI_API_KEY` | (required unless `VERIFY_BASE_URL`) | API key for remote LLM calls |
| `VERIFY_BASE_URL` / `OPENAI_BASE_URL` | OpenAI | OpenAI-compatible endpoint base URL (local-first) |
| `VERIFY_MODEL` | `gpt-4o-mini` | Model for generation + judging |
| `VERIFY_JUDGE_BASE_URL` | = `VERIFY_BASE_URL` | H5: separate judge endpoint (optional) |
| `VERIFY_JUDGE_MODEL` | = `VERIFY_MODEL` | H5: separate (typically stronger) judge model |

## Expected Results

Based on design document predictions:

- **H1**: Injection should improve compliance by ~20%+ (target: control ~60%, experiment ~85%)
- **H2**: Instructive format should improve compliance by ~15-20% over descriptive
- **H3**: 1500 tokens should be sweet spot (too low = missing rules, too high = noise/dilution)
- **H4**: Mis-injection rate should be < 5% for clearly unrelated memories
- **H5**: Lesson injection should raise specific-knowledge conveyance (measured 2026-08-26: control 0% → experiment 90%, CONFIRMED)
- **H7**: Skill injection should raise specific-step conveyance; trigger mis-fire < 5% (measured 2026-08-27 against a real server: control 0% → experiment 78%, mis-fire 0/40, both CONFIRMED)
- **H6**: Semantic facts should be conveyed, stable across sessions, and corrections should propagate via supersede (measured 2026-08-27 against a real server: 0%→100% conveyance, 100% consistency, 100% correction propagation, all CONFIRMED)
- **H5**: Lesson injection should raise pitfall-avoidance (target: control low, experiment ≥ +10%)

## Cost

Using `gpt-4o-mini` with 5 samples per experiment:
- H1: ~10 API calls (generate) + ~10 (judge) = ~20 calls
- H2: ~10 + ~10 = ~20 calls
- H3: ~20 + ~20 = ~40 calls  
- H4: ~5 calls
- H5: ~10 + ~10 = ~20 calls
- **Total (all)**: ~105 calls ≈ $0.06-0.12 (free on a local endpoint)

With 20 samples: ~420 calls ≈ $0.25-0.50
