"""MemVault plugin for Hermes Agent — shared memory for AI agents.

Injects recalled memories via ``pre_llm_call`` before the model sees the
first prompt of a session, and hands the session text to the extractor at
session end so durable learnings land in the review inbox (unreviewed, same
trust boundary as every other automated source).

Talks to the MemVault HTTP backend over the stdlib only (urllib); if no
backend answers, every call degrades to a no-op and Hermes keeps working.

Verify-on-install (Hermes plugin API may rename hooks between versions):
- "pre_llm_call" hook name and the shape of the return value that injects
  context (see README.md); extraction hook unset until a session-end hook
  is confirmed — call ``extract_session`` manually or wire it when verified.
"""

from __future__ import annotations

import json
import os
import urllib.request

BASE = os.environ.get("MEMVAULT_HTTP_URL", "http://127.0.0.1:3777")
AGENT = os.environ.get("MEMVAULT_AGENT_ID", "hermes")
EXTRACT = os.environ.get("MEMVAULT_HOOK_EXTRACT", "0") == "1"
TIMEOUT = float(os.environ.get("MEMVAULT_TIMEOUT", "5"))
MAX_TRANSCRIPT_BYTES = 200_000


def _post(path: str, payload: dict) -> str | None:
    """POST JSON, returning the body on success and None on any failure."""
    try:
        req = urllib.request.Request(
            f"{BASE}{path}",
            data=json.dumps(payload).encode("utf-8"),
            headers={"content-type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(req, timeout=TIMEOUT) as resp:
            return resp.read().decode("utf-8", errors="replace")
    except Exception:
        return None


class MemVaultPlugin:
    name = "memvault"

    def __init__(self) -> None:
        self._injected_sessions: set[str] = set()

    def pre_llm_call(self, state):
        """Recall memories once per session before the first LLM call.

        ``state`` is whatever Hermes passes to pre-LLM hooks; we only read a
        session identifier (any mapping key among session_id/id) for
        deduplication. Unrecognized states simply re-inject nothing: the
        first call still injects, later calls hit the fallback below.
        """
        session_key = _session_of(state)
        if session_key is not None and session_key in self._injected_sessions:
            return None
        if session_key is not None:
            self._injected_sessions.add(session_key)
        text = _post("/api/session?output=plain", {"agent_id": AGENT})
        if not text or not text.strip():
            return None
        return _as_context(text.strip())

    def extract_session(self, transcript: str) -> int | None:
        """Push raw session text to the extractor; returns draft count or None.

        Off unless MEMVAULT_HOOK_EXTRACT=1. Wire this to Hermes'
        session-end/turn-end hook once its name is verified; it is safe to
        call manually as well.
        """
        if not EXTRACT:
            return None
        body = _post(
            "/api/extract",
            {"text": transcript[-MAX_TRANSCRIPT_BYTES:], "agent_id": AGENT},
        )
        if body is None:
            return None
        try:
            return int(json.loads(body).get("extracted", 0))
        except Exception:
            return None


def _session_of(state) -> str | None:
    getter = getattr(state, "get", None)
    if getter is None and isinstance(state, dict):
        getter = state.get
    if getter is None:
        return None
    for key in ("session_id", "sessionId", "id"):
        value = getter(key)
        if isinstance(value, str) and value:
            return value
    return None


def _as_context(text: str):
    """Return the injection payload Hermes understands.

    Placeholder for the exact pre_llm_call return shape — a bare string is
    the least-surprising choice; adjust here (and only here) when verified.
    """
    return text
