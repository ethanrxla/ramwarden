#!/usr/bin/env bash
# Set up ThreatWarden's hosted Wazuh server on a fresh Ubuntu 24.04 machine
# (built for an Oracle Cloud Always Free ARM instance; any 64-bit Ubuntu works).
# Run as root:  sudo bash setup.sh
#
# What it does, and why (see docs/threatwarden-hosting.md):
#   1. 4 GB of swap, so a burst of indexing can't OOM a 12 GB box.
#   2. Wazuh all-in-one (manager + indexer + dashboard), official installer.
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

echo "==> Swap"
if ! swapon --show | grep -q /swapfile; then
  fallocate -l 4G /swapfile
  chmod 600 /swapfile
  mkswap /swapfile >/dev/null
  swapon /swapfile
  grep -q '^/swapfile' /etc/fstab || echo '/swapfile none swap sw 0 0' >> /etc/fstab
fi

echo "==> Packages"
apt-get update -y
apt-get install -y curl iptables-persistent

echo "==> Wazuh ${WAZUH_SERIES} all-in-one (takes 10-20 minutes)"
cd /root
if [[ ! -f wazuh-install-files.tar ]]; then
  curl -sSfO "https://packages.wazuh.com/${WAZUH_SERIES}/wazuh-install.sh"
  # -i skips the installer's hardware check, which can refuse 2-core machines
  # that run a small deployment fine.
  bash ./wazuh-install.sh -a -i
fi

echo "==> Per-agent keys only: disable password enrolment"
CONF=/var/ossec/etc/ossec.conf
perl -0pi -e 's|(<auth>\s*<disabled>)no(</disabled>)|${1}yes${2}|' "$CONF"
systemctl restart wazuh-manager

echo "==> Firewall: agents in on 1514/tcp, everything else stays closed"
if ! iptables -C INPUT -p tcp --dport 1514 -j ACCEPT 2>/dev/null; then
  # Oracle's Ubuntu images end INPUT with a REJECT; insert before it.
  iptables -I INPUT 5 -p tcp -m state --state NEW --dport 1514 -j ACCEPT
fi
netfilter-persistent save >/dev/null

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
echo "Dashboard (for you only), from your own computer:"
echo "  ssh -L 8443:127.0.0.1:443 ubuntu@<server>   then open https://localhost:8443"
