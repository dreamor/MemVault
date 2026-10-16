#!/usr/bin/env python3
"""Assert that every release-locked channel publicly serves the released version.

    scripts/verify-release-channels.py v0.5.0   # release.yml -> verify-release job
    scripts/verify-release-channels.py          # no argument: use the latest GitHub Release
    scripts/verify-release-channels.py --skip dockerhub --skip homebrew

`release.yml` only *publishes*: it has never checked that the bytes actually
landed. That is how a bare `ghcr` tag could be deleted out from under the MCP
Registry listing without anything going red, and how a channel could silently
skip its publish (an unset secret used to no-op green) while the run still said
"success". This script turns "we pushed" into "we verified it is live".

It checks, over the public network, that all of these agree on one version:

  github-release   the `v<version>` GitHub Release is published (not a draft)
                   and carries the 4 platform archives + SHA256SUMS
  crates-io        all four workspace crates are on crates.io at <version>
  homebrew         dreamor/homebrew-tap's Formula/memvault.rb pins <version>
  ghcr             ghcr.io/dreamor/memvault:<v> resolves
  dockerhub        docker.io/dreamor/memvault:<v> resolves
  mcp-registry     io.github.dreamor/memvault <version> is `active` AND the
                   ghcr image tag it advertises still resolves (a listing that
                   points at a deleted image is exactly the 0.3.0 bug)

Deliberately NOT checked: the npm `@dreamor/dsh-memvault` package and the
Obsidian plugin. Both release on their own schedule, so a version difference
there is not drift (see docs/DISTRIBUTION.md).

Exit code is 0 only when every requested channel passes. Transient propagation
lag (a freshly published crates.io version, a just-pushed image) is absorbed by
retrying each channel a few times before declaring failure.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

OWNER = "dreamor"
REPO = "MemVault"
GA_REPO = f"{OWNER}/{REPO}"

CRATES = ["memvault-core", "memvault-cli", "memvault-mcp", "memvault-proxy"]

# Lowercase: ghcr requires it and Docker Hub is case-insensitive-with-lower.
IMAGE_PATH = "dreamor/memvault"
GHCR_IMAGE = f"ghcr.io/{IMAGE_PATH}"
DOCKERHUB_IMAGE = IMAGE_PATH
HOMEBREW_TAP = f"{OWNER}/homebrew-tap"
HOMEBREW_FORMULA_PATH = "Formula/memvault.rb"
MCP_SERVER_NAME = "io.github.dreamor/memvault"

USER_AGENT = f"memvault-release-verifier (+https://github.com/{GA_REPO})"

# The archives release.yml's build-binaries matrix must have uploaded, plus the
# cumulative checksum file the one-line installers verify against.
RELEASE_ASSETS = [
    "memvault-x86_64-unknown-linux-gnu.tar.gz",
    "memvault-aarch64-unknown-linux-gnu.tar.gz",
    "memvault-aarch64-apple-darwin.tar.gz",
    "memvault-x86_64-pc-windows-msvc.zip",
    "SHA256SUMS",
]

OCI_ACCEPT = ", ".join(
    [
        "application/vnd.oci.image.index.v1+json",
        "application/vnd.docker.distribution.manifest.list.v2+json",
        "application/vnd.oci.image.manifest.v1+json",
        "application/vnd.docker.distribution.manifest.v2+json",
    ]
)


class CheckFailed(Exception):
    """A channel does not serve the expected version."""


def http(url: str, *, method: str = "GET", headers: dict[str, str] | None = None,
         timeout: int = 30) -> tuple[int, bytes, dict[str, str]]:
    """Return (status, body, headers); HTTP error statuses come back, not raised."""
    request_headers = {"User-Agent": USER_AGENT, "Accept": "application/json"}
    request_headers.update(headers or {})
    req = urllib.request.Request(url, method=method, headers=request_headers)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.status, resp.read(), dict(resp.headers)
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read(), dict(exc.headers or {})
    except urllib.error.URLError as exc:  # DNS/TLS/timeout
        raise CheckFailed(f"{url} unreachable: {exc.reason}") from exc


def github_headers() -> dict[str, str]:
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    return {"Authorization": f"Bearer {token}"} if token else {}


def wait_for(desc: str, fn, attempts: int, delay: int):
    """Run fn(), retrying CheckFailed to absorb propagation lag."""
    last: CheckFailed | None = None
    for attempt in range(1, attempts + 1):
        try:
            return fn()
        except CheckFailed as exc:
            last = exc
            if attempt < attempts:
                print(f"    ~ {desc}: {exc} — retry {attempt}/{attempts} in {delay}s")
                time.sleep(delay)
    assert last is not None
    raise last


# --------------------------------------------------------------------------- #
# one function per channel; each returns a short "what we saw" string
# --------------------------------------------------------------------------- #
def check_github_release(version: str) -> str:
    tag = f"v{version}"
    status, body, _ = http(
        f"https://api.github.com/repos/{GA_REPO}/releases/tags/{tag}",
        headers=github_headers(),
    )
    if status == 404:
        raise CheckFailed(f"no GitHub Release for {tag}")
    if status != 200:
        raise CheckFailed(f"GitHub API returned HTTP {status} for {tag}")
    release = json.loads(body)
    if release.get("draft"):
        raise CheckFailed(f"release {tag} is still a draft")
    names = {asset["name"] for asset in release.get("assets", [])}
    missing = [name for name in RELEASE_ASSETS if name not in names]
    if missing:
        raise CheckFailed(f"release {tag} is missing: {', '.join(missing)}")
    return f"{tag} published, {len(names)} assets, archives + SHA256SUMS present"


def check_crates_io(version: str) -> str:
    for crate in CRATES:
        status, _, _ = http(f"https://crates.io/api/v1/crates/{crate}/{version}")
        if status == 404:
            raise CheckFailed(f"crates.io has no {crate} {version}")
        if status != 200:
            raise CheckFailed(f"crates.io API HTTP {status} for {crate} {version}")
    return f"{', '.join(CRATES)} all at {version}"


def check_homebrew(version: str) -> str:
    url = f"https://raw.githubusercontent.com/{HOMEBREW_TAP}/HEAD/{HOMEBREW_FORMULA_PATH}"
    status, body, _ = http(url)
    if status != 200:
        raise CheckFailed(f"tap formula HTTP {status} ({url})")
    match = re.search(r'^\s*version\s+"([^"]+)"', body.decode("utf-8", "replace"), re.M)
    if not match:
        raise CheckFailed("tap formula has no `version \"...\"` line")
    if match.group(1) != version:
        raise CheckFailed(f"tap formula pins {match.group(1)}, expected {version}")
    return f"{HOMEBREW_TAP} {HOMEBREW_FORMULA_PATH} pins {version}"


def _ghcr_pull_token() -> str:
    status, body, _ = http(
        f"https://ghcr.io/token?scope=repository:{IMAGE_PATH}:pull&service=ghcr.io"
    )
    if status != 200:
        raise CheckFailed(f"ghcr token endpoint HTTP {status}")
    return json.loads(body)["token"]


def ghcr_manifest_status(tag: str) -> int:
    token = _ghcr_pull_token()
    status, _, _ = http(
        f"https://ghcr.io/v2/{IMAGE_PATH}/manifests/{tag}",
        method="HEAD",
        headers={"Authorization": f"Bearer {token}", "Accept": OCI_ACCEPT},
    )
    return status


def check_ghcr(version: str) -> str:
    tag = f"v{version}"
    status = ghcr_manifest_status(tag)
    if status != 200:
        raise CheckFailed(f"{GHCR_IMAGE}:{tag} → HTTP {status}")
    return f"{GHCR_IMAGE}:{tag} resolves"


def check_dockerhub(version: str) -> str:
    tag = f"v{version}"
    status, _, _ = http(
        f"https://hub.docker.com/v2/repositories/{DOCKERHUB_IMAGE}/tags/{tag}/"
    )
    if status == 404:
        raise CheckFailed(f"docker.io/{DOCKERHUB_IMAGE}:{tag} not found")
    if status != 200:
        raise CheckFailed(f"Docker Hub API HTTP {status} for {tag}")
    return f"docker.io/{DOCKERHUB_IMAGE}:{tag} resolves"


def check_mcp_registry(version: str) -> str:
    encoded = urllib.parse.quote(MCP_SERVER_NAME, safe="")
    # include_deleted=true so a retired listing reports "status is 'deleted'"
    # instead of impersonating a version that was never published.
    status, body, _ = http(
        f"https://registry.modelcontextprotocol.io/v0/servers/{encoded}/versions/{version}"
        "?include_deleted=true"
    )
    if status == 404:
        raise CheckFailed(f"MCP registry has no {MCP_SERVER_NAME} {version}")
    if status != 200:
        raise CheckFailed(f"MCP registry HTTP {status} for {version}")
    listing = json.loads(body)
    official = listing.get("_meta", {}).get(
        "io.modelcontextprotocol.registry/official", {}
    )
    state = official.get("status")
    if state != "active":
        raise CheckFailed(f"{MCP_SERVER_NAME} {version} status is {state!r}, expected 'active'")
    identifier = listing["server"]["packages"][0]["identifier"]
    if identifier.startswith("ghcr.io/"):
        tag = identifier.rsplit(":", 1)[-1]
        image_status = ghcr_manifest_status(tag)
        if image_status != 200:
            raise CheckFailed(
                f"listing {version} advertises {identifier}, which is missing "
                f"from ghcr (HTTP {image_status}) — dangling listing"
            )
        return f"{MCP_SERVER_NAME} {version} is active → {identifier} (resolves)"
    return f"{MCP_SERVER_NAME} {version} is active → {identifier}"


CHANNELS: dict[str, callable] = {
    "github-release": check_github_release,
    "crates-io": check_crates_io,
    "homebrew": check_homebrew,
    "ghcr": check_ghcr,
    "dockerhub": check_dockerhub,
    "mcp-registry": check_mcp_registry,
}
# Order matters only for readability of the report.
CHANNEL_ORDER = ["github-release", "crates-io", "homebrew", "ghcr", "dockerhub", "mcp-registry"]


def latest_release_version() -> str:
    status, body, _ = http(
        f"https://api.github.com/repos/{GA_REPO}/releases/latest", headers=github_headers()
    )
    if status != 200:
        raise SystemExit(f"cannot resolve the latest release (GitHub API HTTP {status})")
    return json.loads(body)["tag_name"].lstrip("v")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("version", nargs="?", help="release version, e.g. v0.5.0 (default: the latest GitHub Release)")
    parser.add_argument("--skip", action="append", default=[], choices=CHANNEL_ORDER, help="channel to skip (opted-out publish); repeatable")
    parser.add_argument("--only", action="append", default=[], choices=CHANNEL_ORDER, help="check only this channel; repeatable")
    parser.add_argument("--attempts", type=int, default=5, help="attempts per channel (default 5)")
    parser.add_argument("--delay", type=int, default=20, help="seconds between attempts (default 20)")
    parser.add_argument("--json", action="store_true", help="emit a machine-readable result instead of the report")
    args = parser.parse_args()

    version = (args.version or latest_release_version()).lstrip("v")
    selected = list(args.only) if args.only else list(CHANNEL_ORDER)
    selected = [c for c in selected if c not in args.skip]

    if not selected:
        print("nothing to verify (every channel was skipped)", file=sys.stderr)
        return 0

    print(f"Verifying {version} across {len(selected)} channel(s): {', '.join(selected)}\n")

    results = []
    for channel in CHANNEL_ORDER:
        if channel not in selected:
            results.append({"channel": channel, "ok": True, "skipped": True, "detail": "skipped"})
            continue
        try:
            detail = wait_for(channel, lambda c=channel: CHANNELS[c](version), args.attempts, args.delay)
            print(f"  ok    {channel:<15} {detail}")
            results.append({"channel": channel, "ok": True, "skipped": False, "detail": detail})
        except CheckFailed as exc:
            print(f"  FAIL  {channel:<15} {exc}")
            results.append({"channel": channel, "ok": False, "skipped": False, "detail": str(exc)})

    failed = [r for r in results if not r["ok"]]
    if args.json:
        print(json.dumps({"version": version, "ok": not failed, "channels": results}, indent=2))
        return 1 if failed else 0

    print()
    if failed:
        print(f"DRIFT: {len(failed)} of {len(selected)} channel(s) do not serve {version}:")
        for item in failed:
            print(f"  - {item['channel']}: {item['detail']}")
        return 1
    print(f"All {len(selected)} channel(s) serve {version}.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
