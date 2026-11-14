#!/usr/bin/env python3
"""Exercise rollback without changing launchd or requiring administrator rights."""
import importlib.util
from pathlib import Path
import plistlib
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('repair', Path(__file__).with_name('repair-macos-evidence.py'))
repair = importlib.util.module_from_spec(spec)
spec.loader.exec_module(repair)


class RepairTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        root = Path(self.directory.name)
        self.config = root / 'suricata.yaml'
        self.plist = root / 'suricata.plist'
        self.candidate = root / 'candidate.yaml'
        self.backup = root / 'backup'
        self.backup.mkdir()
        self.config.write_bytes(b'old config')
        self.plist.write_bytes(plistlib.dumps({'ProcessType': 'Background'}))
        self.candidate.write_bytes(b'new config')
        for path in [self.config, self.plist]:
            (self.backup / path.name).write_bytes(path.read_bytes())

    def apply(self):
        repair.apply_candidate(self.candidate, self.config, self.plist, self.backup,
                               {'ProcessType': 'Standard'})

    def assert_restored(self):
        for path in [self.config, self.plist]:
            self.assertEqual(path.read_bytes(), (self.backup / path.name).read_bytes())

    def test_success_keeps_candidate(self):
        with patch.object(repair.subprocess, 'run'), patch.object(repair, 'bootstrap'):
            self.apply()
        self.assertEqual(self.config.read_bytes(), b'new config')
        self.assertEqual(plistlib.loads(self.plist.read_bytes())['ProcessType'], 'Standard')

    def test_failed_stop_restores_files_without_duplicate_job(self):
        results = [subprocess.CalledProcessError(5, 'bootout'), subprocess.CompletedProcess([], 0)]
        with patch.object(repair.subprocess, 'run', side_effect=results), patch.object(repair, 'bootstrap') as start:
            with self.assertRaises(subprocess.CalledProcessError):
                self.apply()
            start.assert_not_called()
        self.assert_restored()

    def test_failed_start_restores_and_reloads_previous_config(self):
        def start(_):
            if self.config.read_bytes() == b'new config':
                raise RuntimeError('new job failed')
        results = [subprocess.CompletedProcess([], 0), subprocess.CompletedProcess([], 113)]
        with patch.object(repair.subprocess, 'run', side_effect=results), patch.object(repair, 'bootstrap', side_effect=start) as reload:
            with self.assertRaisesRegex(RuntimeError, 'new job failed'):
                self.apply()
            self.assertEqual(reload.call_count, 2)
        self.assert_restored()

    def test_bootstrap_retries_transient_launchd_error(self):
        results = [subprocess.CompletedProcess([], 5, stderr='I/O error'), subprocess.CompletedProcess([], 0)]
        with patch.object(repair.subprocess, 'run', side_effect=results), patch.object(repair.time, 'sleep') as sleep:
            repair.bootstrap(self.plist)
            sleep.assert_called_once_with(1)

    def test_bootstrap_has_bounded_retries(self):
        failure = subprocess.CompletedProcess([], 5, stderr='I/O error')
        with patch.object(repair.subprocess, 'run', return_value=failure) as run, patch.object(repair.time, 'sleep'):
            with self.assertRaisesRegex(RuntimeError, 'launchd could not reload'):
                repair.bootstrap(self.plist)
            self.assertEqual(run.call_count, 30)


if __name__ == '__main__':
    unittest.main()
