"""The bootstrap task's commands, run against a fake Secrets Manager and a fake process runner.

The draft sample is real `dm-migrate draft-identity-mapping --out /dev/stdout` output.
"""
import base64
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import types
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("bootstrap_task_commands", HERE / "bootstrap-task.py")
bootstrap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bootstrap)

MASTER = {"username": "dm_migrator", "password": "master pass/word"}
APP = {"DM_APP_SECRET": "sa_app_fixture", "DM_ACCOUNTS_WEBHOOK_SECRET": "whsec_fixture",
       "DM_DATA_KEY": base64.b64encode(bytes(range(32))).decode(), "DM_GIPHY_API_KEY": "giphy",
       "DM_RUNTIME_DATABASE_PASSWORD": "runtime-pass"}
ENVIRONMENT = {"APP_SECRET_ARN": "arn:app", "DB_SECRET_ARN": "arn:master", "RUNTIME_SECRET_ARN": "arn:runtime",
               "DB_HOST": "db.fixture", "ACCOUNTS_URL": "https://accounts.example", "DM_APP_ID": "dm"}
DRAFT = """iam_principal_id,accounts_uuid
c:shubham,b97
{
  "ids_in_data": 2,
  "note": "Review every row before applying.",
  "unresolved": {
    "si:cos": "No Silicon Accounts account has the id si:cos now."
  },
  "written": "/dev/stdout"
}
"""
MAPPING = "iam_principal_id,accounts_uuid\nc:shubham,b97\n"


class FakeSecrets:
    def __init__(self, runtime=None):
        self.values = {"arn:app": APP, "arn:master": MASTER}
        if runtime is not None:
            self.values["arn:runtime"] = runtime
        self.read, self.published = [], []

    def get_secret_value(self, SecretId):
        self.read.append(SecretId)
        return {"SecretString": json.dumps(self.values[SecretId])}

    def describe_secret(self, SecretId):
        return {"VersionIdsToStages": {"v1": ["AWSCURRENT"]}} if SecretId in self.values else {}

    def put_secret_value(self, SecretId, SecretString):
        self.published.append((SecretId, json.loads(SecretString)))


