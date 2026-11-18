"""SDK-free evidence primitives shared by the benchmark harnesses."""
from __future__ import annotations

import copy
import datetime as dt
import hashlib
import json
import os
import re
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]


def content_hash(path: str | Path | None) -> str | None:
    """Hash actual bytes, never substitute a URL, identifier or results file."""
    if path is None or not Path(path).is_file():
        return None
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def json_hash(value) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False,
                                     separators=(",", ":")).encode()).hexdigest()


_SECRET_KEY = re.compile(r"(?:api[_-]?key|secret|password|authorization|(?:^|[_-])(?:access[_-]?token|refresh[_-]?token|auth[_-]?token|token)$|^credentials?$)", re.I)


_REDACTION_VALUES = set()


def register_secret(value: str):
    if value:
        _REDACTION_VALUES.add(value)


_CREDENTIAL_HEADER = re.compile(
    r"\b(?:X-MemVault-Api-Key|X-Api-Key|api-key|Authorization)(?:\\?[\"'])?\s*:\s*"
    r"(?P<value>\\\"(?:\\.|[^\"\\])*?\\\"|\\'(?:\\.|[^'\\])*?\\'|"
    r"\"(?:\\.|[^\"\\])*\"|'(?:\\.|[^'\\])*'|"
    r"(?:(?:Basic|Bearer|Digest|Token)\s+)?[^\s\"'\\`,;}]+)", re.I)


def _redact_header(match):
    value = match.group("value")
    quote = (value[:2] if value.startswith(('\\"', "\\'")) else
             value[0] if value.startswith(('"', "'")) else "")
    body = value[len(quote):-len(quote)] if quote else value
    scheme = re.match(r"(?:Basic|Bearer|Digest|Token)\s+", body, re.I)
    replacement = quote + (scheme.group() if scheme else "") + "[REDACTED]" + quote
    return match.group()[:match.start("value") - match.start()] + replacement


def sanitize(value):
    """Preserve request/response structure except credentials, including in URLs.

    Apply at recording boundaries, not to the actual request or scoring input.
    No environment dump or HTTP authorization headers are ever recorded.
    """
    # Learn nested credentials before rendering any sibling echo of them.
    _register_secrets(value)
    return _sanitize(value)


def _register_secrets(value):
    if isinstance(value, dict):
        for key, item in value.items():
            if _SECRET_KEY.search(str(key)) and isinstance(item, str):
                register_secret(item)
            _register_secrets(item)
    elif isinstance(value, (list, tuple)):
        for item in value:
            _register_secrets(item)


def _sanitize(value):
    if isinstance(value, dict):
        return {str(k): "[REDACTED]" if _SECRET_KEY.search(str(k)) else _sanitize(v)
                for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_sanitize(v) for v in value]
    if not isinstance(value, str):
        return value
    value = _CREDENTIAL_HEADER.sub(_redact_header, value)
    for secret in _REDACTION_VALUES:
        value = value.replace(secret, "[REDACTED]")
    for key, secret in os.environ.items():
        if _SECRET_KEY.search(key) and secret:
            value = value.replace(secret, "[REDACTED]")
    value = re.sub(r"([A-Za-z][A-Za-z0-9+.-]*://)[^/\s@]+@", r"\1[REDACTED]@", value)
    value = re.sub(r"([?&](?:api[_-]?key|key|access_token|refresh_token|token|secret|password)=)[^&\s]+",
                   r"\1[REDACTED]", value, flags=re.I)
    value = re.sub(r'("(?:api[_-]?key|access_token|refresh_token|token|secret|password|authorization)"\s*:\s*)"(?:\\.|[^"\\])*"',
                   r'\1"[REDACTED]"', value, flags=re.I)
    value = re.sub(r"\bBearer\s+[^\s\"']+", "Bearer [REDACTED]", value, flags=re.I)
    return re.sub(r"\bsk-[A-Za-z0-9_-]+", "[REDACTED]", value)


def response_data(response):
    if hasattr(response, "model_dump"):
        return response.model_dump(mode="json")
    if isinstance(response, dict):
        return {k: response_data(v) for k, v in response.items()}
    if isinstance(response, list):
        return [response_data(v) for v in response]
    if isinstance(response, (str, int, float, bool)) or response is None:
        return response
    if hasattr(response, "__dict__"):
        return {k: response_data(v) for k, v in vars(response).items() if not k.startswith("_")}
    return None  # explicit missing evidence for unsupported client objects


