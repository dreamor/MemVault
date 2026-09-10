#!/usr/bin/env bash
#
# MemVault official installer.
#
# Downloads the matching release archive from GitHub Releases, verifies its
# SHA-256 against the release's SHA256SUMS, extracts the binaries, and prints
# next steps. Works on Linux and macOS (x86_64 / arm64); Windows users should
# use scripts/install.ps1.
#
# Overrides:
#   MEMVAULT_REPO   GitHub repo (default: dreamor/memvault)
#   MEMVAULT_VERSION  release tag, e.g. v0.2.0 (default: latest)
#   MEMVAULT_PREFIX   install dir (default: $HOME/.memvault/bin)
#
set -euo pipefail

REPO="${MEMVAULT_REPO:-dreamor/memvault}"
VERSION="${MEMVAULT_VERSION:-latest}"
PREFIX="${MEMVAULT_PREFIX:-$HOME/.memvault/bin}"
BASE_URL="https://github.com/$REPO/releases"

log()  { printf '\033[1;36m[mv]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[mv] error:\033[0m %s\n' "$*" >&2; exit 1; }

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Linux)  os="linux" ;;
  Darwin) os="darwin" ;;
  *) fail "unsupported OS: $os (only linux/darwin)";;
esac

case "$arch" in
  x86_64|amd64)  rust_arch="x86_64" ;;
  aarch64|arm64) rust_arch="aarch64" ;;
  *) fail "unsupported architecture: $arch";;
esac

case "$os" in
  linux)
    target="$rust_arch-unknown-linux-gnu"
    ext="tar.gz"
    ;;
  darwin)
    if [ "$rust_arch" = "x86_64" ]; then
      fail "no prebuilt binaries for Intel macOS (fastembed/ONNX Runtime has no x86_64-apple-darwin artifacts). Install from source: git clone https://github.com/dreamor/memvault && cd memvault && cargo build --release"
    fi
    target="aarch64-apple-darwin"
    ext="tar.gz"
    ;;
esac

archive="memvault-$target.$ext"
if [ "$VERSION" = "latest" ]; then
  url="$BASE_URL/latest/download/$archive"
  sums_url="$BASE_URL/latest/download/SHA256SUMS"
else
  url="$BASE_URL/download/$VERSION/$archive"
  sums_url="$BASE_URL/download/$VERSION/SHA256SUMS"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"

log "downloading $REPO $VERSION ($target)"
curl -fL --proto '=https' --tlsv1.2 -o "$archive" "$url"

log "verifying SHA-256"
curl -fL --proto '=https' --tlsv1.2 -o SHA256SUMS "$sums_url"
expected="$(grep -F "$archive" SHA256SUMS | awk '{print $1}' | head -n1)"
[ -n "$expected" ] || fail "no SHA-256 entry for $archive in SHA256SUMS"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$archive" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$archive" | awk '{print $1}')"
fi
[ "$actual" = "$expected" ] || fail "checksum mismatch for $archive (expected $expected, got $actual)"

mkdir -p "$PREFIX"
tar -xzf "$archive"
install -m 0755 "memvault-$target/memvault-cli"   "$PREFIX/"
install -m 0755 "memvault-$target/memvault-mcp"   "$PREFIX/"
install -m 0755 "memvault-$target/memvault-proxy" "$PREFIX/"

log "installed to $PREFIX"
log "add to PATH: export PATH=\"\$HOME/.memvault/bin:\$PATH\""
log "usage: memvault-cli --help / memvault-mcp --help (binary names; the\`memvault\` wrapper name works when ~/.memvault/bin is in PATH)"
