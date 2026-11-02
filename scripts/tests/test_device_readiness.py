import importlib.util
import pathlib
import unittest

path = pathlib.Path(__file__).resolve().parents[1] / "device-readiness.py"
spec = importlib.util.spec_from_file_location("readiness", path)
readiness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(readiness)


class ProjectionTests(unittest.TestCase):
    def test_intel_does_not_export_app_answers_or_unknown_credentials(self):
        data = {"calls_last_hour": 4, "input_tokens": 1724,
                "app_answers": [{"about": "private work"}], "device_token": "secret"}
        self.assertEqual(readiness.project("intel", data), {"calls_last_hour": 4, "input_tokens": 1724})

    def test_threat_does_not_export_alerts_or_engine_detail(self):
        data = {"unlocked": False, "status": {"level": "locked", "detail": "private"},
                "report": {"alerts": ["private file"], "engines": [{"id": "wazuh", "state": "on", "detail": "secret"}]}}
        result = readiness.project("wardens/threat", data)
        self.assertEqual(result["engines"], [{"id": "wazuh", "state": "on"}])
        self.assertNotIn("private", str(result))
        self.assertNotIn("secret", str(result))

    def test_absent_and_malformed_reports_do_not_claim_coverage(self):
        self.assertEqual(readiness.project("health", []), {"state": "invalid_response"})
        self.assertIsNone(readiness.project("wardens/threat", {"report": None})["unlocked"])


if __name__ == "__main__":
    unittest.main()
