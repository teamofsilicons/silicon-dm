#!/usr/bin/env python3
"""DM end to end against a local Silicon Accounts stack: real Carbons and Silicons, real tokens, the real binaries.

    DM_TEST_STACK=/path/to/test-stack.json SILICON_ACCOUNTS_DIR=/path/to/silicon-accounts \\
      scripts/e2e-accounts.sh [--only 1,2,...] [--keep-running] [--run N]

The eight scenarios are the functions `scenario_1` ... `scenario_8` below (tests/e2e/README.md lists them). The
script starts the local DM stack (scripts/dev-accounts.sh) when dm-api is not answering and stops what it started;
scenario 6 restarts dm-api with a local Ting stand-in and back. Every run uses new identities
(dm-e2e-<who>-<run>@example.test, si:dm-e2e-<who>-<run>), writes .mig/e2e/run-<run>/result.json and a transcript
with every token, short-lived token, STK, proof and secret redacted, and exits 1 when a check failed.

Environment: DM_TEST_STACK and SILICON_ACCOUNTS_DIR (required); CARGO_TARGET_DIR or DM_DEV_BIN_DIR (the debug
binaries); SILICON_ACCOUNTS_CLI (the stack's silicon-accounts CLI; default: built in SILICON_ACCOUNTS_DIR);
SILICON_APPS (scenario 7's packer, default: PATH). It only ever talks to loopback URLs.
"""
import argparse
import base64
import hashlib
import hmac
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tarfile
import time
import traceback
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlencode
from urllib.request import ProxyHandler, Request, build_opener

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
import dev_accounts  # noqa: E402  (scripts/dev_accounts.py: the same configuration as the dev stack)

OPENER = build_opener(ProxyHandler({}))
TOKEN_PATTERNS = [
    re.compile(r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}"),
    re.compile(r"\b(?:sar|slt|sap|sapr|stk|whsec|sa_app|sat)_[A-Za-z0-9_-]{8,}"),
]
SCOPES_FROM_INTERFACE = ["dm.conversations.read", "dm.messages.read", "dm.messages.write"]


class Failure(Exception):
    """The run cannot continue (a setup step failed); the message says why."""


class Run:
    """Checks, a redacted transcript and the secrets to redact."""

    def __init__(self, number, directory):
        self.number = number
        self.dir = directory
        self.results = []
        self.lines = []
        self.secrets = set()
        self.scenario = None
        self.homes = []

    def secret(self, *values):
        for value in values:
            if isinstance(value, str) and len(value) >= 8:
                self.secrets.add(value)

    def secrets_in(self, value):
        """Registers every token-like string in a JSON value."""
        if isinstance(value, dict):
            for key, item in value.items():
                if isinstance(item, str) and any(word in key for word in ("token", "stk", "slt", "secret")):
                    self.secret(item)
                else:
                    self.secrets_in(item)
        elif isinstance(value, list):
            for item in value:
                self.secrets_in(item)
        return value

    def redact(self, text):
        text = str(text)
        for secret in sorted(self.secrets, key=len, reverse=True):
            text = text.replace(secret, "<redacted>")
        for pattern in TOKEN_PATTERNS:
            text = pattern.sub("<redacted>", text)
        return text

    def say(self, text):
        line = self.redact(text)
        print(line, flush=True)
        self.lines.append(line)

    def section(self, number, title):
        self.scenario = number
        self.say(f"== scenario {number}: {title}")

    def check(self, name, ok, detail=""):
        detail = self.redact(detail)[:700] if detail else ""
        self.results.append({"scenario": self.scenario, "check": name, "ok": bool(ok), "detail": detail})
        self.say(("  ok  " if ok else "  FAIL ") + name + (f"  [{detail}]" if detail and not ok else ""))
        return bool(ok)

    def show(self, label, value):
        self.say(f"      {label}: {json.dumps(value, sort_keys=True)[:600]}")

    def home(self, name):
        path = self.dir / "homes" / name
        path.mkdir(parents=True, exist_ok=True)
        os.chmod(path, 0o700)
        self.homes.append(path)
        return path

    def save(self):
        failed = [result for result in self.results if not result["ok"]]
        summary = {"run": self.number, "checks": len(self.results), "failed": len(failed),
                   "results": self.results}
        (self.dir / "result.json").write_text(json.dumps(summary, indent=1))
        (self.dir / "transcript.txt").write_text("\n".join(self.lines) + "\n")
        return failed


RUN = None
CONFIG = {}


def load_config():
    env = os.environ.get
    if not env("DM_TEST_STACK") or not env("SILICON_ACCOUNTS_DIR"):
        raise Failure("set DM_TEST_STACK (the stack file) and SILICON_ACCOUNTS_DIR (a silicon-accounts checkout)")
    dev = dev_accounts.load_config()
    stack = json.loads(Path(env("DM_TEST_STACK")).read_text())
    accounts_dir = Path(env("SILICON_ACCOUNTS_DIR"))
    cli = env("SILICON_ACCOUNTS_CLI")
    if not cli:
        for candidate in (accounts_dir / "target" / "migration" / "debug" / "silicon-accounts",
                          accounts_dir / "target" / "debug" / "silicon-accounts"):
            if candidate.exists():
                cli = str(candidate)
                break
    target = dev_accounts.absolute(env("CARGO_TARGET_DIR") or "target")
    return {
        "dev": dev,
        "stack": stack,
        "accounts_dir": accounts_dir,
        "accounts_url": dev["accounts_url"],
        "accounts_api": dev["accounts_api_url"],
        "api": dev["api_url"],
        "ting_stub": f"http://127.0.0.1:{dev['ting_port']}",
        "web_redirect": f"http://localhost:{dev['base']}/auth/callback",
        "interface_redirect": "http://127.0.0.1:9593/interface/callback",
        "dm": dev["bin_dir"] / "dm",
        "release_dm": target / "release" / "dm",
        "tsx": accounts_dir / "testkit" / "node_modules" / ".bin" / "tsx",
        "mint": ROOT / "tests" / "e2e" / "mint.mts",
        "sa_cli": cli,
        "interface_secret": (stack.get("apps") or {}).get("interface", {}).get("app_secret"),
    }


# ---------------------------------------------------------------------------------------------- requests


def request(method, url, body=None, auth=None, headers=None, form=None, data=None, timeout=60):
    """Returns (status, parsed body or None, response headers)."""
    options = dict(headers or {})
    if body is not None:
        data = json.dumps(body).encode()
        options["Content-Type"] = "application/json"
    elif form is not None:
        data = urlencode(form).encode()
        options["Content-Type"] = "application/x-www-form-urlencoded"
    if auth:
        options["Authorization"] = auth
    try:
        with OPENER.open(Request(url, data=data, method=method, headers=options), timeout=timeout) as response:
            status, raw, got = response.status, response.read(), dict(response.headers)
    except HTTPError as error:
        with error:
            status, raw, got = error.code, error.read(), dict(error.headers)
    except (URLError, OSError) as error:
        raise Failure(f"{method} {url}: {getattr(error, 'reason', error)}") from None
    try:
        value = json.loads(raw) if raw else None
    except ValueError:
        value = {"raw": raw[:300].decode(errors="replace")}
    return status, value, got


def envelope_type(method, path):
    """The request envelope type DM expects (crates/protocol `http_type`)."""
    parts = path.split("?")[0].removeprefix("/api/v1/").strip("/").split("/")
    table = [
        (("POST", ["messages"]), "message.create"), (("POST", ["allow-list"]), "allow_list_add"),
        (("DELETE", ["allow-list"]), "allow_list_remove"), (("POST", ["groups"]), "create_group"),
        (("PATCH", ["groups", "*"]), "update_group"), (("DELETE", ["groups", "*", "members"]), "remove_group_members"),
        (("POST", ["groups", "*", "members"]), "invite_group_members"),
        (("DELETE", ["groups", "*", "owners"]), "remove_group_owners"),
        (("POST", ["groups", "*", "owners"]), "add_group_owners"),
        (("POST", ["groups", "*", "accept"]), "accept_group_invitation"),
        (("POST", ["groups", "*", "decline"]), "decline_group_invitation"),
        (("POST", ["groups", "*", "leave"]), "leave_group"), (("POST", ["conversations"]), "create_conversation"),
        (("POST", ["conversations", "*", "messages"]), "message.create"),
        (("PATCH", ["conversations", "*", "messages", "*"]), "message.updated"),
        (("POST", ["conversations", "*", "messages", "*", "receipts"]), "receipt"),
        (("PUT", ["conversations", "*", "draft"]), "put_draft"),
    ]
    for (verb, pattern), kind in table:
        if verb == method and len(pattern) == len(parts) and all(p in ("*", q) for p, q in zip(pattern, parts)):
            return kind
    raise Failure(f"no request type known for {method} {path}")


