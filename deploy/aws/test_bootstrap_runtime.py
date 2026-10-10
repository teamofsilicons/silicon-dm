"""The runtime secret the bootstrap publishes, and the deployment templates that read it.

Runs without AWS: the bootstrap's pure functions are imported and the templates are read as
text. The service's required and retired variables come from src/config.rs itself.
"""
import base64
import importlib.util
from pathlib import Path
import re
import unittest

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
spec = importlib.util.spec_from_file_location("bootstrap_task", HERE / "bootstrap-task.py")
bootstrap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bootstrap)

CONFIG = (REPO / "src" / "config.rs").read_text()
# Every variable the service refuses to start without (required(...) and parse_required(...)).
SERVICE_REQUIRED = set(re.findall(r'(?:parse_required|required)\("([A-Z0-9_]+)"\)', CONFIG))
# Variables of the IAM/Honeycomb era the service ignores and logs.
OBSOLETE = set(re.findall(r'"([A-Z0-9_]+)"', CONFIG.split("OBSOLETE_VARIABLES", 1)[1].split("];", 1)[0]))
DATA_KEY = base64.b64encode(bytes(range(32))).decode()
APP = {
    "DM_APP_SECRET": "sa_app_fixture",
    "DM_ACCOUNTS_WEBHOOK_SECRET": "whsec_fixture",
    "DM_DATA_KEY": DATA_KEY,
    "DM_GIPHY_API_KEY": "giphy-fixture",
    "DM_RUNTIME_DATABASE_PASSWORD": "runtime pass/word",
    "DM_POSTMARK_SERVER_TOKEN": "postmark-fixture",
}
# What a DM 0.14 bootstrap published.
LEGACY_RUNTIME = {
    "DM_IAM_APP_ID": "dm", "DM_IAM_APP_SECRET": "iam-fixture", "DM_IAM_WEBHOOK_SECRET": "iam-hook",
    "DM_IAM_WEBHOOK_KEY_VERSION": "1", "DM_GIPHY_API_KEY": "giphy-fixture",
    "DM_TEST_KEY_ENCRYPTION_KEY": DATA_KEY, "DM_SPACE_STATION_TABLE_KEY": "", "DM_POSTMARK_SERVER_TOKEN": "",
    "DM_HONEYCOMB_SERVICE_TOKEN": "", "DM_HONEYCOMB_BASE_URL": "",
    "DM_DATABASE_URL": bootstrap.database_url("dm_runtime", "runtime pass/word", "db.fixture", "silicon_dm"),
    "DM_TEST_DATABASE_URL": "postgresql://dm_testing:x@test.fixture:5432/silicon_dm_test",
}
RUNTIME_KEYS = {"DM_APP_SECRET", "DM_ACCOUNTS_WEBHOOK_SECRET", "DM_DATA_KEY", "DM_GIPHY_API_KEY",
                "DM_SPACE_STATION_TABLE_KEY", "DM_POSTMARK_SERVER_TOKEN", "DM_DATABASE_URL"}


def runtime(app=None, current=None):
    return bootstrap.runtime_configuration(dict(APP if app is None else app), "db.fixture", current)


