# MemVault Hypothesis Verification

> 📊 **Looking for the public-benchmark harness?** H1–H7 below validate MemVault's
> own design hypotheses on hand-written scenarios. For the third-party benchmark
> run (BEAM instruction/preference adherence — the axis where MemVault's
> follow-through and trusted-memory claims are measurable against other memory
> systems), see [External benchmark harness](#external-benchmark-harness-beam-instructionpreference-adherence)
> at the end of this file, and `../DESIGN.md` §2.6 for why LoCoMo-first was
> rejected.

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

---

## External benchmark harness: BEAM instruction/preference adherence

> Added 2026-11. Distinct from H1–H7 above: those validate MemVault's own design
> hypotheses on hand-written scenarios; this one runs a **public, third-party
> benchmark** so the result is comparable with other memory systems.
> Script: [`beam_adherence.py`](./beam_adherence.py).

### Why BEAM, and why this axis

`../DESIGN.md` §2.6 records the finding that changed the plan: as context windows
passed 1M tokens, LoCoMo and LongMemEval stopped discriminating — the naive
"stuff the whole history into context" baseline scores competitively on them now.
A good LoCoMo number would therefore prove nothing about memory architecture.

BEAM (arXiv 2510.27246, ICLR 2026) was built to ask exactly that question, and
two of its ten ability categories are the ones MemVault is the only surveyed
product built for:

| BEAM category | MemVault claim it tests |
|---|---|
| `instruction_following` | L3 follow-through + L6 trusted memory (§11.1) |
| `preference_following` | same, for stated preferences rather than directives |

Every other surveyed product can *inject*; none measures whether the injection was
**obeyed**. This is the one public axis where MemVault can win by a wide margin
and win for an explainable reason.

### What the probes give us

BEAM's adherence probes carry their own grading contract, which is what makes
this tractable without hand-authoring scenarios:

```
instruction_being_tested / preference_being_tested   the standing rule
expected_compliance                                  what compliance looks like
compliance_indicators[]                              positive markers
non_compliance_signs[]                               negative markers
rubric[]                                             "LLM response should contain: …"
source_chat_ids[]                                    which turns established the rule
```

`source_chat_ids` is provenance — it names the exact dialogue turns the rule came
from, so a seeded memory can carry its evidence instead of being asserted.

### Design of the A/B

Same harness shape as `verify_h6.py` / `verify_h7.py`: a real `memvault-mcp`
subprocess, a throwaway temp store, one memory per probe deleted before the next.

    control     question -> LLM, no memory context
    experiment  question -> LLM, with MemVault's own /api/session injection block

The rule is seeded as a **MUST** memory with `human_reviewed=true` and
`ai_generated=false` — the combination §14.7/§18.3.1 treat as human-authored,
which is what lands it in the *instruction* block rather than as passive context.
That is the mechanism under test, so the harness asserts it rather than assuming
it: every probe records `injection_format` and `instruction_layer_used`, and a run
where any seeded MUST arrived as ordinary context is reported as
`INVALID (harness invariant violated)` instead of producing a verdict. A MUST that
silently degraded to REFERENCE would still put the rule text in the block while
measuring a completely different claim.

Embeddings are **off** by default. That is itself a result: §17.3 says MUST
resolves deterministically with zero embedding calls, so the experiment arm must
still inject correctly with no provider configured. `--embeddings` measures the
semantic path too.

### On scoring honesty

BEAM's rubric items are *semantic* descriptions ("code blocks with syntax
highlighting"), not literal strings — so unlike H5's proper-noun stems they cannot
be scored by substring match. The primary metric is an LLM judge, which is also
what BEAM itself uses and therefore the comparable choice. Two countermeasures for
the survey's warning that judge-prompt sensitivity swings results by ~±10%:

1. `--judge-stability N` re-judges N probes under a reworded prompt and reports
   self-agreement. Below 80% the harness prints a caveat telling you not to cite
   the headline rate without it.
2. Where an objective check *is* reliable it is recorded alongside — for
   `format_instruction` probes, the presence of a markdown fence. The two are
   reported separately and never merged, so a disagreement is visible evidence
   about the judge rather than being averaged away.

The full harness spec (models, base URLs, MemVault commit, prompt templates
verbatim, scoring rules, split, n, selection order) is written into the results
JSON. §2.6 is explicit that most memory benchmarks have no official leaderboard
and that vendor self-reports use incompatible judge models and prompts — so a
number published without its spec is not comparable and should not be cited. The
script says so on stdout when you omit `--out`.

Probes are taken in dataset order, never sampled: a cherry-picked subset is not
comparable with anyone else's number.

### Usage

```bash
pip install openai
export OPENAI_API_KEY=...          # or VERIFY_BASE_URL for a local endpoint
cargo build -p memvault-mcp

# validate the whole harness without spending a single token
python3 docs/experiments/beam_adherence.py --dry-run

# ...and with a real server, exercising seeding + injection (still no LLM calls)
python3 docs/experiments/beam_adherence.py --dry-run --with-server \
    --split 100K --conversations 5 --probes 20

# a real scored run
python3 docs/experiments/beam_adherence.py --split 100K --conversations 3 \
    --judge-stability 10 --out docs/experiments/beam_results.json

# replay a cached row file, no network
python3 docs/experiments/beam_adherence.py --dry-run --with-server \
    --from-file /path/to/beam_row.json
```

No pyarrow or pandas needed: rows come from the HF datasets-server HTTP API and
are cached under `$TMPDIR/memvault-beam-cache/`. Splits are `100K` (20
conversations), `500K` (35) and `1M` (35); the 10M track is not in the public
parquet release.

### Two data quirks the loader handles

- **`probing_questions` is a Python literal string, not JSON** — single-quoted,
  written straight from a Python pipeline. `json.loads` fails on it; the loader
  uses `ast.literal_eval` and falls back to JSON in case a future release
  normalises the column.
- **`time_anchor` is stamped on the first turn of each session only.** A 188-turn
  conversation with three sessions of 60/56/72 turns carries exactly three
  anchors (ids 0, 60, 116). Reading only the source turn's own field yields
  `None` for nearly every probe, and a bounded backwards walk fails too — a turn
  deep in a 72-turn session is further from its anchor than any fixed window
  short of the session length. The loader walks back unbounded to the session
  start and records `time_anchor_origin` as `direct` / `inherited:<id>` / `none`,
  because an inherited anchor is a sound reading of the data but still a reading.

### Plumbing validation (2026-11-05, no LLM calls)

`--dry-run --with-server --split 100K --conversations 5 --probes 20`:

| Check | Result |
|---|---|
| probes extracted | 20 (10 `instruction_following`, 10 `preference_following`) |
| provenance resolvable | 20/20 |
| event-time anchor resolved | 20/20 (all `inherited`) |
| seeded as MUST | 20/20, 0 write failures |
| rule present in injection block | 20/20 |
| delivered via `MustRef` instruction layer | 20/20 |
| injection latency (embeddings off) | **p50 ≈1.2 ms · p95 ≈1.6 ms** (n=20, `/api/session` only; repeats within ±0.1 ms, so one significant figure is the honest precision) |
| injected tokens (est., chars/4) | mean 33 · p95 39 |

The latency figure is measured around the `/api/session` call only, never
including model generation, so it is comparable with mem0's published frame
(~6.7–7.0K tokens per retrieval, **p50 ≤ 1.1 s**). It is the §17.3
deterministic-MUST claim measured rather than asserted — but note it is a
plumbing validation on 20 probes with no embedding provider, **not** a benchmark
score. No adherence number has been produced yet; that needs an LLM key and a
judge-stability measurement, and must not be cited without them.

### Known gap found while building this

`occurred_at` (event-time) is only settable through the *extraction* endpoints
(`POST /api/extract`, MCP `extract_memories`), not through a direct save. So this
harness resolves a session date for every probe and has nowhere to put it — the
anchor is recorded as dataset metadata (`occurred_at_resolved`,
`time_anchor_origin`) and explicitly **not** claimed as memory provenance.
Tracked in `../DESIGN.md` §16.