def api(method, path, token=None, data=None, proof=None, headers=None, key=None):
    """One DM API call; returns (status, the envelope's data or the raw body)."""
    options = {"x-dm-telemetry": "off", **(headers or {})}
    if method not in ("GET", "HEAD"):
        options["idempotency-key"] = key or f"dm-e2e-{RUN.number}-{time.time_ns()}"
    auth = f"Bearer {token}" if token else (f"Proof {proof}" if proof else None)
    body = None if data is None else {"type": envelope_type(method, path), "data": data}
    status, value, _ = request(method, CONFIG["api"] + path, body, auth, options)
    if isinstance(value, dict) and "type" in value and "data" in value:
        value = value["data"]
    return status, value


def code_of(value):
    return ((value or {}).get("error") or {}).get("code") if isinstance(value, dict) else None


def path_of(address):
    return quote(address, safe=":")


def basic(app):
    secret = CONFIG["dev"]["app_secret"] if app == "dm" else (CONFIG["stack"]["apps"].get(app) or {}).get("app_secret")
    return "Basic " + base64.b64encode(f"{app}:{secret}".encode()).decode()


def accounts(method, path, token=None, app=None, body=None, form=None, idempotent=False):
    """One Silicon Accounts call with a first-party token or an app's credentials; returns (status, body)."""
    headers = {"Idempotency-Key": f"dm-e2e-{RUN.number}-{time.time_ns()}"} if idempotent else None
    auth = f"Bearer {token}" if token else (basic(app) if app else None)
    status, value, _ = request(method, CONFIG["accounts_api"] + path, body, auth, headers, form)
    RUN.secrets_in(value)
    return status, value


def wait_for(predicate, seconds=25, interval=0.5):
    deadline = time.time() + seconds
    while True:
        value = predicate()
        if value or time.time() >= deadline:
            return value
        time.sleep(interval)


def db(sql):
    """Rows (lists of fields) from the e2e database, read as the superuser."""
    dev = CONFIG["dev"]
    done = subprocess.run([dev_accounts.find_psql(), "-X", "-q", "-A", "-t", "-F", "|", "-d",
                           f"{dev['server_url']}/{dev['database']}", "-c", sql],
                          capture_output=True, text=True, timeout=60)
    if done.returncode != 0:
        raise Failure(f"psql: {done.stderr.strip()[-300:]}")
    return [line.split("|") for line in done.stdout.splitlines() if line]


# ---------------------------------------------------------------------------------------------- processes


def mint(*args):
    env = {"PATH": f"{Path(CONFIG['tsx']).parent}:/usr/bin:/bin:/opt/homebrew/bin", "HOME": str(RUN.dir),
           "SILICON_ACCOUNTS_DIR": str(CONFIG["accounts_dir"]), "DM_TEST_STACK": os.environ["DM_TEST_STACK"]}
    for attempt in range(2):
        done = subprocess.run([str(CONFIG["tsx"]), str(CONFIG["mint"]), *args], capture_output=True, text=True,
                              env=env, timeout=240)
        if done.returncode == 0:
            return RUN.secrets_in(json.loads(done.stdout))
        if attempt == 0 and "too many" in done.stderr.lower():
            # The stack's per-network code budget is reset every 30 s by the orchestrator's janitor.
            RUN.say("      (the stack refused another email code for now; retrying in 40 s)")
            time.sleep(40)
            continue
        break
    raise Failure(f"mint {args[0]} failed: {RUN.redact(done.stderr.strip()[-800:])}")


def cli_env(home, extra=None):
    return {"PATH": "/usr/bin:/bin", "HOME": str(home), "SILICON_HOME": str(home),
            "SILICON_DM_RELAY_HOME": str(home / "relay"), "DM_API_URL": CONFIG["api"],
            "ACCOUNTS_URL": CONFIG["accounts_url"], **(extra or {})}


def dm(home, *args, stdin=None, timeout=120, binary=None, env=None):
    """Runs the dm CLI in `home`; returns (exit code, stdout, stderr)."""
    done = subprocess.run([str(binary or CONFIG["dm"]), *args], input=stdin, capture_output=True, text=True,
                          env=env or cli_env(home), timeout=timeout)
    return done.returncode, done.stdout, done.stderr


def last_json(text):
    text = (text or "").strip()
    if not text:
        return None
    try:
        return json.loads(text)
    except ValueError:
        try:
            return json.loads(text.splitlines()[-1])
        except ValueError:
            return None


def result_of(stdout):
    """The result of a relayed command (`{"response": {"state", "result"}}`)."""
    return ((last_json(stdout) or {}).get("response") or {}).get("result") or {}


def cli_error(stdout, stderr):
    """The error code of a failed command: relayed (stdout) or local (stderr)."""
    relayed = ((last_json(stdout) or {}).get("response") or {}).get("error") or {}
    local = (last_json(stderr) or {}).get("error") or {}
    return relayed.get("code") or local.get("code")


def sa(home, *args, stdin=None):
    """The local stack's silicon-accounts CLI in its own home (never the production one on this machine)."""
    if not CONFIG["sa_cli"]:
        raise Failure("the stack's silicon-accounts CLI was not found: set SILICON_ACCOUNTS_CLI")
    env = {"PATH": "/usr/bin:/bin", "HOME": str(home), "ACCOUNTS_HOME": str(home), "SILICON_HOME": str(home)}
    done = subprocess.run([CONFIG["sa_cli"], "--url", CONFIG["accounts_url"], "--home", str(home), *args],
                          input=stdin, capture_output=True, text=True, env=env, timeout=120)
    return done.returncode, done.stdout, done.stderr


def device_login(home, first_party_token, label):
    """`dm login` approved by the Carbon whose first-party token is given; returns (exit, result, stderr, code)."""
    process = subprocess.Popen([str(CONFIG["dm"]), "--json", "login", "--label", label], stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, text=True, env=cli_env(home))
    try:
        first = json.loads(process.stdout.readline())
        user_code = first.get("user_code", "")
        status, body = accounts("POST", f"/v1/device/{quote(user_code)}/approve", token=first_party_token)
        if status not in (200, 204):
            process.kill()
            raise Failure(f"device approval refused: {status} {body}")
        rest, err = process.communicate(timeout=120)
        return process.returncode, last_json(rest), err, first
    finally:
        if process.poll() is None:
            process.kill()


def stop_relays():
    for home in RUN.homes:
        try:
            dm(home, "daemon", "stop", timeout=30)
        except (subprocess.TimeoutExpired, OSError):
            pass


def dev(command, *args):
    """scripts/dev_accounts.py <command>; returns its last JSON line."""
    done = subprocess.run([sys.executable, "-I", str(ROOT / "scripts" / "dev_accounts.py"), command, *args],
                          capture_output=True, text=True, timeout=600, env=dict(os.environ))
    if done.returncode != 0:
        raise Failure(f"dev_accounts.py {command} failed: {RUN.redact(done.stderr.strip()[-800:])}")
    for line in done.stdout.splitlines():
        if not line.startswith("{"):
            RUN.say(f"      {line}")
    return last_json(done.stdout) or {}


def api_running():
    try:
        status, _, _ = request("GET", f"{CONFIG['api']}/ready", timeout=3)
        return status == 204
    except Failure:
        return False


def webhook_secret():
    return dev_accounts.webhook_secret(CONFIG["dev"])


def signed_delivery(body, secret, timestamp=None, event_id="forged"):
    """POSTs `body` (bytes) to DM's webhook signed with `secret` the way Silicon Accounts signs."""
    timestamp = str(int(time.time()) if timestamp is None else timestamp)
    headers = {"Content-Type": "application/json", "X-Accounts-Event-Id": event_id,
               "X-Accounts-Timestamp": timestamp}
    if secret is not None:
        digest = hmac.new(secret.encode(), f"{timestamp}.".encode() + body, hashlib.sha256).hexdigest()
        headers["X-Accounts-Signature"] = f"v1={digest}"
    status, value, _ = request("POST", f"{CONFIG['api']}/webhook/", headers=headers, data=body)
    return status, value


def receipts(event_id):
    return db(f"SELECT event_type, outcome FROM dm.accounts_webhook_receipts WHERE event_id = '{event_id}'")


def receipts_for(event_type, uuid):
    return [row[0] for row in db(
        f"SELECT outcome FROM dm.accounts_webhook_receipts WHERE event_type = '{event_type}' "
        f"AND account_uuid = '{uuid}' ORDER BY received_at")]


# ---------------------------------------------------------------------------------------------- identities


class Account:
    def __init__(self, **fields):
        self.__dict__.update(fields)

    def __repr__(self):
        return f"{self.id} ({self.uuid})"


def carbon(name):
    """A new Carbon signed in to the account site (first-party) and to DM through the hosted pages."""
    email = f"dm-e2e-{name}-{RUN.number}@example.test"
    first = mint("carbon", "--email", email)
    tokens = mint("app-signin", "--app", "dm", "--email", email, "--redirect", CONFIG["web_redirect"], "--exchange",
                  "--existing")["tokens"]
    return Account(name=name, email=email, uuid=first["uuid"], id=first["id"], fp=first["access_token"],
                   fp_refresh=first["refresh_token"], token=tokens["access_token"], refresh=tokens["refresh_token"])


