#!/usr/bin/env python3
"""Exercise the real ZIP gate with leaks, privilege expansion and code injection."""
import importlib.util,json,pathlib,tempfile,zipfile
root=pathlib.Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('store_gate',root/'scripts/check-store-package.py');gate=importlib.util.module_from_spec(spec);spec.loader.exec_module(gate)
version=json.loads((root/'extension/manifest.json').read_text())['version']
source=root/f'dist/ramwarden-chrome-{version}.zip'
assert gate.validate(source)['version']==version
with zipfile.ZipFile(source) as z:original={name:z.read(name) for name in z.namelist()}
with tempfile.TemporaryDirectory() as folder:
    def rejects(data):
        path=pathlib.Path(folder)/'bad.zip'
        with zipfile.ZipFile(path,'w') as z:
            for name,content in data.items():z.writestr(name,content)
        try:gate.validate(path)
        except ValueError:return
        raise AssertionError('Unsafe package accepted')
    rejects({**original,'.env':b'private'})
    rejects({**original,'../secret':b'private'})
    manifest=json.loads(original['manifest.json']);manifest['host_permissions']=['<all_urls>']
    rejects({**original,'manifest.json':json.dumps(manifest).encode()})
    rejects({**original,'popup.js':original['popup.js']+b'\neval("injected")'})
    rejects({**original,'popup.js':original['popup.js']+b'\nconst leaked="xai-'+b'a'*40+b'";'})
print('Store ZIP regression gates passed.')
