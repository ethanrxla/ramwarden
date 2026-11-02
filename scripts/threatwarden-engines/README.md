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
capture. EVE records alerts, flow counters. It never adds
IPS/NFQUEUE rules. Wazuh ingests EVE JSON and ClamAV's structured scan events;
local rule IDs 100510/100511 label the antivirus events with `clamav`.

The application UI and hosted enrollment still need integration. Do not report
these engines as enabled on customer devices merely because the server has them.

Windows: `scan.ps1 -Paths C:\path` uses the official ClamAV installation and
writes JSON to `%ProgramData%\ThreatWarden\logs\clamav.json`. Use `-Scheduled`
for the same fail-closed RAM gate. `schedule-windows.ps1` configures daily signature refresh and a Downloads scan.
The scan task uses a process-local RemoteSigned policy for the local wrapper;
system-wide PowerShell settings are unchanged. No resident clamd is enabled.

ET Open signatures refresh daily on Linux through `threatwarden-rules.timer`.
Windows uses `update-windows-rules.ps1`: candidates must pass Suricata's test
before atomically replacing the running rules. Windows rules requiring the
unavailable file-magic feature are excluded, with their count in update-status.json.

Live validation on 2026-11-02: the Oracle manager/indexer/Filebeat and Linux
Suricata services are active; the Windows agent is connected. Windows ClamAV
scanned a benign file successfully as SYSTEM. Both synthetic feed fixtures decode
with the correct engine groups. Live ClamAV scan events and Suricata alerts reach
the manager. Windows Suricata captures the default Wi-Fi interface in passive mode.
Scheduled antivirus scans defer when the RAM daemon's `/psi` endpoint is unavailable;
the installed Windows service currently does not expose that endpoint. This is a
remaining product integration dependency, not a successful scheduled sweep.
