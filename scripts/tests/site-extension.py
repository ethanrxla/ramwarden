#!/usr/bin/env python3
"""The website's extension download must be the extension in this checkout.

web/downloads/ramwarden-chrome.zip is a committed copy of the Chrome package
(the site has no build step). Compared file by file, not byte for byte, with
the package scripts/build-ext.sh just built: a different zlib may compress the
same files differently. Refresh the copy with: bash scripts/build-ext.sh --site
"""
import hashlib,importlib.util,json,pathlib,zipfile
root=pathlib.Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('store_gate',root/'scripts/check-store-package.py');gate=importlib.util.module_from_spec(spec);spec.loader.exec_module(gate)
version=json.loads((root/'extension/manifest.json').read_text())['version']
site=root/'web/downloads/ramwarden-chrome.zip'
def files(path):
    with zipfile.ZipFile(path) as z:return {name:z.read(name) for name in z.namelist()}
fix='run: bash scripts/build-ext.sh --site'
assert gate.validate(site)['version']==version,f'site download is not version {version}; {fix}'
built,served=files(root/f'dist/ramwarden-chrome-{version}.zip'),files(site)
stale=sorted(name for name in built.keys()|served.keys() if built.get(name)!=served.get(name))
assert not stale,f'site download differs from extension/ in {", ".join(stale)}; {fix}'
recorded=site.with_name(site.name+'.sha256').read_text().split()
assert recorded==[hashlib.sha256(site.read_bytes()).hexdigest(),site.name],f'site download checksum file is wrong; {fix}'
print('Site extension download matches extension/.')
