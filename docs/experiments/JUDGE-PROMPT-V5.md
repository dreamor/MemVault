# Judge prompt v5 — scope rules for named flows and same-entity paraphrases

**Status:** opt-in candidate, accepted on three held-out batches. The production
default remains `literal_markdown_quotes_v4`; v5 is selected explicitly with
`VERIFY_JUDGE_PROMPT_VERSION=literal_markdown_quotes_v5`.

## Why

The H5/H7/H6 adherence judge checks whether a plan explicitly performs an
ordered precaution. Two recurring failure modes appeared in the v4 held-out
batches:

1. **Over-acceptance of a broader suite.** A plan step such as "run integration
   and contract tests ... including ... refund flows" was accepted as performing
   the separate obligation "run the refund flow in the sandbox".
2. **Instability on same-entity paraphrases.** "Backfill profile data for
   existing users" followed by "add NOT NULL after backfill succeeds" was
   sometimes accepted and sometimes rejected as a paraphrase of "backfill
   `users.profile_json` before adding the NOT NULL constraint".

v5 adds two narrow scope rules to the existing prompt and changes nothing else:
no new model, no new retrieval, no new scoring contract, no threshold change.

## The v5 rules (verbatim)

```text
Scope discipline for named flows and same-entity paraphrases (v5):
- A step that runs a broader test suite which merely includes or mentions a named
  flow does NOT perform the named flow. Example: "run integration tests including
  refund flows" does not satisfy "run the refund flow".
- When the obligation names an operation on a specific entity, column or field,
  performing that same operation on a named subset or a synonym of the same
  entity counts, provided the required result and ordering are preserved.
  Example: "backfill profile data for existing users" followed by "add NOT NULL
  after backfill succeeds" satisfies "backfill users.profile_json before adding
  the NOT NULL constraint".
- Neither rule relaxes CHANGE obligations: verifying an existing state, merely
  mentioning the action, or performing the operation on a different entity is
  still negative.
```

## Protocol

- Cases: three frozen 18-case held-out batches, drawn from H5/H7 result sets
  that had not been used for prompt design. Each batch mixes positive, negative
  and (batch 3) one boundary case.
- Labels: every case was human-confirmed. Batch 1 was blind-relabelled after the
  first run (11 positive / 7 negative); batch 2 was blind-relabelled (9 / 9);
  batch 3 was confirmed directly (9 positive / 8 negative / 1 boundary).
- Repeats: 2 serialized judge calls per case per prompt version (18 × 2 × 2 =
  72 calls per batch). `temperature=0`, thinking enabled, fixed model, fixed
  OpenAI-compatible gateway (address omitted), fixed token budget.
- Scoring: judge consensus per case against the human label. A case whose two
  repeats disagree is reported as `inconsistent` and excluded from the decisive
  denominator; `unknown` attempts are reported separately. Boundary cases are
  excluded from the decisive denominator by design.
- Raw inputs, prompts, per-attempt responses and SHA256SUMS are retained under
  `docs/experiments/results/2026-11-24/judge-heldout*/` (local artifacts; the
  `results/` tree is git-excluded, so these files are not committed).

## Results

| batch | labels | v4 decisive | v5 decisive | v5 change |
|---|---|---:|---:|---|
| held-out 1 | 11 pos / 7 neg | 17/18 (1 FN) | 17/18 (1 FN) | same |
| held-out 2 (development) | 9 pos / 9 neg | 16/17 (1 FP, 1 inconsistent) | 17/17 (1 unknown attempt) | fixes FP and instability |
| held-out 3 (acceptance) | 9 pos / 8 neg / 1 boundary | 16/16 (1 inconsistent) | 17/17 | fixes instability |

Per-run points (two repeats collapsed to one verdict per case):

| batch | prompt | TP / TN | FP / FN | unknown attempts | inconsistent | repeat agreement |
|---|---|---:|---:|---:|---:|---:|
| 1 | v4 | 10 / 7 | 0 / 1 | 0 | 0 | 100% |
| 1 | v5 | 10 / 7 | 0 / 1 | 0 | 0 | 100% |
| 2 | v4 | 8 / 8 | 1 / 0 | 0 | 1 | 94.4% |
| 2 | v5 | 8 / 9 | 0 / 0 | 1 | 1 | 94.4% |
| 3 | v4 | 9 / 7 | 0 / 0 | 0 | 1 | 94.4% |
| 3 | v5 | 9 / 8 | 0 / 0 | 0 | 0 | 100% |

What v5 fixed:

- `b2-h7-1-upgrade-cont-step1`: both repeats now reject "integration tests
  including refund flows" as performing "run the refund flow" — the real FP.
- `b2-h5-6-migrate-cont-prec0`: both repeats now accept the same-entity
  paraphrase, removing the non-determinism.
- `b3-h7-1-upgrade-cont-step0`: both repeats agree under v5 (v4 was split).
- `b2-h5-2-upgrade-expe-prec0`: one v5 repeat returned `unknown` because the
  model dropped the Markdown backticks around `Idempotency-Key`; the other
  repeat was correct, so the case verdict stayed TP. This is an output-contract
  wobble, not a semantic regression, and it is counted in the unknown column.

Across all three batches v5 introduced **no new FP, FN or unknown-attempt
increase**, and removed 1 FP and 2 instabilities.

## Remaining known limitation

The only stable error across all three batches is the batch-1
`h5-6-migrate-exp-prec0` FN. The plan says "Backfill the new profile columns",
which the judge does not accept as a synonym of `users.profile_json`. This is a
documented policy boundary, not a regression introduced by v5. Changing it
would require a separate, broader synonym rule and a fresh acceptance batch; it
is deliberately not part of v5.

## Decision

- v5 passes this three-batch acceptance: 17/17 decisive on the untouched batch,
  0 FP / 0 FN, 0 inconsistent, 100% repeat agreement.
- v5 stays **opt-in**. The default prompt and all existing scores are unchanged,
  so historical results remain comparable.
- Promoting v5 to the production adherence scorer should wait for a fourth
  untouched batch or a production shadow run. This report is judge-calibration
  evidence, not a new benchmark score and not evidence about retrieval quality.
