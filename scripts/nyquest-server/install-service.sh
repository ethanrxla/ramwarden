#!/usr/bin/env bash
# Private compression sidecar for the hosted API. Run on the API host as root.
set -euo pipefail
BINARY="${1:?path to the built nyquest binary}"
[ "$(id -u)" -eq 0 ] || { echo 'Run this server installer with sudo.'; exit 1; }
test -x "$BINARY"
id nyquest >/dev/null 2>&1 || useradd --system --home-dir /var/lib/ramwarden-nyquest --shell /usr/sbin/nologin nyquest
install -d -m 755 /opt/ramwarden-nyquest
install -d -m 750 -o root -g nyquest /etc/ramwarden-nyquest
install -m 755 "$BINARY" /opt/ramwarden-nyquest/nyquest
cat > /etc/ramwarden-nyquest/config.yaml <<'YAML'
host: "127.0.0.1"
port: 5400
compression_level: 0.5
adaptive_mode: false
semantic_enabled: false
context_optimization: false
openclaw_mode: false
compress_responses: false
privacy_mode: true
log_metrics: false
log_level: "warn"
allow_header_override: false
stability_guard: true
YAML
chown root:nyquest /etc/ramwarden-nyquest/config.yaml
chmod 640 /etc/ramwarden-nyquest/config.yaml
cat > /etc/systemd/system/ramwarden-nyquest.service <<'UNIT'
[Unit]
Description=RamWarden private Nyquest compression sidecar
After=network.target
StartLimitIntervalSec=60
StartLimitBurst=3
[Service]
User=nyquest
Group=nyquest
ExecStart=/opt/ramwarden-nyquest/nyquest --config /etc/ramwarden-nyquest/config.yaml serve
Restart=on-failure
RestartSec=5
StateDirectory=ramwarden-nyquest
WorkingDirectory=/var/lib/ramwarden-nyquest
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
RestrictSUIDSGID=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
IPAddressDeny=any
IPAddressAllow=localhost
MemoryMax=384M
CPUQuota=50%
UMask=0077
[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
systemctl enable --now ramwarden-nyquest
systemctl is-active ramwarden-nyquest