def dm_signin_again(account):
    tokens = mint("app-signin", "--app", "dm", "--email", account.email, "--redirect", CONFIG["web_redirect"],
                  "--exchange", "--existing")["tokens"]
    account.token, account.refresh = tokens["access_token"], tokens["refresh_token"]


def silicon(name, custodian):
    status, created = accounts("POST", "/v1/me/silicons", token=custodian.fp, idempotent=True,
                               body={"id": f"si:dm-e2e-{name}-{RUN.number}", "display_name": f"dm e2e {name}"})
    if status != 201:
        raise Failure(f"creating si:dm-e2e-{name}-{RUN.number} answered {status}: {created}")
    return Account(name=name, uuid=created["silicon"]["uuid"], id=created["silicon"]["id"], stk=created["stk"],
                   custodian=custodian)


def slt(account):
    return mint("slt", "--silicon", account.id, "--stk", account.stk, "--app", "dm")["slt"]


def silicon_token(account):
    """The Silicon signs in to DM the way DM's website would: the server exchanges a short-lived token."""
    status, tokens = accounts("POST", "/v1/oauth/token", app="dm",
                              form={"grant_type": "urn:silicon:params:oauth:grant-type:slt", "slt": slt(account)})
    if status != 200:
        raise Failure(f"exchanging {account.id}'s short-lived token answered {status}: {tokens}")
    account.token, account.refresh = tokens["access_token"], tokens["refresh_token"]
    return account.token


def payload(stdout):
    """What a dm command answered: a relayed command's result, else the JSON it printed."""
    value = last_json(stdout) or {}
    if isinstance(value, dict) and "response" in value:
        return (value.get("response") or {}).get("result") or {}
    return value


def uuids(actors):
    return {actor.get("uuid") for actor in actors or [] if isinstance(actor, dict)}


# ---------------------------------------------------------------------------------------------- scenarios


def scenario_1(ids):
    RUN.section(1, "a Carbon on the API")
    c1, c2 = ids["c1"], ids["c2"]
    RUN.say(f"  c1 is {c1}, c2 is {c2}")
    s, v = api("GET", "/api/v1/accounts")
    RUN.check("discovery without a sign-in names Silicon Accounts and the app dm",
              s == 200 and v.get("app_id") == "dm" and v.get("accounts_url") == CONFIG["accounts_url"], f"{s} {v}")
    s, v = api("GET", "/api/v1/me", c1.token)
    account = (v or {}).get("account") or {}
    RUN.check("/me: the Carbon signed in on the hosted pages is admitted as itself (uuid, id, kind)",
              s == 200 and account.get("uuid") == c1.uuid and account.get("id") == c1.id
              and account.get("type") == "carbon", f"{s} {v}")
    RUN.check("/me: its name comes from DM's user base at Silicon Accounts", bool(account.get("display_name")), v)
    s, v = api("GET", "/api/v1/me")
    RUN.check("no credential -> 401", s == 401, f"{s} {v}")
    s, v = api("GET", "/api/v1/me", c1.fp)
    RUN.check("the Carbon's account-site token (another audience) -> 401 invalid_token",
              s == 401 and code_of(v) == "invalid_token", f"{s} {v}")
    s, v = api("GET", "/api/v1/me", c1.token[:-6] + ("A" * 6 if not c1.token.endswith("A" * 6) else "B" * 6))
    RUN.check("a tampered token -> 401", s == 401, f"{s} {v}")
    s, v = api("GET", "/api/v1/me", c1.token, headers={"x-dm-contract-version": "3"})
    RUN.check("contract 3 -> 406 unsupported_contract", s == 406 and code_of(v) == "unsupported_contract", f"{s} {v}")
    s, v = api("GET", "/api/v1/me", c1.token, headers={"x-org-id": "tos"})
    RUN.check("an X-Org-ID header changes nothing (there are no organizations)", s == 200, f"{s} {v}")

    text = f"hello c2, this is run {RUN.number}"
    key = f"dm-e2e-{RUN.number}-first-message"
    s, sent = api("POST", "/api/v1/messages", c1.token, {"recipient_id": c2.id, "message": text}, key=key)
    RUN.check("create: a message to another Carbon -> 202", s == 202 and sent.get("message") == text, f"{s} {sent}")
    conversation, code = sent.get("conversation_id", ""), sent.get("message-id")
    RUN.check("the new conversation is addressed by both current ids",
              set(conversation.split("::")) == {c1.id, c2.id}, conversation)
    s, again = api("POST", "/api/v1/messages", c1.token, {"recipient_id": c2.id, "message": text}, key=key)
    RUN.check("a retry with the same Idempotency-Key returns the same message",
              s in (200, 202) and again.get("message-id") == code, f"{s} {again}")
    base = f"/api/v1/conversations/{path_of(conversation)}"
    s, page = api("GET", f"{base}/messages", c1.token)
    RUN.check("list: the conversation's messages include it",
              s == 200 and [m.get("message-id") for m in page.get("items", [])].count(code) == 1, f"{s} {page}")
    s, listed = api("GET", "/api/v1/conversations", c2.token)
    RUN.check("list: the other Carbon has the conversation",
              s == 200 and any(c.get("id") == conversation for c in listed.get("items", [])), f"{s} {listed}")
    s, message = api("GET", f"{base}/messages/{code}", c2.token)
    sender = (message or {}).get("sender") or {}
    RUN.check("read: the recipient reads it, the sender shown by current id with its uuid",
              s == 200 and message.get("message") == text and sender.get("id") == c1.id
              and sender.get("uuid") == c1.uuid, f"{s} {message}")
    s, edited = api("PATCH", f"{base}/messages/{code}", c1.token, {"message": f"{text} (edited)"})
    RUN.check("update: the sender edits it and the earlier text goes to its history",
              s == 200 and edited.get("message") == f"{text} (edited)"
              and any(h.get("message") == text for h in edited.get("history", [])), f"{s} {edited}")
    s, v = api("PATCH", f"{base}/messages/{code}", c2.token, {"message": "not mine"})
    RUN.check("update: the recipient cannot edit it", s in (403, 404, 409), f"{s} {v}")
    s, deleted = api("DELETE", f"{base}/messages/{code}", c1.token)
    RUN.check("delete: the sender deletes it, leaving a tombstone without content",
              s == 200 and deleted.get("deleted_at") and deleted.get("message") is None, f"{s} {deleted}")
    s, message = api("GET", f"{base}/messages/{code}", c2.token)
    RUN.check("read after delete: the recipient sees only the tombstone",
              s == 200 and message.get("deleted_at") and message.get("message") is None
              and not message.get("history"), f"{s} {message}")

    s, draft = api("PUT", f"{base}/draft", c1.token, {"message_content": "a draft"}, headers={"if-match": "0"})
    RUN.check("draft: create with version 0 -> version 1", s == 200 and draft.get("version") == 1, f"{s} {draft}")
    s, draft = api("PUT", f"{base}/draft", c1.token, {"message_content": "a newer draft"}, headers={"if-match": "1"})
    RUN.check("draft: replace at the current version -> version 2", s == 200 and draft.get("version") == 2,
              f"{s} {draft}")
    s, conflict = api("PUT", f"{base}/draft", c1.token, {"message_content": "stale"}, headers={"if-match": "1"})
    RUN.check("draft: a stale version -> 409 draft_conflict carrying the current draft",
              s == 409 and code_of(conflict) == "draft_conflict"
              and conflict.get("message_content") == "a newer draft", f"{s} {conflict}")
    s, other = api("GET", f"{base}/draft", c2.token)
    RUN.check("draft: it is private to its author", not (s == 200 and other.get("message_content")), f"{s} {other}")
    s, v = api("DELETE", f"{base}/draft", c1.token)
    RUN.check("draft: delete -> 204", s == 204, f"{s} {v}")
    s, v = api("GET", f"{base}/draft", c1.token)
    RUN.check("draft: gone afterwards -> 404", s == 404, f"{s} {v}")

    name = f"Release notes {RUN.number}"
    s, group = api("POST", "/api/v1/groups", c1.token, {"name": name, "description": "notes", "member_ids": [c2.id]})
    details = (group or {}).get("group") or {}
    gid = (group or {}).get("id", "")
    RUN.check("group: create -> 201, a g: id, the creator its owner, c2 invited (not yet a member)",
              s == 201 and gid.startswith("g:") and uuids(details.get("owners")) == {c1.uuid}
              and c2.uuid in uuids(details.get("pending_invitations"))
              and c2.uuid not in uuids(details.get("members")), f"{s} {group}")
    s, shown = api("GET", f"/api/v1/groups/{path_of(gid)}", c1.token)
    version = ((shown or {}).get("group") or {}).get("version")
    s, updated = api("PATCH", f"/api/v1/groups/{path_of(gid)}", c1.token,
                     {"name": f"{name} (renamed)", "description": "updated"}, headers={"if-match": str(version)})
    RUN.check("group: its owner renames it at the current version",
              s == 200 and updated.get("name") == f"{name} (renamed)" and updated.get("version") == (version or 0) + 1,
              f"{s} {updated}")
    s, v = api("PATCH", f"/api/v1/groups/{path_of(gid)}", c1.token, {"name": "stale"}, headers={"if-match": str(version)})
    RUN.check("group: a stale version -> 409", s == 409, f"{s} {v}")
    s, v = api("GET", f"/api/v1/groups/{path_of(gid)}", c2.token)
    RUN.check("group: the invitee cannot read it before accepting -> 403 invitation_pending",
              s == 403 and code_of(v) == "invitation_pending", f"{s} {v}")
    s, v = api("POST", f"/api/v1/groups/{path_of(gid)}/accept", c2.token, {})
    s2, shown = api("GET", f"/api/v1/groups/{path_of(gid)}", c2.token)
    RUN.check("group: after accepting, the invitee is a member and reads it",
              s == 200 and s2 == 200 and c2.uuid in uuids(((shown or {}).get("group") or {}).get("members")),
              f"{s} {v} / {s2} {shown}")
    ids["conversation_c1_c2"] = conversation


