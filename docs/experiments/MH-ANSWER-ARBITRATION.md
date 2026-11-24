# MH answer arbitration: fixed-context evidence

**Status:** exploratory, positive, not a MemoryAgentBench score
**Scope:** answer selection when the complete support chain is already in the
delivered context
**Last updated:** 2026-11-24

## 1. Question and claim

MemoryAgentBench's Conflict Resolution split gives every corpus fact a serial
number and treats the largest serial in a subject/relation slot as the newest
value. A model can therefore fail in two different places:

1. **retrieval** — the context does not contain the complete support chain; or
2. **arbitration** — the context contains the chain, but the model follows an
   older same-slot value, a cross-slot value, or a world-knowledge guess.

This report isolates the second failure. The tested claim is:

> When every chain fact is already present and the only variable is the answer
> prompt, requiring a visible per-slot latest-value decision and an explicit
> branch traversal materially improves final-answer correctness.

It is **not** evidence that retrieval improves, that the overall MH score rises,
or that the prompt generalises to other models, languages, datasets, or slot
shapes. Those require separate experiments.

## 2. Experimental contract

| Item | Frozen value |
|---|---|
| Benchmark | `ai-hyz/MemoryAgentBench`, `factconsolidation_mh_6k` |
| Arm A | current official MAB `QUERY_TEMPLATE` |
| Arm B | visible slot table; largest serial per subject/relation; traverse the selected branch; no world-knowledge fill |
| Model | `bailian/deepseek-v4.1-flash` |
| Endpoint | `https://ai-hub.ebanma.com/v1`, `chat/completions` |
| Temperature / budget | `0.0` / `3000` completion tokens |
| Extra body | `{"enable_thinking": false}` |
| Scoring | official MAB substring-exact-match on the extracted final answer; no LLM judge |
| Completeness | only `finish_reason=stop` with a recognizable arm-specific final answer enters the accuracy denominator |
| Repeats | 3 per case per arm |
| Missing evidence | answer exactly `insufficient evidence`; never fill gaps from world knowledge |

Both arms receive the same system context and differ only in the answer prompt.
The prompt hashes are recorded in every result file. The 2026-11-20 and
2026-11-24 six-case runs used byte-identical prompt, context, model, budget and
scoring contracts; the replication guard aborts before generation if any of nine
contract fields drift.

The endpoint exposes no `seed` parameter. Because temperature is `0.0`, repeats
measure execution stability under a deterministic contract, not independent
draws from a random sampler. Statistical intervals below are descriptive only.

## 3. Evidence

### 3.1 Six human-confirmed chains, first run (2026-11-20)

Cases: `no11, no36, no39, no57, no92, no94`. `no3` was excluded before the run
because its same-slot Italy head-of-state facts conflict (`#403` versus the newer
`#1777`); it remains excluded.

| Arm | Correct answers | Accuracy | Complete | Case-majority correct | Repeat-consistent cases |
|---|---:|---:|---:|---:|---:|
| A current prompt | 2/18 | 11.1% | 18/18 | 1/6 | 5/6 |
| B slot arbitration | **18/18** | **100%** | 18/18 | **6/6** | **6/6** |

B improved five cases and regressed none. A's only case-level win (`no92`) was
itself unstable (`Austin, Paris, Austin`); B answered `Austin` three times.

Cost from the same 36 calls:

| Arm | Prompt tok/call | Completion tok/call | Total tokens | Provider latency p50 | Mean |
|---|---:|---:|---:|---:|---:|
| A | 726 | 3 | 13,113 | 512 ms | 542 ms |
| B | 804 | 106 | 16,371 | 903 ms | 910 ms |

### 3.2 Independent execution under the same contract (2026-11-24)

This was a replication, not a new design: 36 fresh generations, with all nine
contract fields verified before the first call.

| Run | Arm | Correct | Accuracy | Case-majority correct | Repeat-consistent cases |
|---|---|---:|---:|---:|---:|
| 2026-11-20 | A | 2/18 | 11.1% | 1/6 | 5/6 |
| 2026-11-24 | A | 1/18 | 5.6% | 0/6 | 5/6 |
| 2026-11-20 | B | 18/18 | 100% | 6/6 | 6/6 |
| 2026-11-24 | B | 18/18 | 100% | 6/6 | 6/6 |

