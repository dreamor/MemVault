#!/usr/bin/env python3
"""Real isolated H7 REST wiring smoke; SDK clients are stubs, not live models."""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import sys
import tempfile
from types import ModuleType
from unittest.mock import MagicMock, patch

# Import only the harness; test fixtures would also import unrelated BEAM config.
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
_fake_sdk = ModuleType("openai")
_fake_sdk.OpenAI = MagicMock(side_effect=AssertionError("real REST smoke must not initialize model clients"))
with patch.dict(sys.modules, {"openai": _fake_sdk}):
    import verify_h7 as h7


def check(condition, message):
    if not condition:
        raise RuntimeError(message)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args(argv)
    evidence = {}
    server = None
    with tempfile.TemporaryDirectory(prefix="h7-smoke-user-") as directory:
        home = Path(directory)
        marker = home / "do-not-touch"
        marker.write_bytes(b"unrelated vault marker")
        env_file = home / "user.env"
        env_file.write_text("MEMVAULT_STORAGE_BACKEND=markdown\n", encoding="utf-8")
        polluted = {"MEMVAULT_HOME": str(home), "MEMVAULT_STORAGE_BACKEND": "markdown",
                    "MEMVAULT_ENV_FILE": str(env_file), "MEMVAULT_EMBEDDING_PROVIDER": "openai",
                    "MEMVAULT_EMBEDDING_API_BASE": "http://127.0.0.1:1/do-not-use",
                    "MEMVAULT_DB": str(home / "user.db")}
        try:
            with patch.dict(os.environ, polluted):
                server = h7.MemVaultServer(args.binary.resolve(), evidence=evidence)
                check(evidence["startup"]["status"] == "ready", "isolated server did not become ready")
                check(Path(evidence["startup"]["db"]).parent != home, "server selected the unrelated vault")
                check(Path(evidence["startup"]["env_file"]).read_bytes() == b"", "startup env file is not empty")
                ids = [server.create_skill(sc) for sc in h7.SCENARIOS]
                check(len(set(ids)) == len(h7.SCENARIOS), "skill seeds did not get distinct identities")
                for scenario, expected_id in zip(h7.SCENARIOS, ids):
                    records = []
                    formatted = server.session_formatted(scenario["task"], trace=records)
                    check(h7.matched_skill_status(expected_id, formatted, records)["valid"],
                          "the real isolated server did not explicitly match the seeded skill")
                server.create_decoys("project:h7lab", 10)
                records = []
                formatted = server.session_formatted(h7.UNRELATED_CONTEXTS[0], project="h7lab", trace=records)
                check(h7.session_status(formatted, records)["valid"], "unrelated session is not valid")
                check("[SKILL:" not in formatted, "unrelated trigger context injected a skill")
                check(marker.read_bytes() == b"unrelated vault marker", "unrelated user data changed")
                check(not (home / "user.db").exists(), "server wrote the unrelated database")
                check(set(p.name for p in home.iterdir()) == {"do-not-touch", "user.env"},
                      "server created files in the unrelated vault")
                _fake_sdk.OpenAI.assert_not_called()
                for client in (h7.client, h7.judge_client):
                    if client is not None:
                        client.chat.completions.create.assert_not_called()
        finally:
            if server is not None:
                server.stop()
        cleanup = evidence.get("cleanup", {})
        check(cleanup.get("process_reaped") is True, "server process was not reaped")
        check(cleanup.get("tempdir_removed") is True, "owned temporary vault was not removed")
        check(not cleanup.get("errors"), "server cleanup recorded errors")
    print("H7 real REST isolation, skill injection and cleanup passed; no model calls")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
