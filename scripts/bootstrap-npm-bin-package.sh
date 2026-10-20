#!/usr/bin/env bash
#
# ONE-TIME bootstrap for the `@dreamor/memvault-bin` npm package.
#
# Why a human has to run this once: npm only lets you configure a *trusted
# publisher* for a package that already exists on the registry, and the first
# publish needs interactive 2FA (npm's browser flow), which CI cannot do. After
# this has run once, every release publishes with no token and no OTP —
# .github/workflows/release-npm.yml uses GitHub OIDC trusted publishing.
#
# Usage:
#   scripts/bootstrap-npm-bin-package.sh 0.6.0                # stage, publish, trust, verify
#   scripts/bootstrap-npm-bin-package.sh 0.6.0 --dry-run      # stage + validate, publish nothing
#   scripts/bootstrap-npm-bin-package.sh 0.6.0 --bin-dir DIR  # take the binaries from DIR
#
# The binaries default to the Homebrew Cellar for that version, which holds the
# exact bytes of the release archive — that avoids re-downloading 48 MB from the
# (possibly throttled) GitHub CDN just to bootstrap.
#
# Env knobs: MEMVAULT_NPM_PACKAGE, MEMVAULT_REPO, MEMVAULT_NPM_WORKFLOW.
#
set -euo pipefail

VERSION=""
BIN_DIR=""
DRY_RUN=0

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1; shift ;;
    --bin-dir) BIN_DIR="${2:?--bin-dir needs a path}"; shift 2 ;;
    -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) echo "unknown flag: $1" >&2; exit 2 ;;
    *) VERSION="${1#v}"; shift ;;
  esac
done

[ -n "$VERSION" ] || { echo "usage: bootstrap-npm-bin-package.sh <version> [--dry-run] [--bin-dir DIR]" >&2; exit 2; }

REPO="${MEMVAULT_REPO:-dreamor/MemVault}"
PKG="${MEMVAULT_NPM_PACKAGE:-@dreamor/memvault-bin}"
WORKFLOW="${MEMVAULT_NPM_WORKFLOW:-release-npm.yml}"
REGISTRY="https://registry.npmjs.org"
ROOT="$(git rev-parse --show-toplevel)"
STAGE="${TMPDIR:-/tmp}/memvault-npm-bin-$VERSION"

say() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }

# --- 1. collect the binaries ------------------------------------------------
if [ -z "$BIN_DIR" ]; then
  if brew_prefix="$(brew --prefix memvault 2>/dev/null)" && [ -n "$brew_prefix" ]; then
    BIN_DIR="$brew_prefix/bin"
  fi
fi
[ -n "$BIN_DIR" ] && [ -d "$BIN_DIR" ] || {
  echo "error: could not find the binaries. Pass --bin-dir <dir> containing" >&2
  echo "       memvault-cli, memvault-mcp and memvault-proxy (e.g. the" >&2
  echo "       unpacked memvault-aarch64-apple-darwin release archive)." >&2
  exit 1
}

say "Staging $PKG@$VERSION from $BIN_DIR"
rm -rf "$STAGE"
mkdir -p "$STAGE"
for bin in memvault-cli memvault-mcp memvault-proxy; do
  [ -f "$BIN_DIR/$bin" ] || { echo "error: $BIN_DIR/$bin is missing" >&2; exit 1; }
  install -m 0755 "$BIN_DIR/$bin" "$STAGE/$bin"
done
cp "$ROOT/README.md" "$ROOT/LICENSE" "$STAGE/"

reported="$("$STAGE/memvault-cli" --version 2>/dev/null || true)"
case "$reported" in
  *"$VERSION"*) echo "binaries report: $reported" ;;
  *) echo "error: $BIN_DIR/memvault-cli reports '$reported', expected version $VERSION" >&2
     echo "       pass --bin-dir for the right build." >&2; exit 1 ;;
esac

# `files` is load-bearing: with a .gitignore present and no .npmignore, npm would
# fall back to the .gitignore rules and pack a tarball with no binaries in it.
# `publishConfig.access` is required because scoped packages are private by
# default. Keep this block in sync with .github/workflows/release-npm.yml.
cat > "$STAGE/package.json" <<JSON
{
  "name": "$PKG",
  "version": "$VERSION",
  "description": "MemVault prebuilt binaries for Apple Silicon macOS. Transport for the Homebrew tap (dreamor/tap); not a JavaScript library.",
  "homepage": "https://github.com/$REPO",
  "repository": { "type": "git", "url": "git+https://github.com/$REPO.git" },
  "license": "MIT",
  "os": ["darwin"],
  "cpu": ["arm64"],
  "files": ["memvault-cli", "memvault-mcp", "memvault-proxy", "README.md", "LICENSE"],
  "publishConfig": { "access": "public", "registry": "$REGISTRY" }
}
JSON

