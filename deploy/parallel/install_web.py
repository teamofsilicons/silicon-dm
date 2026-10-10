#!/usr/bin/env python3
"""Stage the Accounts Next.js site on port 4316; preserve the IAM gateway on 4315."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import time
import urllib.request

ROOT = Path('/opt/silicon-accounts-apps/dm')
NAME = 'dm-accounts-web'


def run(args):
    result = subprocess.run(args, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(f'{args[0]} failed with exit {result.returncode}')
    return result.stdout


def install(stage, source):
    if os.geteuid() != 0 or not re.fullmatch('[a-f0-9]{40}', source):
        raise ValueError('Root and the complete reviewed source revision are required')
    if NAME in run(['docker', 'ps', '-a', '--format', '{{.Names}}']).splitlines():
        raise ValueError('New website already exists; inspect it before replacement')
    checksums = dict(line.split(maxsplit=1)[::-1] for line in (stage / 'SHA256SUMS').read_text().splitlines())
    for name in ['gateway.tar', 'gateway.metadata.json']:
        if hashlib.sha256((stage / name).read_bytes()).hexdigest() != checksums[name]:
            raise ValueError('Website artifact checksum mismatch')
    metadata = json.loads((stage / 'gateway.metadata.json').read_text())
    if metadata['revision'] != source or metadata['platform'] != 'linux/arm64':
        raise ValueError('Website build source or target differs')
    with tarfile.open(stage / 'gateway.tar') as archive:
        manifest = json.load(archive.extractfile('manifest.json'))
        config = json.load(archive.extractfile(manifest[0]['Config']))
        if config['config']['Labels']['org.opencontainers.image.revision'] != source:
            raise ValueError('Website image source differs')
        image = 'sha256:' + manifest[0]['Config'].rsplit('/', 1)[-1].removesuffix('.json')
        if image != metadata['image_id']:
            raise ValueError('Website image digest differs')
    secret = json.loads(json.loads(run(['aws', '--region', 'us-east-1', 'secretsmanager',
        'get-secret-value', '--secret-id', 'silicon-dm/accounts-production/runtime']))['SecretString'])
    if secret['app_id'] != 'dm' or secret['database'] != 'silicon_dm_accounts':
        raise ValueError('Expected the independent Accounts runtime')
    values = {'APP_ID': 'dm', 'APP_SECRET': secret['app_secret'],
              'ACCOUNTS_URL': 'https://accounts.teamofsilicons.com',
              'APP_API_URL': 'http://127.0.0.1:8081', 'PUBLIC_URL': 'https://dm.teamofsilicons.com',
              'SESSION_SECRET': secret['session_secret'], 'HOSTNAME': '0.0.0.0', 'PORT': '4316',
              'NEXT_TELEMETRY_DISABLED': '1'}
    if any('\n' in value or '\r' in value for value in values.values()):
        raise ValueError('Environment values must be single line')
    env = ROOT / 'config/web.env'
    env.write_text(''.join(key + '=' + value + '\n' for key, value in values.items()))
    env.chmod(0o600)
    run(['docker', 'load', '--input', str(stage / 'gateway.tar')])
    run(['docker', 'run', '-d', '--restart', 'unless-stopped', '--name', NAME,
         '--network', 'host', '--memory', '512m', '--pids-limit', '256', '--read-only',
         '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges',
         '--tmpfs', '/tmp:rw,noexec,nosuid,size=64m',
         '--tmpfs', '/app/.next/cache:rw,noexec,nosuid,size=64m,uid=10001,gid=10001',
         '--env-file', str(env), image])
    for _ in range(30):
        try:
            with urllib.request.urlopen('http://127.0.0.1:4316/', timeout=3) as response:
                if response.status == 200 and b'Keep the conversation going' in response.read():
                    break
        except Exception:
            pass
        time.sleep(1)
    else:
        raise RuntimeError('New website readiness failed; old gateway remains running')
    receipt = {'source_revision': source, 'image': image, 'archive_sha256': checksums['gateway.tar'],
               'port': 4316, 'container': NAME, 'local_ready': True,
               'legacy_gateway_running': 'silicon-dm-gateway' in run(['docker', 'ps', '--format', '{{.Names}}']).splitlines()}
    (ROOT / 'web-install-receipt.json').write_text(json.dumps(receipt, indent=2))
    print(json.dumps(receipt))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--stage', type=Path, required=True)
    parser.add_argument('--source', required=True)
    args = parser.parse_args()
    install(args.stage, args.source)
