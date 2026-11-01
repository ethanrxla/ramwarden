import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';

for (const mode of ['missing', 'invalid', 'mismatch', 'valid']) {
  test(`installer ${mode === 'valid' ? 'runs verified artifact' : 'refuses ' + mode + ' checksum'}`, () => {
    const root = mkdtempSync(join(tmpdir(), 'warden-install-test-'));
    try {
      const bin = join(root, 'bin'); mkdirSync(bin);
      const pkg = join(root, 'pkg'); mkdirSync(pkg);
      writeFileSync(join(pkg, 'install.sh'), '#!/bin/bash\nprintf installed > "$TEST_MARKER"\n');
      const archive = join(root, 'package.tar.gz');
      execFileSync('tar', ['czf', archive, '-C', root, 'pkg']);
      const hash = createHash('sha256').update(readFileSync(archive)).digest('hex');
      writeFileSync(join(root, 'manifest.json'), JSON.stringify({artifacts: [{url:'https://example/ramwarden-macos-aarch64.tar.gz',sha256:mode === 'invalid' ? '' : mode === 'mismatch' ? '0'.repeat(64) : hash}]}));
      writeFileSync(join(bin, 'uname'), '#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n', {mode:0o755});
      writeFileSync(join(bin, 'curl'), `#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in https:*) url="$1";; -o) shift; dest="$1";; esac
  shift
done
case "$url" in
  */manifest.json) [ "$TEST_MODE" != missing ] || exit 22; cp "$TEST_ROOT/manifest.json" "$dest";;
  *) cp "$TEST_ROOT/package.tar.gz" "$dest";;
esac
`, {mode:0o755});
      const marker = join(root, 'installed');
      const result = spawnSync('sh', ['web/install.sh'], {encoding:'utf8',env:{...process.env,PATH:bin+':'+process.env.PATH,TEST_ROOT:root,TEST_MODE:mode,TEST_MARKER:marker}});
      assert.equal(result.status === 0, mode === 'valid', result.stdout + result.stderr);
      assert.equal(existsSync(marker), mode === 'valid');
    } finally { rmSync(root,{recursive:true,force:true}); }
  });
}