Across both executions, B is 36/36 answers and 12/12 run-case majorities; A is
3/36 answers and has a correct majority in only 1 of the 6 unique cases (1/12
run-case majorities). Cross-run answer agreement is 33/36. All three
disagreements are in arm A (`no11`, `no92`); B has no cross-run disagreement.

The important correction is to A's base rate: 2/18 was the top of a very low
range, not a stable 11%. On these six chains A is roughly 0–10%. B's effect did
not depend on a single batch.

### 3.3 Held-out two-hop cases with real retrieval context (2026-11-24)

The first held-out construction produced 12 three-hop chains, but the frozen
retrieval arm delivered the complete chain for only 1/12. The arm performs one
first-hop query plus bridge queries derived from first-hop facts; it never issues
the third-hop query. Running an answer-only test there would score a correct
`insufficient evidence` as an arbitration failure and would mostly measure
retrieval.

A second set was therefore built from 15 two-hop questions whose complete chains
the **same frozen retrieval arm** already delivered (`raw_hybrid`, top-k 20,
candidate-k 250, 5 keyword anchors, 5 bridges × 5). Selection used retrieval
feasibility only, never answer correctness. The 15 chains were human-confirmed on
2026-11-24; the labels and their fingerprint
(`human_labels_sha256 = 2472b46f…`) are retained in the local archive. The
prompt pair and hashes are identical to the six-case run.

Excluded after review:

- `no23` — implicit relation hop; the corpus has club → sport → country, not the
  relation the question asks for;
- `no82` — relation substitution; the chain jumps to `head_of_state`;
- `no2`, `no89` — duplicate `sport → country_of_origin` mechanism already
  represented in the confirmed six;
- `no15` — implicit relation inference; retained as a separate diagnostic, not
  part of arbitration A/B;
- `no3` — the pre-existing same-slot conflict above.

Results (15 cases × 2 arms × 3 repeats = 90 generations):

| Arm | Correct answers | Accuracy | Complete | Case-majority correct | Repeat-consistent cases |
|---|---:|---:|---:|---:|---:|
| A current prompt | 9/45 | 20.0% | 45/45 | 3/15 | 15/15 |
| **B slot arbitration** | **45/45** | **100%** | 45/45 | **15/15** | 14/15 |

B improved 12 cases and regressed none. Its only inconsistency was `no90`
(`French` twice, `French, English` once); all three answers are
substring-correct, so accuracy remains 45/45 while consistency is 14/15.

Of A's 36 wrong answers, **21 are literally the older same-slot competitor**
shown in the context. The other 15 are neither gold nor a listed same-slot
competitor; they are cross-slot or world-knowledge values. This attribution is
the mechanism evidence: the failure is often branch selection, not absence of
the needed fact.

Cost from the same 90 calls:

| Arm | Prompt tok/call | Completion tok/call | Total tokens | Provider latency p50 | Mean |
|---|---:|---:|---:|---:|---:|
| A | 625 | 3 | 28,293 | 491 ms | 503 ms |
| B | 704 | 113 | 36,754 | 877 ms | 888 ms |

B adds about 79 prompt tokens and 110 completion tokens per call for the visible
slot table and derivation.

## 4. Consolidated view

| Evidence set | Cases | A correct | B correct | A majority | B majority | B improved | B regressed |
|---|---:|---:|---:|---:|---:|---:|---:|
| Six confirmed, run 1 | 6 | 2/18 (11.1%) | 18/18 (100%) | 1/6 | 6/6 | 5 | 0 |
| Six confirmed, run 2 | 6 | 1/18 (5.6%) | 18/18 (100%) | 0/6 | 6/6 | 6 | 0 |
| Held-out 2-hop, as-retrieved | 15 | 9/45 (20.0%) | 45/45 (100%) | 3/15 | 15/15 | 12 | 0 |
| **Pooled six-case runs** | 12 run-cases | 3/36 (8.3%) | 36/36 (100%) | 1/12 | 12/12 | 11 | 0 |

Descriptive Wilson 95% intervals for answer accuracy:

