#!/usr/bin/env bash
#
# Generates a Homebrew formula for MemVault from a published GitHub release.
#
# Homebrew distributes via a dedicated tap repository (e.g.
# github.com/dreamor/homebrew-memvault). This script downloads the two macOS
# archives from GitHub Releases, computes their SHA-256, and prints a complete
# formula to stdout. Linux users install via scripts/install.sh or `cargo install`.
#
# Usage:
#   ./scripts/update-homebrew-formula.sh v0.2.0            # write to tap:
#   ./scripts/update-homebrew-formula.sh v0.2.0 > ../homebrew-memvault/Formula/memvault.rb
#
set -euo pipefail

VERSION="${1:?usage: update-homebrew-formula.sh <tag e.g. v0.2.0>}"
REPO="${MEMVAULT_REPO:-dreamor/memvault}"
BASE="https://github.com/$REPO/releases/download/$VERSION"
VERSION_NO_V="${VERSION#v}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

sha_for() {
  local target="$1"
  curl -fL --proto '=https' --tlsv1.2 -o "$tmp/$target.tar.gz" "$BASE/memvault-$target.tar.gz"
  shasum -a 256 "$tmp/$target.tar.gz" | awk '{print $1}'
}

echo "Fetching $VERSION checksum..." >&2
ARM64_SHA="$(sha_for aarch64-apple-darwin)"

cat <<FORMULA
class Memvault < Formula
  desc "Shared memory layer for every AI agent you run (CLI + MCP server)"
  homepage "https://github.com/$REPO"
  url "https://github.com/$REPO/releases/download/$VERSION/memvault-aarch64-apple-darwin.tar.gz"
  sha256 "$ARM64_SHA"
  version "$VERSION_NO_V"
  license "MIT"

  on_macos do
    # Intel Macs are not shipped as prebuilt binaries (no x86_64-apple-darwin
    # ONNX Runtime); Intel users install from source.
    only_arm64
  end

  def install
    bin.install "memvault-cli"
    bin.install "memvault-mcp"
    bin.install "memvault-proxy"
  end

  test do
    system "#{bin}/memvault-cli", "--version"
  end
end
FORMULA
