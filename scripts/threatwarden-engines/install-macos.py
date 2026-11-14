#!/usr/bin/env python3
"""Explicitly opted-in macOS endpoint setup; invoke with admin rights.

Install Homebrew clamav/suricata first. Run using Suricata's Python (PyYAML).
Backs up the Wazuh config and previous plists; never touches account settings.
"""
import argparse
import datetime
import os
import pathlib
import plistlib
import pwd
import shutil
import subprocess
import sys
import yaml

BASE = pathlib.Path('/Library/Application Support/ThreatWarden')
LOG = pathlib.Path('/var/log/threatwarden')
PLISTS = pathlib.Path('/Library/LaunchDaemons')


def run(*args):
    result = subprocess.run(args, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    if result.returncode:
        print(result.stdout, flush=True)
        result.check_returncode()
    return result.stdout


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--user', required=True)
    a = p.parse_args()
    if sys.platform != 'darwin' or os.geteuid() != 0:
        raise SystemExit('macOS admin rights required')
    user = pwd.getpwnam(a.user)
    if user.pw_uid < 500:
        raise SystemExit('Choose the signed-in user')
    os.umask(0o027)
    prefix = pathlib.Path('/opt/homebrew' if pathlib.Path('/opt/homebrew/bin/suricata').exists() else '/usr/local')
    # Homebrew 1.5.4_1 has a known OpenSSL 4 signature verification regression.
    # Never skip database verification to work around a broken package.
    scanner = (prefix / 'bin/clamscan').resolve()
    if '1.5.4_1' in scanner.parts:
        raise SystemExit('Upgrade Homebrew ClamAV to revision 1.5.4_2 or newer before setup (homebrew-core#322048).')
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    backup = BASE / 'backups' / stamp
    backup.mkdir(parents=True, mode=0o700)
    for path in [BASE / 'clamav', BASE / 'rules', LOG / 'suricata']:
        path.mkdir(parents=True, exist_ok=True, mode=0o750)
    # Only this user may read private event files; scripts/config remain root-owned.
    for directory in [BASE, BASE / 'clamav', LOG, LOG / 'suricata']:
        run('/bin/chmod', '+a', f'{a.user} allow read,execute,readattr,readextattr,readsecurity,file_inherit,directory_inherit', str(directory))
    source = pathlib.Path(__file__).resolve().parent
    for name in ['scan.py', 'refresh-macos.sh']:
        shutil.copyfile(source / name, BASE / name)
        os.chmod(BASE / name, 0o700)
    fresh = BASE / 'freshclam.conf'
    fresh.write_text(f'DatabaseDirectory "{BASE / "clamav"}"\nDatabaseOwner root\nDatabaseMirror database.clamav.net\n')
    # Never start clamd: definitions are loaded only while a scan runs.
    run(str(prefix / 'bin/freshclam'), '--config-file=' + str(fresh))
    config = yaml.safe_load((prefix / 'etc/suricata/suricata.yaml').read_text())
    interface = next(line.split(':', 1)[1].strip() for line in run('/sbin/route', '-n', 'get', 'default').splitlines() if 'interface:' in line)
    config['default-log-dir'] = str(LOG / 'suricata')
    config['default-rule-path'] = str(BASE / 'rules')
    config['rule-files'] = ['suricata.rules']
    config['runmode'] = 'single'
    config['pcap'] = [{'interface': interface, 'promisc': False, 'checksum-checks': 'auto'}]
    config['stats'] = {'enabled': True, 'interval': 30}
    config['detect'] = dict(config.get('detect') or {}, profile='low')
    config['outputs'] = [{'eve-log': {'enabled': True, 'filetype': 'regular',
        'filename': 'eve-%Y-%m-%d.json', 'rotate-interval': 'day', 'buffer-size': 0, 'metadata': False,
        'types': [{'alert': {'payload': False, 'payload-printable': False, 'packet': False,
                            'metadata': False, 'http-body': False, 'http-body-printable': False,
                            'websocket-payload': False, 'websocket-payload-printable': False,
                            'tagged-packets': False}}, 'flow',
                  {'stats': {'totals': True, 'threads': False, 'deltas': False}}]}}]
    target = BASE / 'suricata.yaml'
    if target.exists():
        shutil.copy2(target, backup / target.name)
    candidate = BASE / 'suricata.candidate.yaml'
    candidate.write_text('%YAML 1.1\n---\n' + yaml.safe_dump(config))
    run(str(prefix / 'bin/suricata-update'), '--suricata-conf', str(candidate), '--suricata', str(prefix / 'bin/suricata'), '-D', str(BASE / 'update'), '-o', str(BASE / 'rules'), '--no-reload')
    run(str(prefix / 'bin/suricata'), '-T', '-c', str(candidate), '-l', str(LOG / 'suricata'))
    candidate.replace(target)

    def job(label, command, hour=None):
        path = PLISTS / (label + '.plist')
        if path.exists():
            shutil.copy2(path, backup / path.name)
        data = dict(Label=label, ProgramArguments=command, ProcessType='Standard' if hour is None else 'Background', Nice=10,
                    StandardOutPath=str(LOG / (label + '.log')), StandardErrorPath=str(LOG / (label + '.log')),
                    Umask=0o027)
        if hour is None:
            data.update(RunAtLoad=True, KeepAlive=True, ThrottleInterval=30)
        else:
            data['StartCalendarInterval'] = dict(Hour=hour, Minute=15)
        path.write_bytes(plistlib.dumps(data)); os.chmod(path, 0o644)
        subprocess.run(['/bin/launchctl', 'bootout', 'system/' + label], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        run('/bin/launchctl', 'bootstrap', 'system', str(path))

    job('com.ramwarden.suricata', [str(prefix / 'bin/suricata'), '-c', str(target), '--pcap=' + interface, '-l', str(LOG / 'suricata')])
    job('com.ramwarden.signatures', [str(prefix / 'bin/freshclam'), '--config-file=' + str(fresh)], 3)
    job('com.ramwarden.rules', ['/bin/sh', str(BASE / 'refresh-macos.sh'), str(prefix)], 2)
    job('com.ramwarden.scan', ['/usr/bin/python3', str(BASE / 'scan.py'), '--scheduled', str(pathlib.Path(user.pw_dir) / 'Downloads')], 4)

    run(sys.executable, str(source / 'configure-macos-agent.py'))
    print(f'Setup complete. Capture interface: {interface}. Daily scan: Downloads, RAM-gated. Backup: {backup}')


if __name__ == '__main__':
    main()
