#!/usr/bin/env python3
"""Embed the reviewed bootstrap literally, without shell or Fn::Sub expansion."""
import argparse
import json
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--stack-policy", action="store_true")
args = parser.parse_args()
if args.stack_policy:
    print(json.dumps({"Statement": [
        {"Effect": "Allow", "Principal": "*", "Action": "Update:*", "Resource": "*"},
        {"Effect": "Deny", "Principal": "*", "Action": ["Update:Replace", "Update:Delete"],
         "Resource": ["LogicalResourceId/GatewayInstance", "LogicalResourceId/StateVolume",
                      "LogicalResourceId/StateAttachment"]},
    ]}, indent=2))
else:
    directory = Path(__file__).resolve().parent
    template = (directory / "gateway.yaml").read_text()
    bootstrap = (directory / "gateway-bootstrap.sh").read_text()
    marker = "'__GATEWAY_BOOTSTRAP__'"
    if template.count(marker) != 1:
        raise SystemExit("Expected exactly one gateway bootstrap marker.")
    print(template.replace(marker, json.dumps(bootstrap)))