class Commands(unittest.TestCase):
    def run_main(self, argv, outputs=None, runtime=None, environment=None, fail=None):
        """(secrets, calls, stdout) of bootstrap.main(argv); calls are (arguments, env, stdin).

        The fakes stay on self.secrets and self.calls, for tests that expect a refusal."""
        secrets, calls = self.secrets, self.calls = FakeSecrets(runtime), []

        def run(arguments, input=None, text=None, env=None, capture_output=None, check=None):
            calls.append((list(arguments), env or {}, input))
            key = " ".join(arguments[:2])
            if fail and fail in key:
                return subprocess.CompletedProcess(arguments, 3, "", "boom near " + MASTER["password"])
            return subprocess.CompletedProcess(arguments, 0, (outputs or {}).get(key, ""), "")

        fake_boto3 = types.SimpleNamespace(client=lambda name: secrets)
        stdout = io.StringIO()
        with mock.patch.dict(os.environ, environment or ENVIRONMENT, clear=True), \
                mock.patch.dict(sys.modules, {"boto3": fake_boto3}), \
                mock.patch.object(bootstrap.subprocess, "run", run), contextlib.redirect_stdout(stdout):
            bootstrap.main(argv)
        return secrets, calls, stdout.getvalue()

    def test_bootstrap_checks_continuity_before_touching_the_database(self):
        with self.assertRaisesRegex(RuntimeError, "cannot rotate DM_DATABASE_URL"):
            self.run_main([], runtime={"DM_DATABASE_URL": "postgresql://other"})
        self.assertEqual(self.calls, [], "nothing ran against the database")
        self.assertEqual(self.secrets.published, [])

    def test_bootstrap_configures_migrates_and_publishes(self):
        secrets, calls, output = self.run_main([])
        programs = [arguments[0] for arguments, _, _ in calls]
        self.assertEqual(programs, ["psql", "psql", "dm-migrate", "psql"])
        self.assertIn("ALTER ROLE dm_runtime PASSWORD", calls[1][2])
        self.assertIn("runtime_role=dm_runtime", calls[3][0])
        (arn, runtime), = secrets.published
        self.assertEqual(arn, "arn:runtime")
        self.assertEqual(runtime["DM_APP_SECRET"], "sa_app_fixture")
        self.assertIn("bootstrap completed", output)

    def test_migrate_only_migrates_and_grants(self):
        secrets, calls, _ = self.run_main(["migrate"])
        self.assertEqual([arguments[0] for arguments, _, _ in calls], ["dm-migrate", "psql"])
        self.assertTrue(all(stdin is None for _, _, stdin in calls), "no role or password SQL")
        self.assertEqual(secrets.read, ["arn:master"], "the application secret is not needed")
        self.assertEqual(secrets.published, [])
        self.assertEqual(calls[0][1]["DM_ENVIRONMENT"], "production")
        self.assertIn("sslmode=verify-full", calls[0][1]["DM_DATABASE_URL"])

    def test_status_prints_the_applied_migrations(self):
        rows = "39|accounts identity|t\n40|accounts partition behaviour|t\n"
        _, calls, output = self.run_main(["status"], outputs={"psql -X": rows})
        self.assertIn("public._sqlx_migrations", " ".join(calls[0][0]))
        self.assertEqual(output.strip(), rows.strip())

    def test_draft_prints_the_mapping_between_markers_and_the_report(self):
        _, calls, output = self.run_main(["draft-identity-mapping"], outputs={"dm-migrate draft-identity-mapping": DRAFT})
        (arguments, env, _), = calls
        self.assertEqual(arguments, ["dm-migrate", "draft-identity-mapping", "--out", "/dev/stdout"])
        self.assertEqual((env["DM_APP_SECRET"], env["ACCOUNTS_URL"], env["DM_LOG_FILTER"]),
                         ("sa_app_fixture", "https://accounts.example", "error"))
        self.assertIn("----- BEGIN mapping.csv -----\niam_principal_id,accounts_uuid\nc:shubham,b97\n"
                      "----- END mapping.csv -----", output)
        self.assertEqual(json.loads(output.split("----- END mapping.csv -----", 1)[1])["ids_in_data"], 2)

    def test_link_identities_feeds_the_mapping_on_stdin(self):
        environment = dict(ENVIRONMENT, DM_IDENTITY_MAPPING=MAPPING.rstrip("\n"))
        _, calls, output = self.run_main(["link-identities", "--dry-run"], environment=environment,
                                         outputs={"dm-migrate link-identities": '{"dry_run": true}'})
        (arguments, env, stdin), = calls
        self.assertEqual(arguments, ["dm-migrate", "link-identities", "--file", "/dev/stdin", "--dry-run"])
        self.assertEqual(stdin, MAPPING)
        self.assertEqual(env["DM_APP_SECRET"], "sa_app_fixture")
        self.assertEqual(output.strip(), '{"dry_run": true}')

    def test_link_identities_refusals(self):
        cases = [
            (["link-identities"], {}, "DM_IDENTITY_MAPPING is empty"),
            (["link-identities"], {"DM_IDENTITY_MAPPING": "c:a,zQo\n"}, "must start with the header"),
            (["link-identities", "--offline"], {"DM_IDENTITY_MAPPING": MAPPING}, "only --dry-run and --force-relink"),
            (["link-identities"], {"DM_IDENTITY_MAPPING": MAPPING, "ACCOUNTS_URL": ""}, "ACCOUNTS_URL"),
            (["vacuum"], {}, "Unknown command: vacuum"),
            (["migrate", "--now"], {}, "Unknown command: migrate --now"),
        ]
        for argv, extra, message in cases:
            with self.subTest(argv=argv, extra=extra), self.assertRaisesRegex(RuntimeError, message):
                self.run_main(argv, environment=dict(ENVIRONMENT, **extra))

    def test_a_failed_operator_command_reports_stderr_without_secrets(self):
        with self.assertRaises(RuntimeError) as failure:
            self.run_main(["link-identities"], environment=dict(ENVIRONMENT, DM_IDENTITY_MAPPING=MAPPING),
                          fail="dm-migrate link-identities")
        self.assertIn("dm-migrate link-identities failed with exit 3: boom near [redacted]", str(failure.exception))
        self.assertNotIn(MASTER["password"], str(failure.exception))

    def test_split_draft_without_rows_or_without_a_mapping(self):
        rows, report = bootstrap.split_draft("iam_principal_id,accounts_uuid\n{\n  \"ids_in_data\": 0\n}\n")
        self.assertEqual(rows, ["iam_principal_id,accounts_uuid"])
        self.assertEqual(json.loads(report), {"ids_in_data": 0})
        with self.assertRaisesRegex(RuntimeError, "printed no mapping"):
            bootstrap.split_draft('{"error": "x"}\n')


if __name__ == "__main__":
    unittest.main()