def scenario_2(ids):
    RUN.section(2, "a Silicon on the CLI")
    s1, c1 = ids["s1"], ids["c1"]
    RUN.say(f"  s1 is {s1}, custodian {c1}")
    home = RUN.home("s1-cli")
    token = slt(s1)
    code, out, err = dm(home, "--json", "login", "--slt-stdin", stdin=token)
    login = last_json(out) or {}
    RUN.check("dm login --slt-stdin signs the Silicon in as itself, with its custodian",
              code == 0 and login.get("uuid") == s1.uuid and login.get("kind") == "silicon"
              and (login.get("custodian") or {}).get("uuid") == c1.uuid, err[-400:])
    RUN.show("login", {k: login.get(k) for k in ("uuid", "id", "kind", "method", "custodian")})
    files = [p for p in home.rglob("*") if p.is_file()]
    RUN.check("the short-lived token is in no output and no file",
              token not in out + err and all(token.encode() not in p.read_bytes() for p in files))
    config = home / ".silicon-dm" / "config.json"
    RUN.check("the sign-in file is private (0600)",
              config.exists() and config.stat().st_mode & 0o777 == 0o600, str(config))
    code, out, err = dm(home, "login", "status", "--json")
    status = last_json(out) or {}
    RUN.check("dm login status --json: authenticated and verified, with uuid, id and kind",
              code == 0 and status.get("authenticated") is True and status.get("verified") is True
              and status.get("uuid") == s1.uuid and status.get("id") == s1.id and status.get("kind") == "silicon",
              out[-400:])
    text = f"status report {RUN.number}"
    code, out, err = dm(home, "--json", "messages", "send", c1.id, "--text", text, "--wait")
    sent = payload(out)
    RUN.check("messages send: the Silicon writes to its custodian", code == 0 and sent.get("message") == text,
              (out + err)[-400:])
    message = sent.get("message-id")
    code, out, err = dm(home, "--json", "messages", "list", c1.id)
    RUN.check("messages list: the message is there",
              code == 0 and any(m.get("message-id") == message for m in payload(out).get("items", [])),
              (out + err)[-400:])
    code, out, err = dm(home, "--json", "messages", "edit", c1.id, message, "--text", f"{text} v2")
    RUN.check("messages edit: replaced", code == 0 and payload(out).get("message") == f"{text} v2",
              (out + err)[-400:])
    draft = RUN.dir / "draft.json"
    draft.write_text(json.dumps({"message_content": "next report"}))
    code, out, err = dm(home, "--json", "drafts", "put", c1.id, "--data", str(draft), "--version", "0")
    RUN.check("drafts put: its private draft is saved", code == 0 and payload(out).get("version") == 1,
              (out + err)[-400:])
    code, out, err = dm(home, "--json", "drafts", "get", c1.id)
    RUN.check("drafts get: read back", code == 0 and payload(out).get("message_content") == "next report",
              (out + err)[-400:])
    code, out, err = dm(home, "--json", "groups", "create", "--name", f"S1 notes {RUN.number}")
    group = payload(out)
    RUN.check("groups create: a group the Silicon owns",
              code == 0 and str(group.get("id", "")).startswith("g:")
              and uuids((group.get("group") or {}).get("owners")) == {s1.uuid}, (out + err)[-400:])
    ids["group_s1"] = group.get("id", "")
    code, out, err = dm(home, "--json", "messages", "send", ids["group_s1"], "--text", f"group history {RUN.number}",
                        "--wait")
    RUN.check("it writes the first message of its group", code == 0, (out + err)[-400:])
    code, out, err = dm(home, "--json", "conversations", "list")
    listed = {c.get("id") for c in payload(out).get("items", [])}
    RUN.check("conversations list: the custodian conversation and the group",
              code == 0 and ids["group_s1"] in listed and any(s1.id in str(c) and c1.id in str(c) for c in listed),
              str(listed))
    code1, out1, _ = dm(home, "iam", "--json")
    code2, out2, _ = dm(home, "accounts", "--json")
    RUN.check("the hidden `dm iam --json` prints exactly what `dm accounts --json` prints",
              code1 == 0 and code2 == 0 and last_json(out1) == last_json(out2)
              and (last_json(out2) or {}).get("app_id") == "dm", out1[-200:])
    code, out, err = dm(home, "--json", "logout")
    result = last_json(out) or {}
    RUN.check("dm logout ends the sign-in at Silicon Accounts",
              code == 0 and result.get("signed_out") is True and result.get("revoked") is True, (out + err)[-300:])
    code, out, err = dm(home, "login", "status", "--json")
    RUN.check('then dm login status --json is exactly {"authenticated":false} (exit 0)',
              code == 0 and out.strip() == '{"authenticated":false}', out.strip())
    token = slt(s1)
    code, out, err = dm(home, "--json", "login", token)
    RUN.check("dm login <slt> (the form Silicon runtimes run) signs in again",
              code == 0 and (last_json(out) or {}).get("uuid") == s1.uuid, err[-300:])
    ids["s1_home"] = home


def scenario_3(ids):
    RUN.section(3, "a Carbon on the CLI with the device flow")
    c1, s1 = ids["c1"], ids["s1"]
    home = RUN.home("c1-cli")
    code, result, err, first = device_login(home, c1.fp, f"dm e2e {RUN.number}")
    RUN.check("dm login prints the code and the approval page first",
              first.get("event") == "device_code" and bool(first.get("user_code"))
              and str(first.get("verification_uri", "")).startswith(CONFIG["accounts_url"]),
              json.dumps({k: first.get(k) for k in ("event", "verification_uri")}))
    RUN.check("once the Carbon approves it, the CLI is signed in as that Carbon",
              code == 0 and (result or {}).get("uuid") == c1.uuid and result.get("kind") == "carbon"
              and result.get("method") == "device", err[-300:])
    code, out, err = dm(home, "--json", "messages", "list", s1.id)
    texts = [m.get("message") for m in payload(out).get("items", [])]
    RUN.check("one command: the Carbon reads its Silicon's message",
              code == 0 and f"status report {RUN.number} v2" in texts, (out + err)[-300:])
    code, out, err = dm(home, "login", "status", "--json")
    status = last_json(out) or {}
    RUN.check("login status lists the Silicon it looks after",
              status.get("verified") is True and s1.uuid in uuids(status.get("custodian_of")), out[-300:])
    ids["c1_home"] = home


def allowed(value):
    return {((item or {}).get("account") or {}).get("uuid") for item in (value or {}).get("items", [])}


