#!/usr/bin/env python3
"""A stand-in for Ting on this machine, for DM's end-to-end tests (never deployed, standard library only).

It answers the two Ting calls DM makes and checks every proof DM presents with Silicon Accounts
(POST /v1/proofs/verify), using the credentials of the app that stands in for Ting at the local stack:

  POST /v1/subscriptions   Authorization: Proof sap_...   {"app_id": "dm", "for": {"uuid", "id"}}  -> 201 {"id"}
      needs a valid User verification proof from the issuer, for this app, with tings.subscribe, for that uuid
  GET  /v1/ws?protocol=v1  (WebSocket) -> {"op":"ready","protocol":"v1","receiver_id"}; then each
      {"op":"send","request_id","proof_token","body"} -> {"op":"accepted",...} or {"op":"error","error":{"code"}}
      needs a valid App verification proof from the issuer, for this app, with tings.send

Test controls (loopback only):
  POST /control/refuse {"op": "send"|"subscribe", "code": "recipient_not_registered", "count": 1}
  GET  /control/log    every call and what its proof verified as (proof id, kind, apps, account, scopes and a
                       SHA-256 prefix of the token: never the token itself)
  POST /control/reset  forget the log and planned refusals

usage: TING_STUB_APP_SECRET=... dev_ting_stub.py --port 4122 --accounts-api http://127.0.0.1:9589 \\
           --app interface --issuer dm [--calls calls.jsonl]
"""
import argparse
import base64
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
import struct
import sys
import threading
import time
from urllib.error import HTTPError, URLError
from urllib.request import ProxyHandler, Request, build_opener
import uuid

WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
OPENER = build_opener(ProxyHandler({}))
LOCK = threading.Lock()
CALLS = []
REFUSALS = []  # [{"op", "code", "count"}]
SETTINGS = {}


def digest(token):
    return hashlib.sha256(token.encode()).hexdigest()[:12]


