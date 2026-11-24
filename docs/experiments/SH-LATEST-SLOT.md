# SH latest-slot arbitration: fixed-context evidence

**Status:** exploratory, positive, not a MemoryAgentBench score
**Scope:** answer selection on single-hop SH questions whose official gold fact
is already the newest fact in its delivered subject/relation slot
**Last updated:** 2026-11-24

## 1. Question and claim

MemoryAgentBench's FactConsolidation SH split is single-hop, but it still tests
conflict resolution: every fact has a serial, and the largest serial in a
subject/relation slot is the newest value. The delivered context can therefore
contain the right slot and both the old and the new value while the answer prompt
still follows the old one.

This experiment isolates that failure. The tested claim is:

> When the official gold fact is already the newest value in one delivered slot,
> requiring a visible per-slot latest-value decision and an explicit branch
> traversal makes the answer follow the newest value instead of the stale one.

It is **not** evidence about retrieval quality, overall SH accuracy, other MAB
subsets, other models, or production prompt choice. Retrieval is frozen and no
fact, top-k, chunking or token budget is changed.

## 2. Experimental contract

| Item | Frozen value |
|---|---|
| Benchmark | `ai-hyz/MemoryAgentBench`, `factconsolidation_sh_6k`; separate `factconsolidation_sh_32k` replication |
| Gold | official dataset gold; no model or human relabel |
| Arm A | current official MAB `QUERY_TEMPLATE` |
| Arm B | visible slot table; largest serial per subject/relation; traverse the selected branch; no world-knowledge fill |
| Context | as retrieved by the 2026-11-19 raw-keyword run, copied unchanged |
| Model | `bailian/deepseek-v4.1-flash` |
| Endpoint | OpenAI-compatible gateway, `chat/completions` (address omitted) |
| Temperature / budget | `0.0` / `3000` completion tokens |
| Extra body | `{"enable_thinking": false}` |
| Scoring | official MAB substring-exact-match on the extracted final answer; no LLM judge |
| Completeness | only `finish_reason=stop` with a recognizable arm-specific final answer enters the accuracy denominator |
| Repeats | 3 per case per arm → **438 generations** on 6k; **372** on 32k |
| Missing evidence | answer exactly `insufficient evidence`; never fill gaps from world knowledge |

Both arms receive the same system context and differ only in the answer prompt.
The prompt hashes are `16dfd7ef…` (A) and `ea6034ca…` (B), byte-identical to the
MH fixed-context runs. The endpoint exposes no `seed`; with temperature `0.0`,
repeats measure execution stability under a deterministic contract, not
independent random draws.

## 3. Case selection

`docs/experiments/sh_latest_slot_cases.py` is deterministic and calls no model.
A question enters the set only when all of these hold in the frozen retrieval
result:

1. a delivered fact contains the official gold;
2. that fact's subject is named in the question text;
3. every delivered gold-bearing fact belongs to one subject/relation slot;
4. the gold fact has the largest serial in that slot across the full corpus;
5. at least one older same-slot fact is also delivered.

Selection never reads a model prediction or the source `search_correct` field.

| Selection outcome | Count |
|---|---:|
| source questions | 100 |
| **selected** | **73** |
| excluded: no older same-slot competitor in context | 26 |
| excluded: no question-named gold fact | 1 |

The 27 exclusions mean this is **not** a representative 100-question SH score.
The selected contexts contain 12–20 facts (mean 19.5, median 20). The source run
happened to answer 29/73 of them correctly end to end; that is provenance only
and is not used in selection or scoring.

The 32k replication repeats the same selector on a separate dry-run retrieval
(`factconsolidation_sh_32k`, 2,310 facts, keyword top-20, 100 questions, no LLM
calls; gold coverage 98% top-20 / 81% top-1 / 95% top-5). It selected **62/100**
(32 excluded for no older same-slot competitor in context, 2 for gold not being
the latest same-slot fact, 4 for no question-named gold fact). Every selected
32k context contains exactly 20 delivered facts.