- held-out A: 20.0% [10.9%, 33.8%];
- held-out B: 100% [92.1%, 100%];
- pooled six-case A: 8.3% [2.9%, 21.8%];
- pooled six-case B: 100% [90.4%, 100%].

The intervals describe these fixed case sets; they are not confidence statements
about a random sample of all MH questions. The cases were selected for retrieval
feasibility, and repeated generations at temperature 0 are not independent
samples. No significance test is used as a substitute for held-out coverage.

## 5. What this does and does not establish

**Established within the tested boundary**

- With the full chain already delivered, the current MAB answer prompt often
  follows an older same-slot value. On the held-out set, 21/36 wrong answers are
  literal old-slot values.
- A visible slot table plus latest-serial selection plus explicit traversal
  removes this failure on all tested cases: 45/45 held-out and 36/36 across the
  two six-case executions.
- The effect replicated under a guarded byte-identical contract and appeared on
  cases selected before any answer was generated.
- The benefit is not universal across all errors: A also produced cross-slot and
  world-knowledge answers that the mechanism does not directly explain.

**Not established**

- **No end-to-end MAB score.** A retrieval miss cannot be repaired by answer
  arbitration; the armed B prompt returns `insufficient evidence` when a slot is
  missing. The 3-hop dry run is a concrete example: only 1/12 complete chains
  were delivered.
- **No retrieval gain.** Retrieval, top-k, fusion and candidate generation were
  frozen. This experiment changes only the answer prompt.
- **No generalisation beyond the sample.** The chains are human-confirmed
  reconstructions, not native MAB support annotations; the corpus is synthetic
  and contains injected false facts; the result is one model, one endpoint, one
  prompt pair, and two-hop chains with two conflicting slots per hop.
- **No production decision by itself.** The next gates are context-length
  stress, an independent SH latest-slot experiment, and then a wider regression
  only if those pass.

## 6. Threats to validity

1. **Case selection:** held-out means "not used to design the prompt", not a
   random sample. Retrieval feasibility was a selection criterion and may
   correlate with chain shape.
2. **Label provenance:** the dataset supplies no support annotations. Chains
   were reconstructed deterministically and human-confirmed, but remain
   `human-confirmed reconstruction`, not official ground truth.
3. **Deterministic sampling:** temperature 0 makes repeats tests of execution
   stability. Provider-side nondeterminism still appeared in A, and once in B.
4. **Scoring extraction:** B's visible table necessarily mentions old values, so
   scoring uses only its `STAGE 2 — ANSWER:` field (or a single bare answer).
   Whole-response substring matches are diagnostics, never the primary score.
5. **Prompt coupling:** the slot table changes both reasoning order and output
   length. Token/latency cost is therefore part of the treatment, not noise.
6. **One model:** the result should not be read as a property of all LLMs or of
   MemVault's production injection channel.

## 7. Artifacts and reproduction

All raw outputs are in the local, intentionally untracked result archive
(`docs/experiments/results/` is excluded by `.git/info/exclude`). Original
results were not modified.

| Evidence | Local path |
|---|---|
| Six-case run 1 | `docs/experiments/results/2026-11-20/answer-arbitration/` |
| Six-case run 2 | `docs/experiments/results/2026-11-24/answer-arbitration-replication/` |
| 15-case held-out 2-hop | `docs/experiments/results/2026-11-24/mh-heldout-2hop/` |
| 3-hop rejection archive | `docs/experiments/results/2026-11-24/mh-heldout-candidates/` |

Reproduction harness:

```bash
.venv/bin/python docs/experiments/answer_arbitration.py \
  --source docs/experiments/results/2026-11-24/mh-heldout-2hop/retrieval-context.json \
  --baseline docs/experiments/results/2026-11-24/mh-heldout-2hop/chains.json \
  --cases no7,no10,no14,no18,no38,no42,no47,no51,no56,no65,no68,no74,no80,no90,no97 \
  --repeats 3 \
  --out docs/experiments/results/2026-11-24/mh-heldout-2hop/answer-arbitration/results.json
```

Model calls require `.venv/bin/python`, `~/.config/memvault/bench.env`, and a
working `VERIFY_BASE_URL` / `OPENAI_API_KEY`; no release binary is involved.
