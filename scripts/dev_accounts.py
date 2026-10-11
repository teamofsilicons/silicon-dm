#!/usr/bin/env python3
"""Run DM on this machine against a local Silicon Accounts stack (never a deployed one).

    scripts/dev-accounts.sh [--build] [--fresh] [--ting]    # dev_accounts.py up: start (idempotent)
    scripts/dev-accounts-stop.sh [--keep-webhook] [--drop]   # dev_accounts.py down: stop what up started
    python3 scripts/dev_accounts.py restart [--ting]         # restart dm-api (same database, secret, data key)
    python3 scripts/dev_accounts.py status                   # what runs, as JSON (never a secret)
    python3 scripts/dev_accounts.py env                      # shell exports that point the dm CLI here

`up` does, each step only when needed:
  1. checks that Silicon Accounts answers on a loopback URL;
  2. with --build, builds dm-api, dm-migrate and the dm CLI;
  3. creates the database (with --fresh: drops it first), applies DM's migrations as the database owner, and
     gives a runtime role exactly the grants of deploy/runtime-grants.sql (dm-api runs as that role, as in
     production);
  4. points DM's webhook at Silicon Accounts to http://127.0.0.1:<base+1>/webhook/ with DM's own app credentials
     (PUT /v1/apps/dm/webhook), remembering the setup it replaced so `down` can put it back;
  5. starts dm-api on 127.0.0.1:<base+1>; with --ting also a Ting stand-in on 127.0.0.1:<base+2>
     (scripts/dev_ting_stub.py) and dm-api with DM_TING_URL pointing at it;
  6. proves a test delivery (POST /v1/apps/dm/webhook/test) reaches dm-api and verifies; a stale signing secret
     is rotated (POST .../webhook/rotate-secret) and dm-api restarted with the new one.

The webhook signing secret (when the stack made a new one) and DM_DATA_KEY are kept in <dir>/dev (mode 0600,
never in git) and reach dm-api only through its environment. The data key is generated once, so sync cursors
and stored proofs stay valid across restarts.

Configuration (environment):
  DM_TEST_STACK           JSON stack file: accounts_public_url, accounts_api_url, apps.dm.app_secret,
                          apps.dm.webhook_secret_seeded, apps.interface.app_secret (the testkit's stack file shape)
  ACCOUNTS_URL            Silicon Accounts public URL, the token issuer (default: stack file, else http://localhost:9590)
  ACCOUNTS_API_URL        where DM calls Silicon Accounts (default: stack file, else ACCOUNTS_URL)
  DM_APP_SECRET           DM's app secret at that stack (default: stack file)
  DM_DEV_BASE             port block base (default 4120: web 4120, API 4121, Ting stand-in 4122)
  DM_DEV_POSTGRES_URL     server URL of a superuser (default postgres://postgres@127.0.0.1:5460)
  DM_DEV_DATABASE         database name (default dm_e2e); the runtime role is <database>_runtime
  DM_DEV_DIR              state directory (default .mig): pids/, logs/, dev/
  DM_DEV_BIN_DIR          where the binaries are (default $CARGO_TARGET_DIR/debug, else target/debug)
  DM_DEV_PROOF_ISSUERS    DM_PROOF_ISSUERS for dm-api (default: the five scopes, each from `interface`, the
                          testkit's stand-in for the Silicon Interface)
  DM_DEV_TING_APP         the app at the stack that stands in for Ting with --ting (default `interface`: the stack
                          has no `ting` app); its secret comes from the stack file
  PSQL_BIN_DIR            directory with psql (default: PATH, then Homebrew's postgresql@16)
"""
import argparse
import base64
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time
from urllib.error import HTTPError, URLError
from urllib.parse import urlparse
from urllib.request import ProxyHandler, Request, build_opener
import uuid

ROOT = Path(__file__).resolve().parents[1]
APP_ID = "dm"
WEBHOOK_PATH = "/webhook/"
LOOPBACK = {"localhost", "127.0.0.1", "::1"}
PROOF_SCOPES = ("dm.conversations.read", "dm.messages.read", "dm.messages.write", "dm.receipts.write",
                "dm.drafts.write")
OPENER = build_opener(ProxyHandler({}))


class Failure(Exception):
    """A precise reason the command cannot continue."""


