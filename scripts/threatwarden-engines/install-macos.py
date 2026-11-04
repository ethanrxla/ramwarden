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
import xml.etree.ElementTree as ET
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
        'filename': 'eve-%Y-%m-%d.json', 'rotate-interval': 'day',
        'types': [{'alert': {'payload': False, 'packet': False, 'http': False, 'tls': False}}, 'flow', 'stats']}}]
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
        data = dict(Label=label, ProgramArguments=command, ProcessType='Background', Nice=10,
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

    # Wazuh allows sibling ossec_config elements; preserve them and comments.
    wazuh = pathlib.Path('/Library/Ossec/etc/ossec.conf')
    original = wazuh.read_text()
    shutil.copy2(wazuh, backup / 'ossec.conf')
    xml = ET.fromstring('<root>' + original + '</root>', parser=ET.XMLParser(target=ET.TreeBuilder(insert_comments=True)))
    root = xml.find('ossec_config')
    if root is None:
        raise RuntimeError('Missing ossec_config')
    for location in [str(LOG / 'clamav.json'), str(LOG / 'suricata/eve-*.json')]:
        if not any(n.text == location for n in xml.findall('.//localfile/location')):
            local = ET.SubElement(root, 'localfile')
            ET.SubElement(local, 'log_format').text = 'json'
            ET.SubElement(local, 'location').text = location
    for response in xml.findall('.//active-response'):
        disabled = response.find('disabled')
        if disabled is None:
            disabled = ET.SubElement(response, 'disabled')
        disabled.text = 'yes'
    (LOG / 'clamav.json').touch(exist_ok=True)
    wazuh.write_text('\n'.join(ET.tostring(n, encoding='unicode') for n in xml))
    try:
        run('/Library/Ossec/bin/wazuh-logcollector', '-t')
    except Exception:
        shutil.copy2(backup / 'ossec.conf', wazuh)
        raise
    run('/Library/Ossec/bin/wazuh-control', 'restart')
    print(f'Setup complete. Capture interface: {interface}. Daily scan: Downloads, RAM-gated. Backup: {backup}')


if __name__ == '__main__':
    main()