def scenario_4(ids):
    RUN.section(4, "the custodian rule, sharing by id, and Silicons only reached by accounts they allowed")
    c1, c2, s1, s2 = ids["c1"], ids["c2"], ids["s1"], ids["s2"]
    home, gid = ids["s1_home"], ids["group_s1"]
    group, history = f"/api/v1/groups/{path_of(gid)}", f"/api/v1/conversations/{path_of(gid)}/messages"
    texts = lambda value: [m.get("message") for m in (value or {}).get("items", [])]  # noqa: E731

    s, v = api("GET", f"/api/v1/conversations?for={quote(s1.id)}", c1.token)
    RUN.check("the custodian lists its Silicon's conversations (?for=si:...), its group included",
              s == 200 and gid in [c.get("id") for c in v.get("items", [])], f"{s} {v}")
    code, out, err = dm(ids["c1_home"], "--json", "conversations", "list", "--for", s1.id)
    RUN.check("... and from its CLI (dm conversations list --for)",
              code == 0 and gid in [c.get("id") for c in payload(out).get("items", [])], (out + err)[-300:])
    s, v = api("GET", group, c1.token)
    RUN.check("the custodian reads the group its Silicon owns", s == 200 and v.get("id") == gid, f"{s} {v}")
    s, v = api("GET", history, c1.token)
    RUN.check("... and its history", s == 200 and f"group history {RUN.number}" in texts(v), f"{s} {v}")
    version = ((api("GET", group, c1.token)[1] or {}).get("group") or {}).get("version")
    s, v = api("PATCH", group, c1.token, {"name": f"S1 notes {RUN.number}", "description": "kept by its custodian"},
               headers={"if-match": str(version)})
    RUN.check("the custodian manages it as an owner (changes its description)",
              s == 200 and v.get("description") == "kept by its custodian", f"{s} {v}")
    s, v = api("POST", history, c1.token, {"message": "written as the Silicon?"})
    RUN.check("but never acts as its Silicon: writing there -> 403 custodian_read_only",
              s == 403 and code_of(v) == "custodian_read_only", f"{s} {v}")

    s, v = api("GET", group, c2.token)
    RUN.check("an unrelated Carbon cannot see the group -> 404", s == 404, f"{s} {v}")
    s, v = api("GET", history, c2.token)
    RUN.check("... nor its messages -> 404", s == 404, f"{s} {v}")
    s, v = api("GET", f"/api/v1/conversations?for={quote(s1.id)}", c2.token)
    RUN.check("... nor list the Silicon's conversations -> 403 not_custodian",
              s == 403 and code_of(v) == "not_custodian", f"{s} {v}")

    code, out, err = dm(home, "--json", "groups", "invite", gid, "--member", c2.id)
    pending = uuids((payload(out) or {}).get("pending_invitations"))
    RUN.check("the Silicon shares its group with c2 by id (dm groups invite): an invitation",
              code == 0 and c2.uuid in pending, (out + err)[-400:])
    s, v = api("GET", "/api/v1/invitations", c2.token)
    RUN.check("c2 sees the invitation", s == 200 and gid in [i.get("conversation_id") for i in v.get("items", [])],
              f"{s} {v}")
    s, v = api("POST", f"{group}/accept", c2.token, {})
    RUN.check("c2 accepts it", s == 200, f"{s} {v}")
    s, v = api("GET", history, c2.token)
    RUN.check("c2 now reads the group, its whole history included",
              s == 200 and f"group history {RUN.number}" in texts(v), f"{s} {v}")
    code, out, err = dm(home, "--json", "groups", "remove", gid, "--member", c2.id)
    RUN.check("the Silicon unshares it (dm groups remove)", code == 0, (out + err)[-400:])
    s, v = api("GET", group, c2.token)
    RUN.check("c2 cannot see the group any more -> 404", s == 404, f"{s} {v}")
    s, v = api("GET", history, c2.token)
    RUN.check("... nor its messages -> 404", s == 404, f"{s} {v}")

    s, v = api("POST", "/api/v1/messages", c2.token, {"recipient_id": s1.id, "message": "hello stranger"})
    RUN.check("c2 cannot start a conversation with the Silicon -> 403 contact_not_allowed",
              s == 403 and code_of(v) == "contact_not_allowed", f"{s} {v}")
    s, v = api("POST", "/api/v1/groups", c2.token, {"name": f"C2 room {RUN.number}", "member_ids": [s1.id]})
    RUN.check("... nor invite it to a group -> 403 contact_not_allowed",
              s == 403 and code_of(v) == "contact_not_allowed", f"{s} {v}")
    silicon_token(s2)
    s, v = api("POST", "/api/v1/messages", s2.token, {"recipient_id": s1.id, "message": f"hi from s2 {RUN.number}"})
    RUN.check("another Silicon of the same custodian reaches it without being allowed", s == 202, f"{s} {v}")

    code, out, err = dm(home, "--json", "allow", "add", c2.id)
    RUN.check("the Silicon allows c2 (dm allow add)", code == 0 and c2.uuid in allowed(payload(out)),
              (out + err)[-400:])
    s, v = api("GET", f"/api/v1/allow-list?for={quote(s1.id)}", c1.token)
    RUN.check("its custodian sees the list (?for=si:...)", s == 200 and c2.uuid in allowed(v), f"{s} {v}")
    s, v = api("POST", "/api/v1/messages", c2.token, {"recipient_id": s1.id, "message": f"hello s1 {RUN.number}"})
    RUN.check("now c2 can start a conversation with it", s == 202, f"{s} {v}")
    s, v = api("POST", "/api/v1/groups", c2.token, {"name": f"C2 room {RUN.number}", "member_ids": [s1.id]})
    RUN.check("and invite it to a group (the Silicon still accepts or declines)",
              s == 201 and s1.uuid in uuids(((v or {}).get("group") or {}).get("pending_invitations")), f"{s} {v}")
    code, out, err = dm(home, "--json", "allow", "remove", c2.id)
    RUN.check("the Silicon removes c2 from its list (dm allow remove)",
              code == 0 and c2.uuid not in allowed(payload(out)), (out + err)[-400:])
    s, v = api("POST", "/api/v1/groups", c2.token, {"name": f"C2 second room {RUN.number}", "member_ids": [s1.id]})
    RUN.check("a new invitation from c2 is refused again -> 403 contact_not_allowed",
              s == 403 and code_of(v) == "contact_not_allowed", f"{s} {v}")
    s, v = api("POST", "/api/v1/messages", c2.token, {"recipient_id": s1.id, "message": "still there?"})
    RUN.check("the conversation they already have keeps working (the list governs new contact)", s == 202,
              f"{s} {v}")
    s, v = api("POST", f"/api/v1/allow-list?for={quote(s1.id)}", c1.token, {"account_ids": [c2.id]})
    RUN.check("the custodian allows an account for its Silicon", s == 200 and c2.uuid in allowed(v), f"{s} {v}")
    s, v = api("DELETE", f"/api/v1/allow-list?for={quote(s1.id)}", c1.token, {"account_ids": [c2.id]})
    RUN.check("... and removes it", s == 200 and c2.uuid not in allowed(v), f"{s} {v}")
    s, v = api("POST", f"/api/v1/allow-list?for={quote(s1.id)}", c2.token, {"account_ids": [c2.id]})
    RUN.check("an unrelated Carbon cannot allow itself -> 403 not_custodian",
              s == 403 and code_of(v) == "not_custodian", f"{s} {v}")


def delivery_of(event_type, uuid):
    status, page = accounts("GET", "/v1/apps/dm/webhook/deliveries?limit=100", app="dm")
    if status != 200:
        return None
    return next((d for d in page.get("items", []) if d.get("type") == event_type and d.get("account_uuid") == uuid),
                None)


def delivery(delivery_id):
    return accounts("GET", f"/v1/apps/dm/webhook/deliveries/{delivery_id}", app="dm")[1] or {}


def answered_again(delivery_id, attempts_before):
    """The delivery was attempted again (a replay adds an attempt) and DM answered 2xx."""
    current = delivery(delivery_id)
    attempts = current.get("attempts") or []
    return len(attempts) > attempts_before and 200 <= (attempts[-1].get("status_code") or 0) < 300


