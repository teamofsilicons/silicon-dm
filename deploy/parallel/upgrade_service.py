#!/usr/bin/env python3
"""Upgrade the Accounts DM API and worker to a verified 0.17 image: migrate, grant, and turn speech on.

Run on the production host as root after `docker load` of the checksum-verified backend archive. The old services
stop before the migrations run, so no 0.16 process ever writes beside a 0.17 one; 0.17's migrations (0043 onwards)
only add tables, columns, functions and triggers that 0.16 never touches, so rolling the code back afterwards is safe.
The old containers keep their names with a suffix, and their complete configuration and environment files are saved
next to the receipt, for rollback.

Speech: DM_DEEPGRAM_API_KEY comes from the runtime secret's `deepgram_api_key` field (Deepgram voices and
transcription). It is written only into the two 0600 environment files, never printed.
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
SECRET_ID = 'silicon-dm/accounts-production/runtime'
VERSION = '0.17.1'


def run(*args, stdin=None, env=None):
    result = subprocess.run(args, capture_output=True, text=True, input=stdin, env=env)
    if result.returncode:
        # The command's own words, without anything that could carry a secret (env files and passwords never appear
        # in argv; psql and dm-migrate print no credentials).
        detail = (result.stderr or result.stdout).strip().splitlines()[-3:]
        raise RuntimeError(f'{args[0]} {args[1]} failed (exit {result.returncode}): ' + ' | '.join(detail))
    return result.stdout


def private_write(path, text):
    path.write_text(text)
    path.chmod(0o600)


def environment(old_env, secret):
    """The new service environment: the running one, plus speech."""
    values = dict(item.split('=', 1) for item in old_env)
    key = secret.get('deepgram_api_key', '')
    if not re.fullmatch(r'[A-Za-z0-9_.-]{20,200}', key):
        raise ValueError('The runtime secret has no usable deepgram_api_key')
    values['DM_DEEPGRAM_API_KEY'] = key
    # 0.17 holds up to three LISTEN connections per process outside the pool and batches its reads; 8 leaves room for a
    # burst of sends next to the long polls (the instance allows far more than two processes ask for).
    values['DM_DATABASE_MAX_CONNECTIONS'] = str(max(int(values.get('DM_DATABASE_MAX_CONNECTIONS', '4') or 4), 8))
    if any('\n' in value or '\r' in value or '\0' in value for value in values.values()):
        raise ValueError('Environment values must be single line')
    return values


def ready(url, check, attempts=60):
    for _ in range(attempts):
        try:
            with urllib.request.urlopen(url, timeout=3) as response:
                if check(response):
                    return True
        except Exception:
            pass
        time.sleep(1)
    return False


def upgrade(image, revision, memory, grants):
    if os.geteuid() != 0 or not re.fullmatch(r'[a-f0-9]{40}', revision):
        raise ValueError('Root and the reviewed full revision are required')
    if not re.fullmatch(r'[0-9]+m', memory):
        raise ValueError('Memory must look like 768m')
    new = json.loads(run('docker', 'image', 'inspect', image))[0]
    if (new['Architecture'], new['Os']) != ('arm64', 'linux'):
        raise ValueError('Expected a Linux ARM64 image')
    labels = new['Config']['Labels']
    if labels.get('org.opencontainers.image.revision') != revision or labels.get('org.opencontainers.image.version') != VERSION:
        raise ValueError('Image revision or version mismatch')
    before = {name: json.loads(run('docker', 'inspect', name))[0] for name in NAMES}
    for value in before.values():
        if not value['State']['Running'] or value['HostConfig']['NetworkMode'] != 'host':
            raise ValueError('Expected both existing host-network services to be running')
        if any(m['Destination'] != '/rds-ca.pem' for m in value['Mounts']):
            raise ValueError('Unexpected mount: preserve it explicitly before proceeding')
    secret = json.loads(json.loads(run('aws', '--region', 'us-east-1', 'secretsmanager', 'get-secret-value',
                                       '--secret-id', SECRET_ID))['SecretString'])
    if secret.get('app_id') != 'dm' or secret.get('database') != 'silicon_dm_accounts':
        raise ValueError('Expected the independent Accounts runtime')
    envs = {name: environment(before[name]['Config']['Env'], secret) for name in NAMES}

    stamp = time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())
    backup = ROOT / ('best-chat-upgrade-' + stamp)
    backup.mkdir(mode=0o700)
    private_write(backup / 'containers.json', json.dumps(before, indent=2) + '\n')
    for role in ['api', 'worker', 'migration']:
        shutil.copy2(ROOT / 'config' / (role + '.env'), backup / (role + '.env'))
    common = ['--network', 'host', '--read-only', '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges',
              '--tmpfs', '/tmp:rw,noexec,nosuid,size=64m',
              '--mount', f'type=bind,source={ROOT}/rds-ca.pem,target=/rds-ca.pem,readonly']
    stopped = []
    try:
        for name in NAMES:
            run('docker', 'stop', '--time', '30', name)
            run('docker', 'rename', name, name + '-before-0.17-' + stamp)
            stopped.append(name)
        migration = run('docker', 'run', '--rm', *common, '--env-file', str(ROOT / 'config' / 'migration.env'),
                        new['Id'], 'dm-migrate')
        private_write(backup / 'migration.log', migration)
        owner = 'dm_accounts_migrator'
        pg_env = {**os.environ, 'PGHOST': secret['database_host'], 'PGDATABASE': secret['database'], 'PGUSER': owner,
                  'PGPASSWORD': secret['role_passwords'][owner], 'PGSSLMODE': 'verify-full', 'PGSSLROOTCERT': '/rds-ca.pem'}
        client = next(x for x in run('docker', 'images', '--format', '{{.Repository}}:{{.Tag}}').splitlines()
                      if x.startswith('postgres:') and '<none>' not in x)
        psql = ['docker', 'run', '--rm', '-i', '--network', 'host', '--read-only', '--cap-drop', 'ALL',
                '--mount', f'type=bind,source={ROOT}/rds-ca.pem,target=/rds-ca.pem,readonly']
        for key in ['PGHOST', 'PGDATABASE', 'PGUSER', 'PGPASSWORD', 'PGSSLMODE', 'PGSSLROOTCERT']:
            psql += ['-e', key]
        psql += [client, 'psql', '-X', '-q', '-v', 'ON_ERROR_STOP=1', '-v', 'runtime_role=dm_accounts_runtime']
        run(*psql, stdin=grants.read_text(), env=pg_env)
        for name in NAMES:
            role = name.removeprefix('dm-accounts-')
            env = ROOT / 'config' / (role + '.env')
            private_write(env, ''.join(f'{key}={value}\n' for key, value in envs[name].items()))
            run('docker', 'run', '-d', '--name', name, '--restart', 'unless-stopped', *common,
                '--pids-limit', str(before[name]['HostConfig']['PidsLimit'] or 256), '--memory', memory,
                '--env-file', str(env), new['Id'], 'dm-' + role)
        live = ready('http://127.0.0.1:8081/ready', lambda r: r.status == 204)
        speech = ready('http://127.0.0.1:8081/api/v1/voices',
                       lambda r: r.status == 200 and json.loads(r.read())['data']['enabled'] is True, attempts=10)
        running = all(json.loads(run('docker', 'inspect', name))[0]['State']['Running'] for name in NAMES)
        if not (live and speech and running):
            raise RuntimeError(f'New services failed readiness (ready={live}, speech={speech}, running={running})')
    except Exception:
        existing = set(run('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines())
        for name in reversed(NAMES):
            old = name + '-before-0.17-' + stamp
            if old in existing:
                subprocess.run(['docker', 'rm', '-f', name], capture_output=True)
                run('docker', 'rename', old, name)
            role = name.removeprefix('dm-accounts-')
            shutil.copy2(backup / (role + '.env'), ROOT / 'config' / (role + '.env'))
            if name in stopped:
                run('docker', 'start', name)
        raise
    receipt = {'revision': revision, 'version': VERSION, 'image': new['Id'], 'memory': memory,
               'previous_image': before[NAMES[0]]['Image'], 'backup': str(backup), 'ready': True, 'speech': True}
    private_write(backup / 'receipt.json', json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', required=True)
    parser.add_argument('--revision', required=True)
    parser.add_argument('--memory', default='768m')
    parser.add_argument('--grants', type=Path, required=True)
    args = parser.parse_args()
    upgrade(args.image, args.revision, args.memory, args.grants)