def http(method, url, body=None, basic=None, bearer=None, headers=None, timeout=15):
    """One request; returns (status, parsed JSON body or None). Network errors raise Failure."""
    options = {"Accept": "application/json", **(headers or {})}
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        options["Content-Type"] = "application/json"
    if basic:
        options["Authorization"] = "Basic " + base64.b64encode(":".join(basic).encode()).decode()
    if bearer:
        options["Authorization"] = "Bearer " + bearer
    try:
        with OPENER.open(Request(url, data=data, method=method, headers=options), timeout=timeout) as response:
            status, raw = response.status, response.read()
    except HTTPError as error:
        with error:
            status, raw = error.code, error.read()
    except (URLError, OSError) as error:
        raise Failure(f"{method} {url} failed: {getattr(error, 'reason', error)}") from None
    if not raw:
        return status, None
    try:
        return status, json.loads(raw)
    except ValueError:
        return status, {"raw": raw[:300].decode(errors="replace")}


def loopback_url(name, value):
    parsed = urlparse(value)
    if parsed.scheme not in ("http", "https") or (parsed.hostname or "") not in LOOPBACK:
        raise Failure(f"{name} must be a Silicon Accounts stack on this machine (localhost or 127.0.0.1); got "
                      f"{value!r}. This script never talks to a deployed Silicon Accounts.")
    return value.rstrip("/")


def absolute(path):
    path = Path(path)
    return path if path.is_absolute() else ROOT / path


def find_psql():
    candidates = []
    if os.environ.get("PSQL_BIN_DIR"):
        candidates.append(Path(os.environ["PSQL_BIN_DIR"]) / "psql")
    found = shutil.which("psql")
    if found:
        candidates.append(Path(found))
    candidates += [Path("/opt/homebrew/opt/postgresql@16/bin/psql"), Path("/usr/local/opt/postgresql@16/bin/psql")]
    for candidate in candidates:
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return str(candidate)
    raise Failure("psql was not found: set PSQL_BIN_DIR to the directory that holds it.")


def load_config():
    env = os.environ.get
    stack_file = env("DM_TEST_STACK")
    stack = {}
    if stack_file:
        try:
            stack = json.loads(Path(stack_file).read_text())
        except (OSError, ValueError) as error:
            raise Failure(f"DM_TEST_STACK={stack_file} is not a readable JSON stack file: {error}") from None
    apps = stack.get("apps") or {}
    app = apps.get(APP_ID) or {}
    accounts_url = loopback_url("ACCOUNTS_URL", env("ACCOUNTS_URL") or stack.get("accounts_public_url")
                                or "http://localhost:9590")
    accounts_api_url = loopback_url("ACCOUNTS_API_URL", env("ACCOUNTS_API_URL") or stack.get("accounts_api_url")
                                    or accounts_url)
    app_secret = env("DM_APP_SECRET") or app.get("app_secret")
    if not app_secret:
        raise Failure("DM's app secret at the local stack is unknown: set DM_TEST_STACK to the stack file "
                      "(apps.dm.app_secret) or DM_APP_SECRET.")
    base = int(env("DM_DEV_BASE") or 4120)
    target = absolute(env("CARGO_TARGET_DIR") or "target")
    directory = absolute(env("DM_DEV_DIR") or ".mig")
    database = env("DM_DEV_DATABASE") or "dm_e2e"
    if not database.startswith("dm_") or not database.replace("_", "").isalnum():
        raise Failure(f"DM_DEV_DATABASE must be a plain name starting with dm_ (got {database!r}).")
    server = (env("DM_DEV_POSTGRES_URL") or "postgres://postgres@127.0.0.1:5460").rstrip("/")
    ting_app = env("DM_DEV_TING_APP") or "interface"
    return {
        "accounts_url": accounts_url,
        "accounts_api_url": accounts_api_url,
        "app_secret": app_secret,
        "seeded_webhook_secret": app.get("webhook_secret_seeded"),
        "ting_app": ting_app,
        "ting_app_secret": (apps.get(ting_app) or {}).get("app_secret"),
        "base": base,
        "api_port": base + 1,
        "ting_port": base + 2,
        "api_url": f"http://127.0.0.1:{base + 1}",
        "webhook_url": f"http://127.0.0.1:{base + 1}{WEBHOOK_PATH}",
        "server_url": server,
        "database": database,
        "runtime_role": f"{database}_runtime",
        "bin_dir": absolute(env("DM_DEV_BIN_DIR") or target / "debug"),
        "dir": directory,
        "proof_issuers": env("DM_DEV_PROOF_ISSUERS") or ",".join(f"{scope}=interface" for scope in PROOF_SCOPES),
    }


