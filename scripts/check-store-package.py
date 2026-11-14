#!/usr/bin/env python3
"""Fail closed on accidental private files, broad required hosts and remote code."""
import argparse,hashlib,json,re,struct,zipfile
from pathlib import Path
ALLOWED={'manifest.json','activity.js','background.js','popup.js','warden.js','popup.html','icons/icon16.png','icons/icon48.png','icons/icon128.png'}
def validate(path):
    with zipfile.ZipFile(path) as archive:
        names=[x.filename for x in archive.infolist() if not x.is_dir()]
        if len(names)!=len(set(names)) or set(names)!=ALLOWED:raise ValueError('ZIP must contain exactly the approved runtime files')
        for entry in archive.infolist():
            if '..' in Path(entry.filename).parts or entry.filename.startswith('/') or '\\' in entry.filename or (entry.external_attr>>16)&0o170000==0o120000:raise ValueError('Unsafe archive path or link')
        manifest=json.loads(archive.read('manifest.json'))
        if manifest.get('manifest_version')!=3 or manifest.get('background')!={'service_worker':'background.js'}:raise ValueError('Expected Chrome Manifest V3 service worker')
        if set(manifest.get('permissions',[]))!={'tabs','storage','alarms','tabGroups'}:raise ValueError('Permission disclosure must match reviewed permissions')
        if set(manifest.get('host_permissions',[]))!={'http://localhost:7823/*','http://127.0.0.1:7823/*'}:raise ValueError('Only loopback hosts may be required')
        if manifest.get('optional_host_permissions')!=['http://*/*'] or 'browser_specific_settings' in manifest:raise ValueError('Unexpected Chrome manifest fields')
        for name in names:
            data=archive.read(name)
            if name.endswith(('.js','.html','.json')):
                text=data.decode('utf-8')
                if re.search(r'(?:xai-[A-Za-z0-9]{24,}|sk-(?:proj-)?[A-Za-z0-9_-]{30,}|-----BEGIN (?:RSA |OPENSSH |EC )?PRIVATE KEY-----|mongodb(?:\+srv)?://[^\s"\']+:[^\s"\']+@)',text):raise ValueError('Possible credential in package')
                if re.search(r'\beval\s*\(|new\s+Function\s*\(|importScripts\s*\(\s*["\']https?://|<script[^>]+src=["\']https?://',text,re.I):raise ValueError('Remote/dynamic executable code is not allowed')
            if name.endswith('.png'):
                size=int(re.search(r'icon(\d+)',name).group(1))
                if data[:8]!=b'\x89PNG\r\n\x1a\n' or struct.unpack('>II',data[16:24])!=(size,size):raise ValueError('Invalid icon dimensions')
    return {'package':str(path),'version':manifest['version'],'sha256':hashlib.sha256(Path(path).read_bytes()).hexdigest(),'files':len(names),'status':'package checks passed; publisher and live URL review still required'}
if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('package');args=p.parse_args()
    print(json.dumps(validate(args.package),indent=2))
