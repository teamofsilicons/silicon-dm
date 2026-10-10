#!/usr/bin/env python3
"""Run one bootstrap-task command as a one-off Fargate task and print what it printed.

    run-bootstrap-task.py --stack STACK [--mapping mapping.csv] [--print-only] [COMMAND ...]

    run-bootstrap-task.py --stack silicon-dm status
    run-bootstrap-task.py --stack silicon-dm                    # the full bootstrap
    run-bootstrap-task.py --stack silicon-dm draft-identity-mapping
    run-bootstrap-task.py --stack silicon-dm --mapping mapping.csv link-identities --dry-run

It reads the cluster, the bootstrap task definition and the task security group from the
stack's outputs and the private subnets from its parameters, starts the task in the app
security group (the only place that reaches the database), waits for it to stop, prints its
CloudWatch log and exits with the container's exit code. `--mapping` passes the file as
DM_IDENTITY_MAPPING. `--print-only` prints the run-task request and starts nothing.

Uses the AWS CLI with your usual credentials (AWS_PROFILE, AWS_REGION, or --profile/--region).
"""
import argparse
import json
import subprocess
import sys
import tempfile
import time

MAPPING_HEADER = "iam_principal_id,accounts_uuid"
# ECS caps a RunTask override at 8192 characters of JSON.
OVERRIDE_LIMIT = 8192
LOG_GROUP = "/silicon-dm/production/bootstrap"


class Refusal(Exception):
    """A reason to stop, printed as is."""


def aws(arguments, profile=None, region=None):
    command = ["aws", *arguments, "--output", "json"]
    if profile:
        command += ["--profile", profile]
    if region:
        command += ["--region", region]
    result = subprocess.run(command, capture_output=True, text=True, check=False)
    if result.returncode:
        raise Refusal(f"aws {' '.join(arguments[:2])} failed: {result.stderr.strip()[:2000]}")
    return json.loads(result.stdout) if result.stdout.strip() else {}


def stack_settings(stack):
    """Cluster, task definition, security group and subnets from a describe-stacks answer."""
    outputs = {item["OutputKey"]: item["OutputValue"] for item in stack.get("Outputs", [])}
    parameters = {item["ParameterKey"]: item.get("ParameterValue") for item in stack.get("Parameters", [])}
    wanted = {"cluster": outputs.get("ClusterName"), "task_definition": outputs.get("BootstrapTaskDefinitionArn"),
              "security_group": outputs.get("TaskSecurityGroupId"),
              "subnets": [parameters.get("PrivateSubnetA"), parameters.get("PrivateSubnetB")]}
    missing = [name for name, value in wanted.items() if not value or (isinstance(value, list) and not all(value))]
    if missing:
        raise Refusal(f"the stack does not name {', '.join(missing)}; is it DM's Fargate stack (deploy/aws/fargate.yaml)?")
    return wanted


def overrides(command, mapping_text=None):
    container = {"name": "bootstrap", "command": command}
    if mapping_text is not None:
        first = next((line.strip() for line in mapping_text.splitlines()
                      if line.strip() and not line.strip().startswith("#")), "")
        if first != MAPPING_HEADER:
            raise Refusal(f"the mapping must start with the header {MAPPING_HEADER}")
        container["environment"] = [{"name": "DM_IDENTITY_MAPPING", "value": mapping_text}]
    document = {"containerOverrides": [container]}
    size = len(json.dumps(document))
    if size > OVERRIDE_LIMIT:
        raise Refusal(f"the override is {size} characters; ECS accepts {OVERRIDE_LIMIT}. Split the mapping into "
                      "smaller files and run link-identities once per file: each run links its own rows and keeps "
                      "earlier links")
    return document


def run_task_arguments(settings, override_file):
    network = ("awsvpcConfiguration={subnets=[" + ",".join(settings["subnets"]) + "],securityGroups=["
               + settings["security_group"] + "],assignPublicIp=DISABLED}")
    return ["ecs", "run-task", "--cluster", settings["cluster"], "--task-definition", settings["task_definition"],
            "--launch-type", "FARGATE", "--platform-version", "1.4.0", "--started-by", "dm-cutover",
            "--network-configuration", network, "--overrides", "file://" + override_file]


