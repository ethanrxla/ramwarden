# Endpoint detection sensors

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

Endpoint status is measured locally. Do not report sensors as enabled on a
customer device merely because the hosted server has them.

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
the Windows memory daemon was restarted through its existing login task and the
endpoint now responds. A reclaim-pressure response correctly defers scans. The
daemon task has no runtime cutoff and retries after failures. Deferral is not a
successful scheduled sweep.

## macOS and measured endpoint status

Install current Homebrew `clamav` and `suricata`, then run from this directory:

    sudo /opt/homebrew/opt/suricata/libexec/bin/python install-macos.py --user YOUR_LOGIN

Use `/usr/local` on Intel Homebrew installations. ClamAV 1.5.4_1 has a known
signature-verification failure with OpenSSL 4; upgrade to 1.5.4_2 or newer
before setup. Verification must remain enabled. See
https://github.com/Homebrew/homebrew-core/issues/322048.

The installer validates Suricata configuration/rules, captures passively on
the current default-route interface, adds Wazuh log inputs, and schedules daily
signature/rule updates and a RAM-gated Downloads scan. Switching adapters needs
a setup rerun to select the new interface. Encrypted payloads remain encrypted;
this is endpoint/interface coverage, not a claim to inspect every VPN tunnel.
Packet payloads, tagged packet dumps and application-layer metadata are disabled
using Suricata 8 supported settings. An offline synthetic DNS alert verifies that
the query and payload fields are omitted on both Windows and macOS. Scan and service
configuration backups live under `/Library/Application Support/ThreatWarden/backups`.
The user receives read-only access to evidence; launchd scripts remain root-owned.
`scan.py /absolute/path` is the explicit on-demand scan command (run with sudo).

For the already-installed Windows sensors, run `enable-windows-evidence.ps1`
elevated. It validates a candidate configuration before replacement, adds periodic
capture counters, refreshes the existing scheduled scan/update tasks, and grants
read-only evidence access. This operator script targets the established
`D:\ThreatWarden` installation; it is not a general-purpose Windows installer.

Windows evidence permissions distinguish directories from files: inherited
folder grants alone did not make existing protected EVE files readable. The
repair script grants RX explicitly to existing files and inheritable RX to their
directories, with a regression covering protected files and newly rotated files.
It does not grant write access to sensor scripts, logs or definitions.

The Windows Suricata 8.0.7 build emits an incorrect timezone offset when DST is
supported, causing fresh counters to appear in the future. The installer sets
`TZ=UTC0` only in the Suricata service's Environment value, preserving other
variables and backing up the old value. It does not change the Windows timezone.
The source issue is in `WinStrftime` in
[Suricata 8.0.7 util-time.c](https://github.com/OISF/suricata/blob/suricata-8.0.7/src/util-time.c).
Use the offline privacy test with `--check-clock` and process environment
`TZ=UTC0` to verify the EVE timestamp equals the synthetic PCAP timestamp.
Live validation on November 4 confirmed timestamped successful Windows scan
events at the manager, fresh capture counters and zero reported capture drops.

The daemon's paid `/wardens/threat` report now includes local sensor evidence and
`sensors.network.connections`. Connection inventory is established TCP only,
limited to what the OS exposes to the current user (UDP and other users may be
absent). It performs no DNS resolution and sends none of this inventory to the
hosted service. It does not infer per-app bandwidth from connection counts.
The Mac companion shows this inventory and engine details. Other clients can
consume the same report; their connection table UI is not added here.

Missing logs, malformed output, missing timestamps, stale scan results, deferred
scans and stale capture counters never become a healthy green state. EVE reads
are capped at 512 KiB; snapshots at 512 connections and a 12-second deadline.
A quiet interface with zero captured packets is reported as unverified/watch.
Read-only operator diagnostic, with remote addresses omitted by default:

    cargo run -p ramwarden-wazuh --example sensors

Add `-- --connections` to inspect private endpoint addresses locally. This
example does not install or replace the running daemon. A successful build does
not imply that the new UI/daemon has been deployed.

For an existing Mac setup, `repair-macos-evidence.py` applies the supported EVE
privacy settings without refreshing definitions or touching accounts. It backs up
configuration, validates the candidate and retries a launchd reload briefly while
the old job exits. If reloading still fails, it restores the prior configuration.
Suricata uses Standard launchd I/O scheduling with nice=10; scheduled scans and
updates remain Background jobs. This addresses a startup path where initial rule
loading was observed taking 36 minutes; startup timing must still be measured on
the target machine rather than assumed.

Offline regression commands (these never transmit the fixture packet):

    powershell -NoProfile -File scripts/threatwarden-engines/test-eve-policy.ps1
    python scripts/threatwarden-engines/test-eve-privacy.py --suricata PATH --config CANDIDATE --eve-dir OUTPUT

The privacy test loads only its synthetic rule. Use a candidate config whose EVE
output points into a disposable test directory; do not point this test at live logs.
