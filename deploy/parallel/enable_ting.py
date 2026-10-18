#!/usr/bin/env python3
"""Replace the Accounts DM API/worker with a verified image and enable Ting.

Run on the existing production host after loading the checksum-verified image.
No schema migration or historical handoff replay is performed. Old containers,
their complete configuration and environment files are retained for rollback.
"""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time
import urllib.request

ROOT = Path('/opt/silicon-accounts-apps/dm')
NAMES = ['dm-accounts-api', 'dm-accounts-worker']
TING = 'https://backend.ting.teamofsilicons.com'


def run(*args):
    result = subprocess.run(args, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(f'{args[0]} {args[1]} failed (exit {result.returncode})')
    return result.stdout


def private_json(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')
    path.chmod(0o600)


def upgrade(image, revision):
    if os.geteuid() != 0 or not re.fullmatch(r'[a-f0-9]{40}', revision):
        raise ValueError('Root and the reviewed full revision are required')
    new = json.loads(run('docker', 'image', 'inspect', image))[0]
    if (new['Architecture'], new['Os']) != ('arm64', 'linux'):
        raise ValueError('Expected a Linux ARM64 image')
    labels = new['Config']['Labels']
    if labels['org.opencontainers.image.revision'] != revision or labels['org.opencontainers.image.version'] != '0.16.1':
        raise ValueError('Image revision/version mismatch')
    before = {name: json.loads(run('docker', 'inspect', name))[0] for name in NAMES}
    for value in before.values():
        if not value['State']['Running'] or value['HostConfig']['NetworkMode'] != 'host':
            raise ValueError('Expected both existing host-network services to be running')
        if any(m['Destination'] != '/rds-ca.pem' for m in value['Mounts']):
            raise ValueError('Unexpected mount: preserve it explicitly before proceeding')
    stamp = time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())
    backup = ROOT / ('ting-upgrade-' + stamp)
    backup.mkdir(mode=0o700)
    private_json(backup / 'containers.json', before)
    for role in ['api', 'worker']:
        shutil.copy2(ROOT / 'config' / (role + '.env'), backup / (role + '.env'))
    try:
        for name in NAMES:
            run('docker', 'stop', '--time', '30', name)
            old = name + '-before-ting-' + stamp
            run('docker', 'rename', name, old)
        for name in NAMES:
            role = name.removeprefix('dm-accounts-')
            old = before[name]
            values = dict(item.split('=', 1) for item in old['Config']['Env'])
            values['DM_TING_URL'] = TING
            values['DM_TING_APP_ID'] = 'ting'
            if any('\n' in value or '\r' in value for value in values.values()):
                raise ValueError('Environment values must be single line')
            env = ROOT / 'config' / (role + '.env')
            env.write_text(''.join(f'{key}={value}\n' for key, value in values.items()))
            env.chmod(0o600)
            run('docker', 'run', '-d', '--name', name, '--restart', 'unless-stopped',
                '--network', 'host', '--read-only', '--cap-drop', 'ALL',
                '--security-opt', 'no-new-privileges', '--pids-limit', str(old['HostConfig']['PidsLimit']),
                '--memory', str(old['HostConfig']['Memory']), '--tmpfs', '/tmp:rw,noexec,nosuid,size=64m',
                '--mount', f'type=bind,source={ROOT}/rds-ca.pem,target=/rds-ca.pem,readonly',
                '--env-file', str(env), new['Id'], 'dm-' + role)
        ready = False
        for _ in range(30):
            try:
                with urllib.request.urlopen('http://127.0.0.1:8081/ready', timeout=2) as response:
                    ready = response.status == 204
            except Exception:
                pass
            if ready:
                break
            time.sleep(1)
        if not ready or any(not json.loads(run('docker', 'inspect', name))[0]['State']['Running'] for name in NAMES):
            raise RuntimeError('New services failed readiness')
    except Exception:
        existing = set(run('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines())
        for name in reversed(NAMES):
            old = name + '-before-ting-' + stamp
            if old in existing:
                subprocess.run(['docker', 'rm', '-f', name], capture_output=True)
                run('docker', 'rename', old, name)
            role = name.removeprefix('dm-accounts-')
            shutil.copy2(backup / (role + '.env'), ROOT / 'config' / (role + '.env'))
            run('docker', 'start', name)
        raise
    receipt = {'revision': revision, 'image': new['Id'], 'ting_url': TING,
               'backup': str(backup), 'ready': ready}
    private_json(backup / 'receipt.json', receipt)
    print(json.dumps(receipt))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', required=True)
    parser.add_argument('--revision', required=True)
    args = parser.parse_args()
    upgrade(args.image, args.revision)