## 4. Results

All 438 generations completed (`438/438`), so every case contributes a valid
pair; there are no incomplete-case exclusions.

| Arm | Correct answers | Accuracy | Complete | Case-majority correct | Repeat-consistent cases |
|---|---:|---:|---:|---:|---:|
| A current prompt | 88/219 | 40.2% | 219/219 | 30/73 (41.1%) | 65/73 |
| **B slot arbitration** | **219/219** | **100%** | 219/219 | **73/73** | **73/73** |

Paired outcome over the 73 valid cases:

- **B improved 43 cases**;
- **B regressed 0 cases**;
- the remaining 30 cases were already correct under A;
- A had no case-level ties (every case had a strict majority).

Repeat consistency: A produced one distinct answer in 65/73 cases; B in 73/73.
A's inconsistent cases were `no5, no15, no20, no23, no28, no83, no84, no86`; the
correct answer still won the majority in five of them.

### 4.1 The failure is stale-branch selection

Of A's **131 wrong answers, all 131** are exactly a listed older same-slot value
in the delivered context. There is no residual class of cross-slot or
world-knowledge errors in this selected set: when A is wrong here, it is wrong
because it selected the stale branch. B has 0 wrong answers, so B has no
old-branch selections to attribute.

Representative cases (full per-case predictions are in the archive README):

| Case | Question | Gold (newest) | Older competitor | A answer | B answer |
|---|---|---|---|---|---|
| no8 | What is the official language of Japan? | Swedish | Japanese | Japanese ×3 | Swedish ×3 |
| no12 | What position does Robert Parish play? | quarterback | center | center ×3 | quarterback ×3 |
| no67 | Who is the employer of Dave Filoni? | British Broadcasting Corporation | Lucasfilm | Lucasfilm ×3 | British Broadcasting Corporation ×3 |
| no94 | Who was Rand al'Thor created by? | Ferdowsi | Robert Jordan | Robert Jordan ×3 | Ferdowsi ×3 |
| no97 | Who is Walter Chrysler's child? | Charles Frederick, Duke of Holstein-Gottorp | Walter Percy Chrysler Jr. | Walter Percy Chrysler Jr. ×3 | Charles Frederick… ×3 |

These are synthetic corpus revisions, not world-knowledge questions. That is the
point: the prompt must prefer the newest serial even when the older value looks
more plausible.

### 4.2 SH 32k corpus-scale replication (2026-11-24)

The 62 selected 32k cases were run under the same prompts, model, endpoint,
temperature, budget and scoring contract: 62 × 2 × 3 = **372 generations**, all
complete.

| Arm | Correct answers | Accuracy | Complete | Case-majority correct | Repeat-consistent cases |
|---|---:|---:|---:|---:|---:|
| A current prompt | 89/186 | 47.8% | 186/186 | 31/62 (50.0%) | 56/62 |
| **B slot arbitration** | **186/186** | **100%** | 186/186 | **62/62** | **62/62** |

B improved **31** cases and regressed **0**; the other 31 cases were already
correct under A. All **97** wrong A answers are exactly a listed older same-slot
value; B has 0 wrong answers. A's six non-repeat-consistent cases were
`no11, no43, no47, no86, no87, no95`; five still had a correct majority.

| Arm | Prompt tok/call | Completion tok/call | Total tokens | Wall-clock p50 | Mean |
|---|---:|---:|---:|---:|---:|
| A | 472 | 4 | 88,461 | 1,057 ms | 1,013 ms |
| B | 550 | 69 | 115,105 | 1,170 ms | 1,204 ms |

B costs **+30.1% total tokens** and **+18.9% mean wall-clock** (+10.7% at p50;
+113 ms p50, +191 ms mean).

This is a **corpus-scale replication, not a long-context test**: top-20 still
delivers exactly 20 facts per case. It does not measure retrieval quality; the
62 selected cases are those where retrieval happened to deliver both the newest
gold fact and an older same-slot competitor.

