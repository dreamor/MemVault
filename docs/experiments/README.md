# MemVault Hypothesis Verification

> ⚠️ **历史验证（2026-08-11）**：本目录是 MemVault 早期对 `docs/DESIGN.md` Appendix C 四项核心设计假设（H1-H4）的验证实验，结论全部 CONFIRMED 后已归档。仅作为设计决策的历史佐证保留；脚本保留供复现，如需重新验证请按下方说明运行。完整结果见 `REPORT.md`。
>
> **H5（2026-08-26，CONFIRMED ✓）**：三类记忆演进计划（`docs/MEMORY-EVOLUTION-PLAN.md`）Phase A 的验收假设——「教训注入降低同类任务重复失败率」。本地开源模型实测：知识传达率 0% → 90%（+90%）。设计与结果详见 `REPORT.md` 的 H5 章节。


Automated experiments to validate the core design assumptions in `../DESIGN.md` Appendix C (H1–H4) and the episodic-memory acceptance hypothesis from `../MEMORY-EVOLUTION-PLAN.md` (H5).

## Hypotheses

| ID | Hypothesis | Method |
|----|-----------|--------|
| H1 | Pre-Prompt Injection improves compliance rate | A/B: no injection vs injected, LLM-as-judge |
| H2 | Instructive `[MUST]` format > descriptive format | A/B: two format styles, same memories |
| H3 | Optimal Token Budget ≈ 1500 tokens | Vary budget (500/1000/1500/2000), measure compliance |
| H4 | Router mis-injection rate < 5% | Feed unrelated memories + coding context, judge relevance |
| H5 | Lesson injection reduces repeat-failure rate | A/B: no lesson vs injected lesson; primary = objective knowledge-conveyance check |

### H5 说明（情景记忆验收，2026-08-26 CONFIRMED ✓）

H5 模拟真实的情景记忆闭环：每类任务（deploy / migrate / upgrade / refactor / release）都有一个**不显而易见的项目专属事实**作为坑（如 `DASHBOARD_CDN` 必须指向新 CDN 域名——模型无法凭常识猜出）。教训文本使用与线上一致的格式——
- 反思产出（`reflection.rs` 规则路径）：`Before '<task_type>' tasks, verify: <cause>`
- 注入格式（`session_start`）：`[REF] When working on '<task_type>' tasks: <lesson>`

对照组不注入教训、实验组注入教训，分别让模型产出任务计划。**主判定为客观指标**：计划中是否出现该场景的唯一专名（知识是否被传达）；LLM 裁判仅作参考（校准发现小模型裁判对模糊计划存在正/负偏差，详见 `REPORT.md` H5 章节）。结果：对照组 0% vs 实验组 90%，**CONFIRMED**。

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
