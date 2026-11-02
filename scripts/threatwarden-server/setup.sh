#!/usr/bin/env bash
# Set up ThreatWarden's hosted Wazuh server on a fresh Ubuntu 24.04 machine
# (built for an Oracle Cloud Always Free ARM instance; any 64-bit Ubuntu works).
# Run as root:  sudo bash setup.sh
#
# What it does, and why (see docs/threatwarden-hosting.md):
#   1. 4 GB of swap, so a burst of indexing can't OOM a 12 GB box.
#   2. Wazuh manager and indexer, official component installers.
#   3. Turns off password enrolment: ramwarden-api registers each agent itself
#      and hands out per-agent keys, so port 1515 is never needed.
#   4. Opens only 1514/tcp (agents) in the host firewall. The manager API
#      (55000), indexer (9200) and dashboard (443) stay closed to the internet;
#      reach the dashboard through an SSH tunnel.
set -euo pipefail

WAZUH_SERIES="${WAZUH_SERIES:-4.14}"

if [[ $EUID -ne 0 ]]; then
  echo "Run as root: sudo bash $0" >&2
  exit 1
fi

# Refuse unsupported hosts before touching swap or packages.
. /etc/os-release
[[ "$ID" == ubuntu && "$VERSION_ID" == 24.04 ]] || { echo "Use a fresh Ubuntu 24.04 server." >&2; exit 1; }
[[ "$(uname -m)" == aarch64 || "$(uname -m)" == x86_64 ]] || exit 1
[[ "$WAZUH_SERIES" =~ ^[0-9]+\.[0-9]+$ ]] || exit 1
export DEBIAN_FRONTEND=noninteractive

echo "==> Swap"
if ! swapon --show | grep -q /swapfile; then
  fallocate -l 4G /swapfile
  chmod 600 /swapfile
  mkswap /swapfile >/dev/null
  swapon /swapfile
  grep -q '^/swapfile' /etc/fstab || echo '/swapfile none swap sw 0 0' >> /etc/fstab
fi

echo "==> Packages"
echo 'DPkg::Lock::Timeout "600";' > /etc/apt/apt.conf.d/99threatwarden-lock-timeout
apt-get update -y
apt-get install -y curl iptables-persistent

# Enforce management isolation before installing any listening services. Keep
# SSH and unrelated traffic untouched; cover IPv6 as well as IPv4.
for firewall in iptables ip6tables; do
  "$firewall" -N RAMWARDEN_WAZUH 2>/dev/null || true
  "$firewall" -F RAMWARDEN_WAZUH
  "$firewall" -A RAMWARDEN_WAZUH -i lo -j RETURN
  "$firewall" -A RAMWARDEN_WAZUH -p tcp -m multiport --dports 443,1515,55000,9200 -j DROP
  "$firewall" -A RAMWARDEN_WAZUH -p tcp --dport 1514 -j ACCEPT
  "$firewall" -A RAMWARDEN_WAZUH -j RETURN
  "$firewall" -C INPUT -j RAMWARDEN_WAZUH 2>/dev/null || "$firewall" -I INPUT 1 -j RAMWARDEN_WAZUH
done
netfilter-persistent save >/dev/null

echo "==> Wazuh ${WAZUH_SERIES} manager and indexer"
cd /root
if [[ -f wazuh-install-files.tar ]] && ! systemctl is-active --quiet wazuh-manager wazuh-indexer; then
  echo "An earlier Wazuh installation is incomplete or stopped. Diagnose it before rerunning; refusing to overwrite keys." >&2
  exit 1
fi
if [[ ! -f wazuh-install-files.tar ]]; then
  curl -sSfO "https://packages.wazuh.com/${WAZUH_SERIES}/wazuh-install.sh"
  cat > config.yml <<'CONFIG'
nodes:
  indexer:
    - name: wazuh-indexer
      ip: 127.0.0.1
  server:
    - name: wazuh-server
      ip: 127.0.0.1
CONFIG
  bash ./wazuh-install.sh -g
  bash ./wazuh-install.sh -wi wazuh-indexer
  bash ./wazuh-install.sh -s
  bash ./wazuh-install.sh -ws wazuh-server
fi

echo "==> Per-agent keys only: disable password enrolment"
CONF=/var/ossec/etc/ossec.conf
perl -0pi -e 's|(<auth>\s*<disabled>)no(</disabled>)|${1}yes${2}|' "$CONF"
python3 - "$CONF" <<'POLICY'
from pathlib import Path
import sys
p = Path(sys.argv[1])
s = p.read_text()
if '<!-- ThreatWarden observe-only policy -->' not in s:
    s += '\n<ossec_config><!-- ThreatWarden observe-only policy --><active-response><disabled>yes</disabled></active-response></ossec_config>\n'
p.write_text(s)
POLICY
systemctl restart wazuh-manager

systemctl is-active --quiet wazuh-manager wazuh-indexer

echo
echo "Done."
echo
echo "Passwords (keep them safe; they go into ramwarden-api's environment, never into the app):"
echo "  sudo tar -O -xf /root/wazuh-install-files.tar wazuh-install-files/wazuh-passwords.txt"
echo
echo "ramwarden-api needs:"
echo "  WAZUH_MANAGER_URL=https://127.0.0.1:55000   WAZUH_API_USER=wazuh-wui   WAZUH_API_PASSWORD=<wazuh-wui password>"
echo "  WAZUH_INDEXER_URL=https://127.0.0.1:9200    WAZUH_INDEXER_USER=admin   WAZUH_INDEXER_PASSWORD=<admin password>"
echo "  WAZUH_AGENT_HOST=<the DNS name agents connect to, e.g. agents.ramwarden.app>"
echo
echo "Optional dashboard, if installed separately, is private and requires an SSH tunnel:"
echo "  ssh -L 8443:127.0.0.1:443 ubuntu@<server>   then open https://localhost:8443"