## 5. Cost (SH 6k)

| Arm | Prompt tok/call | Completion tok/call | Total tokens | Wall-clock p50 | Mean |
|---|---:|---:|---:|---:|---:|
| A | 460 | 4 | 101,491 | 1,053 ms | 1,040 ms |
| B | 538 | 66 | 132,296 | 1,162 ms | 1,178 ms |

B costs **+30.4% total tokens**. Per-call wall-clock is **+10.3% at p50** and
**+13.3% on the mean** (+108 ms p50, +139 ms mean). These timings are measured
around the throttled request helper, so the absolute values include the harness's
1.1 s request spacing (`VERIFY_MIN_CALL_INTERVAL_S`); the A/B difference is the
informative part. The extra cost is the visible slot table and path. This is a
diagnostic prompt, not a free production replacement.

## 6. Interpretation and boundaries

Within this boundary, the result is unambiguous: when the newest same-slot fact
was already delivered, the explicit slot decision selected it, while the current
prompt selected the stale value in 131/219 answers on 6k and 97/186 on 32k. This
is the SH counterpart of the MH fixed-context result and extends it from
multi-hop chains to single-hop slot conflicts, then across two corpus scales.

Boundaries:

- two subsets (`factconsolidation_sh_6k` and `factconsolidation_sh_32k`), one
  model, one prompt pair;
- 27/100 questions were excluded on 6k and 38/100 on 32k for lack of an
  in-context older competitor (or other selection failures), so neither is an SH
  benchmark score;
- 32k changes corpus size, not delivered-context length: every selected 32k
  context is still 20 facts;
- the official gold is sometimes deliberately counter-intuitive; the experiment
  tests conflict arbitration, not factual plausibility;
- contexts are as retrieved, not re-retrieved; no retrieval gain is claimed;
- no `seed`; temperature `0.0` makes repeats a stability check;
- no top-k, budget, chunking, parser or scoring change is included.

## 7. Relationship to MH and next step

The MH work showed the same mechanism on two-hop chains, including a +40-fact
distractor stress. The SH work shows it is not specific to multi-hop shape: it
also applies when the entire task is one subject/relation slot with an older and
a newer value. The 32k run then replicated it on a corpus with seven times as
many facts, while keeping the delivered context at 20 facts.

The remaining regression question is whether the same holds at larger corpus
scales (64k/262k) or under longer delivered contexts. That is a separate
experiment and must not change retrieval, arbitration and scoring at the same
time.

## 8. Artifacts and reproduction

The local archives are kept out of Git by `.git/info/exclude`:

- `docs/experiments/results/2026-11-24/sh-latest-slot/` — 6k;
- `docs/experiments/results/2026-11-24/sh-32k-latest-slot/` — 32k;
- `docs/experiments/results/2026-11-24/sh-32k-retrieval/` — 32k retrieval-only
  dry run.

The 6k archive contains:

- `context.json` — frozen 73-question as-retrieved context;
- `baseline.json` — one-hop official-gold support records;
- `labels.json` — deterministic selection packet and full exclusion list;
- `SH_LATEST_SLOT_PACKET.md` — human-readable case table;
- `answer-arbitration/results.json` — 438 generations with usage, latency and
  traces;
- `answer-arbitration/analysis.json` — aggregate, paired counts, per-case
  predictions, cost and old-branch attribution;
- `README.md` and `SHA256SUMS`.

The 32k archive contains the equivalent context, baseline, labels, packet,
`answer-arbitration/results.json` (372 generations),
`answer-arbitration/analysis.json`, README and `SHA256SUMS`.

Code:

- `docs/experiments/sh_latest_slot_cases.py` — deterministic selector;
- `docs/experiments/answer_arbitration.py` — fixed-context A/B harness;
- `docs/experiments/analyze_answer_arbitration.py` — read-only summarizer.