def wait_until_stopped(settings, task, profile, region, poll_seconds, timeout_seconds):
    deadline = time.monotonic() + timeout_seconds
    while True:
        described = aws(["ecs", "describe-tasks", "--cluster", settings["cluster"], "--tasks", task], profile, region)
        tasks = described.get("tasks") or []
        if not tasks:
            raise Refusal(f"ECS no longer describes {task}")
        if tasks[0].get("lastStatus") == "STOPPED":
            return tasks[0]
        if time.monotonic() >= deadline:
            raise Refusal(f"{task} is still {tasks[0].get('lastStatus')} after {timeout_seconds:g} s; it keeps running "
                          f"(see {LOG_GROUP}); stop it with aws ecs stop-task if it must not")
        time.sleep(poll_seconds)


def print_log(task, profile, region):
    stream = "bootstrap/bootstrap/" + task.rsplit("/", 1)[-1]
    token = None
    while True:
        arguments = ["logs", "get-log-events", "--log-group-name", LOG_GROUP, "--log-stream-name", stream,
                     "--start-from-head"]
        if token:
            arguments += ["--next-token", token]
        try:
            page = aws(arguments, profile, region)
        except Refusal as error:
            print(f"(no log: {error})", file=sys.stderr)
            return
        for event in page.get("events", []):
            print(event.get("message", ""))
        if page.get("nextForwardToken") in (None, token):
            return
        token = page["nextForwardToken"]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--stack", required=True, help="DM's Fargate CloudFormation stack")
    parser.add_argument("--mapping", help="mapping CSV passed as DM_IDENTITY_MAPPING (link-identities)")
    parser.add_argument("--print-only", action="store_true", help="print the run-task request; start nothing")
    parser.add_argument("--profile")
    parser.add_argument("--region")
    parser.add_argument("--poll-seconds", type=float, default=10)
    parser.add_argument("--timeout-seconds", type=float, default=3600)
    parser.add_argument("command", nargs=argparse.REMAINDER, help="bootstrap-task.py command and flags")
    args = parser.parse_args(argv)
    command = [part for part in args.command if part != "--"]
    try:
        if args.mapping and (not command or command[0] != "link-identities"):
            raise Refusal("--mapping is only for link-identities")
        if command and command[0] == "link-identities" and not args.mapping:
            raise Refusal("link-identities needs --mapping mapping.csv")
        mapping_text = open(args.mapping, encoding="utf-8").read() if args.mapping else None
        document = overrides(command, mapping_text)
        stack = aws(["cloudformation", "describe-stacks", "--stack-name", args.stack], args.profile, args.region)
        settings = stack_settings((stack.get("Stacks") or [{}])[0])
        with tempfile.NamedTemporaryFile("w", suffix=".json", prefix="dm-bootstrap-override-") as handle:
            json.dump(document, handle)
            handle.flush()
            request = run_task_arguments(settings, handle.name)
            if args.print_only:
                print("aws " + " ".join(request))
                print(json.dumps(document, indent=2))
                return 0
            started = aws(request, args.profile, args.region)
        if started.get("failures") or not started.get("tasks"):
            raise Refusal(f"ECS did not start the task: {json.dumps(started.get('failures'))}")
        task = started["tasks"][0]["taskArn"]
        print(f"started {task}; waiting for it to stop", file=sys.stderr)
        stopped = wait_until_stopped(settings, task, args.profile, args.region, args.poll_seconds,
                                     args.timeout_seconds)
        print_log(task, args.profile, args.region)
        container = next((c for c in stopped.get("containers", []) if c.get("name") == "bootstrap"), {})
        code = container.get("exitCode")
        print(f"task stopped: exit {code}, {stopped.get('stoppedReason', '')}".rstrip(", "), file=sys.stderr)
        return code if isinstance(code, int) else 1
    except (Refusal, OSError, json.JSONDecodeError) as error:
        print(f"run-bootstrap-task: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
