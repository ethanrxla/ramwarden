#!/bin/sh
set -eu
prefix="$1"
base='/Library/Application Support/ThreatWarden'
"$prefix/bin/suricata-update" --suricata-conf "$base/suricata.yaml" --suricata "$prefix/bin/suricata" -D "$base/update" -o "$base/rules" --no-reload
# suricata-update validates candidates before installation; reload only on success.
/bin/launchctl kill SIGUSR2 system/com.ramwarden.suricata
# Daily EVE files only, outside the current day. Keep seven days locally.
/usr/bin/find /var/log/threatwarden/suricata -type f -name 'eve-*.json' -mtime +7 -delete
