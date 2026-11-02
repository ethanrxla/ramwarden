import contextlib, io, json, pathlib, runpy, subprocess, tempfile, unittest
from unittest.mock import patch
SCRIPT = pathlib.Path(__file__).with_name('scan.py')
class ScanPolicy(unittest.TestCase):
    def scan(self, pressure, output='', code=0):
        with tempfile.TemporaryDirectory() as d:
            real_open = open
            opened = []
            def mapped(path, *args, **kwargs):
                f = real_open(pathlib.Path(d) / pathlib.Path(path).name, *args, **kwargs)
                opened.append(f)
                return f
            response = io.BytesIO(json.dumps(pressure).encode())
            with patch('os.geteuid', return_value=0), patch('os.makedirs'), patch('sys.argv', [str(SCRIPT), '--scheduled', '/tmp/example']), patch('builtins.open', mapped), patch('urllib.request.urlopen', return_value=response), patch('subprocess.run', return_value=subprocess.CompletedProcess([], code, output)) as run, contextlib.redirect_stdout(io.StringIO()):
                try:
                    runpy.run_path(str(SCRIPT), run_name='__main__')
                except SystemExit as e:
                    status = e.code
            for f in opened:
                f.close()
            log = pathlib.Path(d) / 'threatwarden.json'
            return status, run.call_args, log.read_text() if log.exists() else ''
    def test_pressure_or_unknown_defers_without_loading_antivirus(self):
        for pressure in [{'rung':'suspend'}, {}, {'rung':'kill'}]:
            status, call, log = self.scan(pressure)
            self.assertEqual((status, call, log), (0, None, ''))
    def test_detection_is_logged_without_removing_or_moving_file(self):
        status, call, log = self.scan({'rung':'release'}, '/tmp/example: Test FOUND\n', 1)
        self.assertEqual(status, 1)
        self.assertEqual(call.args[0], ['clamscan','--recursive','--infected','--','/tmp/example'])
        self.assertEqual(json.loads(log.splitlines()[0])['event'], 'malware_found')
if __name__ == '__main__': unittest.main()
