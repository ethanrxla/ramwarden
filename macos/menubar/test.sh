#!/usr/bin/env bash
# Compile the real app without replacing the user's installed bundle, then
# exercise the pane model without starting NSApplication or contacting a daemon.
set -euo pipefail
cd "$(dirname "$0")"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
swiftc -O -o "$TMP/RamWarden" main.swift Companion.swift Wardens.swift
sed '/^let lockPath = /,$d' main.swift > "$TMP/App.swift"
cp tests.swift "$TMP/main.swift"
swiftc -o "$TMP/tests" "$TMP/App.swift" Companion.swift Wardens.swift "$TMP/main.swift"
"$TMP/tests"
