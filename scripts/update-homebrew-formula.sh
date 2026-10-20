#!/usr/bin/env bash
#
# Generates the Homebrew formula for MemVault from the published npm binary
# package, and prints it to stdout.
#
# Why npm and not the GitHub Release archive: the formula used to `url` the
# 48 MB GitHub Release tarball directly. On throttled / corporate networks
# github.com is shaped to a few KB/s, so `brew install memvault` spent tens of
# minutes on one download and looked hung. The identical bytes are published as
# the npm package `@dreamor/memvault-bin`, and registry.npmmirror.com (the npm
# China mirror) serves that tarball at multi-MB/s — so the formula points at the
# mirror, with registry.npmjs.org as a Homebrew `mirror` fallback (both serve
# the same bytes, so the one sha256 covers both).
#
# npm tarballs nest everything under a single `package/` directory. Homebrew's
# tarball strategy automatically enters a lone top-level directory, so the
# `bin.install` lines below need no `cd`.
#
# Usage:
#   ./scripts/update-homebrew-formula.sh v0.6.0           # sha256 fetched from the mirror
#   ./scripts/update-homebrew-formula.sh v0.6.0 <sha256>  # use a known sha256 (no network)
#   ./scripts/update-homebrew-formula.sh v0.6.0 > ../homebrew-tap/Formula/memvault.rb
#
# Called by .github/workflows/release-npm.yml, which owns the tap.
#
set -euo pipefail

VERSION="${1:?usage: update-homebrew-formula.sh <tag e.g. v0.6.0> [sha256]}"
GIVEN_SHA="${2:-}"
REPO="${MEMVAULT_REPO:-dreamor/MemVault}"
PKG="${MEMVAULT_NPM_PACKAGE:-@dreamor/memvault-bin}"
VERSION_NO_V="${VERSION#v}"

PRIMARY_URL="https://registry.npmmirror.com/$PKG/-/memvault-bin-$VERSION_NO_V.tgz"
FALLBACK_URL="https://registry.npmjs.org/$PKG/-/memvault-bin-$VERSION_NO_V.tgz"

if [ -n "$GIVEN_SHA" ]; then
  SHA="$GIVEN_SHA"
else
  attempts="${MEMVAULT_FORMULA_ATTEMPTS:-40}"
  delay="${MEMVAULT_FORMULA_DELAY:-15}"
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT

  echo "Fetching the npm tarball checksum from $PRIMARY_URL ..." >&2
  SHA=""
  for attempt in $(seq 1 "$attempts"); do
    if curl -fsSL --retry 3 --retry-delay 5 --proto '=https' --tlsv1.2 -o "$tmp/pkg.tgz" "$PRIMARY_URL"; then
      SHA="$(shasum -a 256 "$tmp/pkg.tgz" | awk '{print $1}')"
      break
    fi
    # npmmirror syncs from npmjs lazily, so a just-published version can 404 for
    # a minute or two. Poll instead of failing a release on mirror timing.
    echo "npmmirror does not serve $VERSION_NO_V yet (attempt $attempt/$attempts); retrying in ${delay}s" >&2
    sleep "$delay"
  done

  if [ -z "$SHA" ]; then
    echo "error: npmmirror never served $PRIMARY_URL after $attempts attempts." >&2
    echo "hint: confirm @dreamor/memvault-bin@$VERSION_NO_V is published, or pass the sha256 explicitly." >&2
    exit 1
  fi
fi

cat <<FORMULA
class Memvault < Formula
  desc "Shared memory layer for every AI agent you run (CLI + MCP server)"
  homepage "https://github.com/$REPO"
  # Shipped as the @dreamor/memvault-bin npm tarball and pulled from the npm
  # China mirror, which serves it at multi-MB/s where the same archive from
  # github.com crawls. npmjs.org serves byte-identical bytes, so it is a
  # checksum-compatible fallback.
  url "$PRIMARY_URL"
  mirror "$FALLBACK_URL"
  sha256 "$SHA"
  version "$VERSION_NO_V"
  license "MIT"

  # Intel Macs are not shipped as prebuilt binaries (no x86_64-apple-darwin
  # ONNX Runtime); Intel users install from source.
  #
  # Note: \`only_arm64\` is NOT valid Homebrew DSL and hard-fails formula
  # parsing (observed live: undefined local variable). \`depends_on arch:\`
  # is the supported way to express an Apple Silicon-only formula.
  depends_on arch: :arm64

  # The archive is an npm package, so every file sits under \`package/\`.
  # Homebrew's tarball strategy enters that lone top-level directory itself.
  def install
    bin.install "memvault-cli"
    bin.install "memvault-mcp"
    bin.install "memvault-proxy"
  end

  # \`brew services start memvault\` (or \`brew services restart memvault\`)
  # runs the REST backend that hosts the embedded Web Dashboard, and starts
  # it again at login (LaunchAgent on macOS / systemd user unit on Linux).
  # --auto-port keeps behavior sane if another MemVault instance is already
  # serving the preferred port: it adopts that instance instead of failing.
  service do
    # The Web Dashboard is embedded in the binary — no --serve-web needed.
    run [opt_bin/"memvault-mcp", "--transport", "http", "--port", "3777", "--auto-port"]
    keep_alive false
    log_path var/"log/memvault.log"
    error_log_path var/"log/memvault-error.log"
  end

  test do
    system "#{bin}/memvault-cli", "--version"
  end
end
FORMULA