def record(entry):
    entry = {"at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), **entry}
    with LOCK:
        CALLS.append(entry)
        if SETTINGS.get("calls"):
            with open(SETTINGS["calls"], "a") as handle:
                handle.write(json.dumps(entry) + "\n")


def planned_refusal(op):
    with LOCK:
        for refusal in REFUSALS:
            if refusal["op"] == op and refusal["count"] > 0:
                refusal["count"] -= 1
                return refusal["code"]
    return None


def verify(token):
    """What Silicon Accounts says about the proof, checked with the stand-in app's own credentials."""
    auth = base64.b64encode(f"{SETTINGS['app']}:{SETTINGS['secret']}".encode()).decode()
    request = Request(f"{SETTINGS['accounts_api']}/v1/proofs/verify", method="POST",
                      data=json.dumps({"proof_token": token}).encode(),
                      headers={"Authorization": f"Basic {auth}", "Content-Type": "application/json"})
    try:
        with OPENER.open(request, timeout=15) as response:
            return json.loads(response.read())
    except HTTPError as error:
        with error:
            return {"valid": False, "verify_status": error.code}
    except (URLError, OSError) as error:
        return {"valid": False, "verify_error": str(getattr(error, "reason", error))}


def summarize(token, verification):
    user = verification.get("user") or {}
    return {
        "token_sha256": digest(token),
        "valid": verification.get("valid") is True,
        "proof_id": verification.get("proof_id"),
        "kind": verification.get("kind"),
        "issuing_app": (verification.get("issuing_app") or {}).get("app_id"),
        "receiving_app": (verification.get("receiving_app") or {}).get("app_id"),
        "user_uuid": user.get("uuid"),
        "user_id": user.get("id"),
        "scopes": verification.get("scopes"),
        "expires_at": verification.get("expires_at"),
    }


def acceptable(proof, kind, scope):
    return (proof["valid"] and proof["kind"] == kind and proof["issuing_app"] == SETTINGS["issuer"]
            and proof["receiving_app"] == SETTINGS["app"] and scope in (proof["scopes"] or []))


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):  # one line per request on stderr (the dev log)
        sys.stderr.write("%s %s\n" % (time.strftime("%H:%M:%S"), fmt % args))

    def reply(self, status, body):
        raw = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def body(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        try:
            return json.loads(raw) if raw else {}
        except ValueError:
            return None

    def do_GET(self):
        if self.path == "/control/log":
            with LOCK:
                return self.reply(200, {"calls": list(CALLS), "refusals": [dict(r) for r in REFUSALS]})
        if self.path.split("?")[0] == "/v1/ws" and self.headers.get("Upgrade", "").lower() == "websocket":
            return self.websocket()
        return self.reply(404, {"error": {"code": "not_found"}})

    def do_POST(self):
        if self.path == "/control/refuse":
            plan = self.body() or {}
            with LOCK:
                REFUSALS.append({"op": plan.get("op", "send"), "code": plan.get("code", "recipient_not_registered"),
                                 "count": int(plan.get("count", 1))})
            return self.reply(200, {"planned": True})
        if self.path == "/control/reset":
            with LOCK:
                CALLS.clear()
                REFUSALS.clear()
            return self.reply(200, {"reset": True})
        if self.path == "/v1/subscriptions":
            return self.subscribe()
        return self.reply(404, {"error": {"code": "not_found"}})

    def subscribe(self):
        header = self.headers.get("Authorization", "")
        token = header[len("Proof "):].strip() if header.startswith("Proof ") else ""
        body = self.body() or {}
        target = (body.get("for") or {}) if isinstance(body, dict) else {}
        proof = summarize(token, verify(token)) if token else {"valid": False, "token_sha256": None}
        entry = {"op": "subscribe", "app_id": body.get("app_id"), "for": target, "proof": proof}
        refusal = planned_refusal("subscribe")
        if refusal:
            record({**entry, "answer": refusal})
            return self.reply(409, {"error": {"code": refusal}})
        if not (acceptable(proof, "user_verification", "tings.subscribe") and proof["user_uuid"] == target.get("uuid")
                and body.get("app_id") == SETTINGS["issuer"]):
            record({**entry, "answer": "invalid_proof"})
            return self.reply(401, {"error": {"code": "invalid_proof"}})
        subscription = f"sub_{uuid.uuid4().hex[:12]}"
        record({**entry, "answer": subscription})
        return self.reply(201, {"id": subscription})

    # ------------------------------------------------------------------------------------ WebSocket

    def websocket(self):
        key = self.headers.get("Sec-WebSocket-Key", "")
        accept = base64.b64encode(hashlib.sha1((key + WS_GUID).encode()).digest()).decode()
        self.send_response(101, "Switching Protocols")
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", accept)
        self.end_headers()
        self.wfile.flush()
        self.close_connection = True
        receiver = f"stub-{uuid.uuid4().hex[:8]}"
        self.send_text({"op": "ready", "protocol": "v1", "receiver_id": receiver})
        while True:
            message = self.read_message()
            if message is None:
                return
            try:
                frame = json.loads(message)
            except ValueError:
                return
            if frame.get("op") != "send":
                return
            self.send_text(self.handle_send(frame))

    def handle_send(self, frame):
        request_id, token = frame.get("request_id"), frame.get("proof_token") or ""
        try:
            body = json.loads(frame.get("body") or "{}")
        except ValueError:
            body = {}
        proof = summarize(token, verify(token)) if token else {"valid": False, "token_sha256": None}
        entry = {"op": "send", "key": body.get("key"), "type": body.get("type"), "for": body.get("for"),
                 "event": (body.get("data") or {}).get("event"), "proof": proof}
        refusal = planned_refusal("send")
        if refusal:
            record({**entry, "answer": refusal})
            return {"op": "error", "request_id": request_id, "error": {"code": refusal}}
        if not acceptable(proof, "app_verification", "tings.send"):
            record({**entry, "answer": "invalid_proof"})
            return {"op": "error", "request_id": request_id, "error": {"code": "invalid_proof"}}
        notification = f"tng_{uuid.uuid4().hex[:12]}"
        record({**entry, "answer": notification})
        return {"op": "accepted", "request_id": request_id, "status": "accepted", "key": body.get("key"),
                "id": notification, "created_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "silent": False}

    def read_exact(self, count):
        data = b""
        while len(data) < count:
            chunk = self.rfile.read(count - len(data))
            if not chunk:
                return None
            data += chunk
        return data

    def read_message(self):
        """One complete text message (control frames answered in between); None when the peer is gone."""
        parts = []
        while True:
            header = self.read_exact(2)
            if header is None:
                return None
            fin, opcode = header[0] & 0x80, header[0] & 0x0F
            length, masked = header[1] & 0x7F, header[1] & 0x80
            if length == 126:
                length = struct.unpack(">H", self.read_exact(2) or b"\0\0")[0]
            elif length == 127:
                length = struct.unpack(">Q", self.read_exact(8) or b"\0" * 8)[0]
            mask = self.read_exact(4) if masked else b"\0\0\0\0"
            payload = self.read_exact(length) if length else b""
            if mask is None or payload is None:
                return None
            payload = bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))
            if opcode == 0x8:
                return None
            if opcode == 0x9:
                self.send_frame(0xA, payload)
                continue
            if opcode == 0xA:
                continue
            parts.append(payload)
            if fin:
                return b"".join(parts).decode()

    def send_frame(self, opcode, payload):
        length = len(payload)
        if length < 126:
            header = struct.pack(">BB", 0x80 | opcode, length)
        elif length < 65536:
            header = struct.pack(">BBH", 0x80 | opcode, 126, length)
        else:
            header = struct.pack(">BBQ", 0x80 | opcode, 127, length)
        self.wfile.write(header + payload)
        self.wfile.flush()

    def send_text(self, value):
        self.send_frame(0x1, json.dumps(value).encode())


def main():
    parser = argparse.ArgumentParser(description="A local stand-in for Ting (DM end-to-end tests only).")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--accounts-api", required=True)
    parser.add_argument("--app", required=True, help="the app at the stack that stands in for Ting")
    parser.add_argument("--issuer", default="dm", help="the app whose proofs are accepted")
    parser.add_argument("--calls", help="also append every call to this JSON lines file")
    args = parser.parse_args()
    secret = os.environ.get("TING_STUB_APP_SECRET")
    if not secret:
        sys.exit("dev_ting_stub: set TING_STUB_APP_SECRET to the stand-in app's secret (never on the command line)")
    SETTINGS.update(app=args.app, secret=secret, issuer=args.issuer, calls=args.calls,
                    accounts_api=args.accounts_api.rstrip("/"))
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.daemon_threads = True
    print(f"Ting stand-in on 127.0.0.1:{args.port}, verifying proofs as `{args.app}` for issuer `{args.issuer}`",
          flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
