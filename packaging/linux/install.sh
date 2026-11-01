#!/bin/sh
# Install RamWarden for the current user from an unpacked release tarball.
# No root needed. Run from the tarball's top directory, which is what the
# launcher does for updates:
#
#   tar xzf ramwarden-<version>-linux-x86_64.tar.gz -C dir && (cd dir && sh install.sh)
#   sh install.sh uninstall      # remove binaries and units, keep config
#
# The .deb is the other way in (system-wide, with the page-out helper's
# capability). Use one or the other: both start a daemon on port 7823.
set -eu

BIN="$HOME/.local/bin"
UNITS="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
CONF="${XDG_CONFIG_HOME:-$HOME/.config}/ramwarden/config.toml"
HERE="$(cd "$(dirname "$0")" && pwd)"

if [ "${1:-}" = "uninstall" ]; then
    systemctl --user disable --now ramwarden-update.timer ramwarden.service 2>/dev/null || true
    rm -f "$UNITS/ramwarden.service" "$UNITS/ramwarden-update.service" "$UNITS/ramwarden-update.timer"
    for b in ramwarden ramwarden-daemon ramwarden-launcher; do rm -f "$BIN/$b"; done
    systemctl --user daemon-reload 2>/dev/null || true
    echo "RamWarden removed (config left in $CONF)"
    exit 0
fi

if [ -x /usr/bin/ramwarden-daemon ]; then
    echo "RamWarden is already installed from the .deb; update it with apt instead." >&2
    exit 1
fi

mkdir -p "$BIN" "$UNITS" "$(dirname "$CONF")"
# Write each binary beside its final name and rename over it, so a running
# daemon keeps its old inode and the swap is atomic.
for b in ramwarden-daemon ramwarden-launcher ramwarden; do
    if [ -f "$HERE/bin/$b" ]; then
        install -m 0755 "$HERE/bin/$b" "$BIN/.$b.new"
        mv -f "$BIN/.$b.new" "$BIN/$b"
    fi
done

# The packaged units name /usr/bin; point them at ~/.local/bin.
for u in ramwarden.service ramwarden-update.service ramwarden-update.timer; do
    sed "s|/usr/bin/|%h/.local/bin/|g" "$HERE/units/$u" > "$UNITS/$u"
done
[ -f "$CONF" ] || install -m 0644 "$HERE/config.toml" "$CONF"

if [ -f "$HERE/bin/ramwarden" ]; then
    APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
    mkdir -p "$APPS"
    sed "s|^Exec=ramwarden|Exec=$BIN/ramwarden|" "$HERE/ramwarden.desktop" > "$APPS/ramwarden.desktop"
fi

if systemctl --user daemon-reload 2>/dev/null; then
    systemctl --user enable ramwarden.service ramwarden-update.timer >/dev/null 2>&1 || true
    # restart, not start: an update must replace the running daemon.
    systemctl --user restart ramwarden.service || true
    systemctl --user start ramwarden-update.timer || true
    echo "RamWarden installed and running: http://127.0.0.1:7823"
else
    echo "RamWarden installed to $BIN. No systemd user session; start it with: $BIN/ramwarden-daemon"
fi
