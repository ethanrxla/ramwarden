#!/usr/bin/env bash
# Install only bundled files, preserving user state and rolling back failed updates.
set -euo pipefail
PAYLOAD="$(cd "$(dirname "$0")/payload" && pwd)"
BASE="$HOME"; TEST=0; APP_MODE=0; DEFINITIONS=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    --app)
      INSTALLED_APP="${2:?app required}"
      case "$INSTALLED_APP" in /Applications/RamWarden.app|"$HOME/Applications/RamWarden.app") ;; *) echo 'Move RamWarden to Applications first.'; exit 2 ;; esac
      APP_MODE=1; shift 2 ;;
    --test-root)
      BASE="${2:?temporary root required}"
      case "$BASE" in /tmp/*|/private/tmp/*|/var/folders/*) ;; *) echo 'Test root must be temporary.'; exit 2 ;; esac
      TEST=1; shift 2 ;;
    --definitions) DEFINITIONS="${2:?definitions required}"; shift 2 ;;
    --verify)
      for ITEM in ramwarden-daemon ramwarden-usage-sync ramwarden ramwarden-launcher config.toml extension; do test -e "$PAYLOAD/$ITEM"; done
      echo 'Bundled installation payload verified.'; exit 0 ;;
    *) echo 'Unknown installer argument.'; exit 2 ;;
  esac
done
[ "$(id -u)" -ne 0 ] || { echo 'Open RamWarden as your normal Mac user.'; exit 1; }
BIN_DIR="$BASE/.local/bin"; CONFIG_DIR="$BASE/.config/ramwarden"; AGENTS="$BASE/Library/LaunchAgents"
SUPPORT="$BASE/Library/Application Support/RamWarden"
BACKUP="$SUPPORT/Backups/$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$BIN_DIR" "$CONFIG_DIR" "$AGENTS" "$SUPPORT" "$BASE/Applications" "$BASE/Library/Logs" "$BACKUP/LaunchAgents"
chmod 700 "$BACKUP" "$BACKUP/LaunchAgents"
STAGE="$(mktemp -d "$BASE/Applications/.warden-install.XXXXXX")"
DOMAIN="gui/$(id -u)"; CHANGED=0; SUCCESS=0
LABELS='com.ramwarden.daemon com.ramwarden.menubar com.ramwarden.usage'
BINS='ramwarden-daemon ramwarden-usage-sync ramwarden ramwarden-launcher'
stop_job() {
  [ "$TEST" -eq 0 ] || return 0
  if [ "$APP_MODE" -eq 1 ] && [ "$1" = com.ramwarden.menubar ]; then return 0; fi
  launchctl bootout "$DOMAIN/$1" 2>/dev/null || true
  for _ in $(seq 1 50); do launchctl print "$DOMAIN/$1" >/dev/null 2>&1 || return 0; sleep .2; done
  echo "Timed out stopping $1"; return 1
}
cleanup() {
  CODE=$?
  trap - EXIT
  if [ "$SUCCESS" -eq 0 ] && [ "$CHANGED" -eq 1 ]; then
    echo 'Installation stopped; restoring previous components.'
    for LABEL in $LABELS; do stop_job "$LABEL" || true; done
    for ITEM in $BINS; do
      if [ -f "$BACKUP/$ITEM" ]; then cp -p "$BACKUP/$ITEM" "$BIN_DIR/$ITEM"; else rm -f "$BIN_DIR/$ITEM"; fi
    done
    for LABEL in $LABELS; do
      if [ -f "$BACKUP/LaunchAgents/$LABEL.plist" ]; then cp -p "$BACKUP/LaunchAgents/$LABEL.plist" "$AGENTS/$LABEL.plist"; else rm -f "$AGENTS/$LABEL.plist"; fi
    done
    if [ "$APP_MODE" -eq 0 ]; then
      rm -rf "$BASE/Applications/RamWarden.app"
      [ ! -d "$BACKUP/RamWarden.app" ] || ditto "$BACKUP/RamWarden.app" "$BASE/Applications/RamWarden.app"
    fi
    rm -rf "$SUPPORT/extension"
    [ ! -d "$BACKUP/extension" ] || ditto "$BACKUP/extension" "$SUPPORT/extension"
    if [ "$TEST" -eq 0 ]; then
      for LABEL in $LABELS; do
        if [ "$APP_MODE" -eq 1 ] && [ "$LABEL" = com.ramwarden.menubar ]; then continue; fi
        [ ! -f "$AGENTS/$LABEL.plist" ] || launchctl bootstrap "$DOMAIN" "$AGENTS/$LABEL.plist" || true
      done
    fi
    echo "Rollback backup: $BACKUP"
  fi
  rm -rf "$STAGE"
  for ITEM in $BINS; do rm -f "$BIN_DIR/.$ITEM.new"; done
  exit "$CODE"
}
trap cleanup EXIT
for ITEM in $BINS; do
  test -x "$PAYLOAD/$ITEM"
  [ ! -f "$BIN_DIR/$ITEM" ] || cp -p "$BIN_DIR/$ITEM" "$BACKUP/$ITEM"
  install -m 755 "$PAYLOAD/$ITEM" "$BIN_DIR/.$ITEM.new"
done
for LABEL in $LABELS; do
  [ ! -f "$AGENTS/$LABEL.plist" ] || cp -p "$AGENTS/$LABEL.plist" "$BACKUP/LaunchAgents/$LABEL.plist"
  if [ -n "$DEFINITIONS" ]; then plutil -lint "$DEFINITIONS/$LABEL.plist" >/dev/null; fi
done
[ -f "$CONFIG_DIR/config.toml" ] || install -m 600 "$PAYLOAD/config.toml" "$CONFIG_DIR/config.toml"
if [ "$APP_MODE" -eq 0 ]; then
  ditto "$PAYLOAD/RamWarden.app" "$STAGE/RamWarden.app"
  [ ! -d "$BASE/Applications/RamWarden.app" ] || ditto "$BASE/Applications/RamWarden.app" "$BACKUP/RamWarden.app"
fi
ditto "$PAYLOAD/extension" "$STAGE/extension"
[ ! -d "$SUPPORT/extension" ] || ditto "$SUPPORT/extension" "$BACKUP/extension"
CHANGED=1
for LABEL in $LABELS; do stop_job "$LABEL"; done
if [ "$APP_MODE" -eq 0 ]; then
  rm -rf "$BASE/Applications/RamWarden.app"; mv "$STAGE/RamWarden.app" "$BASE/Applications/RamWarden.app"
fi
rm -rf "$SUPPORT/extension"; mv "$STAGE/extension" "$SUPPORT/extension"
for ITEM in $BINS; do mv -f "$BIN_DIR/.$ITEM.new" "$BIN_DIR/$ITEM"; done
if [ -n "$DEFINITIONS" ]; then
  for LABEL in $LABELS; do install -m 600 "$DEFINITIONS/$LABEL.plist" "$AGENTS/$LABEL.plist"; done
fi
# Failure injection is accepted only inside an explicitly disposable test root.
if [ "$TEST" -eq 1 ] && [ "${RAMWARDEN_INSTALL_TEST_FAIL:-}" = after-swap ]; then exit 9; fi
if [ "$TEST" -eq 0 ]; then
  for LABEL in $LABELS; do
    test -f "$AGENTS/$LABEL.plist"
    if [ "$APP_MODE" -eq 1 ] && [ "$LABEL" = com.ramwarden.menubar ] && launchctl print "$DOMAIN/$LABEL" >/dev/null 2>&1; then continue; fi
    launchctl bootstrap "$DOMAIN" "$AGENTS/$LABEL.plist"
  done
fi
ln -sf ramwarden "$BIN_DIR/ramwarden-doctor"
SUCCESS=1
echo 'Installed. Your settings, secrets, account tokens, history, and pet colors were preserved.'
echo 'Enable the bundled browser extension in each browser profile you use.'