class RuntimeSecret(unittest.TestCase):
    def test_first_use_publishes_exactly_the_accounts_settings(self):
        document = runtime()
        self.assertEqual(set(document), RUNTIME_KEYS)
        self.assertEqual(document["DM_SPACE_STATION_TABLE_KEY"], "")
        self.assertTrue(document["DM_DATABASE_URL"].startswith("postgresql://dm_runtime:runtime%20pass%2Fword@db.fixture:5432/silicon_dm?"))
        self.assertIn("sslmode=verify-full", document["DM_DATABASE_URL"])
        self.assertNotIn("DM_RUNTIME_DATABASE_PASSWORD", document)

    def test_the_first_accounts_bootstrap_keeps_the_014_keys_for_a_rollback(self):
        document = runtime(current=LEGACY_RUNTIME)
        for name in bootstrap.RETIRED:
            self.assertEqual(document[name], LEGACY_RUNTIME[name], name)
        self.assertEqual(set(document) - set(bootstrap.RETIRED), RUNTIME_KEYS)
        self.assertEqual(runtime(current=document), document, "a rerun changes nothing")

    def test_database_url_and_data_key_never_rotate(self):
        published = runtime()
        with self.assertRaisesRegex(RuntimeError, "cannot rotate DM_DATABASE_URL"):
            runtime(dict(APP, DM_RUNTIME_DATABASE_PASSWORD="another"), published)
        other_key = base64.b64encode(bytes(32)).decode()
        with self.assertRaisesRegex(RuntimeError, "cannot rotate DM_DATA_KEY"):
            runtime(dict(APP, DM_DATA_KEY=other_key), published)
        rotated = runtime(dict(APP, DM_APP_SECRET="sa_app_rotated", DM_GIPHY_API_KEY="new"), published)
        self.assertEqual(rotated["DM_APP_SECRET"], "sa_app_rotated", "app and provider secrets may change")

    def test_bad_fields_are_named_and_never_echoed(self):
        cases = {
            "DM_APP_SECRET": "",
            "DM_DATA_KEY": "not base64 at all!",
            "DM_ACCOUNTS_WEBHOOK_SECRET": "sa_app_pasted_in_the_wrong_field",
            "DM_GIPHY_API_KEY": "REPLACE_ME",
            "DM_RUNTIME_DATABASE_PASSWORD": "line\nbreak",
        }
        for name, value in cases.items():
            with self.subTest(name), self.assertRaises(RuntimeError) as refused:
                runtime(dict(APP, **{name: value}))
            self.assertIn(name, str(refused.exception))
            if value:
                self.assertNotIn(value, str(refused.exception))
        with self.assertRaisesRegex(RuntimeError, "DM_APP_SECRET"):
            runtime({key: value for key, value in APP.items() if key != "DM_APP_SECRET"})
        with self.assertRaisesRegex(RuntimeError, "DM_POSTMARK_SERVER_TOKEN"):
            runtime(dict(APP, DM_POSTMARK_SERVER_TOKEN="a\x00b"))

    def test_data_key_reads_as_the_service_reads_it(self):
        url_safe = base64.urlsafe_b64encode(bytes([251] * 32)).decode()
        self.assertEqual(len(bootstrap.data_key_bytes(url_safe)), 32)
        self.assertEqual(len(bootstrap.data_key_bytes(url_safe.rstrip("="))), 32)
        self.assertEqual(len(bootstrap.data_key_bytes(DATA_KEY)), 32)
        with self.assertRaisesRegex(RuntimeError, "exactly 32 bytes"):
            runtime(dict(APP, DM_DATA_KEY=base64.b64encode(bytes(16)).decode()))


class Templates(unittest.TestCase):
    def test_config_rs_was_read(self):
        self.assertTrue({"ACCOUNTS_URL", "DM_APP_SECRET", "DM_DATA_KEY"} <= SERVICE_REQUIRED, SERVICE_REQUIRED)
        self.assertTrue({"DM_IAM_APP_SECRET", "DM_TEST_DATABASE_URL", "DM_HONEYCOMB_BASE_URL"} <= OBSOLETE, OBSOLETE)

    def test_fargate_tasks_read_only_what_the_bootstrap_publishes(self):
        text = (HERE / "fargate.yaml").read_text()
        references = re.findall(r"\$\{RuntimeSecret\}:([A-Z0-9_]+)::", text)
        self.assertEqual(len(references), 2 * len(RUNTIME_KEYS), "API and worker read the same secrets")
        self.assertEqual(set(references), RUNTIME_KEYS)
        self.assertEqual(set(runtime()), RUNTIME_KEYS)

    def test_fargate_tasks_carry_every_required_setting_and_no_retired_one(self):
        text = (HERE / "fargate.yaml").read_text()
        names = set(re.findall(r"- Name: ([A-Z][A-Z0-9_]+)\n", text))
        self.assertLessEqual(SERVICE_REQUIRED, names)
        self.assertFalse(names & OBSOLETE, names & OBSOLETE)
        self.assertNotIn("IamBaseUrl", text)

    def test_ec2_user_data_writes_every_required_setting_and_no_retired_one(self):
        source = (HERE / "bootstrap.sh").read_text()
        written = set(re.findall(r"^    '([A-Z][A-Z0-9_]+)': ", source, re.MULTILINE))
        self.assertLessEqual(SERVICE_REQUIRED, written)
        self.assertFalse(written & OBSOLETE, written & OBSOLETE)
        self.assertNotIn("IamBaseUrl", (HERE / "production.yaml").read_text())

    def test_env_example_lists_every_required_setting_and_no_retired_one(self):
        text = (REPO / ".env.example").read_text()
        names = set(re.findall(r"^#?\s*([A-Z][A-Z0-9_]+)=", text, re.MULTILINE))
        self.assertLessEqual(SERVICE_REQUIRED, names)
        self.assertFalse(names & OBSOLETE, names & OBSOLETE)


if __name__ == "__main__":
    unittest.main()
