#!/usr/bin/env bash
# Pre-seed ort-sys's prebuilt ONNX Runtime cache with curl.
#
# Why: ort-sys downloads a static libonnxruntime at build time with ureq and a
# 30-minute global timeout, and it does not retry. On networks where that
# connection stalls, `cargo build` looks hung at `Compiling ort-sys`. curl
# populates the exact cache directory ort-sys checks before downloading, so the
# build skips its downloader entirely.
#
# Usage:
#   scripts/fetch-ort-cache.sh                    # current host target
#   ORT_TARGET=aarch64-apple-darwin scripts/fetch-ort-cache.sh
#   ORT_CACHE_DIR=/tmp/ort scripts/fetch-ort-cache.sh
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
target=${ORT_TARGET:-$(rustc -vV | awk '/^host: /{print $2}')}
cargo_home=${CARGO_HOME:-$HOME/.cargo}
lock=$repo_root/Cargo.lock

version=$(awk '
  $0 == "name = \"ort-sys\"" { found = 1; next }
  found && /^version = / { gsub(/"/, "", $3); print $3; exit }
' "$lock")
if [ -z "$version" ]; then
  echo "error: ort-sys not found in $lock" >&2
  exit 1
fi

dist=$(find "$cargo_home/registry/src" -maxdepth 5 -type f \
  -path "*/ort-sys-$version/build/download/dist.tsv" -print -quit)
if [ -z "$dist" ]; then
  echo "error: ort-sys $version sources not found under $cargo_home/registry/src" >&2
  echo "run 'cargo fetch' once, then retry" >&2
  exit 1
fi

# ort-sys enables no extra backend features here, so the build script picks the
# "none" row when present and otherwise the first row for the target.
row=$(awk -F'\t' -v t="$target" '
  $1 != t { next }
  $2 == "none" { print; found = 1; exit }
  first == "" { first = $0 }
  END { if (!found && first != "") print first }
' "$dist")
if [ -z "$row" ]; then
  echo "error: no prebuilt ONNX Runtime distribution for target $target" >&2
  echo "supported targets are listed in $dist" >&2
  exit 1
fi

url=$(printf '%s\n' "$row" | cut -f3)
hash=$(printf '%s\n' "$row" | cut -f4)

case "$(uname -s)" in
  Darwin) default_cache="$HOME/Library/Caches/ort.pyke.io" ;;
  Linux) default_cache="${XDG_CACHE_HOME:-$HOME/.cache}/ort.pyke.io" ;;
  *) default_cache="" ;;
esac
cache_root=${ORT_CACHE_DIR:-$default_cache}
if [ -z "$cache_root" ]; then
  echo "error: set ORT_CACHE_DIR on this OS" >&2
  exit 1
fi

dest="$cache_root/dfbin/$target/$hash"
if [ -f "$dest/libonnxruntime.a" ]; then
  echo "already cached: $dest"
  exit 0
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "downloading $url"
curl -fL --retry 5 --retry-delay 2 --connect-timeout 20 --max-time 900 \
  -o "$tmp/ort.tar.lzma2" "$url"

if command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$tmp/ort.tar.lzma2" | awk '{print $1}')
else
  actual=$(sha256sum "$tmp/ort.tar.lzma2" | awk '{print $1}')
fi
if [ "$actual" != "$hash" ]; then
  echo "error: checksum mismatch (got $actual, expected $hash)" >&2
  exit 1
fi

mkdir -p "$tmp/out"
python3 - "$tmp/ort.tar.lzma2" "$tmp/out" <<'PY'
import lzma
import sys
import tarfile
from io import BytesIO

src, dest = sys.argv[1], sys.argv[2]
raw = lzma.decompress(
    open(src, "rb").read(),
    format=lzma.FORMAT_RAW,
    filters=[{"id": lzma.FILTER_LZMA2, "dict_size": 1 << 26}],
)
with tarfile.open(fileobj=BytesIO(raw)) as archive:
    try:
        archive.extractall(dest, filter="data")
    except TypeError:  # Python < 3.12
        archive.extractall(dest)
PY

# Only a fully extracted directory may land at `dest`: ort-sys treats any
# existing directory as a valid cache entry.
mkdir -p "$(dirname "$dest")"
rm -rf "$dest" "$dest.tmp"
mv "$tmp/out" "$dest.tmp"
mv "$dest.tmp" "$dest"
echo "cached ONNX Runtime for $target at $dest"
