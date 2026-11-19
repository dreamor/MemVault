# Native task benchmark protocol v2

This protocol applies to Rust `memvault-cli bench`, not the Python H5/H7 or
public BEAM/MAB harnesses. A retrieved or selected lesson ID is not task success.
A judged pass evaluates a proposed plan against a known pitfall; it is not
execution evidence.

## Production and diagnostic review modes

```bash
# Live history; preserves evaluation-history persistence.
# This reads your vault, so use it only when that is intended.
memvault-cli bench --mode production --json
memvault-cli bench --mode diagnostic --json
```

The default remains diagnostic for compatibility with PR #167. Diagnostic mode
changes only the two review-gate overrides; all other selection behavior is shared
with production. Production mode resolves the normal configured gates. Reports
include effective settings, namespace/profile, review state, target lifecycle,
and selection-stage diagnostics.

This benchmark does not repair production lifecycle policy. The current
backlink-based lesson expansion can reintroduce an expired lesson after ordinary
retrieval excluded it. The synthetic expired case exposes that baseline behavior
through lifecycle and stage evidence; a selected/delivered ID is not proof that
the target is live or useful. Changing that production path is separate work.

Selection uses a preview of the same production pipeline. It does not record
memory access, skill exposure, or schedule embedding backfill. A live CLI run
still saves evaluation history; fixture runs do not open that side store.

The keyword retrieval baseline and injection candidate pool are distinct. The
report retains returned keyword ranks/scores/sources/tier, initial and later
selection IDs, actual skip/degradation reasons, and the original episode backlink.
A target never seen in candidates is not assigned an invented quality reason.
Missing targets are valid diagnostic gaps; non-NotFound storage failures remain
errors. The live episode scan limit is applied before lesson-bearing filtering.

## Selection versus delivery

`injected` remains the compatibility field for target ID selection. The rendered
delivery prefix is separately reported through delivered IDs and
`target_delivery_state`. These fields measure prepared context, even without a
judge; they are not proof that a model request occurred. The experimental-plan
stage evidence records the provider-call attempt and its response or error.

The plan context uses production trust-aware formatting and relevant conflict
warnings. An `instruction` field alone does not make a reference or unreviewed
memory a trusted MUST. The additional model-context budget retains a complete
memory prefix, with complete trust wrappers/XML/conflict text. It does not
slice a UTF-8 code point or a partial instruction.

Unicode-character and byte counts are distinct. Router degradation, formatter
exclusions, and additional character-cap exclusions are recorded separately.
The exact delivered context is available in JSON. Old byte/character token
heuristics are explicitly estimates, not API billing data.

## Native model evidence and paired judging

Without `--judge`, plan success and token savings are unmeasured. Zero counters
are not a measured zero success rate. Enabling `--judge` may call configured
models and should be an explicit operator decision.

The maximum application-level sequence remains four single requests:

1. Plan without memory.
2. Plan with delivered memory.
3. Judge the first plan against the supplied pitfall.
4. Judge the second plan against the same pitfall.

Earlier evidence remains available when a later stage fails. No transport or
model retry is added by the benchmark. Formal paired scores require all four
responses to be complete (`finish_reason=stop`) with strict JSON: nonempty string
`plan`, then boolean `pass` and nonempty string `reason`. Missing fields,
truncation, fences, duplicate keys, type coercion, or unpaired verdicts do not
enter the qualified denominator.

The additive extractor evidence interface retains request/response, status,
requested/returned model separately, raw/parsed reply, finish reason, usage,
errors and elapsed time. Existing text-only extractors remain supported, but
unknown metadata is not fabricated as `stop` or zero usage. Operational text is
kept separately from credential-safe serialized evidence.

Observed API tokens and coverage are recorded across attempted calls, including
unqualified outcomes. Missing usage stays unknown. Legacy geometric-retry
cost/savings models are labeled assumptions, not observed task savings.

Live JSON contains task text and rendered memory content. Treat it as a vault
export, not a public-safe artifact: provider-credential redaction is not general
secret detection for arbitrary memory prose. Review real inputs before sharing;
the committed fixture contains synthetic content only.

## Isolated frozen fixtures and review-mode A/B

```bash
# No user vault, env file, provider, or evaluation-history initialization.
# No --judge means no model calls.
memvault-cli bench \
  --fixture docs/experiments/fixtures/task_bench_v1.json \
  --agent-id fixture --compare-modes --json
```

Fixture schema v1 contains a fixture ID, memory and episode snapshots, explicitly
ordered sample episode IDs, and optional credential-free profiles/project.
Duplicate IDs, missing episode parents, unknown versions, managed-state snapshots
and structured credentials are rejected. Evidence/relation graphs are not part of
schema v1; this is not an arbitrary live-vault snapshot importer. A missing lesson
target is intentionally allowed as a negative case.

Each mode seeds a new independent in-memory SQLite store. Fixtures do not call
outcome reflection, embedding, or history persistence. Early dispatch bypasses
normal env-file/vault/backend/agent-registry initialization, even if the supplied
real database or Markdown configuration is unusable.

`--limit` bounds the ordered selected fixture cases (default 20); the report
retains input count, selected IDs, exact file SHA-256 and configuration hashes.
Configuration hashes include effective gates/profile/project and scoring mode,
not just CLI arguments. Production review gates still resolve process-level
configuration; bypassing the user env file does not make those gates constant.
Profiles that overflow selector arithmetic are rejected before store construction.
Production/diagnostic mode comparisons report paired retrieval, selection and
delivery transitions. These are not the model without/with-memory pair and
cannot establish task success without qualified model evidence.

The committed file is a **synthetic template**, not a representative production
workload or published benchmark. It includes reviewed/draft/human-authored,
missing/archived/expired and Unicode examples. Additional budget, conflict,
namespace and provenance cases are exercised by isolated unit tests. Router
recency/lifecycle selection still uses wall clock, which is reported rather than
claimed as bitwise time reproducibility.

## Lesson origin and active-task scope

User-text normalization is shared by transcript, correction, and terminal
inference paths. Known host envelopes are recognized at structural boundaries;
inline/fenced/quoted or explanatory examples are not dropped because they mention
an internal tag. Ambiguous provenance is not a claim of verified host origin.

Outcome inference uses a structurally selected active request group and coherent
evidence window, rather than pairing the opening request with an unrelated later
failure. Metadata records the selector policy, original request sequence group,
selected evidence sequences, window bounds and hashes. With delayed/interleaved
tools the evidence sequences can be non-contiguous: the bounds are not a claim
that every intervening request was included. Retry streaks belong to request/tool
groups, and repeated Stop reports for one group remain idempotent without merging
another execution merely because its task text is identical. Judge text remains
bounded; source and selected-window hashes are computed before judge truncation.
This is structural attribution, not semantic intent detection.

Episode provenance is optional nullable JSON (schema migration 35). Old records
remain readable with unknown provenance. Reflection records rule/LLM source,
origin episode and verified existing trace references. No raw JSONL/L0 outcome
content is copied by default and no synthetic trace IDs are invented. Hashes
cannot reconstruct original wording; raw retention would require separate opt-in.

Known internal-scaffolding contamination is checked before lesson embedding,
saving or backlinking, including recurrence drafts. Legitimate machine-generated
lessons remain unreviewed REFERENCE drafts with the existing confidence. Existing
bad rows are not rewritten or deleted automatically; review gates are not
weakened merely to improve injection counts.