def scenario_5(ids):
    RUN.section(5, "Silicon Accounts webhooks")
    c1, c2, s1 = ids["c1"], ids["c2"], ids["s1"]
    before = silicon_token(s1)
    old_id, new_id = s1.id, f"si:dm-e2e-s1x-{RUN.number}"
    s, v = accounts("POST", f"/v1/me/silicons/{s1.uuid}/id", token=c1.fp, body={"id": new_id}, idempotent=True)
    RUN.check("the custodian changes its Silicon's id at Silicon Accounts", s == 200 and v.get("id") == new_id,
              f"{s} {v}")
    s1.id = new_id
    got = wait_for(lambda: receipts_for("account.id_changed", s1.uuid))
    RUN.check("account.id_changed reaches DM and is applied once", got == ["applied"], got)
    s, v = api("GET", "/api/v1/me", before)
    RUN.check("DM shows the new id, even to a token minted with the old one",
              s == 200 and (v.get("account") or {}).get("id") == new_id, f"{s} {v}")
    address = f"{c1.id}::{new_id}"
    s, v = api("GET", "/api/v1/conversations", c1.token)
    RUN.check("the custodian's conversation is now addressed by the new id",
              s == 200 and address in [c.get("id") for c in v.get("items", [])], f"{s} {v}")
    s, v = api("GET", f"/api/v1/conversations/{path_of(address)}/messages", c1.token)
    RUN.check("... and keeps its history", s == 200 and len(v.get("items", [])) >= 1, f"{s} {v}")
    s, v = api("GET", f"/api/v1/conversations/{path_of(c1.id + '::' + old_id)}/messages", c1.token)
    RUN.check("the old address no longer names it", s in (400, 404), f"{s} {v}")
    code, out, err = dm(ids["s1_home"], "login", "status", "--json")
    RUN.check("the Silicon's CLI shows its new id", (last_json(out) or {}).get("id") == new_id, out[-300:])

    found = delivery_of("account.id_changed", s1.uuid)
    RUN.check("Silicon Accounts lists the delivery", bool(found), "not found")
    if found:
        attempts = len(delivery(found["id"]).get("attempts") or [])
        s, v = accounts("POST", "/v1/apps/dm/webhook/replay", app="dm", body={"delivery_ids": [found["id"]]},
                        idempotent=True)
        RUN.check("Silicon Accounts replays it (the same event_id)", s == 200 and found["id"] in v.get("replayed", []),
                  f"{s} {v}")
        again = wait_for(lambda: answered_again(found["id"], attempts))
        RUN.check("DM acknowledges the replayed delivery (2xx)", again, delivery(found["id"]))
        RUN.check("... and ignores it: the event is recorded and applied exactly once",
                  receipts(found["event_id"]) == [["account.id_changed", "applied"]], receipts(found["event_id"]))
        ids["replayable"] = found

    secret = webhook_secret()
    now = time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime())
    forged_id = f"0192f00d-0000-7000-8000-{RUN.number:0>12}"
    forged = json.dumps({"app_id": "dm", "type": "account.id_changed", "event_id": forged_id, "occurred_at": now,
                         "silicon": None, "data": {"uuid": c1.uuid, "membership_id": f"dm:{c1.uuid}",
                                                   "kind": "carbon", "old_id": c1.id,
                                                   "new_id": f"c:forged-{RUN.number}"}}).encode()
    s, v = signed_delivery(forged, "whsec_" + "x" * 43, event_id=forged_id)
    RUN.check("a delivery signed with another secret -> 401", s == 401, f"{s} {v}")
    s, v = signed_delivery(forged, None, event_id=forged_id)
    RUN.check("an unsigned delivery -> 401", s == 401, f"{s} {v}")
    s, v = signed_delivery(forged, secret, timestamp=int(time.time()) - 600, event_id=forged_id)
    RUN.check("a correctly signed delivery from 10 minutes ago -> 401", s == 401, f"{s} {v}")
    s, v = signed_delivery(b'{"hello": "world"}', secret)
    RUN.check("a correctly signed body that is not an event -> 400", s == 400, f"{s} {v}")
    s, v = api("GET", "/api/v1/me", c1.token)
    RUN.check("nothing forged was applied or recorded",
              (v.get("account") or {}).get("id") == c1.id and receipts(forged_id) == [], f"{s} {v}")

    name = f"Ada Renamed {RUN.number}"
    s, v = accounts("PATCH", "/v1/me", token=c1.fp, body={"display_name": name}, idempotent=True)
    RUN.check("c1 renames itself at Silicon Accounts", s == 200, f"{s} {v}")
    got = wait_for(lambda: (api("GET", "/api/v1/me", c1.token)[1].get("account") or {}).get("display_name") == name)
    RUN.check("account.updated reaches DM: the new name shows", got, api("GET", "/api/v1/me", c1.token)[1])

    sa_home = RUN.home("s1-silicon-accounts")
    code, out, err = sa(sa_home, "login", "--silicon", s1.id, "--stk-stdin", stdin=s1.stk)
    RUN.check("the Silicon signs in to the silicon-accounts CLI with its STK", code == 0, (out + err)[-300:])
    code, out, err = sa(sa_home, "login", "--app", "dm", "-q")
    token = out.strip()
    RUN.secret(token)
    cli_home = RUN.home("s1-cli-again")
    code, out, err = dm(cli_home, "--json", "login", "--slt-stdin", stdin=token)
    RUN.check("`silicon-accounts login --app dm -q | dm login --slt-stdin` signs it in to DM",
              code == 0 and (last_json(out) or {}).get("uuid") == s1.uuid, (out + err)[-300:])
    removed_token = silicon_token(s1)
    s, v = api("GET", "/api/v1/me", removed_token)
    RUN.check("its access token works before it removes DM", s == 200, f"{s} {v}")
    code, out, err = sa(sa_home, "--json", "apps", "remove", "dm")
    RUN.check("the Silicon removes DM (silicon-accounts apps remove dm)", code == 0, (out + err)[-300:])
    got = wait_for(lambda: receipts_for("membership.access_removed", s1.uuid))
    RUN.check("membership.access_removed reaches DM", got == ["applied"], got)
    s, v = api("GET", "/api/v1/me", removed_token)
    RUN.check("its earlier access token is refused afterwards -> 401 session_revoked",
              s == 401 and code_of(v) == "session_revoked", f"{s} {v}")
    code, out, err = dm(cli_home, "login", "status", "--json")
    RUN.check("its CLI sign-in has ended: dm login status --json says signed out (exit 0)",
              code == 0 and (last_json(out) or {}).get("authenticated") is False, out[-300:])
    code, out, err = dm(ids["s1_home"], "--json", "whoami")
    RUN.check("its other CLI home is refused too, saying why", code != 0 and bool(cli_error(out, err)),
              (out + err)[-300:])
    ids["removed_token"] = removed_token
    s, v = api("GET", "/api/v1/me", silicon_token(s1))
    RUN.check("signing in to DM again works (access comes back with a new sign-in)", s == 200, f"{s} {v}")

    s, created = api("POST", "/api/v1/groups", c2.token, {"name": f"C2 desk {RUN.number}"})
    gid = (created or {}).get("id", "")
    version = ((created or {}).get("group") or {}).get("version", 1)
    s, v = api("PATCH", f"/api/v1/groups/{path_of(gid)}", c2.token, {"name": f"C2 desk {RUN.number} a"},
               headers={"if-match": str(version)})
    RUN.check("a group change (checked live at Silicon Accounts) works while c2 is signed in", s == 200, f"{s} {v}")
    s, v = accounts("POST", "/v1/oauth/revoke", app="dm", form={"token": c2.refresh, "token_type_hint": "refresh_token"})
    RUN.check("DM's website signs c2 out (revokes its refresh token)", s in (200, 204), f"{s} {v}")
    got = wait_for(lambda: receipts_for("membership.signed_out", c2.uuid))
    RUN.check("membership.signed_out (app_revoked) reaches DM, which ends nothing else for it", got == ["ignored"], got)
    s, v = api("PATCH", f"/api/v1/groups/{path_of(gid)}", c2.token, {"name": f"C2 desk {RUN.number} b"},
               headers={"if-match": str(version + 1)})
    RUN.check("the next change checked live is refused at once -> 401 session_revoked",
              s == 401 and code_of(v) == "session_revoked", f"{s} {v}")
    dm_signin_again(c2)


def stub(method, path, body=None):
    return request(method, CONFIG["ting_stub"] + path, body)[1] or {}


def stub_calls(op=None):
    return [call for call in stub("GET", "/control/log").get("calls", []) if op is None or call.get("op") == op]


def issue_proof(subject_token, receiving_app, scopes):
    return accounts("POST", "/v1/proofs/user-verification", app="interface", idempotent=True,
                    body={"subject_token": subject_token, "receiving_app": receiving_app, "scopes": scopes})


