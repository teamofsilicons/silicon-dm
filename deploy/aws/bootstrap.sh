#!/bin/bash
set -euo pipefail
umask 077
export AWS_DEFAULT_REGION='${AWS::Region}'
APP_SECRET_ARN='${AppSecretArn}'
DB_SECRET_ARN='${Database.MasterUserSecret.SecretArn}'
TEST_DB_SECRET_ARN='${TestingDatabase.MasterUserSecret.SecretArn}'
export DB_HOST='${Database.Endpoint.Address}'
export TEST_DB_HOST='${TestingDatabase.Endpoint.Address}'
export BACKEND_IMAGE='${BackendImageUri}'
export PUBLIC_HOST='${PublicHostName}'
export IAM_BASE_URL='${IamBaseUrl}'
STACK_ID='${AWS::StackId}'
SIGNAL_ID=$(hostname)
cleanup() {
  result=$?
  rm -f /etc/silicon-dm/{app,master,test-master}.json /etc/silicon-dm/migration.env
  status=FAILURE
  if [ "$result" -eq 0 ]; then status=SUCCESS; fi
  aws cloudformation signal-resource --stack-name "$STACK_ID" --logical-resource-id AutoScalingGroup \
    --unique-id "$SIGNAL_ID" --status "$status" || true
  exit "$result"
}
trap cleanup EXIT

dnf install --assumeyes docker jq postgresql15 python3
systemctl enable --now docker amazon-ssm-agent
install -d -m 0700 /etc/silicon-dm /opt/silicon-dm
curl --fail --silent --show-error --location --retry 3 \
  https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem \
  --output /opt/silicon-dm/aws-rds-global-bundle.pem
chmod 0644 /opt/silicon-dm/aws-rds-global-bundle.pem

# Runtime containers do not need AWS credentials. Keep IMDS inaccessible from
# their bridge network; the host Docker log driver still uses the instance role.
cat > /etc/systemd/system/silicon-dm-container-network.service <<'UNIT'
[Unit]
Description=Block DM container access to EC2 metadata
Requires=docker.service
After=docker.service
PartOf=docker.service
Before=silicon-dm-api.service silicon-dm-worker.service
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/bin/sh -c '/usr/sbin/iptables -C DOCKER-USER -d 169.254.169.254/32 -j DROP 2>/dev/null || /usr/sbin/iptables -I DOCKER-USER -d 169.254.169.254/32 -j DROP'
[Install]
WantedBy=multi-user.target
UNIT

aws ecr get-login-password | docker login --username AWS --password-stdin '${AWS::AccountId}.dkr.ecr.${AWS::Region}.amazonaws.com'
docker pull "$BACKEND_IMAGE"
aws secretsmanager get-secret-value --secret-id "$APP_SECRET_ARN" --query SecretString --output text > /etc/silicon-dm/app.json
aws secretsmanager get-secret-value --secret-id "$DB_SECRET_ARN" --query SecretString --output text > /etc/silicon-dm/master.json
aws secretsmanager get-secret-value --secret-id "$TEST_DB_SECRET_ARN" --query SecretString --output text > /etc/silicon-dm/test-master.json

# Parse secret JSON as data. Credentials never appear in shell commands, user
# data, process arguments, or logs. Passwords are URL encoded for SQLx.
python3 <<'PYTHON'
import base64, json, os, pathlib, subprocess, urllib.parse
root = pathlib.Path('/etc/silicon-dm')
app = json.loads((root / 'app.json').read_text())
required = ['DM_IAM_APP_ID', 'DM_IAM_APP_SECRET', 'DM_IAM_WEBHOOK_SECRET',
            'DM_GIPHY_API_KEY', 'DM_TEST_KEY_ENCRYPTION_KEY',
            'DM_RUNTIME_DATABASE_PASSWORD', 'DM_TEST_DATABASE_PASSWORD']
for key in required:
    value = app.get(key)
    if not isinstance(value, str) or not value.strip() or any(c in value for c in '\r\n\x00'):
        raise SystemExit('Missing or invalid Secrets Manager field: ' + key)
    if value.startswith('REPLACE_'):
        raise SystemExit('Incomplete Secrets Manager field: ' + key)
