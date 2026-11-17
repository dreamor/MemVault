"""SDK-free, quote-grounded judgments for H5/H7; unknown is never a failure."""
from __future__ import annotations

import json
import os
import re
from urllib.parse import urlsplit, urlunsplit

DEFAULT_JUDGE_MAX_TOKENS = 2048
JUDGE_MAX_TOKENS = int(os.getenv("VERIFY_JUDGE_MAX_TOKENS", str(DEFAULT_JUDGE_MAX_TOKENS)))
JUDGE_RETRY_MAX_TOKENS = int(
    os.getenv("VERIFY_JUDGE_RETRY_MAX_TOKENS", str(max(JUDGE_MAX_TOKENS, 4096)))
)


def _credential_env(name: str) -> bool:
    """Credential names only, not tokenizer switches or numeric token budgets."""
    name = name.upper()
    if name.startswith("TOKENIZERS_") or re.search(
        r"(?:^|_)(?:BUDGET|MAX|MIN|LIMIT|COUNT|SIZE|LENGTH|COST)(?:_|$)", name
    ):
        return False
    return name in {"API_KEY", "TOKEN", "ACCESS_TOKEN", "AUTH_TOKEN", "BEARER_TOKEN", "SECRET", "PASSWORD"} or name.endswith(
        ("_API_KEY", "_TOKEN", "_SECRET", "_SECRET_KEY", "_PASSWORD")
    )


