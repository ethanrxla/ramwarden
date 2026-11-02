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

# Never run a release whose manifest or checksum cannot be verified.
command -v python3 >/dev/null || die "python3 is required to verify the release manifest."
curl -fsSL "$REPO/manifest.json" -o "$tmp/manifest.json" || die "release manifest unavailable; not installing."
want=$(python3 - "$tmp/manifest.json" "$file" <<'PYHASH'
import json, re, sys
m = json.load(open(sys.argv[1]))
matches = [a.get("sha256", "") for w in m.get("wardens", [m])
           for a in w.get("artifacts", [])
           if a.get("url", "").endswith("/" + sys.argv[2])]
if len(matches) != 1 or not re.fullmatch(r"[0-9a-fA-F]{64}", matches[0]):
    raise SystemExit("Missing, duplicate, or invalid release checksum; not installing.")
print(matches[0].lower())
PYHASH
) || die "could not verify manifest; not installing."
if command -v sha256sum >/dev/null; then got=$(sha256sum "$tmp/$file" | cut -d' ' -f1)
else got=$(shasum -a 256 "$tmp/$file" | cut -d' ' -f1); fi
[ "$got" = "$want" ] || die "checksum mismatch for $file; not installing."
say "checksum verified"

mkdir -p "$tmp/x"
tar xzf "$tmp/$file" -C "$tmp/x"
inst=$(find "$tmp/x" -maxdepth 2 -name install.sh | head -n 1)
[ -n "$inst" ] || die "the release has no install.sh."
(cd "$(dirname "$inst")" && bash ./install.sh)

say "installed. Open RamWarden, then click Sign in for Pro, or keep it local and free."
