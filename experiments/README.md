# MemVault Hypothesis Verification

Automated experiments to validate the core design assumptions in `docs/DESIGN.md` Appendix C.

## Hypotheses

| ID | Hypothesis | Method |
|----|-----------|--------|
| H1 | Pre-Prompt Injection improves compliance rate | A/B: no injection vs injected, LLM-as-judge |
| H2 | Instructive `[MUST]` format > descriptive format | A/B: two format styles, same memories |
| H3 | Optimal Token Budget ≈ 1500 tokens | Vary budget (500/1000/1500/2000), measure compliance |
| H4 | Router mis-injection rate < 5% | Feed unrelated memories + coding context, judge relevance |

## Usage

```bash
# Prerequisites
pip install openai
export OPENAI_API_KEY=sk-...

# Run all experiments (5 samples each, ~2 min)
python experiments/verify_hypotheses.py --samples 5

# Run single hypothesis
python experiments/verify_hypotheses.py --hypothesis H1 --samples 10

# High-confidence run (more samples, ~10 min)
python experiments/verify_hypotheses.py --samples 20
```

## Configuration

| Env Var | Default | Description |
|---------|---------|-------------|
| `OPENAI_API_KEY` | (required) | API key for LLM calls |
| `VERIFY_MODEL` | `gpt-4o-mini` | Model for generation + judging |

## Expected Results

Based on design document predictions:

- **H1**: Injection should improve compliance by ~20%+ (target: control ~60%, experiment ~85%)
- **H2**: Instructive format should improve compliance by ~15-20% over descriptive
- **H3**: 1500 tokens should be sweet spot (too low = missing rules, too high = noise/dilution)
- **H4**: Mis-injection rate should be < 5% for clearly unrelated memories

## Cost

Using `gpt-4o-mini` with 5 samples per experiment:
- H1: ~10 API calls (generate) + ~10 (judge) = ~20 calls
- H2: ~10 + ~10 = ~20 calls
- H3: ~20 + ~20 = ~40 calls  
- H4: ~5 calls
- **Total (all)**: ~85 calls ≈ $0.05-0.10

With 20 samples: ~340 calls ≈ $0.20-0.40
