#!/bin/sh
# RamWarden one-line installer for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/ethanrxla/ramwarden/main/web/install.sh | sh
#
# Downloads the latest release for this machine from GitHub, checks its
# SHA-256 against the release manifest, and runs the release's own installer
# as the current user (no root on macOS or for the per-user Linux install).
# The artifact names are the ones the release workflow publishes
# (docs/specs/updates.md). Nothing is sent anywhere except the downloads.
set -eu

REPO="https://github.com/ethanrxla/ramwarden/releases/latest/download"

say() { printf '%s\n' "ramwarden: $*"; }
die() { printf '%s\n' "ramwarden: $*" >&2; exit 1; }

os=$(uname -s)
arch=$(uname -m)
case "$os/$arch" in
  Darwin/arm64)          target="macos-aarch64" ;;
  Linux/x86_64|Linux/amd64) target="linux-x86_64" ;;
  Darwin/*) die "RamWarden for macOS needs Apple silicon (this Mac is $arch)." ;;
  *) die "no RamWarden build for $os $arch yet. See https://github.com/ethanrxla/ramwarden to build from source." ;;
esac

command -v curl >/dev/null || die "curl is required."
command -v tar  >/dev/null || die "tar is required."

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
file="ramwarden-$target.tar.gz"

say "downloading $file"
curl -fsSL "$REPO/$file" -o "$tmp/$file" \
  || die "could not download $file. If no release has been published yet, build from source: https://github.com/ethanrxla/ramwarden"

# The manifest lists each artifact's SHA-256. Check it when we can parse it.
if curl -fsSL "$REPO/manifest.json" -o "$tmp/manifest.json" 2>/dev/null && command -v python3 >/dev/null; then
  want=$(python3 - "$tmp/manifest.json" "$file" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
for w in m.get("wardens", [m]):
    for a in w.get("artifacts", []):
        if a.get("url", "").endswith("/" + sys.argv[2]):
            print(a.get("sha256", "")); raise SystemExit
PY
)
  if [ -n "$want" ]; then
    if command -v sha256sum >/dev/null; then got=$(sha256sum "$tmp/$file" | cut -d' ' -f1)
    else got=$(shasum -a 256 "$tmp/$file" | cut -d' ' -f1); fi
    [ "$got" = "$want" ] || die "checksum mismatch for $file; not installing."
    say "checksum verified"
  fi
fi

mkdir -p "$tmp/x"
tar xzf "$tmp/$file" -C "$tmp/x"
inst=$(find "$tmp/x" -maxdepth 2 -name install.sh | head -n 1)
[ -n "$inst" ] || die "the release has no install.sh."
(cd "$(dirname "$inst")" && sh ./install.sh)

say "installed. Open RamWarden, then click Sign in for Pro, or keep it local and free."
