#!/usr/bin/env python3
"""Repair only EVE logging and startup scheduling on an already-installed Mac.

Run as admin with Suricata's Python/PyYAML; validates before replacing config.
"""
import datetime
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import time


def bootstrap(path):
    # bootout can return before launchd has finished tearing down the old job.
    # Retry briefly instead of treating that transient EIO as a permanent failure.
    for _ in range(30):
        result = subprocess.run(['/bin/launchctl', 'bootstrap', 'system', str(path)], capture_output=True, text=True)
        if result.returncode == 0:
            return
        time.sleep(1)
    raise RuntimeError('launchd could not reload the job: ' + result.stderr.strip())


def apply_candidate(candidate, config_path, plist_path, backup, job):
    """Restore both files on any apply/reload failure, including failed bootout."""
    try:
        candidate.replace(config_path)
        plist_path.write_bytes(plistlib.dumps(job))
        subprocess.run(['/bin/launchctl', 'bootout', 'system/com.ramwarden.suricata'], check=True)
        bootstrap(plist_path)
    except (OSError, subprocess.CalledProcessError, RuntimeError):
        shutil.copy2(backup / config_path.name, config_path)
        shutil.copy2(backup / plist_path.name, plist_path)
        # Failed bootout may leave the old job registered. Starting a second
        # instance then fails needlessly; only bootstrap if the job is absent.
        loaded = subprocess.run(['/bin/launchctl', 'print', 'system/com.ramwarden.suricata'],
                                capture_output=True)
        if loaded.returncode != 0:
            bootstrap(plist_path)
        raise


def main():
    import yaml

    if os.geteuid() != 0:
        raise SystemExit('Administrator rights required')
    os.umask(0o027)
    base = Path('/Library/Application Support/ThreatWarden')
    config_path = base / 'suricata.yaml'
    plist_path = Path('/Library/LaunchDaemons/com.ramwarden.suricata.plist')
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    backup = base / 'backups' / ('evidence-' + stamp)
    backup.mkdir(parents=True)
    shutil.copy2(config_path, backup / config_path.name)
    shutil.copy2(plist_path, backup / plist_path.name)
    config = yaml.safe_load(config_path.read_text())
    eve = next(entry['eve-log'] for entry in config['outputs'] if 'eve-log' in entry)
    eve.update({'metadata': False, 'buffer-size': 0})
    eve['types'] = [{'alert': {'metadata': False, 'payload': False, 'payload-printable': False,
                    'packet': False, 'http-body': False, 'http-body-printable': False,
                    'websocket-payload': False, 'websocket-payload-printable': False,
                    'tagged-packets': False}}, 'flow', {'stats': {'totals': True, 'threads': False, 'deltas': False}}]
    candidate = base / 'suricata.evidence-candidate.yaml'
    candidate.write_text('%YAML 1.1\n---\n' + yaml.safe_dump(config))
    job = plistlib.loads(plist_path.read_bytes())
    executable = job['ProgramArguments'][0]
    subprocess.run([executable, '-T', '-c', str(candidate), '-l', '/var/log/threatwarden/suricata'], check=True)
    # Test ordinary I/O scheduling after observing a 36-minute startup with
    # Background scheduling. Keep nice=10 for steady-state CPU priority.
    job['ProcessType'] = 'Standard'
    apply_candidate(candidate, config_path, plist_path, backup, job)
    print('Capture evidence repaired. Backup:', backup)


if __name__ == '__main__':
    main()
