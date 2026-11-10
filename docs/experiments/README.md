# MemVault Hypothesis Verification

> 📊 **Looking for the public-benchmark harnesses?** H1–H7 below validate
> MemVault's own design hypotheses on hand-written scenarios. Two third-party
> benchmarks are at the end of this file: [BEAM](#external-benchmark-harness-beam-instructionpreference-adherence)
> for instruction/preference adherence and [MemoryAgentBench](#memoryagentbench-conflict-resolution)
> for factual conflict resolution. `../DESIGN.md` §2.6 records why LoCoMo-first
> was rejected.

> ⚠️ **Historical verification (2026-08-11)**: This directory holds MemVault's early verification experiments for the four core design hypotheses (H1–H4) in `../DESIGN.md` Appendix C; all conclusions were CONFIRMED and archived. Kept as historical evidence for the design decisions — scripts are retained for reproducibility; re-run them as described below to re-verify. Full results in `REPORT.md`.
>
> **H5 (2026-08-26, CONFIRMED ✓)**: Phase A acceptance hypothesis of the three-memory-type evolution (see `../DESIGN.md` §14) — "lesson injection reduces the repeat-failure rate on similar tasks". Measured with local open-source models: knowledge conveyance 0% → 90% (+90%). Design and results in the H5 section of `REPORT.md`.
>
> **H7 (2026-08-27, CONFIRMED ✓)**: Phase B procedural-memory acceptance — "skill injection improves first-attempt success" + "trigger mis-fire rate < 5%". Measured against a real `memvault-mcp` server: specific-step conveyance 0% → 78% (+78%), zero mis-injections across 40 unrelated contexts. See the H7 section of `REPORT.md` (including a quota defect exposed and fixed by the experiment).
>
> **H6 (2026-08-27, CONFIRMED ✓ ×3)**: Phase C semantic-memory acceptance — knowledge conveyance 0%→100%, cross-session consistency 100%, supersede correction propagation 100%. See the H6 section of `REPORT.md`.
>
> **Runtime regression (2026-08-28)**: Injection / outcome-loop / audit-trail plumbing regression — drives a real `memvault-mcp --transport http` subprocess + throwaway store (skill injection / mis-injection 0/20 / outcome loop / skipped audit trail / model auto-detection) 6/6 PASS; also includes the 2026-08-28 deepseek-v4-flash cross-validation. Script `verify_ollama_runtime.py`, results in the "Runtime regression" section of `REPORT.md`.


## Status at a glance

This directory mixes historical evidence, a live harness, and planned work. The
status is stated here so a file being present is never mistaken for a result.

| Area | Status | What is actually shipped |
|------|--------|--------------------------|
| H1–H4 | **Historical** | Archived 2026-08-11 verification on five samples; the same local model acted as agent and judge. Kept for reproducibility, not presented as a public benchmark score. |
| H5, H6, H7 | **Historical acceptance** | Local-model acceptance tests, with H6/H7 driving a real `memvault-mcp` subprocess. Scripts and results are retained in `REPORT.md`; these are internal hypotheses, not cross-system benchmark numbers. |
| Runtime regression | **Archived regression evidence** | `verify_ollama_runtime.py` exercises injection, the outcome loop, and the audit trail against a throwaway server/store. Useful as a regression check. |
| BEAM adherence | **Active harness, scored** | `beam_adherence.py` downloads the public BEAM rows, seeds human-authored MUST rules, runs the control/experiment A/B, and records its full scoring spec. 2026-11-10: 100K 31.3% → 88.8% (n=80, judge stability 90%); 500K 65.0% → 87.5% (n=40, judge stability 60%). |
| TRACE-RECALL-BENCH | **Planned** | The protocol and an offline channel-3a Criterion bench exist. The end-to-end `verify_trace_recall.py` does not exist, so no H8 verdict is available and the results tables are templates. |
| MemoryAgentBench | **Active harness, scored** | `memory_agent_bench.py` seeds a Conflict Resolution row verbatim, scores with the official `substring_exact_match`, and records the full spec. SH 6K: 53% vs 4% control (n=100). MH 32K keyword retrieval: **negative** — 18% gold coverage at top-20, 5% vs 1%. |

Automated experiments to validate the core design assumptions in `../DESIGN.md` Appendix C (H1–H4) and the episodic/procedural/semantic-memory acceptance hypotheses from `../DESIGN.md` §14 (H5, H7, H6).

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
`ai_generated=false` — the combination §13.7/§8.3.4 treat as human-authored,
which is what lands it in the *instruction* block rather than as passive context.
That is the mechanism under test, so the harness asserts it rather than assuming
it: every probe records `injection_format` and `instruction_layer_used`, and a run
where any seeded MUST arrived as ordinary context is reported as
`INVALID (harness invariant violated)` instead of producing a verdict. A MUST that
silently degraded to REFERENCE would still put the rule text in the block while
measuring a completely different claim.

Embeddings are **off** by default. That is itself a result: §6.1.2 says MUST
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
(~6.7–7.0K tokens per retrieval, **p50 ≤ 1.1 s**). It is the §6.1.2
deterministic-MUST claim measured rather than asserted — but note it is a
plumbing validation on 20 probes with no embedding provider, **not** a benchmark
score. No adherence number has been produced yet; that needs an LLM key and a
judge-stability measurement, and must not be cited without them.

### Scored runs (2026-11-10)

Both runs use `bailian/deepseek-v4.1-flash` as the answer model and
`bailian_text/deepseek-v4-pro` as the judge, temperature 0, embeddings off.

| | 100K split | 500K split |
|---|---|---|
| Conversations / probes | 20 / 80 | 10 of 35 / 40 |
| Probe mix | 40 instruction · 40 preference | 20 instruction · 20 preference |
| Control adherence | 31.3% | 65.0% |
| Experiment adherence | **88.8%** | **87.5%** |
| Delta | **+57.5 pp** | **+22.5 pp** |
| `instruction_following` | 27.5% → 77.5% | 45.0% → 75.0% |
| `preference_following` | 35.0% → 100.0% | 85.0% → 100.0% |
| Rule delivered via `MustRef` | 80/80 | 40/40 |
| Empty answers | 0 | 0 |
| Judge unparsed | 0 | 0 |
| Judge stability (reworded) | 90% (n=20) | **60% (n=10)** |
| Injection latency | p50 4.4 ms · p95 4.9 ms | p50 4.8 ms · p95 5.7 ms |
| Injected tokens (est.) | mean 30.7 | mean 29 |

Reading the two runs:

- The 100K result is the cleaner one: the experiment arm is almost three times
  the control rate and the judge is stable at 90%.
- The 500K run has a **higher control rate** (65%) and **judge stability of only
  60%**, so its +22.5 pp is a weaker claim — the probes in that sample are
  easier for an unassisted model and the judge flips verdicts under rewording.
  It is recorded, not headlined, and the two splits must not be compared with
  each other as if they were the same sample.
- Across both, the mechanism-level check holds: every seeded rule reached the
  agent as `MustRef`, with zero empty answers, zero unparsed verdicts, and
  single-digit-millisecond injection. The adherence delta is therefore
  attributable to the injected instruction layer, not to a harness artefact.

### Known gap found while building this

`occurred_at` (event-time) is only settable through the *extraction* endpoints
(`POST /api/extract`, MCP `extract_memories`), not through a direct save. So this
harness resolves a session date for every probe and has nowhere to put it — the
anchor is recorded as dataset metadata (`occurred_at_resolved`,
`time_anchor_origin`) and explicitly **not** claimed as memory provenance.
Tracked in `../DESIGN.md` §15.

## MemoryAgentBench: Conflict Resolution

> Added 2026-11-10. Script: [`memory_agent_bench.py`](./memory_agent_bench.py).

### Why this split

BEAM measures whether an injected *instruction* is obeyed. It says nothing about
whether a corpus of *facts* can be recalled and reconciled. MemoryAgentBench's
Conflict Resolution split is built for exactly that: each row is a numbered fact
list where a larger serial number means a newer fact, and the 100 questions per
row are written so that the model's pre-training answer is usually the stale
one. The official metric is objective (`substring_exact_match`), so unlike BEAM
there is no judge to calibrate and no judge-sensitivity caveat.

### What ran (2026-11-10)

| | |
|---|---|
| Dataset | `ai-hyz/MemoryAgentBench`, `Conflict_Resolution`, subset `factconsolidation_sh_6k` |
| Corpus | 455 facts seeded verbatim, serial prefixes preserved |
| Questions | 100 (the whole row) |
| Retrieval arm | `/api/search`, keyword FTS, top-20 → official `agentic_memory_agent` prompt |
| Control arm | same answer instruction, no memory block |
| Answer model | `bailian/deepseek-v4.1-flash`, temperature 0 |
| Scoring | official `substring_exact_match` (normalize → any ground truth is a substring of the prediction) |
| Embeddings | off (`MEMVAULT_EMBEDDING_PROVIDER=off`), so this is the keyword path |

Facts are seeded with `force_insert=true` on purpose. Delta-write would merge two
facts about the same subject into one memory, resolving the benchmark's conflict
*before* retrieval; the measurement would then be about the writer instead of
about whether retrieval plus in-context reconciliation works. The benchmark wants
both the old and the new fact reachable, with their serial numbers intact.

### Results

| Metric | Value |
|---|---|
| **Search accuracy** | **53.0%** (53/100) |
| Control accuracy | 4.0% (4/100) |
| Delta | **+49.0 pp** |
| Retrieval gold coverage (top-20) | 99% |
| Retrieval gold coverage (top-5) | 94% |
| Retrieval gold coverage (top-1) | 88% |
| Mean retrieved | 19.5 memories · ≈281 tokens (chars/4 estimate) |
| Search latency | p50 **23.5 ms** · p95 **27.9 ms** (n=100, single 455-fact store) |
| Seeding | 455 facts in 0.33 s |

### Reading the result

Retrieval is not the bottleneck: in 99 of 100 questions an accepted answer was
in the top-20, and in 43 of the 47 failures it was in the **top-5** — the model
saw the correct, newer fact and answered with its pre-training fact anyway. The
failure mode is the benchmark's intended trap, not a missing memory:

| Question | Gold (newer serial) | Model answered | Pre-training answer |
|---|---|---|---|
| Which sport is goaltender associated with? | `pesäpallo` (310) | `ice hockey` | yes |
| What is Nobuhiro Watsuki famous for? | `The Fairly OddParents` (259) | `Rurouni Kenshin` | yes |
| What is the official language of Japan? | `Swedish` (439) | `Japanese` | yes |
| Who is the author of Sidereus Nuncius? | `Samuel Beckett` (401) | `Galileo Galilei` | yes |

So the honest summary is: MemVault carries the corpus and retrieves the
conflicting facts reliably; the 53% ceiling comes from `deepseek-v4.1-flash`
not following the "larger serial wins" instruction when it contradicts world
knowledge. A stronger answer model, or a memory layer that supersedes rather
than exposes stale facts, is the next thing to test — neither is claimed here.

**Not leaderboard-comparable.** The official `agentic_memory_agent` may issue
multiple searches and repairs; this harness issues exactly one search per
question. The number is comparable **across MemVault versions and retrieval
configurations**, not against published MemoryAgentBench leaderboard entries.

### Second run: MH 32K (2026-11-10) — a negative result worth keeping

`factconsolidation_mh_32k` has the same 100 questions over 2,310 facts. The
keyword path does not scale to it:

| Retrieval depth | Gold coverage | Gold in top-5 | Gold in top-1 |
|---|---|---|---|
| top-20 | 18% | 7% | 4% |
| top-50 | 34% | 6% | 3% |
| top-100 | 43% | 8% | 2% |

With top-20 the end-to-end result was **5% search vs 1% control** (n=100), and
unlike the SH 6K run the failures are *not* an instruction problem: in 82 of 100
questions an accepted answer never reached the prompt at all. These are
multi-hop questions ("the spouse of the author of …") whose two supporting
facts share few surface terms with the question, so BM25 ranks thousands of
weakly-matching facts above them. Raising `top_k` buys coverage slowly (43% at
100 memories ≈ 1,578 tokens) and would drown the model in distractors before it
reaches the ceiling.

Conclusion: **keyword-only retrieval is adequate for a small single-hop corpus
and not adequate for multi-hop at 32K.** The next step is an embedding provider
for `hybrid`/`semantic` search (the harness already accepts it; only the global
`--arm` wiring is keyword today), plus query decomposition for multi-hop
questions. No code change is made here to paper over the result — the point of
running the intake order was to find this before claiming MH support.

### The injection channel is deliberately not the fact-recall arm

`/api/session` injects MUST memories deterministically; a REFERENCE fact corpus
produces an empty formatted block (verified on this binary). That is §6.1.2
working as designed, not a defect: automatic injection is for standing
instructions (measured by BEAM), while facts are recalled by search. Reporting
the empty injection as a retrieval failure would be dishonest, so `--arm inject`
exists only as a diagnostic and the headline number is `--arm search`.

### Usage

```bash
pip install pyarrow openai
export VERIFY_BASE_URL=... OPENAI_API_KEY=... VERIFY_MODEL=...
cargo build -p memvault-mcp

# retrieval-only: seed the corpus and score gold coverage, no LLM calls
python3 docs/experiments/memory_agent_bench.py --dry-run --questions 100

# full search + control run
python3 docs/experiments/memory_agent_bench.py \
    --subsets factconsolidation_sh_6k --questions 100 \
    --top-k 20 --arm both \
    --out docs/experiments/mab_results.sh6k.search20.json
```

`mab_results*.json` is git-ignored, like `beam_results*.json`: the results file
carries the full harness spec, but the committed evidence is the table above.

### Remaining intake order

1. **FactConsolidation-SH (6K)** — done 2026-11-10 (this section).
2. **FactConsolidation-MH (32K)** — run 2026-11-10; **negative** for keyword
   retrieval (18% gold coverage at top-20, 5% accuracy vs 1% control). Needs
   hybrid/semantic retrieval before it can be read as a memory-system result.
3. **EventQA** — only after the two conflict slices are stable; it adds
   long-range temporal questions without changing the core harness.

LoCoMo and LifeBench are not on this path: `../DESIGN.md` §2.6 explains why a
good LoCoMo number no longer discriminates a memory architecture from a large
context window.
