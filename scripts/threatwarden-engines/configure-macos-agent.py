#!/usr/bin/env python3
"""Preserve Wazuh's relaxed XML dialect; validate changes with Wazuh itself."""
import datetime
import os
from pathlib import Path
import shutil
import subprocess


def main():
    if os.geteuid() != 0:
        raise SystemExit('Administrator rights required')
    os.umask(0o027)
    config = Path('/Library/Ossec/etc/ossec.conf')
    original = config.read_text()
    begin, end = '<!-- ThreatWarden sensors begin -->', '<!-- ThreatWarden sensors end -->'
    if begin in original:
        first, remainder = original.split(begin, 1)
        if end not in remainder:
            raise SystemExit('Incomplete sensor block; configuration retained')
        original = first + remainder.split(end, 1)[1]
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    backup = config.with_name('ossec.conf.threatwarden-' + stamp)
    shutil.copy2(config, backup)
    block = '''
<ossec_config>
  <localfile><log_format>json</log_format><location>/var/log/threatwarden/clamav.json</location></localfile>
  <localfile><log_format>json</log_format><location>/var/log/threatwarden/suricata/eve-*.json</location></localfile>
  <active-response><disabled>yes</disabled></active-response>
</ossec_config>
'''
    Path('/var/log/threatwarden/clamav.json').touch(exist_ok=True)
    config.write_text(original + '\n' + begin + block + end + '\n')
    result = subprocess.run(['/Library/Ossec/bin/wazuh-logcollector', '-t'], capture_output=True, text=True)
    if result.returncode:
        shutil.copy2(backup, config)
        raise SystemExit('Wazuh rejected the change; original restored: ' + result.stderr[-2000:])
    subprocess.run(['/Library/Ossec/bin/wazuh-control', 'restart'], check=True)
    print('Wazuh sensor feeds configured; backup:', backup)


if __name__ == '__main__':
    main()
