#!/usr/bin/env bash
# Run the Linux test suite from a Windows checkout inside WSL.
# The target dir lives in the WSL home: building on /mnt/c is several times slower.
#
# Known environmental failure under WSL2: madvise's "syscall exists" test, because
# the WSL kernel does not expose process_madvise. It passes on a real Linux boot.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/rw-target}"
cargo test --workspace --no-fail-fast "$@" 2>&1 \
  | grep -E '^(test result|error:|error\[|failures:|    [a-z_]+::)|FAILED|panicked' || true
