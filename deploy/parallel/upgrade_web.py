#!/usr/bin/env python3
"""Replace only the Accounts website, retaining the previous container for rollback."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import time
import install_web

def upgrade(stage, source):
    if not re.fullmatch('[0-9a-f]{40}', source):
        raise ValueError('Full source revision required')
    meta = json.loads((stage / 'gateway.metadata.json').read_text())
    checks = dict(line.split(maxsplit=1)[::-1] for line in (stage / 'SHA256SUMS').read_text().splitlines())
    for name in ('gateway.tar', 'gateway.metadata.json'):
        if hashlib.sha256((stage / name).read_bytes()).hexdigest() != checks[name]:
            raise ValueError('Artifact checksum differs')
    if meta['revision'] != source or meta['platform'] != 'linux/arm64':
        raise ValueError('Unexpected source or target')
    run = install_web.run
    current = json.loads(run(['docker', 'inspect', install_web.NAME]))[0]
    if current['Name'] != '/dm-accounts-web' or current['HostConfig']['NetworkMode'] != 'host':
        raise ValueError('Unexpected container')
    if 'PORT=4316' not in current['Config']['Env'] or not current['State']['Running']:
        raise ValueError('Expected the live isolated Accounts website')
    backup = 'dm-accounts-web-before-' + str(int(time.time()))
    # Preload the image before the brief restart. Old IAM gateway never changes.
    run(['docker', 'load', '--input', str(stage / 'gateway.tar')])
    # 30 s, not docker's 10: the website cancels the Briefcase reservations of uploads under way as it stops.
    run(['docker', 'stop', '--time', '30', install_web.NAME])
    run(['docker', 'rename', install_web.NAME, backup])
    try:
        install_web.install(stage, source)
    except BaseException:
        if install_web.NAME in run(['docker', 'ps', '-a', '--format', '{{.Names}}']).splitlines():
            run(['docker', 'stop', '--time', '30', install_web.NAME])
            run(['docker', 'rename', install_web.NAME, 'dm-accounts-web-failed-' + str(int(time.time()))])
        run(['docker', 'rename', backup, install_web.NAME])
        run(['docker', 'start', install_web.NAME])
        raise
    print(json.dumps({'previous_container': backup, 'previous_image': current['Image'], 'source': source}))

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--stage', type=Path, required=True)
    parser.add_argument('--source', required=True)
    args = parser.parse_args()
    upgrade(args.stage, args.source)