try:
    key_bytes = base64.b64decode(app['DM_TEST_KEY_ENCRYPTION_KEY'], validate=True)
except ValueError:
    raise SystemExit('DM_TEST_KEY_ENCRYPTION_KEY is not valid base64') from None
if len(key_bytes) != 32:
    raise SystemExit('DM_TEST_KEY_ENCRYPTION_KEY must encode 32 bytes')
version = app.get('DM_IAM_WEBHOOK_KEY_VERSION')
if type(version) is not int or version < 1:
    raise SystemExit('DM_IAM_WEBHOOK_KEY_VERSION must be a positive integer')
ca = '/opt/silicon-dm/aws-rds-global-bundle.pem'
ssl = 'sslmode=verify-full&sslrootcert=' + ca

def database_url(user, password, host, database):
    return 'postgresql://' + urllib.parse.quote(user, safe='') + ':' + urllib.parse.quote(password, safe='') + '@' + host + ':5432/' + database + '?' + ssl

def write_env(filename, values):
    path = root / filename
    path.write_text(''.join(k + '=' + str(v) + '\n' for k, v in values.items()))
    path.chmod(0o600)

def configure_database(master_file, host, database, role, password, testing=False):
    credentials = json.loads((root / master_file).read_text())
    environment = dict(os.environ, PGHOST=host, PGPORT='5432', PGDATABASE=database,
        PGUSER=credentials['username'], PGPASSWORD=credentials['password'],
        PGSSLMODE='verify-full', PGSSLROOTCERT=ca, DM_ROLE_PASSWORD=password)
    # RDS reports available before the template can launch this instance. A
    # real authenticated TLS query proves readiness, unlike pg_isready alone.
    subprocess.run(['psql', '-X', '--set', 'ON_ERROR_STOP=1', '--command', 'SELECT 1'],
                   env=environment, check=True)
    sql = r"""\getenv role_password DM_ROLE_PASSWORD
BEGIN;
DO $roles$
BEGIN
  IF pg_catalog.to_regrole('%s') IS NULL THEN
    CREATE ROLE %s LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOINHERIT NOBYPASSRLS;
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = '%s'
      AND rolcanlogin
      AND NOT (rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls OR rolinherit)
  ) THEN
    RAISE EXCEPTION 'DM database role does not have the required restricted attributes';
  END IF;
END;
$roles$;
ALTER ROLE %s PASSWORD :'role_password';
REVOKE ALL ON DATABASE %s FROM PUBLIC;
GRANT CONNECT ON DATABASE %s TO %s;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
""" % (role, role, role, role, database, database, role)
    if testing:
        sql += 'GRANT CREATE ON DATABASE ' + database + ' TO ' + role + ';\n'
    sql += 'COMMIT;\n'
    subprocess.run(['psql', '-X', '--set', 'ON_ERROR_STOP=1'], input=sql, text=True,
                   env=environment, check=True)
    if not testing:
        write_env('migration.env', {
            'DM_ENVIRONMENT': 'production',
            'DM_DATABASE_URL': database_url(credentials['username'], credentials['password'], host, database),
            'DM_DATABASE_MAX_CONNECTIONS': 2,
            'DM_DATABASE_ACQUIRE_TIMEOUT_SECONDS': 15,
            'DM_DATABASE_STATEMENT_TIMEOUT_SECONDS': 300,
            'DM_LOG_FILTER': 'silicon_dm=info',
        })
        subprocess.run(['docker', 'run', '--rm', '--read-only', '--cap-drop', 'ALL',
            '--security-opt', 'no-new-privileges', '--tmpfs', '/tmp:size=16m,mode=1777',
            '--volume', ca + ':' + ca + ':ro', '--env-file', str(root / 'migration.env'),
            os.environ['BACKEND_IMAGE'], 'dm-migrate'], check=True)
        subprocess.run(['psql', '-X', '--set', 'ON_ERROR_STOP=1', '--set', 'runtime_role=' + role,
                        '--file', '/opt/silicon-dm/runtime-grants.sql'], env=environment, check=True)

