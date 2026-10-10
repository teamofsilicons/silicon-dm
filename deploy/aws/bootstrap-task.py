#!/usr/bin/env python3
"""One-off ECS bootstrap: master credentials never enter runtime task definitions.

It configures the restricted `dm_runtime` role, applies the production migrations
and runtime grants, and publishes the runtime secret the API and worker read:
DM's Silicon Accounts app secret, webhook secret and data key, the provider keys
and the restricted database URL. Messages name fields, never values.
"""
import base64
import binascii
import json
import os
import subprocess
import sys
import urllib.parse

CA = '/opt/silicon-dm/aws-rds-global-bundle.pem'
SSL = 'sslmode=verify-full&sslrootcert=' + CA

# Fields of the operator-managed application secret (AppSecretArn).
REQUIRED = ['DM_APP_SECRET', 'DM_ACCOUNTS_WEBHOOK_SECRET', 'DM_DATA_KEY',
            'DM_GIPHY_API_KEY', 'DM_RUNTIME_DATABASE_PASSWORD']
OPTIONAL = ['DM_SPACE_STATION_TABLE_KEY', 'DM_POSTMARK_SERVER_TOKEN']
# Runtime keys a bootstrap must never change once published: a new database URL
# would cut off running tasks, and a new data key resets every sync cursor and
# makes stored proofs unreadable. Rotations are separately reviewed procedures.
CONTINUOUS = ['DM_DATABASE_URL', 'DM_DATA_KEY']
# Runtime keys from before Silicon Accounts. DM 0.15 reads none of them, but the
# 0.14 task definitions still name them, so they are carried over unchanged and a
# rollback to those task definitions can start. Remove them after the rollback
# window (docs/migration/cutover.md).
RETIRED = ['DM_IAM_APP_ID', 'DM_IAM_APP_SECRET', 'DM_IAM_WEBHOOK_SECRET', 'DM_IAM_WEBHOOK_KEY_VERSION',
           'DM_TEST_KEY_ENCRYPTION_KEY', 'DM_TEST_DATABASE_URL',
           'DM_HONEYCOMB_SERVICE_TOKEN', 'DM_HONEYCOMB_BASE_URL']


def database_url(user, password, host, database):
    return ('postgresql://' + urllib.parse.quote(user, safe='') + ':'
            + urllib.parse.quote(password, safe='') + '@' + host + ':5432/'
            + database + '?' + SSL)