def final_completion(records: list[dict]) -> dict:
    """Describe the last returned response, not an earlier retry's completion."""
    for index in range(len(records) - 1, -1, -1):
        response = records[index].get("response")
        if response is None:
            continue
        choices = response.get("choices") if isinstance(response, dict) else None
        choice = choices[0] if isinstance(choices, list) and choices else None
        reason = choice.get("finish_reason") if isinstance(choice, dict) else None
        reason = reason if isinstance(reason, str) else None
        return {"record_index": index, "finish_reason": reason,
                "complete": reason == "stop", "truncated": reason == "length",
                "finish_reason_available": reason is not None}
    return {"record_index": None, "finish_reason": None,
            "complete": False, "truncated": False, "finish_reason_available": False}


def checkout_identity(root: Path = REPO_ROOT) -> dict:
    def git(*args):
        try:
            result = subprocess.run(["git", "-C", str(root), *args],
                                    capture_output=True, text=True, check=False)
            return result.stdout.strip() if result.returncode == 0 else None
        except OSError:
            return None
    status = git("status", "--porcelain", "--untracked-files=all")
    diff = git("diff", "--binary", "HEAD")
    untracked = git("ls-files", "--others", "--exclude-standard")
    untracked_hashes = ({path: content_hash(root / path) for path in untracked.splitlines()}
                        if untracked is not None else None)
    return {"commit": git("rev-parse", "HEAD"), "branch": git("branch", "--show-current"),
            "version": git("describe", "--tags", "--always"),
            "dirty": bool(status) if status is not None else None,
            "dirty_status_sha256": json_hash(status) if status is not None else None,
            "tracked_diff_sha256": json_hash(diff) if diff is not None else None,
            "untracked_content_sha256": json_hash(untracked_hashes) if untracked_hashes is not None else None}


def execution_identity(harness: Path, binary: str | Path | None, dataset: dict, *, checkout=None) -> dict:
    """Snapshot before launch/model work; callers must retain, not recapture, it."""
    return {"schema_version": 2, "capture_phase": "before_execution",
            "captured_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "harness_path": str(harness.resolve()),
            "binary_path": str(Path(binary).resolve()) if binary is not None else None,
            "harness_sha256": content_hash(harness), "binary_sha256": content_hash(binary),
            "dataset": copy.deepcopy(dataset),
            "checkout": checkout_identity() if checkout is None else checkout}


_SERVER_ENV_KEYS = ("PATH", "LANG", "LC_ALL", "SYSTEMROOT", "SSL_CERT_FILE", "SSL_CERT_DIR",
                    "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "http_proxy", "https_proxy", "no_proxy")
EMBEDDING_ENV_KEYS = ("MEMVAULT_EMBEDDING_PROVIDER", "MEMVAULT_EMBEDDING_API_BASE",
                      "MEMVAULT_EMBEDDING_MODEL", "MEMVAULT_EMBEDDING_DIM",
                      "MEMVAULT_EMBEDDING_API_KEY", "MEMVAULT_EMBEDDING_CACHE_DIR")


def server_environment() -> dict:
    return {k: os.environ[k] for k in _SERVER_ENV_KEYS if k in os.environ}


def isolate_server_environment(run_dir: str, db_name: str, requested: dict) -> tuple[dict, Path, Path]:
    """Own every vault path and explicitly bypass the user's startup .env.

    Verified in memvault-mcp/src/main.rs (--env-file) and core storage/backend.rs:
    a Markdown backend ignores --db, so a temporary --db alone is NOT isolation.
    Only requested embedding options and basic transport settings may survive.
    """
    home = Path(run_dir).resolve()
    env_file, db = home / "empty.env", home / db_name
    env_file.write_bytes(b"")
    env = {k: v for k, v in requested.items() if k in _SERVER_ENV_KEYS or k in EMBEDDING_ENV_KEYS}
    env.update({"HOME": str(home), "TMPDIR": str(home), "TEMP": str(home), "TMP": str(home),
                "MEMVAULT_HOME": str(home), "MEMVAULT_ENV_FILE": str(env_file),
                "MEMVAULT_DB": str(db), "MEMVAULT_STORAGE_BACKEND": "sqlite"})
    return env, env_file, db