configure_database('master.json', os.environ['DB_HOST'], 'silicon_dm', 'dm_runtime', app['DM_RUNTIME_DATABASE_PASSWORD'])
# Test schemas migrate lazily through their isolated environment lifecycle.
configure_database('test-master.json', os.environ['TEST_DB_HOST'], 'silicon_dm_test', 'dm_testing', app['DM_TEST_DATABASE_PASSWORD'], testing=True)
values = {
    'DM_ENVIRONMENT': 'production',
    'DM_BIND_ADDR': '0.0.0.0:8080',
    'DM_PUBLIC_BASE_URL': 'https://' + os.environ['PUBLIC_HOST'] + '/api/v1',
    'DM_DATABASE_URL': database_url('dm_runtime', app['DM_RUNTIME_DATABASE_PASSWORD'], os.environ['DB_HOST'], 'silicon_dm'),
    'DM_TEST_DATABASE_URL': database_url('dm_testing', app['DM_TEST_DATABASE_PASSWORD'], os.environ['TEST_DB_HOST'], 'silicon_dm_test'),
    'DM_DATABASE_MAX_CONNECTIONS': 16,
    'DM_DATABASE_MIN_CONNECTIONS': 1,
    'DM_TEST_DATABASE_MAX_CONNECTIONS': 4,
    'DM_DATABASE_ACQUIRE_TIMEOUT_SECONDS': 10,
    'DM_DATABASE_STATEMENT_TIMEOUT_SECONDS': 60,
    'DM_REQUEST_TIMEOUT_SECONDS': 120,
    'DM_SHUTDOWN_TIMEOUT_SECONDS': 130,
    'DM_MAX_HTTP_BODY_BYTES': 134217728,
    'DM_IAM_BASE_URL': os.environ['IAM_BASE_URL'],
    'DM_IAM_APP_ID': app['DM_IAM_APP_ID'],
    'DM_IAM_APP_SECRET': app['DM_IAM_APP_SECRET'],
    'DM_IAM_WEBHOOK_SECRET': app['DM_IAM_WEBHOOK_SECRET'],
    'DM_IAM_WEBHOOK_KEY_VERSION': version,
    'DM_TEST_KEY_ENCRYPTION_KEY': app['DM_TEST_KEY_ENCRYPTION_KEY'],
    'DM_GIPHY_API_KEY': app['DM_GIPHY_API_KEY'],
    'DM_LOG_FILTER': 'info,silicon_dm=info',
}
write_env('api.env', values)
values['DM_DATABASE_MAX_CONNECTIONS'] = 4
write_env('worker.env', values)
PYTHON

for component in api worker; do
  port_option=''
  if [ "$component" = api ]; then port_option='--publish 8080:8080'; fi
  cat > "/etc/systemd/system/silicon-dm-$component.service" <<UNIT
[Unit]
Description=Silicon DM production $component
Requires=docker.service silicon-dm-container-network.service
After=docker.service network-online.target silicon-dm-container-network.service
[Service]
Restart=always
RestartSec=5
TimeoutStopSec=145
ExecStartPre=-/usr/bin/docker rm -f silicon-dm-$component
ExecStart=/usr/bin/docker run --name silicon-dm-$component $port_option --user 10001:10001 --read-only --cap-drop ALL --security-opt no-new-privileges --tmpfs /tmp:size=16m,mode=1777 --volume /opt/silicon-dm/aws-rds-global-bundle.pem:/opt/silicon-dm/aws-rds-global-bundle.pem:ro --env-file /etc/silicon-dm/$component.env --log-driver awslogs --log-opt awslogs-region=$AWS_DEFAULT_REGION --log-opt awslogs-group=/silicon-dm/production/$component --log-opt tag=$component "$BACKEND_IMAGE" dm-$component
ExecStop=/usr/bin/docker stop --time 135 silicon-dm-$component
[Install]
WantedBy=multi-user.target
UNIT
done
systemctl daemon-reload
systemctl enable --now silicon-dm-container-network silicon-dm-api silicon-dm-worker
for attempt in $(seq 1 120); do
  if [ "$(curl --silent --max-time 5 --output /dev/null --write-out '%{http_code}' http://127.0.0.1:8080/ready)" = 204 ] \
    && systemctl is-active --quiet silicon-dm-worker; then
    exit 0
  fi
  sleep 2
done
echo 'DM failed its local readiness check.' >&2
exit 1
