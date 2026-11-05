#!/usr/bin/env python3
"""Offline Suricata regression: a benign synthetic DNS packet, never transmitted.

Usage: python test-eve-privacy.py --suricata /path/to/suricata --config candidate.yaml
The config's EVE filename must be writable. Only the generated test rule is loaded.
"""
import argparse
import datetime
import json
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import time
import uuid


def fixture(timestamp):
    query = b''.join(bytes([len(label)]) + label for label in b'privacy-check.invalid'.split(b'.')) + b'\0'
    dns = struct.pack('!6H', 42, 0x0100, 1, 0, 0, 0) + query + struct.pack('!2H', 1, 1)
    udp = struct.pack('!4H', 42424, 53, 8 + len(dns), 0) + dns
    ip = struct.pack('!BBHHHBBH4s4s', 0x45, 0, 20 + len(udp), 1, 0, 64, 17, 0,
                     socket.inet_aton('192.0.2.10'), socket.inet_aton('192.0.2.53'))
    checksum = sum(struct.unpack('!10H', ip))
    while checksum >> 16:
        checksum = (checksum & 65535) + (checksum >> 16)
    ip = ip[:10] + struct.pack('!H', (~checksum) & 65535) + ip[12:]
    ethernet = bytes.fromhex('0200000000020200000000010800')
    packet = ethernet + ip + udp
    return struct.pack('<IHHIIII', 0xa1b2c3d4, 2, 4, 0, 0, 65535, 1) + struct.pack('<IIII', timestamp, 0, len(packet), len(packet)) + packet


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--suricata', required=True)
    p.add_argument('--config', required=True)
    p.add_argument('--eve-dir', required=True, help='Directory containing the candidate config EVE output')
    p.add_argument('--check-clock', action='store_true', help='Also verify alert time matches the PCAP timestamp')
    a = p.parse_args()
    message = 'ThreatWarden privacy fixture ' + uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix='rw-eve-privacy-') as directory:
        root = Path(directory)
        timestamp = int(time.time())
        (root / 'fixture.pcap').write_bytes(fixture(timestamp))
        (root / 'test.rules').write_text(f'alert dns any any -> any any (msg:"{message}"; dns.query; content:"privacy-check.invalid"; sid:9000001; rev:1;)\n')
        result = subprocess.run([a.suricata, '-c', a.config, '-r', str(root / 'fixture.pcap'),
                                 '-S', str(root / 'test.rules'), '--runmode', 'single', '-l', directory],
                                capture_output=True, text=True, timeout=90)
        if result.returncode:
            raise SystemExit(result.stderr[-2000:] + result.stdout[-2000:])
        events = []
        for path in Path(a.eve_dir).glob('eve*.json'):
            with path.open() as source:
                events += [json.loads(line) for line in source if line.strip()]
        alerts = [v for v in events if v.get('alert', {}).get('signature_id') == 9000001
                  and v['alert'].get('signature') == message]
        assert alerts, 'Synthetic DNS rule did not fire'
        forbidden = {'payload', 'payload_printable', 'packet', 'dns', 'http', 'tls', 'files', 'fileinfo', 'websocket'}
        for alert in alerts:
            assert not forbidden.intersection(alert), 'Private protocol/payload fields leaked'
            assert 'privacy-check.invalid' not in json.dumps(alert), 'Query name leaked'
            if a.check_clock:
                parsed = datetime.datetime.strptime(alert['timestamp'], '%Y-%m-%dT%H:%M:%S.%f%z')
                assert parsed.tzinfo is not None and abs(parsed.timestamp() - timestamp) < 1, 'EVE timezone does not match capture time'
        print(f'Offline privacy regression passed: {len(alerts)} synthetic alert(s), no query or payload fields.')


if __name__ == '__main__':
    main()