say "Package contents"
# Print the packed manifest, including the file mode: the executables must stay
# 0755 through the tar round trip, or Homebrew installs them non-executable.
( cd "$STAGE" && npm pack --dry-run --json 2>/dev/null | node -e '
let raw = "";
process.stdin.on("data", (c) => { raw += c; }).on("end", () => {
  const [d] = JSON.parse(raw);
  const mb = (n) => (n / 1048576).toFixed(1);
  console.log(`  ${d.filename}: ${d.entryCount} files, ${mb(d.size)} MB packed`);
  for (const f of d.files) {
    const mode = (f.mode & 0o777).toString(8).padStart(4, "0");
    console.log(`    ${f.path.padEnd(18)} ${mb(f.size).padStart(8)} MB  mode ${mode}`);
  }
});' )

if [ "$DRY_RUN" = "1" ]; then
  say "Dry run: validating the publish without sending anything"
  ( cd "$STAGE" && npm publish --dry-run --access public --registry="$REGISTRY" )
  say "Dry run complete. Re-run without --dry-run to publish."
  exit 0
fi

# --- 2. publish (interactive 2FA) -------------------------------------------
if curl -fsS --max-time 30 -o /dev/null "$REGISTRY/$(python3 -c 'import urllib.parse,sys;print(urllib.parse.quote(sys.argv[1],safe=""))' "$PKG")/$VERSION" 2>/dev/null; then
  say "$PKG@$VERSION is already on npmjs.org — skipping the publish"
else
  say "Publishing $PKG@$VERSION to npmjs.org"
  echo "npm will ask for a one-time password (or open a browser URL)."
  ( cd "$STAGE" && npm publish --access public --registry="$REGISTRY" )
fi

# --- 3. trusted publisher ---------------------------------------------------
say "Configuring the GitHub trusted publisher"
if npm trust list "$PKG" --registry "$REGISTRY" 2>/dev/null | grep -qi "github"; then
  echo "a GitHub trusted publisher is already configured:"
  npm trust list "$PKG" --registry "$REGISTRY" || true
else
  # Workflow filename matters: npm matches the OIDC `workflow_ref` claim, which
  # is the *calling* workflow. Keep the publish job in release-npm.yml — moving
  # it into release.yml would silently break publishing.
  npm trust github "$PKG" \
    --repo "$REPO" \
    --file "$WORKFLOW" \
    --allow-publish \
    --yes \
    --registry "$REGISTRY"
fi

# --- 4. wait for the mirror and take the checksum from its bytes ------------
# Delegate to the formula generator: it owns the npmmirror "lazy mirror" wait
# (request the metadata, kick the sync task, poll the *version* endpoint) and
# returns the sha256 of the bytes the mirror actually serves. Waiting on the
# tarball URL instead is what made the first bootstrap poll 404s for ten minutes
# and then give up on a perfectly good publish.
say "Waiting for registry.npmmirror.com to mirror $PKG@$VERSION"
formula="$(bash "$ROOT/scripts/update-homebrew-formula.sh" "v$VERSION")"
mirror_sha="$(printf '%s\n' "$formula" | sed -n 's/^  sha256 "\([0-9a-f]\{64\}\)"$/\1/p')"
[ -n "$mirror_sha" ] || { echo "error: could not read a sha256 out of the generated formula" >&2; exit 1; }

npmjs_sha="$(curl -fsSL --proto '=https' "$REGISTRY/$PKG/-/memvault-bin-$VERSION.tgz" | shasum -a 256 | awk '{print $1}')"
echo "  npmjs.org  sha256 $npmjs_sha"
echo "  npmmirror  sha256 $mirror_sha"
[ "$npmjs_sha" = "$mirror_sha" ] || { echo "error: the mirror serves different bytes" >&2; exit 1; }

say "Bootstrap complete"
cat <<DONE
$PKG@$VERSION is live on npmjs.org and mirrored by npmmirror (identical bytes).

Next — point the Homebrew tap at it (the release workflow does this
automatically from now on):

  ./scripts/update-homebrew-formula.sh v$VERSION $mirror_sha > ../homebrew-tap/Formula/memvault.rb
  cd ../homebrew-tap && git add Formula/memvault.rb \\
    && git commit -m "memvault $VERSION" && git push

Then confirm the whole chain with:

  python3 scripts/verify-release-channels.py v$VERSION --only npm-bin --only homebrew
DONE
