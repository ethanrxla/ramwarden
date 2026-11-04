#!/usr/bin/env python3
"""Scan-only Linux/macOS bridge. No resident scanner or automatic remediation."""
import argparse
import fcntl
import json
import os
import pathlib
import subprocess
import sys
import time
import urllib.request


def event(log, kind, **fields):
    log.write(json.dumps(dict(engine='threatwarden_clamav', event=kind,
                             time=int(time.time()), **fields)) + '\n')
    log.flush()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--scheduled', action='store_true')
    parser.add_argument('paths', nargs='+')
    args = parser.parse_args()
    if os.geteuid() != 0:
        raise SystemExit('Run the scan as root')
    os.umask(0o027)
    mac = sys.platform == 'darwin'
    base = pathlib.Path('/Library/Application Support/ThreatWarden' if mac else '/run/threatwarden')
    base.mkdir(mode=0o750, parents=True, exist_ok=True)
    lock = (base / 'clamav.lock').open('w')
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        return 0
    logdir = pathlib.Path('/var/log/threatwarden')
    logdir.mkdir(mode=0o750, parents=True, exist_ok=True)
    with (logdir / 'clamav.json').open('a') as log:
        if args.scheduled:
            try:
                with urllib.request.urlopen('http://127.0.0.1:7823/psi', timeout=3) as response:
                    clear = json.load(response).get('rung') == 'release'
            except Exception:
                clear = False
            if not clear:
                event(log, 'scan_deferred', reason='memory_not_clear')
                return 0
        scanner = '/opt/homebrew/bin/clamscan' if mac else 'clamscan'
        if mac and not pathlib.Path(scanner).exists():
            scanner = '/usr/local/bin/clamscan'
        command = [scanner, '--recursive', '--infected']
        if mac:
            command += ['--database=' + str(base / 'clamav')]
        event(log, 'scan_started', scope='selected_paths', path_count=len(args.paths))
        import tempfile
        with tempfile.TemporaryFile(mode='w+') as output:
            try:
                result = subprocess.run(command + ['--', *args.paths], stdout=output,
                                        stderr=subprocess.STDOUT, timeout=3 * 3600)
                code = result.returncode
            except (OSError, subprocess.TimeoutExpired):
                event(log, 'scan_finished', exit_code=2)
                return 2
            output.seek(0)
            for line in output:
                if line.rstrip().endswith(' FOUND'):
                    event(log, 'malware_found', detail=line.strip()[:4096])
            event(log, 'scan_finished', exit_code=code)
        return code


if __name__ == '__main__':
    raise SystemExit(main())
