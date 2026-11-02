#!/usr/bin/env python3
"""Scan-only ClamAV bridge. Scheduled runs fail closed without a clear RAM rung."""
import argparse, fcntl, json, os, subprocess, urllib.request
p = argparse.ArgumentParser()
p.add_argument('--scheduled', action='store_true')
p.add_argument('paths', nargs='+')
a = p.parse_args()
if os.geteuid() != 0:
    raise SystemExit('Run the scan as root')
os.makedirs('/run/threatwarden', mode=0o700, exist_ok=True)
lock = open('/run/threatwarden/clamav.lock', 'w')
try:
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    raise SystemExit(0)
if a.scheduled:
    try:
        with urllib.request.urlopen('http://127.0.0.1:7823/psi', timeout=3) as r:
            clear = json.load(r).get('rung') == 'release'
    except Exception:
        clear = False
    if not clear:
        print('Deferred: RamWarden has not reported clear memory pressure.')
        raise SystemExit(0)
# No shell, deletion, quarantine, or automatic remediation.
result = subprocess.run(['clamscan', '--recursive', '--infected', '--', *a.paths],
                        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
with open('/var/log/clamav/threatwarden.json', 'a') as log:
    for line in result.stdout.splitlines():
        if line.endswith(' FOUND'):
            log.write(json.dumps({'engine': 'threatwarden_clamav', 'event': 'malware_found',
                                  'detail': line[:4096]}) + '\n')
    log.write(json.dumps({'engine': 'threatwarden_clamav', 'event': 'scan_finished',
                         'exit_code': result.returncode}) + '\n')
print(result.stdout)
raise SystemExit(result.returncode)
