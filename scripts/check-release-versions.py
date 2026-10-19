#!/usr/bin/env python3
"""Fail when the release tag and MemVault's shipped manifests disagree.

    scripts/check-release-versions.py v0.4.4   # release gate (runs in release.yml)
    scripts/check-release-versions.py         # no argument: manifests must agree
                                              # with Cargo.toml (runs in CI)

Every channel ships its version in its own file, and nothing used to compare
them, so drift was silent: `dashboard/package.json` sat at 0.4.0 while the
0.4.2/0.4.3 archives were already published, and v0.4.1 reached npm plus a
stray ghcr image without ever being tagged or published anywhere else. This
script is the single list of version-bearing files; `release.yml` gates every
tag push on it and CI runs the consistency half on every build.
"""

from __future__ import annotations

import json
import pathlib
import re
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent

# --- files whose top-level JSON "version" is the release version --------------
JSON_VERSION_FILES = [
    "plugin.json",
    "plugins/memvault/plugin.json",
    "plugins/memvault/.claude-plugin/plugin.json",
    "gemini-extension.json",
    ".qoder-plugin/plugin.json",
    "dsh-plugin/package.json",
    "pi-extension/package.json",
    "dashboard/package.json",
]

# --- version tables in the READMEs; the Obsidian plugin versions on its own --
README_TABLES = ["README.md", "README.zh-CN.md"]
README_SKIP_LINE = re.compile(r"obsidian", re.IGNORECASE)

# --- "verify your install" output, which prints the product version ----------
VERSION_OUTPUT_FILES = ["README.md", "README.zh-CN.md", "docs/INSTALL.md"]
VERSION_OUTPUT = re.compile(r"#\s*prints\s+memvault(?:-mcp)?\s+(\d+\.\d+\.\d+)|^#\s*memvault\s+(\d+\.\d+\.\d+)$", re.MULTILINE)

# --- the MCP registry listing points at the image tag release.yml pushes -----
MCP_REGISTRY = "integrations/mcp-registry/server.json"
MCP_REGISTRY_IMAGE = re.compile(r"ghcr\.io/dreamor/memvault:(v\d+\.\d+\.\d+)$")


class Mismatch(Exception):
    pass


def read_json(rel: str):
    return json.loads((ROOT / rel).read_text(encoding="utf-8"))


def collect() -> dict[str, str]:
    """Every version-bearing file, mapped to the version it currently carries."""
    found: dict[str, str] = {}
    for rel in JSON_VERSION_FILES:
        found[rel] = str(read_json(rel)["version"])

    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    found["Cargo.toml (workspace)"] = workspace["workspace"]["package"]["version"]

    for crate in ("memvault-cli", "memvault-mcp", "memvault-proxy"):
        rel = f"crates/{crate}/Cargo.toml"
        manifest = tomllib.loads((ROOT / rel).read_text(encoding="utf-8"))
        found[f"{rel} (memvault-core dep)"] = manifest["dependencies"]["memvault-core"]["version"]

    dsh_lock = read_json("dsh-plugin/package-lock.json")
    found["dsh-plugin/package-lock.json"] = str(dsh_lock["version"])
    found['dsh-plugin/package-lock.json (packages[""])'] = str(dsh_lock["packages"][""]["version"])

    hermes = (ROOT / "integrations/hermes/plugin.yaml").read_text(encoding="utf-8")
    match = re.search(r"^version:\s*(\S+)$", hermes, re.MULTILINE)
    if not match:
        raise Mismatch("integrations/hermes/plugin.yaml: no top-level `version:` line")
    found["integrations/hermes/plugin.yaml"] = match.group(1)

    registry = read_json(MCP_REGISTRY)
    found[MCP_REGISTRY] = str(registry["version"])
    identifier = str(registry["packages"][0]["identifier"])
    match = MCP_REGISTRY_IMAGE.search(identifier)
    if not match:
        raise Mismatch(
            f"{MCP_REGISTRY}: packages[0].identifier must be "
            f"ghcr.io/dreamor/memvault:v<version> (found {identifier!r})"
        )
    found[f"{MCP_REGISTRY} (image tag)"] = match.group(1).lstrip("v")

    for rel in README_TABLES:
        for number, line in enumerate((ROOT / rel).read_text(encoding="utf-8").splitlines(), 1):
            if line.startswith("|") and not README_SKIP_LINE.search(line):
                for version in re.findall(r"\b\d+\.\d+\.\d+\b", line):
                    found[f"{rel}:{number}"] = version

    for rel in VERSION_OUTPUT_FILES:
        for number, line in enumerate((ROOT / rel).read_text(encoding="utf-8").splitlines(), 1):
            match = VERSION_OUTPUT.search(line)
            if match:
                found[f"{rel}:{number} (--version output)"] = match.group(1) or match.group(2)

    return found


def main() -> int:
    target = sys.argv[1].removeprefix("v") if len(sys.argv) > 1 else None
    if target and not re.fullmatch(r"\d+\.\d+\.\d+", target):
        print(f"error: {sys.argv[1]!r} is not a v<major>.<minor>.<patch> tag", file=sys.stderr)
        return 2

    try:
        found = collect()
    except Mismatch as err:
        print(f"error: {err}", file=sys.stderr)
        return 1

    if target is None:
        # Consistency mode: every manifest must agree with the workspace version.
        target = found["Cargo.toml (workspace)"]

    width = max(len(name) for name in found)
    bad = []
    for name, version in sorted(found.items()):
        mark = "ok  " if version == target else "FAIL"
        if version != target:
            bad.append((name, version))
        print(f"  {mark} {name.ljust(width)}  {version}")

    print(f"\nexpected {target}: {len(found) - len(bad)}/{len(found)} files agree")
    if bad:
        print(f"\nversion drift — {len(bad)} file(s) do not carry {target}:")
        for name, version in bad:
            print(f"  {name}: {version}")
        print("\nBump them in the same commit as the tag (see docs/RELEASING.md).")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
