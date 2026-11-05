"""Exercise bundled installation and rollback only in disposable temporary homes."""
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile

SOURCE = Path(__file__).resolve().parents[2] / 'macos/setup/install-payload.sh'
BINS = ('ramwarden-daemon', 'ramwarden-usage-sync', 'ramwarden', 'ramwarden-launcher')
LABELS = ('com.ramwarden.daemon', 'com.ramwarden.menubar', 'com.ramwarden.usage')

for fail in (False, True):
    with tempfile.TemporaryDirectory(prefix='warden-install-contract-', dir='/tmp') as raw:
        root = Path(raw)
        payload = root/'resources/payload'
        payload.mkdir(parents=True)
        script = payload.parent/'install-payload.sh'
        shutil.copy2(SOURCE, script)
        home = root/'home'
        config = home/'.config/ramwarden'
        config.mkdir(parents=True)
        (config/'config.toml').write_text('original-config')
        (config/'secrets.toml').write_text('synthetic-secret')
        agents = home/'Library/LaunchAgents'
        agents.mkdir(parents=True)
        definitions = root/'definitions'
        definitions.mkdir()
        for label in LABELS:
            (agents/(label+'.plist')).write_bytes(plistlib.dumps({'Label':label,'fixture':'original'}))
            (definitions/(label+'.plist')).write_bytes(plistlib.dumps({'Label':label,'fixture':'new'}))
        binary_dir = home/'.local/bin'
        binary_dir.mkdir(parents=True)
        for name in BINS:
            new = payload/name
            new.write_text('#!/bin/sh\n# new\n')
            new.chmod(0o755)
            (binary_dir/name).write_text('original-binary')
        for folder in (payload/'RamWarden.app', payload/'extension'):
            folder.mkdir()
            (folder/'version').write_text('new')
        old_app = home/'Applications/RamWarden.app'
        old_app.mkdir(parents=True)
        (old_app/'version').write_text('original')
        old_extension = home/'Library/Application Support/RamWarden/extension'
        old_extension.mkdir(parents=True)
        (old_extension/'version').write_text('original')
        (payload/'config.toml').write_text('default')
        env = dict(os.environ)
        env['RAMWARDEN_INSTALL_TEST_FAIL'] = 'after-swap' if fail else ''
        result = subprocess.run(['bash',str(script),'--test-root',str(home),'--definitions',str(definitions)],env=env,capture_output=True,text=True)
        assert result.returncode == (9 if fail else 0), result.stdout+result.stderr
        assert (config/'config.toml').read_text() == 'original-config'
        assert (config/'secrets.toml').read_text() == 'synthetic-secret'
        expected = 'original' if fail else 'new'
        assert (old_app/'version').read_text() == expected
        assert (old_extension/'version').read_text() == expected
        for name in BINS:
            assert (binary_dir/name).read_text() == ('original-binary' if fail else '#!/bin/sh\n# new\n')
        for label in LABELS:
            assert plistlib.loads((agents/(label+'.plist')).read_bytes())['fixture'] == expected
            if not fail:
                assert (agents/(label+'.plist')).stat().st_mode & 0o777 == 0o600
        print('Rollback passed' if fail else 'Installation passed')
