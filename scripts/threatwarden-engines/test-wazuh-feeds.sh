#!/usr/bin/env bash
# Decode synthetic fixtures only; these are not submitted as live alerts.
set -euo pipefail
fixture_dir=$(mktemp -d)
trap 'rm -rf "$fixture_dir"' EXIT
clam='{"engine":"threatwarden_clamav","event":"malware_found","detail":"Synthetic integration fixture; no malware file"}'
net='{"timestamp":"2026-11-02T00:00:00.000000+0000","event_type":"alert","src_ip":"192.0.2.10","dest_ip":"198.51.100.20","proto":"TCP","alert":{"signature_id":2000001,"signature":"ThreatWarden synthetic IDS fixture","category":"Test","severity":2}}'
printf '%s\n' "$clam" | /var/ossec/bin/wazuh-logtest 2>&1 | tee "$fixture_dir/clam.txt"
grep -q '100511' "$fixture_dir/clam.txt"
printf '%s\n' "$net" | /var/ossec/bin/wazuh-logtest 2>&1 | tee "$fixture_dir/network.txt"
grep -q 'suricata' "$fixture_dir/network.txt"
