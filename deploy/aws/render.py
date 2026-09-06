#!/usr/bin/env python3
"""Embed the reviewed bootstrap and canonical runtime grants in CloudFormation."""
from pathlib import Path

here = Path(__file__).resolve().parent
repo = here.parents[1]
bootstrap = (here / 'bootstrap.sh').read_text()
grants = (repo / 'deploy/runtime-grants.sql').read_text()
marker = "# Parse secret JSON as data."
bootstrap = bootstrap.replace(marker, "cat > /opt/silicon-dm/runtime-grants.sql <<'DM_RUNTIME_GRANTS'\n" + grants + "DM_RUNTIME_GRANTS\n\n" + marker)
# EC2 permits 16 KiB of decoded user data. Leave enough room for parameter
# substitution, which expands resource references to real ARNs and host names.
if len(bootstrap.encode()) > 14500:
    raise SystemExit('Bootstrap is too large for EC2 user data after substitution')
path = here / 'production.yaml'
source = path.read_text()
start = source.index('        UserData:')
end = source.index('  AutoScalingGroup:', start)
source = source[:start] + '        UserData:\n          Fn::Base64: !Sub |\n' + ''.join('            ' + line + '\n' for line in bootstrap.splitlines()) + '\n' + source[end:]
path.write_text(source)
print('Rendered production.yaml (' + str(len(bootstrap.encode())) + ' user-data bytes before parameter substitution)')