def scenario_6(ids):
    RUN.section(6, "proofs: DM accepting User verification from interface, and DM proving itself to Ting")
    c1, c2, s1, s2 = ids["c1"], ids["c2"], ids["s1"], ids["s2"]
    if not getattr(s2, "token", None):
        silicon_token(s2)
    interface = mint("app-signin", "--app", "interface", "--email", c1.email, "--redirect",
                     CONFIG["interface_redirect"], "--exchange", "--existing")["tokens"]
    s, issued = issue_proof(interface["access_token"], "dm", SCOPES_FROM_INTERFACE)
    RUN.check("interface (signed in as c1) obtains a User verification proof for dm",
              s in (200, 201) and bool((issued or {}).get("proof_token")), f"{s} {issued}")
    proof, proof_id = issued.get("proof_token"), issued.get("proof_id")
    address = f"{c1.id}::{s1.id}"
    s, v = api("GET", "/api/v1/conversations", proof=proof)
    RUN.check("DM accepts it and acts as c1: c1's conversations (dm.conversations.read)",
              s == 200 and address in [c.get("id") for c in v.get("items", [])], f"{s} {v}")
    s, v = api("GET", f"/api/v1/conversations/{path_of(address)}/messages", proof=proof)
    RUN.check("... their messages (dm.messages.read)", s == 200 and len(v.get("items", [])) >= 1, f"{s} {v}")
    s, sent = api("POST", "/api/v1/messages", proof=proof,
                  data={"recipient_id": c2.id, "message": f"sent through interface {RUN.number}"})
    RUN.check("... sends as c1, recording the app it came through (dm.messages.write)",
              s == 202 and ((sent or {}).get("sender") or {}).get("uuid") == c1.uuid
              and sent.get("via_app") == "interface", f"{s} {sent}")
    s, v = api("GET", "/api/v1/sync?reset=true", proof=proof)
    RUN.check("... and follows the sync feed (dm.messages.read)", s == 200 and bool(v.get("cursor")), f"{s} {v}")
    s, v = api("PUT", f"/api/v1/conversations/{path_of(address)}/draft", proof=proof,
               data={"message_content": "x"}, headers={"if-match": "0"})
    RUN.check("a scope it was not given (dm.drafts.write) -> 403 proof_scope_missing",
              s == 403 and code_of(v) == "proof_scope_missing", f"{s} {v}")
    for path in ("/api/v1/allow-list", "/api/v1/me"):
        s, v = api("GET", path, proof=proof)
        RUN.check(f"a route that needs the account's own sign-in ({path}) -> 403 proof_not_accepted",
                  s == 403 and code_of(v) == "proof_not_accepted", f"{s} {v}")
    s, other = issue_proof(interface["access_token"], "remind", ["dm.messages.read"])
    status2, v = api("GET", "/api/v1/conversations", proof=(other or {}).get("proof_token"))
    RUN.check("a proof made for another receiving app (remind) -> 401 invalid_proof",
              s in (200, 201) and status2 == 401 and code_of(v) == "invalid_proof", f"{s} / {status2} {v}")
    s, app_proof = accounts("POST", "/v1/proofs/app-verification", app="interface", idempotent=True,
                            body={"receiving_app": "dm", "scopes": ["dm.messages.read"]})
    status2, v = api("GET", "/api/v1/conversations", proof=(app_proof or {}).get("proof_token"))
    RUN.check("an App verification proof (an app speaking for no account) -> 403 proof_not_accepted",
              s in (200, 201) and status2 == 403 and code_of(v) == "proof_not_accepted", f"{s} / {status2} {v}")
    s, v = api("GET", "/api/v1/conversations", proof="sap_not-a-real-proof-at-all")
    RUN.check("a made-up proof -> 401 invalid_proof", s == 401 and code_of(v) == "invalid_proof", f"{s} {v}")
    s, refreshed = accounts("POST", "/v1/proofs/refresh", app="interface",
                            body={"proof_refresh_token": issued.get("proof_refresh_token")})
    RUN.check("interface refreshes the proof (the same proof, a new token)",
              s == 200 and refreshed.get("proof_id") == proof_id and refreshed.get("proof_token") != proof, f"{s}")
    s, v = api("GET", "/api/v1/conversations", proof=refreshed.get("proof_token"))
    RUN.check("DM accepts the refreshed proof", s == 200, f"{s} {v}")
    s, v = accounts("POST", "/v1/proofs/revoke", app="interface", body={"proof_id": proof_id})
    RUN.check("interface revokes the proof", s == 204, f"{s} {v}")
    started = time.time()
    refused = wait_for(lambda: api("GET", "/api/v1/conversations", proof=refreshed.get("proof_token"))[0] == 401,
                       seconds=45, interval=2)
    RUN.check(f"DM refuses the revoked proof once its 30-second verification cache has passed "
              f"({time.time() - started:.0f} s)", refused)
    for extra in (other, app_proof):
        if (extra or {}).get("proof_id"):
            accounts("POST", "/v1/proofs/revoke", app="interface", body={"proof_id": extra["proof_id"]})

    info = dev("restart", "--ting")
    ting_app = CONFIG["dev"]["ting_app"]
    RUN.check(f"dm-api restarted with DM_TING_URL at the local Ting stand-in (Ting's app id here: {ting_app})",
              info.get("ting") is True and bool((info.get("ting_stub") or {}).get("pid")), info)
    stub("POST", "/control/reset")
    for account in (c1, s2):
        api("GET", "/api/v1/me", account.token)
    subscriptions = wait_for(lambda: {c["for"].get("uuid") for c in stub_calls("subscribe")} >= {c1.uuid, s2.uuid})
    calls = stub_calls("subscribe")
    first = next((c for c in calls if c["for"].get("uuid") == c1.uuid), {})
    proof_seen = first.get("proof") or {}
    RUN.check("DM enrols each account at Ting on its next request, with a User verification proof Silicon Accounts "
              "confirms (issuer dm, receiver Ting's app, that account, subscriptions.register)",
              subscriptions and proof_seen.get("valid") and proof_seen.get("kind") == "user_verification"
              and proof_seen.get("issuing_app") == "dm" and proof_seen.get("receiving_app") == ting_app
              and proof_seen.get("user_uuid") == c1.uuid and proof_seen.get("scopes") == ["subscriptions.register"]
              and str(first.get("answer", "")).startswith("sub_"), calls)
    s, v = api("GET", "/api/v1/me", c1.token)
    RUN.check("/me reports the account enrolled", s == 200 and (v.get("delivery") or {}).get("enrolled") is True,
              f"{s} {v.get('delivery') if isinstance(v, dict) else v}")
    stub("POST", "/control/refuse", {"op": "send", "code": "recipient_not_registered", "count": 1})
    s, sent = api("POST", "/api/v1/messages", s2.token, {"recipient_id": c1.id, "message": f"via ting {RUN.number}"})
    accepted = wait_for(lambda: [c for c in stub_calls("send") if str(c.get("answer", "")).startswith("tng_")],
                        seconds=40)
    sends = stub_calls("send")
    refusal = next((c for c in sends if c.get("answer") == "recipient_not_registered"), {})
    good = accepted[0]["proof"] if accepted else {}
    RUN.check("the message's handoffs reach Ting with DM's App verification proof, confirmed by Silicon Accounts "
              "(issuer dm, receiver Ting's app, tings.send)",
              s == 202 and bool(accepted) and good.get("valid") and good.get("kind") == "app_verification"
              and good.get("issuing_app") == "dm" and good.get("receiving_app") == ting_app
              and good.get("scopes") == ["tings.send"], sends)
    target = (refusal.get("for") or {}).get("uuid")
    renewed = wait_for(lambda: [c for c in stub_calls("subscribe") if c["for"].get("uuid") == target], seconds=20)
    RUN.check("Ting said one recipient was not registered; DM re-enrolled it by refreshing the stored proof "
              "(same proof id, new token, still valid)",
              bool(refusal) and len(renewed) >= 2
              and renewed[-1]["proof"]["proof_id"] == renewed[0]["proof"]["proof_id"]
              and renewed[-1]["proof"]["token_sha256"] != renewed[0]["proof"]["token_sha256"]
              and renewed[-1]["proof"]["valid"] is True, renewed)
    retried = wait_for(lambda: [c for c in stub_calls("send") if c.get("key") == refusal.get("key")
                                and str(c.get("answer", "")).startswith("tng_")], seconds=40)
    RUN.check("the refused handoff is retried and accepted", bool(retried), stub_calls("send"))
    handoffs = ("SELECT count(*) FILTER (WHERE h.accepted_at IS NOT NULL), count(*) FROM dm.ting_handoffs h "
                "JOIN dm.messages m ON m.conversation_id = h.conversation_id AND m.sequence = h.message_sequence "
                f"WHERE m.text_content = 'via ting {RUN.number}' AND h.event = 'message.created'")
    rows = wait_for(lambda: (lambda r: r if r and r[0][0] == r[0][1] else None)(db(handoffs)), seconds=30)
    RUN.check("DM records every handoff of that message as accepted by Ting",
              bool(rows) and int(rows[0][1]) >= 1, rows or db(handoffs))
    before_ids = {c["proof"]["proof_id"] for c in stub_calls("send") if str(c.get("answer", "")).startswith("tng_")}
    stub("POST", "/control/refuse", {"op": "send", "code": "proof_expired", "count": 1})
    api("POST", "/api/v1/messages", s2.token, {"recipient_id": c1.id, "message": f"via ting again {RUN.number}"})
    after = wait_for(lambda: [c for c in stub_calls("send") if str(c.get("answer", "")).startswith("tng_")
                              and c["proof"]["proof_id"] not in before_ids], seconds=40)
    RUN.check("after Ting refused DM's proof as expired, DM sends with a newly issued App verification proof",
              bool(after) and after[0]["proof"]["valid"] is True, stub_calls("send")[-4:])
    info = dev("restart")
    RUN.check("dm-api restarted without Ting (the default)", info.get("ting") is False, info)


def native_target():
    machine = os.uname().machine
    system = sys.platform
    if system == "darwin":
        return "macos-aarch64" if machine == "arm64" else "macos-x86_64"
    return "linux-aarch64" if machine in ("aarch64", "arm64") else "linux-x86_64"


def isolated_packer():
    """silicon-apps for local-only commands, with an empty home and a server address nothing listens on."""
    for candidate in (os.environ.get("SILICON_APPS"), shutil.which("silicon-apps"),
                      str(Path.home() / ".cargo" / "bin" / "silicon-apps"),
                      str(Path.home() / ".apps" / "bin" / "silicon-apps")):
        if candidate and Path(candidate).is_file():
            return candidate
    return None


