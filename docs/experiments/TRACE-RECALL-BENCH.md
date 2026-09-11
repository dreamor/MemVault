# TRACE-RECALL-BENCH — handoff vs. compaction vs. recall cost

> **Status**: the protocol is in place and the results tables are a template (to be filled). The offline-measurable part (channel 3's ingest and payload sizes) is implemented by `crates/memvault-core/benches/trace_recall_cost.rs`; channels 1–2 and the per-successful-task judgement need a real LLM + judge and are not automated yet.
>
> **Goal**: a cost/value benchmark for trace ingestion — prove that recalling existing memories is not more expensive than re-reading the session.
> **Reference**: funes handoff-vs-recall benchmark — <https://huggingface.co/datasets/dacorvo/funes-handoff-recall-benchmark> · <https://huggingface.co/blog/funes>

## Hypothesis

**H8**: on tasks whose answers depend on prior knowledge from a previous session, **the recall channel's weighted tokens per successful task ≤ the hand-written handoff channel**, and ≤ the compaction-continuation channel; i.e. MemVault's `session_start` injection + `get_memory_evidence` expansion is cheaper in tokens than having the agent (or a human) rewrite a handoff document, or compacting the long session and continuing.

funes reported recall to be 4–8x cheaper than hand-written handoff; this benchmark **does not presuppose we must hit that** — it reports the three channels as measured, and failures are kept as failures.

## Two-task construction rule

Construct **2** tasks, each of which must satisfy:

1. **The answer depends on prior knowledge from the previous session** — there is a project-specific fact that "cannot be guessed" (e.g. an internal CDN domain, a required header, a migration backfill step, a private environment variable name). A model without the prior knowledge cannot guess it, so the control group necessarily fails.
2. **A single objective criterion** — whether the task answer contains the pre-registered **unique proper-noun stem** (success = it appears in the plan / answer / artifact). This follows the H5/H7 primary-metric convention: objective keyword hits take precedence over an LLM judge.
3. **Both tasks share the same prior session history** — so the "load information" cost of the three channels is directly comparable and the task difficulty is identical.
4. **Success is judged as binary** — no "partial success"; the judge is only a **reference** when channels 1–2 produce the handoff/compaction text. The primary criterion remains the proper-noun hit.

Reuse the existing H5/H7 scenario library (the deploy / migrate / upgrade pitfalls in `verify_hypotheses.py` / `verify_h7.py`), binding one project-specific fact to each task.

## Three channels and measurement steps

Each channel runs the same 2 tasks for **N rounds** (N ≥ 5 suggested), recording per-round **weighted tokens** and **success**.

### Channel 1 — hand-written handoff (handoff)

Have one LLM (standing in for "the previous session's agent") read the prior session history and produce a handoff document; then have a **fresh-session** LLM complete the task using only that handoff document.

Measurement steps:
1. Record `prompt_tokens + completion_tokens` for generating the handoff document (if the handoff is human-written, count the tokens of an equivalent-information document and note the source in the results table).
2. Fresh session: record `handoff_tokens + task_prompt_tokens + completion_tokens`.
3. Judge success against the two-task criterion.

**This channel's tokens come from the LLM provider's usage fields** (both the generation and consumption legs count — the handoff is not free).

### Channel 2 — compaction continuation (compaction)

In the same long session, first do the prior task, let compaction trigger, then continue with the target task.

Measurement steps:
1. Record the pre-compaction session tokens, the tokens of the compaction call itself, and the post-compaction continuation tokens.
2. Because compaction "flattens" discoveries (funes), record the target task's failures after compaction — that is the value signal beyond cost for this channel.
3. Judge success.

### Channel 3 — recall — **measurable offline**

At new-session start, `session_start` injects the existing memories; when the task needs source material, the agent calls `get_memory_evidence(memory_id)` to expand the raw evidence block (the read-only evidence tool).

Measurement has two layers:

**3a. Offline-measurable (implemented, no LLM needed)** — `cargo bench -p memvault-core --bench trace_recall_cost`:
- ingest a synthetic transcript (`SIGNAL_TURNS` + `FILLER_TURNS` in `benches/trace_recall_cost.rs`, 40 turns of which 4 carry extractable signal) via `ingest_for_agent`;
- report **ingest throughput** (turns/session);
- report **injected-payload size vs full-transcript size** (compression ratio in bytes and tokens).

**3b. End-to-end (needs an LLM, same criterion as 3a)** — drive a real `memvault-mcp` subprocess:
1. the prior session's artifacts (L0 evidence + distilled candidates) are already in the store;
2. new session: record the tokens of the injected text returned by `session_start`;
3. record the tokens of `get_memory_evidence` expansions during the task (counted only when actually called);
4. total = `injected tokens + expanded tokens + task/completion tokens`;
5. judge success.

## Tokenizer / accounting convention

- **Single convention**: a channel's "weighted tokens" = the sum of all tokens the channel injects/generates for one task, averaged over successful rounds; failed rounds stay in the denominator (failed tasks burn tokens too).
- **Channels 1–2**: use the provider-returned `usage.prompt_tokens` / `usage.completion_tokens` directly (a real tokenizer — preferred).
- **Channel 3a (offline)**: use the repo's existing helper `MemoryRouter::estimate_tokens` (`crates/memvault-core/src/router.rs`; the same formula lives in `router/format.rs`): ASCII ≈ 4 chars/token, CJK ≈ 1.5 chars/token; for ASCII text that is `bytes / 4`, matching funes's rough estimate. **Do not** hand-roll another estimator inside the bench.
- **Channel 3b**: with a real model, use provider usage (a real tokenizer); cross-check the offline part with `estimate_tokens`.
- Weighting: this benchmark applies no priority weighting (each injected memory is counted as-is), which is the minimal interpretable reading of funes's "weighted tokens per successful task".

## Known limitations

- **Channels 1–2 cannot be automated offline**: producing a handoff document and executing compaction both require a real LLM (and compaction behaviour differs per host), so those two channels' numbers **must be filled by an external script + LLM provider usage**; a Rust bench cannot generate them.
- **Channel 3a measures cost only, not success**: the offline part can prove "injected payload ≪ full transcript" but cannot prove task success; success requires 3b or the LLM leg of channels 1–2.
- Single machine, synthetic transcript; real sessions are longer and messier, so the ratio will differ.
- The handoff-side token estimation convention may deviate from provider usage; note the source in the results table.
- **No fabricated numbers**: this table stays an empty template until real runs produce results.

## Results tables (template — to be filled)

Placeholder command (offline part — automatically prints the channel-3a size ratios; paste the output into the table below):

```bash
cargo bench -p memvault-core --bench trace_recall_cost 2>&1 | tee /tmp/trace_recall_cost.txt
```

Placeholder command (end-to-end, **not implemented yet**, needs an LLM endpoint; call it in this shape once implemented):

```bash
# TODO: docs/experiments/verify_trace_recall.py does not exist yet — provider usage is needed to fill channels 1/2/3b
export VERIFY_BASE_URL=http://127.0.0.1:11434/v1
export VERIFY_MODEL=qwen2.5:1.5b-instruct
python docs/experiments/verify_trace_recall.py --channels handoff,compaction,recall --rounds 5
```

### Weighted tokens per successful task

| Task | Channel | Weighted tokens / successful task | Success rate | Notes |
|------|---------|-----------------------------------|--------------|-------|
| Task 1 | handoff | _to fill_ | _to fill_ | provider usage |
| Task 1 | compaction | _to fill_ | _to fill_ | provider usage |
| Task 1 | recall | _to fill_ | _to fill_ | 3b; offline ratio below |
| Task 2 | handoff | _to fill_ | _to fill_ | |
| Task 2 | compaction | _to fill_ | _to fill_ | |
| Task 2 | recall | _to fill_ | _to fill_ | |

### Channel 3a offline size ratio (from a bench run)

| Metric | Value | Source |
|--------|-------|--------|
| Synthetic-session turns | 40 (4 of them signal turns) | `trace_recall_cost.rs` |
| Ingest throughput | _to fill_ turns/session | `trace_ingest_synthetic_session` |
| Injected payload bytes / tokens | _to fill_ | `[trace-recall] recall payload` from `report_payload_ratio` |
| Full-transcript bytes / tokens | _to fill_ | `[trace-recall] full transcript` |
| Compression ratio (full / recall) | _to fill_x bytes / _to fill_x tokens | `[trace-recall] compression ratio` |

> Example (one local run, for reference only — re-run for actual output): the synthetic session ingested 3 evidence / 3 candidates; injected payload 189 B / 47 tokens vs full transcript 1747 B / 437 tokens → **9.24x bytes / 9.30x tokens**. The ratio grows with session length; a small sample does not represent production.

### Verdict

| Hypothesis | Expected | Measured | Verdict |
|------------|----------|----------|---------|
| H8: recall ≤ handoff (tokens per successful task) | ≤ | _to fill_ | _to fill_ |
