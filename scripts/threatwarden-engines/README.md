# Linux detection sensors

Explicit opt-in Ubuntu sensor setup, after the Wazuh manager is healthy:

    sudo bash scripts/threatwarden-engines/install-linux.sh

This configures this host only. A hosted sensor cannot inspect traffic or files
on a customer's computer. Endpoints need their own Wazuh agent and sensors.
Windows Defender remains enabled; Windows Suricata additionally needs Npcap.

ClamAV uses on-demand `threatwarden-scan /path` and never removes or quarantines
files. No resident clamd process is enabled. Signature refresh uses freshclam.
For scheduled use pass `--scheduled`; scanning defers unless the local
RamWarden `/psi` endpoint reports `release` (the clear-memory rung). An absent
or invalid response defers safely. No timer is enabled on the hosted manager,
which does not run the RAM daemon.

Suricata uses the default-route interface, ET Open rules, and passive AF_PACKET
capture. EVE records alerts, flow counters and engine statistics. It never adds
IPS/NFQUEUE rules. Wazuh ingests EVE JSON and ClamAV's structured scan events;
local rule IDs 100510/100511 label the antivirus events with `clamav`.

The application UI and hosted enrollment still need integration. Do not report
these engines as enabled on customer devices merely because the server has them.

Windows: `scan.ps1 -Paths C:\path` uses the official ClamAV installation and
writes JSON to `%ProgramData%\ThreatWarden\logs\clamav.json`. Use `-Scheduled`
for the same fail-closed RAM gate. Signature updates must be configured before
scans; no scheduled scans or resident clamd are enabled by this script.
