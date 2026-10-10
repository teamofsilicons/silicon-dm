"""run-bootstrap-task.py against a fake `aws` executable that records every call."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import textwrap
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("run_bootstrap_task", HERE / "run-bootstrap-task.py")
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)

TASK = "arn:aws:ecs:us-east-1:111111111111:task/silicon-dm-production/0123abcd"
STACK = {"Stacks": [{
    "Outputs": [{"OutputKey": "ClusterName", "OutputValue": "silicon-dm-production"},
                {"OutputKey": "BootstrapTaskDefinitionArn", "OutputValue": "arn:task-definition/silicon-dm-bootstrap:19"},
                {"OutputKey": "TaskSecurityGroupId", "OutputValue": "sg-app"}],
    "Parameters": [{"ParameterKey": "PrivateSubnetA", "ParameterValue": "subnet-a"},
                   {"ParameterKey": "PrivateSubnetB", "ParameterValue": "subnet-b"}]}]}
FAKE_AWS = textwrap.dedent("""\
    #!{python}
    import json, sys
    from pathlib import Path
    state = Path({state!r})
    args = sys.argv[1:]
    calls = json.loads(state.read_text()) if state.exists() else []
    entry = {{"args": args}}
    if args[:2] == ["ecs", "run-task"]:
        entry["overrides"] = json.loads(Path(args[args.index("--overrides") + 1][len("file://"):]).read_text())
    calls.append(entry)
    state.write_text(json.dumps(calls))
    config = json.loads(Path({config!r}).read_text())
    key = " ".join(args[:2])
    if key == "cloudformation describe-stacks":
        print(json.dumps(config["stack"]))
    elif key == "ecs run-task":
        print(json.dumps({{"tasks": [{{"taskArn": {task!r}}}], "failures": []}}))
    elif key == "ecs describe-tasks":
        polls = sum(1 for call in calls if call["args"][:2] == ["ecs", "describe-tasks"])
        status = "STOPPED" if polls >= 2 else "RUNNING"
        print(json.dumps({{"tasks": [{{"lastStatus": status, "stoppedReason": "Essential container in task exited",
                          "containers": [{{"name": "bootstrap", "exitCode": config["exit_code"]}}]}}]}}))
    elif key == "logs get-log-events":
        token = args[args.index("--next-token") + 1] if "--next-token" in args else None
        if token is None:
            print(json.dumps({{"events": [{{"message": line}} for line in config["log"]], "nextForwardToken": "f/1"}}))
        else:
            print(json.dumps({{"events": [], "nextForwardToken": token}}))
    else:
        sys.exit("unexpected aws call: " + key)
    """)


class RunBootstrapTask(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.state, self.config = self.root / "calls.json", self.root / "config.json"
        self.configure()
        fake = self.root / "bin" / "aws"
        fake.parent.mkdir()
        fake.write_text(FAKE_AWS.format(python=sys.executable, state=str(self.state), config=str(self.config),
                                        task=TASK))
        fake.chmod(0o755)
        self.path = str(fake.parent) + os.pathsep + os.environ.get("PATH", "")

    def tearDown(self):
        self.directory.cleanup()

    def configure(self, stack=STACK, exit_code=0, log=("Production migrations and runtime grants complete",)):
        self.config.write_text(json.dumps({"stack": stack, "exit_code": exit_code, "log": list(log)}))

    def run_main(self, argv):
        out, err = io.StringIO(), io.StringIO()
        with mock.patch.dict(os.environ, {"PATH": self.path}), contextlib.redirect_stdout(out), \
                contextlib.redirect_stderr(err):
            code = runner.main(["--stack", "silicon-dm", "--poll-seconds", "0", *argv])
        calls = json.loads(self.state.read_text()) if self.state.exists() else []
        return code, out.getvalue(), err.getvalue(), calls

    def test_runs_the_command_in_the_app_security_group_and_prints_its_log(self):
        code, out, err, calls = self.run_main(["migrate"])
        self.assertEqual(code, 0, err)
        self.assertEqual([" ".join(call["args"][:2]) for call in calls],
                         ["cloudformation describe-stacks", "ecs run-task", "ecs describe-tasks",
                          "ecs describe-tasks", "logs get-log-events", "logs get-log-events"])
        run = calls[1]["args"]
        self.assertEqual(run[run.index("--network-configuration") + 1],
                         "awsvpcConfiguration={subnets=[subnet-a,subnet-b],securityGroups=[sg-app],assignPublicIp=DISABLED}")
        self.assertEqual(run[run.index("--task-definition") + 1], "arn:task-definition/silicon-dm-bootstrap:19")
        self.assertEqual(calls[1]["overrides"], {"containerOverrides": [{"name": "bootstrap", "command": ["migrate"]}]})
        self.assertIn("--log-stream-name", calls[4]["args"])
        self.assertIn("bootstrap/bootstrap/0123abcd", calls[4]["args"])
        self.assertEqual(out.strip(), "Production migrations and runtime grants complete")

    def test_passes_the_mapping_and_returns_the_task_exit_code(self):
        mapping = self.root / "mapping.csv"
        mapping.write_text("iam_principal_id,accounts_uuid\nc:saket,zQo\n")
        self.configure(exit_code=1, log=["DM bootstrap failed: dm-migrate link-identities failed with exit 1"])
        code, out, _, calls = self.run_main(["--mapping", str(mapping), "link-identities", "--dry-run"])
        self.assertEqual(code, 1)
        container = calls[1]["overrides"]["containerOverrides"][0]
        self.assertEqual(container["command"], ["link-identities", "--dry-run"])
        self.assertEqual(container["environment"],
                         [{"name": "DM_IDENTITY_MAPPING", "value": "iam_principal_id,accounts_uuid\nc:saket,zQo\n"}])
        self.assertIn("link-identities failed", out)

    def test_print_only_starts_nothing(self):
        code, out, _, calls = self.run_main(["--print-only", "status"])
        self.assertEqual(code, 0)
        self.assertEqual([call["args"][:2] for call in calls], [["cloudformation", "describe-stacks"]])
        self.assertIn("aws ecs run-task --cluster silicon-dm-production", out)
        self.assertIn('"status"', out)

    def test_refusals(self):
        good = self.root / "good.csv"
        good.write_text("iam_principal_id,accounts_uuid\nc:a,zQo\n")
        headerless = self.root / "headerless.csv"
        headerless.write_text("c:a,zQo\n")
        large = self.root / "large.csv"
        large.write_text("iam_principal_id,accounts_uuid\n" + "".join(f"si:silicon-{n:05d},u{n:05d}\n" for n in range(500)))
        cases = [
            (["link-identities"], "needs --mapping"),
            (["--mapping", str(good), "status"], "only for link-identities"),
            (["--mapping", str(headerless), "link-identities"], "must start with the header"),
            (["--mapping", str(large), "link-identities"], "Split the mapping"),
        ]
        for argv, message in cases:
            with self.subTest(argv=argv):
                code, _, err, calls = self.run_main(argv)
                self.assertEqual(code, 1)
                self.assertIn(message, err)
                self.assertFalse([call for call in calls if call["args"][:2] == ["ecs", "run-task"]])
        self.configure(stack={"Stacks": [{"Outputs": [], "Parameters": []}]})
        code, _, err, _ = self.run_main(["status"])
        self.assertEqual(code, 1)
        self.assertIn("is it DM's Fargate stack", err)


if __name__ == "__main__":
    unittest.main()
