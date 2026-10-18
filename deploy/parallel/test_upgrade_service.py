import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("upgrade_service", Path(__file__).with_name("upgrade_service.py"))
upgrade_service = importlib.util.module_from_spec(spec)
spec.loader.exec_module(upgrade_service)

RUNNING = ["PATH=/usr/local/bin:/usr/bin", "DM_ENVIRONMENT=production", "DM_TING_URL=https://backend.ting.teamofsilicons.com"]
KEY = "k" * 40


class EnvironmentTests(unittest.TestCase):
    def test_keeps_the_running_environment_and_adds_speech(self):
        values = upgrade_service.environment(RUNNING, {"deepgram_api_key": KEY})
        self.assertEqual(values["DM_ENVIRONMENT"], "production")
        self.assertEqual(values["DM_TING_URL"], "https://backend.ting.teamofsilicons.com")
        self.assertEqual(values["DM_DEEPGRAM_API_KEY"], KEY)

    def test_refuses_a_secret_without_a_usable_key(self):
        for secret in [{}, {"deepgram_api_key": ""}, {"deepgram_api_key": "short"}, {"deepgram_api_key": KEY + "\n"}]:
            with self.assertRaises(ValueError):
                upgrade_service.environment(RUNNING, secret)

    def test_refuses_multi_line_values(self):
        with self.assertRaises(ValueError):
            upgrade_service.environment(RUNNING + ["DM_LOG_FILTER=info\nDM_DATA_KEY=x"], {"deepgram_api_key": KEY})


if __name__ == "__main__":
    unittest.main()