def sanitize(value):
    """Preserve evidence, but redact credentials in errors, metadata and raw text."""
    if isinstance(value, dict):
        return {str(key): ("[REDACTED]" if re.search(
            r"(?i)(api[_-]?key|authorization|password|secret|access[_-]?token)", str(key)
        ) else sanitize(item)) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [sanitize(item) for item in value]
    if not isinstance(value, str):
        return value
    for name, secret in os.environ.items():
        if secret and _credential_env(name):
            value = value.replace(secret, "[REDACTED]")
    value = re.sub(r"(?i)\bBearer\s+[^\s\"'<>]+", "Bearer [REDACTED]", value)
    value = re.sub(r"(?i)(\b(?:api[_-]?key|access[_-]?token|password|secret)\b[\"']?\s*[=:]\s*[\"']?)[^\s&\"'<>]+",
                   r"\1[REDACTED]", value)
    value = re.sub(r"\bsk-[A-Za-z0-9_-]+", "[REDACTED]", value)
    return re.sub(r"(https?://)[^/\s@]+@", r"\1[REDACTED]@", value)


def endpoint_record(url):
    """Retain endpoint identity, never its credentials/query/fragment."""
    if not url:
        return None
    try:
        parsed = urlsplit(str(url))
        host = parsed.netloc.rsplit("@", 1)[-1]
        return sanitize(urlunsplit((parsed.scheme, host, parsed.path, "", "")))
    except ValueError:
        return "[INVALID ENDPOINT]"


def normalize_whitespace(text: str) -> str:
    return " ".join(text.split())


def numbered_steps(plan: str) -> dict[int, list[str]]:
    """Use printed step numbers, not positions; duplicate numbers stay ambiguous."""
    steps = {}
    current = None
    lines = []
    for line in plan.splitlines():
        match = re.match(r"^\s*([0-9]+)[.)](?:[ \t]+|(?=[^\W\d_]))", line)
        if match:
            if current is not None:
                steps.setdefault(current, []).append("\n".join(lines))
            # Ground only the body; an enumeration label is not precaution evidence.
            current, lines = int(match.group(1)), [line[match.end():]]
        elif current is not None:
            if line and not line[0].isspace():
                # A separate preamble/outro is not part of the last numbered step.
                steps.setdefault(current, []).append("\n".join(lines))
                current, lines = None, []
            else:
                lines.append(line)
    if current is not None:
        steps.setdefault(current, []).append("\n".join(lines))
    return steps


def _strict_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def _reject_constant(value):
    raise ValueError("non-JSON constant")


def parse_grounded_judgment(raw, plan: str, *, finish_reason=None, error=None,
                            metadata=None, ordered_conditions=None) -> dict:
    """Validate provenance/order, not semantic entailment. Only double null is negative.

    Legacy step/quote cites one compound step. A bindings array may cite several
    real steps for ONE obligation. For a declared ordered relation, bindings are
    aligned to the condition list and checked against actual plan step order.
    """
    result = {"avoided": None, "valid": False, "status": "unknown",
              "reason": None, "step": None, "quote": None, "raw": raw,
              "bindings": [], "ordered_conditions": ordered_conditions or [],
              "grounding_is_semantic_proof": False,
              "metadata": {**(metadata or {}), "finish_reason": finish_reason},
              "error": error}

    def done(reason, avoided=None):
        result.update(reason=reason, avoided=avoided, valid=avoided is not None,
                      status="positive" if avoided is True else "negative" if avoided is False else "unknown")
        return result

    if error is not None:
        return done("request_error")
    # A valid-looking prefix can still be cut off. More tokens are not proof of completion.
    if finish_reason != "stop":
        return done("missing_finish_reason" if finish_reason is None else "incomplete_completion")
    if not isinstance(raw, str) or not raw.strip():
        return done("empty_completion")
    if ordered_conditions is not None and (
        not isinstance(ordered_conditions, list) or any(
            not isinstance(condition, str) or not condition.strip() for condition in ordered_conditions
        )
    ):
        return done("invalid_ordered_conditions")
    try:
        parsed = json.loads(raw, object_pairs_hook=_strict_object, parse_constant=_reject_constant)
    except (ValueError, TypeError):
        return done("malformed_json")
    if not isinstance(parsed, dict):
        return done("invalid_schema")
    if set(parsed) == {"step", "quote"}:
        result.update(step=parsed["step"], quote=parsed["quote"])
        if parsed["step"] is None and parsed["quote"] is None:
            return done("explicit_absence", False)
        bindings = [parsed]
    elif set(parsed) == {"bindings"}:
        bindings = parsed["bindings"]
        if not isinstance(bindings, list) or not bindings:
            return done("invalid_bindings")
        if ordered_conditions and len(bindings) not in (1, len(ordered_conditions)):
            return done("incomplete_ordered_bindings")
    else:
        return done("invalid_schema")

    steps = numbered_steps(plan)
    positions = {number: position for position, number in enumerate(steps)}
    references = []
    for binding in bindings:
        if not isinstance(binding, dict) or set(binding) != {"step", "quote"}:
            return done("invalid_binding_schema")
        step, quote = binding["step"], binding["quote"]
        if type(step) is not int or step <= 0:
            return done("invalid_step")
        if not isinstance(quote, str) or not normalize_whitespace(quote) or not any(
            char.isalnum() for char in quote
        ) or re.fullmatch(r"[0-9]+\s*[.)]", normalize_whitespace(quote)):
            return done("invalid_quote")
        candidates = steps.get(step, [])
        if len(candidates) != 1:
            return done("missing_or_ambiguous_step")
        step_text = candidates[0]
        normalized_quote = normalize_whitespace(quote)
        normalized_step = normalize_whitespace(step_text)
        evidence = {"step_text": step_text, "normalized_step": normalized_step,
                    "normalized_quote": normalized_quote, "raw_match": quote in step_text,
                    "normalized_match": normalized_quote in normalized_step}
        result["bindings"].append({"step": step, "quote": quote, "grounding": evidence})
        if len(bindings) == 1:
            result["grounding"] = evidence
        if normalized_quote not in normalized_step:
            return done("ungrounded_quote")
        references.append(positions[step])
    result["reference_order"] = references
    if any(left > right for left, right in zip(references, references[1:])):
        return done("wrong_reference_order")
    return done("grounded_judge_claim", True)


def _plain(value):
    """Convert SDK usage records without depending on the SDK or dumping the client."""
    if value is None or isinstance(value, (str, bool, int, float)):
        return value
    if isinstance(value, dict):
        return {str(k): _plain(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_plain(v) for v in value]
    if hasattr(value, "model_dump"):
        return _plain(value.model_dump())
    if hasattr(value, "__dict__"):
        return {k: _plain(v) for k, v in vars(value).items() if not k.startswith("_")}
    return None


def judge_quote(client, model: str, precaution: str, plan: str, *, base_url=None,
                ordered_conditions=None) -> dict:
    """One semantic judge request per obligation; retry once if truncated.

    Reasoning models count hidden reasoning tokens against ``max_tokens``.  A
    length-limited completion may therefore start like valid JSON before being
    cut off, so it must never be parsed.  Retry the same prompt once with the
    larger budget and retain both attempts as evidence; if both are truncated,
    the result remains unknown.
    """
    budgets = [JUDGE_MAX_TOKENS]
    if JUDGE_RETRY_MAX_TOKENS > JUDGE_MAX_TOKENS:
        budgets.append(JUDGE_RETRY_MAX_TOKENS)
    metadata = {"requested_model": model, "response_model": None,
                "endpoint": endpoint_record(base_url), "usage": None,
                "max_tokens": budgets[0], "attempts": []}
    relation = ("Required chronological conditions, in this execution order: " +
                json.dumps(ordered_conditions, ensure_ascii=False)) if ordered_conditions else "No additional ordered condition list."
    prompt = f"""PRECAUTION: {precaution}

PLAN:
{plan}

{relation}
Assess whether the PLAN explicitly performs the ENTIRE precaution, including all semantic conditions and execution ordering. Grounding alone does not establish entailment. Paraphrases count. A correct relation may span several numbered steps: backfilling in an earlier step followed by adding NOT NULL in a later step satisfies backfill-before-NOT-NULL. Merely mentioning both actions in the wrong execution order does not.
If the entire precaution is performed by one compound step, cite one nonempty contiguous quote from its body using {{"step": <positive printed integer>, "quote": "<exact body words>"}}.
If several steps jointly satisfy ONE precaution, return {{"bindings": [{{"step": <integer>, "quote": "<exact body words>"}}, ...]}}. Each quote must come from its own cited step body, excluding enumeration labels. Never stitch text across steps into one quote or cite punctuation/numbers alone. References must follow actual plan execution order. When a required chronological condition list is supplied, use exactly one binding per listed condition, in that condition order; one compound-step binding is also allowed if it explicitly satisfies the whole relation. Do not discard the ordering requirement.
If the plan does not perform the entire precaution, return {{"step": null, "quote": null}}. Answer ONLY strict JSON, no markdown or explanation."""
    raw = finish = None
    result = None
    try:
        for attempt, max_tokens in enumerate(budgets, start=1):
            response = client.chat.completions.create(
                model=model,
                messages=[{"role": "user", "content": prompt}],
                max_tokens=max_tokens, temperature=0.0,
            )
            response_model = getattr(response, "model", None)
            usage = _plain(getattr(response, "usage", None))
            choices = getattr(response, "choices", None) or []
            if not choices:
                raw = finish = None
                metadata.update(response_model=response_model, usage=usage,
                                max_tokens=max_tokens)
                metadata["attempts"].append({
                    "attempt": attempt, "max_tokens": max_tokens,
                    "response_model": response_model, "usage": usage,
                    "finish_reason": None, "raw": None,
                })
                result = parse_grounded_judgment(
                    None, plan, error="missing completion choice",
                    metadata=metadata, ordered_conditions=ordered_conditions,
                )
                break
            choice = choices[0]
            finish = getattr(choice, "finish_reason", None)
            raw = getattr(getattr(choice, "message", None), "content", None)
            metadata.update(response_model=response_model, usage=usage,
                            max_tokens=max_tokens)
            metadata["attempts"].append({
                "attempt": attempt, "max_tokens": max_tokens,
                "response_model": response_model, "usage": usage,
                "finish_reason": finish, "raw": raw,
            })
            result = parse_grounded_judgment(
                raw, plan, finish_reason=finish, metadata=metadata,
                ordered_conditions=ordered_conditions,
            )
            if finish != "length" or attempt == len(budgets):
                break
    except Exception as exc:
        result = parse_grounded_judgment(raw, plan, finish_reason=finish,
                                        error=str(exc) or type(exc).__name__, metadata=metadata,
                                        ordered_conditions=ordered_conditions)
    result["precaution"] = precaution
    return sanitize(result)


def aggregate_judgments(items: list[dict]) -> dict:
    """A valid negative dominates; all valid positives pass; everything else is unknown."""
    values = [item.get("avoided") if item.get("valid") is True else None for item in items]
    avoided = False if any(v is False for v in values) else True if values and all(v is True for v in values) else None
    return {"avoided": avoided, "valid": avoided is not None,
            "status": "positive" if avoided is True else "negative" if avoided is False else "unknown",
            "items": items}


def summarize_judgments(judgments) -> dict:
    judgments = list(judgments)
    positives = sum(j.get("valid") is True and j.get("avoided") is True for j in judgments)
    negatives = sum(j.get("valid") is True and j.get("avoided") is False for j in judgments)
    valid = positives + negatives
    return {"total": len(judgments), "valid": valid, "unknown": len(judgments) - valid,
            "positive": positives, "negative": negatives,
            "rate": positives / valid if valid else None}
