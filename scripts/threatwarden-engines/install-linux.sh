#!/usr/bin/env bash
# Explicit opt-in sensor installation on Ubuntu; run only after Wazuh is healthy.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
[[ $EUID == 0 ]] || exit 1
. /etc/os-release
[[ "$ID" == ubuntu ]] || exit 1
[[ -f /var/ossec/etc/ossec.conf ]] || { echo 'Wazuh must be configured first'; exit 1; }
apt-get -o DPkg::Lock::Timeout=600 update
apt-get -o DPkg::Lock::Timeout=600 install -y software-properties-common
add-apt-repository -y ppa:oisf/suricata-stable
apt-get -o DPkg::Lock::Timeout=600 update
# OISF's package includes suricata-update; the separate distro package conflicts.
if dpkg-query -W -f='${Status}' suricata-update 2>/dev/null | grep -q 'install ok installed'; then
    apt-get -o DPkg::Lock::Timeout=600 remove -y suricata-update
fi
apt-get -o DPkg::Lock::Timeout=600 -o Dpkg::Options::=--force-confold install -y clamav suricata python3-yaml
# Never keep ClamAV's scan engine resident. freshclam only updates signatures.
systemctl disable --now clamav-daemon.service clamav-daemon.socket 2>/dev/null || true
install -m 755 "$(dirname "$0")/scan.py" /usr/local/sbin/threatwarden-scan
install -d -o root -g root -m 750 /var/log/threatwarden
 touch /var/log/threatwarden/clamav.json
chmod 640 /var/log/threatwarden/clamav.json
cat > /etc/logrotate.d/threatwarden-clamav <<'ROTATE'
/var/log/threatwarden/clamav.json {
    daily
    rotate 7
    compress
    missingok
    notifempty
    copytruncate
}
ROTATE
suricata-update
python3 - <<'CONFIG'
import subprocess, yaml
from pathlib import Path
interface = subprocess.check_output(['ip', '-json', 'route', 'show', 'default'], text=True)
import json
interface = json.loads(interface)[0]['dev']
p = Path('/etc/suricata/suricata.yaml')
config = yaml.safe_load(p.read_text())
config.setdefault('detect', {})['profile'] = 'low'
config['detect']['sgh-mpm-context'] = 'single'
config['af-packet'] = [{'interface': interface, 'cluster-id': 99, 'cluster-type': 'cluster_flow', 'defrag': True}]
# Avoid recording packet payloads; retain alerts and flow counters for the network view.
for output in config.get('outputs', []):
    if 'eve-log' in output:
        output['eve-log']['types'] = ['alert', 'flow']
p.write_text('%YAML 1.1\n---\n' + yaml.safe_dump(config))
CONFIG
# The distribution service runs IDS; do not add NFQUEUE or IPS firewall rules.
suricata -T -c /etc/suricata/suricata.yaml
python3 - <<'PY'
from pathlib import Path
p = Path('/var/ossec/etc/ossec.conf')
s = p.read_text()
for file in ['/var/log/threatwarden/clamav.json', '/var/log/suricata/eve.json']:
    if f'<location>{file}</location>' not in s:
        s += f'\n<ossec_config><localfile><log_format>json</log_format><location>{file}</location></localfile></ossec_config>\n'
p.write_text(s)
Path('/var/ossec/etc/rules/threatwarden_clamav.xml').write_text('''<group name="clamav,threatwarden,">
<rule id="100510" level="3"><decoded_as>json</decoded_as><field name="engine">^threatwarden_clamav$</field><description>ThreatWarden ClamAV scan event</description></rule>
<rule id="100511" level="12"><if_sid>100510</if_sid><field name="event">^malware_found$</field><description>ClamAV detected malware (scan only)</description></rule>
</group>''')
PY
/var/ossec/bin/wazuh-analysisd -t
systemctl restart wazuh-manager
systemctl enable --now suricata
install -m 644 "$(dirname "$0")/threatwarden-rules.service" /etc/systemd/system/
install -m 644 "$(dirname "$0")/threatwarden-rules.timer" /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now threatwarden-rules.timer
# Scheduling is deliberately opt-in: scans require a running RAM daemon reporting clear.
printf '%s\n' 'Configured. Manual scan: sudo threatwarden-scan /path'
