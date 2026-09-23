#!/usr/bin/env python3
"""Render a reviewed, fail-closed PostgreSQL handoff hold; preview rolls back."""
import argparse
import json
import re
import uuid
from pathlib import Path


def render(manifest, batch, backup_receipt, apply=False):
    if not re.fullmatch(r"[a-zA-Z0-9_-]{1,100}", batch):
        raise ValueError("batch must contain 1-100 letters, numbers, hyphens or underscores")
    if not backup_receipt or len(backup_receipt) > 2048 or "\x00" in backup_receipt:
        raise ValueError("a verified backup receipt is required")
    if not isinstance(manifest, list) or not manifest:
        raise ValueError("manifest must be a nonempty list")
    rows, seen = [], set()
    for item in manifest:
        identifier = str(uuid.UUID(item["handoff_id"]))
        digest = item["body_sha256"]
        if identifier in seen or not isinstance(digest, str) or not re.fullmatch(r"[a-f0-9]{64}", digest):
            raise ValueError("manifest contains a duplicate ID or invalid SHA-256")
        seen.add(identifier)
        rows.append(f"('{identifier}'::uuid,decode('{digest}','hex'))")
    quote = lambda value: "'" + value.replace("'", "''") + "'"
    sql = "BEGIN ISOLATION LEVEL SERIALIZABLE;\nSET LOCAL lock_timeout='15s';\nSET LOCAL statement_timeout='60s';\n"
    sql += "SET LOCAL standard_conforming_strings=on;\n"
    sql += f"SET LOCAL dm.cutover.batch={quote(batch)};\nSET LOCAL dm.cutover.backup={quote(backup_receipt)};\n"
    sql += "CREATE TEMP TABLE cutover_manifest(delivery_id uuid PRIMARY KEY,body_sha256 bytea NOT NULL) ON COMMIT DROP;\n"
    sql += "INSERT INTO cutover_manifest VALUES\n" + ",\n".join(rows) + ";\n"
    sql += Path(__file__).with_suffix(".sql").read_text()
    return sql + ("COMMIT;\n" if apply else "ROLLBACK; -- preview only: no changes retained\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--batch", required=True)
    parser.add_argument("--backup-receipt", required=True)
    parser.add_argument("--apply", action="store_true", help="emit COMMIT instead of preview ROLLBACK")
    args = parser.parse_args()
    print(render(json.loads(args.manifest.read_text()), args.batch, args.backup_receipt, args.apply), end="")


if __name__ == "__main__":
    main()