def data_key_bytes(value):
    """DM_DATA_KEY as DM reads it: standard or URL-safe base64 of exactly 32 bytes."""
    value = value.strip()
    try:
        return base64.b64decode(value, validate=True)
    except (binascii.Error, ValueError):
        pass
    unpadded = value.rstrip('=')
    if not unpadded or any(c not in 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_' for c in unpadded):
        raise RuntimeError('DM_DATA_KEY must be base64 (openssl rand -base64 32)')
    try:
        return base64.urlsafe_b64decode(unpadded + '=' * (-len(unpadded) % 4))
    except (binascii.Error, ValueError):
        raise RuntimeError('DM_DATA_KEY must be base64 (openssl rand -base64 32)') from None


def validate_application(app):
    """Refuses a missing, placeholder or malformed field, naming the field only."""
    for name in REQUIRED:
        value = app.get(name)
        if not isinstance(value, str) or not value.strip() or any(c in value for c in '\r\n\x00'):
            raise RuntimeError('Missing or invalid secret field: ' + name)
        if value.startswith('REPLACE_'):
            raise RuntimeError('Incomplete secret field: ' + name)
    if not app['DM_ACCOUNTS_WEBHOOK_SECRET'].strip().startswith('whsec_'):
        raise RuntimeError('DM_ACCOUNTS_WEBHOOK_SECRET must be the webhook secret from Silicon Accounts (whsec_...)')
    if len(data_key_bytes(app['DM_DATA_KEY'])) != 32:
        raise RuntimeError('DM_DATA_KEY must encode exactly 32 bytes')
    for name in OPTIONAL:
        value = app.get(name, '')
        if not isinstance(value, str) or any(c in value for c in '\r\n\x00'):
            raise RuntimeError('Invalid optional secret field: ' + name)


def runtime_configuration(app, database_host, current):
    """The runtime secret document. `current` is the published one (None on first use)."""
    validate_application(app)
    runtime = {name: app[name].strip() for name in REQUIRED if name != 'DM_RUNTIME_DATABASE_PASSWORD'}
    for name in OPTIONAL:
        runtime[name] = app.get(name, '').strip()
    runtime['DM_DATABASE_URL'] = database_url('dm_runtime', app['DM_RUNTIME_DATABASE_PASSWORD'],
                                             database_host, 'silicon_dm')
    if current:
        for name in CONTINUOUS:
            if name in current and current[name] != runtime[name]:
                raise RuntimeError('Bootstrap cannot rotate ' + name + '; use a reviewed rotation procedure')
        for name in RETIRED:
            if name in current:
                runtime[name] = current[name]
    return runtime


def command(arguments, environment, sql=None):
    # psql can print the failing statement. Capture it privately in process
    # memory and report only the tool name/status, never SQL or credentials.
    result = subprocess.run(arguments, input=sql, text=True, env=environment,
                            capture_output=True, check=False)
    if result.returncode:
        raise RuntimeError(arguments[0] + ' failed with exit ' + str(result.returncode))


def configure_database(master, host, database, role, password):
    environment = dict(os.environ, PGHOST=host, PGPORT='5432', PGDATABASE=database,
                       PGUSER=master['username'], PGPASSWORD=master['password'],
                       PGSSLMODE='verify-full', PGSSLROOTCERT=CA, DM_ROLE_PASSWORD=password)
    print('Checking authenticated TLS connection: ' + database, flush=True)
    command(['psql', '-X', '--set', 'ON_ERROR_STOP=1', '--command', 'SELECT 1'], environment)
    # Role names/database identifiers are fixed by this script, never provided
    # by untrusted input. The password is a quoted psql variable from the env.
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
COMMIT;
""" % (role, role, role, role, database, database, role)
    print('Configuring restricted database role: ' + role, flush=True)
    command(['psql', '-X', '--set', 'ON_ERROR_STOP=1'], environment, sql)
    migration = dict(os.environ, DM_ENVIRONMENT='production',
                     DM_DATABASE_URL=database_url(master['username'], master['password'], host, database),
                     DM_DATABASE_MAX_CONNECTIONS='2', DM_DATABASE_ACQUIRE_TIMEOUT_SECONDS='15',
                     DM_DATABASE_STATEMENT_TIMEOUT_SECONDS='300', DM_LOG_FILTER='silicon_dm=info')
    print('Applying production migrations', flush=True)
    command(['dm-migrate'], migration)
    print('Applying production runtime grants', flush=True)
    command(['psql', '-X', '--set', 'ON_ERROR_STOP=1', '--set', 'runtime_role=' + role,
             '--file', '/opt/silicon-dm/runtime-grants.sql'], environment)
    print('Production migrations and runtime grants complete', flush=True)


def main():
    import boto3  # only the real task needs the AWS SDK; the unit tests do not

    secrets = boto3.client('secretsmanager')

    def secret(arn):
        return json.loads(secrets.get_secret_value(SecretId=arn)['SecretString'])

    app = secret(os.environ['APP_SECRET_ARN'])
    # A normal rerun must not rotate the role password or the data key. Otherwise
    # a later failure could invalidate live tasks before replacement
    # configuration is ready. An empty runtime secret is first use.
    metadata = secrets.describe_secret(SecretId=os.environ['RUNTIME_SECRET_ARN'])
    if metadata.get('DeletedDate'):
        raise RuntimeError('Runtime secret is scheduled for deletion')
    current = secret(os.environ['RUNTIME_SECRET_ARN']) if metadata.get('VersionIdsToStages') else None
    runtime = runtime_configuration(app, os.environ['DB_HOST'], current)
    print('Runtime credential continuity preflight complete', flush=True)

    configure_database(secret(os.environ['DB_SECRET_ARN']), os.environ['DB_HOST'], 'silicon_dm',
                       'dm_runtime', app['DM_RUNTIME_DATABASE_PASSWORD'])
    secrets.put_secret_value(SecretId=os.environ['RUNTIME_SECRET_ARN'], SecretString=json.dumps(runtime))
    print('Restricted runtime secret published; bootstrap completed', flush=True)


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        # Exception messages from AWS/subprocess libraries may contain secrets;
        # this script's own RuntimeErrors name fields and steps only.
        detail = str(error) if type(error) is RuntimeError else type(error).__name__
        print('DM bootstrap failed: ' + detail, file=sys.stderr, flush=True)
        sys.exit(1)