def scenario_7(ids):
    RUN.section(7, "the Silicon Apps discovery commands from a packaged archive, in an empty home")
    env = dict(os.environ, CARGO_TARGET_DIR=str(CONFIG["release_dm"].parent.parent))
    done = subprocess.run(["cargo", "build", "--release", "--locked", "-p", "silicon-dm-cli"], cwd=ROOT, env=env,
                          capture_output=True, text=True, timeout=3000)
    RUN.check("the release CLI builds", done.returncode == 0, done.stderr[-500:])
    version = re.search(r'^version = "([^"]+)"', (ROOT / "crates" / "cli" / "Cargo.toml").read_text(), re.M).group(1)
    target, output = native_target(), RUN.dir / "dist"
    done = subprocess.run([str(ROOT / "scripts" / "package-apps.sh"), version, target, str(CONFIG["release_dm"]),
                           "--output-dir", str(output), "--discovery", "require"],
                          capture_output=True, text=True, timeout=900, cwd=ROOT)
    RUN.check(f"scripts/package-apps.sh {version} {target} packs it (validate, pack, discovery required)",
              done.returncode == 0, (done.stdout + done.stderr)[-700:])
    archive = output / f"dm-{version}-{target}.tar.gz"
    if not archive.exists():
        RUN.check("the archive exists", False, str(archive))
        return
    with tarfile.open(archive) as tar:
        names = sorted(member.name.removeprefix("./") for member in tar.getmembers() if member.isfile())
        extracted = RUN.dir / "extracted"
        extracted.mkdir(exist_ok=True)
        tar.extractall(extracted, filter="data")
    RUN.check("the archive holds exactly apps.yaml and bin/dm", names == ["apps.yaml", "bin/dm"], names)
    manifest = (extracted / "apps.yaml").read_text()
    RUN.check("apps.yaml names app dm, command dm, this version and this target only",
              "app_id: dm" in manifest and "command: dm" in manifest and f"version: {version}" in manifest
              and manifest.count("binary:") == 1 and f"{target}:" in manifest, manifest)
    packer = isolated_packer()
    if packer:
        apps_home = RUN.dir / "apps-home"
        apps_home.mkdir(exist_ok=True)
        clean = {k: v for k, v in os.environ.items() if k not in ("APPS_TOKEN", "APPS_URL", "ACCOUNTS_URL",
                                                                   "SILICON_HOME")}
        clean["SILICON_APPS_NO_DAEMON"] = "1"
        done = subprocess.run([packer, "validate", str(archive), "--home", str(apps_home), "--server",
                               "http://127.0.0.1:9", "--json"], capture_output=True, text=True, env=clean, timeout=300)
        RUN.check("silicon-apps validate accepts the archive (run locally, no server)",
                  done.returncode == 0 and (last_json(done.stdout) or {}).get("valid") is True,
                  (done.stdout + done.stderr)[-400:])
    empty = RUN.dir / "empty-home"
    empty.mkdir(exist_ok=True)
    clean = {"PATH": "/usr/bin:/bin", "HOME": str(empty), "SILICON_HOME": str(empty)}
    binary = extracted / "bin" / "dm"
    code, out, err = dm(empty, "--help", binary=binary, env=clean)
    RUN.check("dm --help: exit 0 with the help text", code == 0 and "dm login" in out, f"exit {code}")
    code, out, err = dm(empty, "accounts", "--json", binary=binary, env=clean)
    discovery = last_json(out) or {}
    RUN.check('dm accounts --json: exit 0, "app_id":"dm", production Silicon Accounts, this version',
              code == 0 and discovery.get("app_id") == "dm"
              and discovery.get("accounts_url") == "https://accounts.teamofsilicons.com"
              and discovery.get("version") == version and '"app_id":"dm"' in out.replace(" ", ""), out[-300:])
    code, out, err = dm(empty, "login", "status", "--json", binary=binary, env=clean)
    RUN.check('dm login status --json: exit 0, exactly {"authenticated":false}',
              code == 0 and out.strip() == '{"authenticated":false}', out.strip())
    code, alias, err = dm(empty, "iam", "--json", binary=binary, env=clean)
    RUN.check("the hidden transition alias dm iam --json prints the same object",
              code == 0 and last_json(alias) == discovery, alias[-200:])
    RUN.check("nothing was written to the empty home", list(empty.iterdir()) == [], str(list(empty.iterdir())))


def scenario_8(ids):
    RUN.section(8, "restart safety")
    c1, s2 = ids["c1"], ids["s2"]
    if not getattr(s2, "token", None):
        silicon_token(s2)
    s, page = api("GET", "/api/v1/sync?reset=true", c1.token)
    cursor = (page or {}).get("cursor", "")
    before = (dev("status").get("api") or {}).get("pid")
    info = dev("restart")
    after = (info.get("api") or {}).get("pid")
    RUN.check("dm-api restarted (a new process, the same database)", bool(after) and after != before,
              f"{before} -> {after}")
    s, v = api("GET", "/api/v1/me", c1.token)
    RUN.check("an access token issued before the restart still works (stateless verification)", s == 200,
              f"{s} {v}")
    code, out, err = dm(ids["c1_home"], "login", "status", "--json")
    RUN.check("the CLI's saved sign-in still verifies", (last_json(out) or {}).get("verified") is True, out[-300:])
    code, out, err = dm(ids["c1_home"], "--json", "conversations", "list")
    RUN.check("... and runs commands", code == 0 and len(payload(out).get("items", [])) >= 1, (out + err)[-300:])
    s, v = api("POST", "/api/v1/messages", s2.token, {"recipient_id": c1.id, "message": f"after restart {RUN.number}"})
    status2, events = api("GET", f"/api/v1/sync?cursor={quote(cursor)}&wait=10", c1.token)
    RUN.check("a sync cursor from before the restart still works and brings the new message",
              s == 202 and status2 == 200 and len((events or {}).get("events", (events or {}).get("items", []))) >= 1,
              f"{s} / {status2} {events}")
    s, v = api("GET", "/api/v1/me", ids.get("removed_token", ""))
    RUN.check("the Silicon that removed DM stays refused (the cutoff is stored, not in memory)",
              s == 401 and code_of(v) == "session_revoked", f"{s} {v}")
    found = ids.get("replayable")
    if found:
        attempts = len(delivery(found["id"]).get("attempts") or [])
        s, v = accounts("POST", "/v1/apps/dm/webhook/replay", app="dm", body={"delivery_ids": [found["id"]]},
                        idempotent=True)
        again = wait_for(lambda: answered_again(found["id"], attempts))
        RUN.check("an event seen before the restart, replayed after it, is acknowledged and not applied again",
                  s == 200 and again and receipts(found["event_id"]) == [["account.id_changed", "applied"]],
                  f"{s} {v} {receipts(found['event_id'])}")
    s, v = signed_delivery(b'{"type":"ping","event_id":"forged-after-restart","data":{}}', "whsec_" + "y" * 43)
    RUN.check("a forged delivery is still refused -> 401", s == 401, f"{s} {v}")


PREREQUISITES = {1: [], 2: [], 3: [2], 4: [2, 3], 5: [2], 6: [2], 7: [], 8: [2, 3, 5]}
SCENARIOS = {1: scenario_1, 2: scenario_2, 3: scenario_3, 4: scenario_4, 5: scenario_5, 6: scenario_6,
             7: scenario_7, 8: scenario_8}


def main():
    global RUN, CONFIG
    parser = argparse.ArgumentParser(description="DM end to end against a local Silicon Accounts stack.")
    parser.add_argument("--only", help="comma-separated scenario numbers (their prerequisites run too)")
    parser.add_argument("--keep-running", action="store_true", help="leave the dev stack running afterwards")
    parser.add_argument("--run", help="run number used in identities (default: from the clock)")
    args = parser.parse_args()
    number = args.run or str(int(time.time()))[-6:]
    RUN = Run(number, ROOT / ".mig" / "e2e" / f"run-{number}")
    RUN.dir.mkdir(parents=True, exist_ok=True)
    try:
        CONFIG = load_config()
    except (Failure, dev_accounts.Failure) as error:
        print(f"accounts_e2e: {error}", file=sys.stderr)
        sys.exit(2)
    RUN.secret(CONFIG["dev"]["app_secret"], CONFIG["interface_secret"], CONFIG["dev"]["ting_app_secret"])
    selected = set(SCENARIOS)
    if args.only:
        wanted = {int(part) for part in args.only.split(",") if part.strip()}
        selected = set(wanted)
        for scenario in wanted:
            selected.update(PREREQUISITES[scenario])
    started = []
    try:
        was_running = api_running()
        info = dev("up")
        RUN.secret(webhook_secret())
        started = info.get("started", []) if not was_running else []
        RUN.say(f"run {number}: dm-api {info['api']['url']} (pid {info['api']['pid']}), webhook test "
                f"{info.get('webhook_test')}, scenarios {sorted(selected)}")
        ids = {"c1": carbon("c1"), "c2": carbon("c2")}
        ids["s1"], ids["s2"] = silicon("s1", ids["c1"]), silicon("s2", ids["c1"])
        RUN.say(f"identities: c1 {ids['c1']}, c2 {ids['c2']}, s1 {ids['s1']}, s2 {ids['s2']} (custodian c1)")
        for scenario in sorted(selected):
            try:
                SCENARIOS[scenario](ids)
            except Exception as error:  # noqa: BLE001 - a broken scenario is a failed check, the run goes on
                RUN.check(f"scenario {scenario} ran to its end", False,
                          f"{type(error).__name__}: {error}\n{traceback.format_exc()[-1500:]}")
    except Failure as error:
        RUN.check("setup", False, str(error))
    finally:
        stop_relays()
        if started and not args.keep_running:
            try:
                dev("down")
            except Failure as error:
                RUN.say(f"stopping the dev stack failed: {error}")
        failed = RUN.save()
        RUN.say(f"\n{len(RUN.results) - len(failed)}/{len(RUN.results)} checks passed; "
                f"result: {RUN.dir / 'result.json'}")
        (RUN.dir / "transcript.txt").write_text("\n".join(RUN.lines) + "\n")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