def paths(config):
    directory = config["dir"]
    return {
        "pids": directory / "pids",
        "logs": directory / "logs",
        "state": directory / "dev",
        "api_pid": directory / "pids" / "dev-api",
        "ting_pid": directory / "pids" / "dev-ting-stub",
        "api_log": directory / "logs" / "dev-api.log",
        "ting_log": directory / "logs" / "dev-ting-stub.log",
        "ting_calls": directory / "dev" / "ting-stub-calls.jsonl",
        "webhook_secret": directory / "dev" / "webhook-secret",
        "webhook_previous": directory / "dev" / "webhook-previous.json",
        "data_key": directory / "dev" / "data-key",
        "mode": directory / "dev" / "mode.json",
    }


def write_private(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as handle:
        handle.write(text)
    os.chmod(path, 0o600)


# ---------------------------------------------------------------------------------------------- database


def psql(config, database, sql=None, file=None, variables=None):
    """Runs psql as the superuser; returns stdout. Failures raise with psql's message."""
    url = f"{config['server_url']}/{database}"
    command = [find_psql(), "-X", "-q", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-d", url]
    for name, value in (variables or {}).items():
        command += ["-v", f"{name}={value}"]
    command += ["-c", sql] if sql is not None else ["-f", str(file)]
    done = subprocess.run(command, capture_output=True, text=True, timeout=120)
    if done.returncode != 0:
        raise Failure(f"psql on {database} failed: {done.stderr.strip()[-400:]}")
    return done.stdout.strip()


def prepare_database(config, fresh):
    name, role = config["database"], config["runtime_role"]
    exists = psql(config, "postgres", f"SELECT 1 FROM pg_database WHERE datname = '{name}'") == "1"
    if exists and fresh:
        psql(config, "postgres", f'DROP DATABASE "{name}" WITH (FORCE)')
        exists = False
        print(f"dropped database {name}")
    if not exists:
        psql(config, "postgres", f'CREATE DATABASE "{name}"')
        print(f"created database {name}")
    migrate = config["bin_dir"] / "dm-migrate"
    if not migrate.exists():
        raise Failure(f"{migrate} does not exist: build it (scripts/dev-accounts.sh --build).")
    env = service_env(config, database_url=f"{config['server_url']}/{name}")
    env["DM_LOG_FILTER"] = "error"
    done = subprocess.run([str(migrate)], env=env, capture_output=True, text=True, timeout=300, cwd=config["dir"])
    if done.returncode != 0:
        raise Failure(f"dm-migrate failed on {name}: {(done.stderr or done.stdout).strip()[-600:]}")
    psql(config, "postgres",
         f"DO $$BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN "
         f"CREATE ROLE {role} LOGIN; END IF; END$$")
    psql(config, name, file=ROOT / "deploy" / "runtime-grants.sql", variables={"runtime_role": role})
    print(f"migrations applied to {name}; {role} has the runtime grants")


# ---------------------------------------------------------------------------------------------- processes


def service_env(config, database_url=None, ting=False):
    files = paths(config)
    env = {
        "PATH": "/usr/bin:/bin",
        "HOME": str(files["state"]),
        "DM_ENVIRONMENT": "development",
        "DM_BIND_ADDR": f"127.0.0.1:{config['api_port']}",
        "DM_PUBLIC_BASE_URL": f"{config['api_url']}/api/v1",
        "DM_DATABASE_URL": database_url or (
            f"postgres://{config['runtime_role']}@{urlparse(config['server_url']).hostname}:"
            f"{urlparse(config['server_url']).port or 5432}/{config['database']}"),
        "DM_LOG_FILTER": "info,silicon_dm=debug",
        "DM_SHUTDOWN_TIMEOUT_SECONDS": "5",
        "ACCOUNTS_URL": config["accounts_url"],
        "ACCOUNTS_API_URL": config["accounts_api_url"],
        "DM_APP_ID": APP_ID,
        "DM_APP_SECRET": config["app_secret"],
        "DM_ACCOUNTS_WEBHOOK_SECRET": webhook_secret(config) or "whsec_not-configured-yet",
        "DM_DATA_KEY": data_key(config),
        "DM_PROOF_ISSUERS": config["proof_issuers"],
        # GIF routes answer provider errors without a real key; nothing else needs Giphy.
        "DM_GIPHY_API_KEY": "development-without-giphy",
    }
    if ting:
        env["DM_TING_URL"] = f"http://127.0.0.1:{config['ting_port']}"
        env["DM_TING_APP_ID"] = config["ting_app"]
    return env


def data_key(config):
    path = paths(config)["data_key"]
    if not path.exists():
        write_private(path, base64.b64encode(os.urandom(32)).decode())
    return path.read_text().strip()


def webhook_secret(config):
    path = paths(config)["webhook_secret"]
    if path.exists():
        return path.read_text().strip()
    return config["seeded_webhook_secret"]


def pid_of(path, needle):
    """The pid in `path` when that process is alive and is `needle` (a reused pid is not ours)."""
    try:
        pid = int(path.read_text().strip())
    except (OSError, ValueError):
        return None
    try:
        os.kill(pid, 0)
    except OSError:
        return None
    command = subprocess.run(["ps", "-o", "command=", "-p", str(pid)], capture_output=True, text=True).stdout
    return pid if needle in command else None


def stop_process(path, needle, label):
    pid = pid_of(path, needle)
    if pid is None:
        path.unlink(missing_ok=True)
        return False
    os.kill(pid, signal.SIGTERM)
    for _ in range(100):
        time.sleep(0.1)
        if pid_of(path, needle) is None:
            break
    else:
        os.kill(pid, signal.SIGKILL)
        print(f"{label} (pid {pid}) did not stop within 10 s; killed")
    path.unlink(missing_ok=True)
    return True


def start_process(command, env, pid_path, log_path):
    pid_path.parent.mkdir(parents=True, exist_ok=True)
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log = open(log_path, "ab")
    log.write(f"\n==== started {time.strftime('%Y-%m-%dT%H:%M:%S')} ====\n".encode())
    log.flush()
    process = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                               cwd=str(env.get("HOME", ROOT)), start_new_session=True)
    log.close()
    pid_path.write_text(str(process.pid))
    return process.pid


def wait_ready(url, seconds, log_path, label):
    deadline = time.time() + seconds
    while time.time() < deadline:
        try:
            status, _ = http("GET", url, timeout=2)
            if status in (200, 204):
                return
        except Failure:
            pass
        time.sleep(0.25)
    tail = log_path.read_text(errors="replace").splitlines()[-15:] if log_path.exists() else []
    raise Failure(f"{label} did not become ready at {url} within {seconds} s; last log lines:\n" + "\n".join(tail))


def start_api(config, ting):
    files = paths(config)
    binary = config["bin_dir"] / "dm-api"
    if not binary.exists():
        raise Failure(f"{binary} does not exist: build it (scripts/dev-accounts.sh --build).")
    files["state"].mkdir(parents=True, exist_ok=True)
    pid = start_process([str(binary)], service_env(config, ting=ting), files["api_pid"], files["api_log"])
    wait_ready(f"{config['api_url']}/ready", 60, files["api_log"], "dm-api")
    write_private(files["mode"], json.dumps({"ting": ting}))
    return pid


def start_ting_stub(config):
    files = paths(config)
    if not config["ting_app_secret"]:
        raise Failure(f"--ting needs the secret of `{config['ting_app']}` (the app standing in for Ting) in "
                      f"DM_TEST_STACK (apps.{config['ting_app']}.app_secret).")
    env = {"PATH": "/usr/bin:/bin", "HOME": str(files["state"]), "TING_STUB_APP_SECRET": config["ting_app_secret"]}
    command = [sys.executable, "-I", str(ROOT / "scripts" / "dev_ting_stub.py"), "--port", str(config["ting_port"]),
               "--accounts-api", config["accounts_api_url"], "--app", config["ting_app"], "--issuer", APP_ID,
               "--calls", str(files["ting_calls"])]
    pid = start_process(command, env, files["ting_pid"], files["ting_log"])
    wait_ready(f"http://127.0.0.1:{config['ting_port']}/control/log", 20, files["ting_log"], "the Ting stand-in")
    return pid


# ---------------------------------------------------------------------------------------------- webhook


def app_call(config, method, path, body=None, idempotent=False):
    headers = {"Idempotency-Key": f"dm-dev-{uuid.uuid4()}"} if idempotent else None
    return http(method, f"{config['accounts_api_url']}{path}", body, basic=(APP_ID, config["app_secret"]),
                headers=headers)


def point_webhook(config):
    """PUT DM's webhook URL to the local dm-api; remembers the setup it replaced. Returns the URL it replaced."""
    files = paths(config)
    status, current = app_call(config, "GET", f"/v1/apps/{APP_ID}/webhook")
    if status != 200:
        raise Failure(f"GET /v1/apps/{APP_ID}/webhook answered {status}: {current}")
    previous = current.get("url")
    if previous != config["webhook_url"] and not files["webhook_previous"].exists():
        write_private(files["webhook_previous"], json.dumps({"url": previous, "events": current.get("events")}))
    if previous == config["webhook_url"]:
        return previous
    status, saved = app_call(config, "PUT", f"/v1/apps/{APP_ID}/webhook", {"url": config["webhook_url"]},
                             idempotent=True)
    if status != 200:
        raise Failure(f"PUT /v1/apps/{APP_ID}/webhook answered {status}: {saved}")
    if saved.get("secret"):
        # The stack had no secret stored, so it made one; it is shown only now.
        write_private(files["webhook_secret"], saved["secret"])
    print(f"DM's webhook now posts to {config['webhook_url']} (was {previous})")
    return previous


def restore_webhook(config):
    files = paths(config)
    if not files["webhook_previous"].exists():
        return None
    previous = json.loads(files["webhook_previous"].read_text())
    if not previous.get("url"):
        files["webhook_previous"].unlink()
        return None
    status, saved = app_call(config, "PUT", f"/v1/apps/{APP_ID}/webhook",
                             {"url": previous["url"], "events": previous.get("events")}, idempotent=True)
    if status != 200:
        raise Failure(f"restoring DM's webhook answered {status}: {saved}")
    files["webhook_previous"].unlink()
    return previous["url"]


def prove_delivery(config):
    """Queues a ping and waits for its outcome: 'delivered', 'refused' (signature) or 'failed: …'."""
    status, queued = app_call(config, "POST", f"/v1/apps/{APP_ID}/webhook/test", {}, idempotent=True)
    if status != 202:
        raise Failure(f"POST /v1/apps/{APP_ID}/webhook/test answered {status}: {queued}")
    delivery = queued["delivery_id"]
    deadline = time.time() + 30
    last = {}
    while time.time() < deadline:
        status, last = app_call(config, "GET", f"/v1/apps/{APP_ID}/webhook/deliveries/{delivery}")
        if status == 200 and last.get("status") == "delivered":
            return "delivered"
        if status == 200 and last.get("last_status") == 401:
            return "refused"
        time.sleep(0.5)
    return f"failed: {last.get('status')} {last.get('last_status')} {str(last.get('last_error'))[:200]}"


def ensure_secret_works(config, ting):
    outcome = prove_delivery(config)
    if outcome == "refused":
        status, rotated = app_call(config, "POST", f"/v1/apps/{APP_ID}/webhook/rotate-secret", {}, idempotent=True)
        if status != 200 or not rotated.get("secret"):
            raise Failure(f"rotating DM's webhook secret answered {status}")
        write_private(paths(config)["webhook_secret"], rotated["secret"])
        print("the stored webhook secret was stale; rotated it at the stack and restarted dm-api")
        stop_process(paths(config)["api_pid"], "dm-api", "dm-api")
        start_api(config, ting)
        outcome = prove_delivery(config)
    if outcome != "delivered":
        raise Failure(f"a test delivery from Silicon Accounts did not reach dm-api: {outcome}")
    return outcome


# ---------------------------------------------------------------------------------------------- commands


def build():
    env = dict(os.environ)
    env.setdefault("CARGO_TARGET_DIR", str(ROOT / "target"))
    command = ["cargo", "build", "--locked", "-p", "silicon-dm", "--bins", "-p", "silicon-dm-cli"]
    print("$ " + " ".join(command))
    if subprocess.run(command, cwd=ROOT, env=env).returncode != 0:
        raise Failure("cargo build failed (output above).")


def current_mode(config):
    try:
        return json.loads(paths(config)["mode"].read_text()).get("ting", False)
    except (OSError, ValueError):
        return False


def up(config, args):
    files = paths(config)
    status, _ = http("GET", f"{config['accounts_api_url']}/.well-known/jwks.json")
    if status != 200:
        raise Failure(f"Silicon Accounts at {config['accounts_api_url']} did not answer (/.well-known/jwks.json: {status}).")
    if args.build:
        build()
    prepare_database(config, args.fresh)
    previous = point_webhook(config)
    started = []
    if args.ting:
        if pid_of(files["ting_pid"], "dev_ting_stub.py") is None:
            start_ting_stub(config)
            started.append("ting-stub")
    running = pid_of(files["api_pid"], "dm-api")
    if running is not None and current_mode(config) != args.ting:
        stop_process(files["api_pid"], "dm-api", "dm-api")
        running = None
    if running is None:
        start_api(config, args.ting)
        started.append("api")
    if not args.ting:
        stop_process(files["ting_pid"], "dev_ting_stub.py", "the Ting stand-in")
    outcome = ensure_secret_works(config, args.ting)
    print(json.dumps(summary(config, started=started, webhook_test=outcome, webhook_was=previous)))


def down(config, args):
    files = paths(config)
    stopped = [label for label, path, needle in (("api", files["api_pid"], "dm-api"),
                                                 ("ting-stub", files["ting_pid"], "dev_ting_stub.py"))
               if stop_process(path, needle, label)]
    restored = None if args.keep_webhook else restore_webhook(config)
    if args.drop:
        psql(config, "postgres", f'DROP DATABASE IF EXISTS "{config["database"]}" WITH (FORCE)')
        psql(config, "postgres", f"DROP ROLE IF EXISTS {config['runtime_role']}")
    print(json.dumps({"stopped": stopped, "webhook_restored_to": restored,
                      "database": "dropped" if args.drop else f"kept ({config['database']})"}))


def restart(config, args):
    files = paths(config)
    stop_process(files["api_pid"], "dm-api", "dm-api")
    if args.ting and pid_of(files["ting_pid"], "dev_ting_stub.py") is None:
        start_ting_stub(config)
    if not args.ting:
        stop_process(files["ting_pid"], "dev_ting_stub.py", "the Ting stand-in")
    start_api(config, args.ting)
    print(json.dumps(summary(config, started=["api"])))


def summary(config, **extra):
    files = paths(config)
    return {
        "api": {"pid": pid_of(files["api_pid"], "dm-api"), "url": config["api_url"], "log": str(files["api_log"])},
        "ting_stub": {"pid": pid_of(files["ting_pid"], "dev_ting_stub.py"),
                      "url": f"http://127.0.0.1:{config['ting_port']}", "stands_in_as": config["ting_app"]},
        "ting": current_mode(config),
        "database": config["database"],
        "runtime_role": config["runtime_role"],
        "accounts_url": config["accounts_url"],
        "accounts_api_url": config["accounts_api_url"],
        "webhook_url": config["webhook_url"],
        "proof_issuers": config["proof_issuers"],
        **extra,
    }


def env_command(config, _args):
    print(f"export DM_API_URL={config['api_url']}")
    print(f"export ACCOUNTS_URL={config['accounts_url']}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    commands = parser.add_subparsers(dest="command", required=True)
    up_parser = commands.add_parser("up", help="start (idempotent)")
    up_parser.add_argument("--build", action="store_true", help="cargo build the binaries first")
    up_parser.add_argument("--fresh", action="store_true", help="drop and recreate the database")
    up_parser.add_argument("--ting", action="store_true", help="deliver through a local Ting stand-in")
    down_parser = commands.add_parser("down", help="stop what up started")
    down_parser.add_argument("--keep-webhook", action="store_true", help="leave DM's webhook pointing here")
    down_parser.add_argument("--drop", action="store_true", help="also drop the database and the runtime role")
    restart_parser = commands.add_parser("restart", help="restart dm-api")
    restart_parser.add_argument("--ting", action="store_true", help="deliver through a local Ting stand-in")
    commands.add_parser("status", help="what runs, as JSON")
    commands.add_parser("env", help="shell exports for the dm CLI")
    args = parser.parse_args()
    try:
        config = load_config()
        for directory in ("pids", "logs", "state"):
            paths(config)[directory].mkdir(parents=True, exist_ok=True)
        handler = {"up": up, "down": down, "restart": restart, "env": env_command,
                   "status": lambda c, _a: print(json.dumps(summary(c)))}[args.command]
        handler(config, args)
    except Failure as error:
        print(f"dev_accounts: {error}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
