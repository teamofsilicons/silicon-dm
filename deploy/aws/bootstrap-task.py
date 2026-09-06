#!/usr/bin/env python3
"""One-off ECS bootstrap: master credentials never enter runtime task definitions."""
import base64
import json
import os
import subprocess
import sys
import urllib.parse

import boto3

CA = '/opt/silicon-dm/aws-rds-global-bundle.pem'
SSL = 'sslmode=verify-full&sslrootcert=' + CA


def database_url(user, password, host, database):
    return ('postgresql://' + urllib.parse.quote(user, safe='') + ':'
            + urllib.parse.quote(password, safe='') + '@' + host + ':5432/'
            + database + '?' + SSL)


def command(arguments, environment, sql=None):
    # psql can print the failing statement. Capture it privately in process
    # memory and report only the tool name/status, never SQL or credentials.
    result = subprocess.run(arguments, input=sql, text=True, env=environment,
                            capture_output=True, check=False)
    if result.returncode:
        raise RuntimeError(arguments[0] + ' failed with exit ' + str(result.returncode))


def configure_database(master, host, database, role, password, testing=False):
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
""" % (role, role, role, role, database, database, role)
    if testing:
        sql += 'GRANT CREATE ON DATABASE ' + database + ' TO ' + role + ';\n'
    sql += 'COMMIT;\n'
    print('Configuring restricted database role: ' + role, flush=True)
    command(['psql', '-X', '--set', 'ON_ERROR_STOP=1'], environment, sql)
    if not testing:
        migration = dict(os.environ, DM_ENVIRONMENT='production',
                         DM_DATABASE_URL=database_url(master['username'], master['password'], host, database),
                         DM_DATABASE_MAX_CONNECTIONS='2', DM_DATABASE_ACQUIRE_TIMEOUT_SECONDS='15',
                         DM_DATABASE_STATEMENT_TIMEOUT_SECONDS='300', DM_LOG_FILTER='silicon_dm=info')
        print('Applying production migrations', flush=True)
        command(['dm-migrate'], migration)
        print('Applying production runtime grants', flush=True)
        command(['psql', '-X', '--set', 'ON_ERROR_STOP=1', '--set', 'runtime_role=' + role,
                 '--file', '/opt/silicon-dm/runtime-grants.sql'], environment)
    print(('Testing role configured' if testing else 'Production migrations and runtime grants complete'), flush=True)


def main():
    secrets = boto3.client('secretsmanager')

    def secret(arn):
        return json.loads(secrets.get_secret_value(SecretId=arn)['SecretString'])

    app = secret(os.environ['APP_SECRET_ARN'])
    required = ['DM_IAM_APP_ID', 'DM_IAM_APP_SECRET', 'DM_IAM_WEBHOOK_SECRET',
                'DM_GIPHY_API_KEY', 'DM_TEST_KEY_ENCRYPTION_KEY',
                'DM_RUNTIME_DATABASE_PASSWORD', 'DM_TEST_DATABASE_PASSWORD']
    for name in required:
        value = app.get(name)
        if not isinstance(value, str) or not value.strip() or any(c in value for c in '\r\n\x00'):
            raise ValueError('Missing or invalid secret field: ' + name)
        if value.startswith('REPLACE_'):
            raise ValueError('Incomplete secret field: ' + name)
    if len(base64.b64decode(app['DM_TEST_KEY_ENCRYPTION_KEY'], validate=True)) != 32:
        raise ValueError('DM_TEST_KEY_ENCRYPTION_KEY must encode 32 bytes')
    if type(app.get('DM_IAM_WEBHOOK_KEY_VERSION')) is not int or app['DM_IAM_WEBHOOK_KEY_VERSION'] < 1:
        raise ValueError('DM_IAM_WEBHOOK_KEY_VERSION must be a positive JSON integer')

    runtime = {key: str(app[key]) for key in ['DM_IAM_APP_ID', 'DM_IAM_APP_SECRET', 'DM_IAM_WEBHOOK_SECRET',
               'DM_IAM_WEBHOOK_KEY_VERSION', 'DM_GIPHY_API_KEY', 'DM_TEST_KEY_ENCRYPTION_KEY']}
    runtime['DM_DATABASE_URL'] = database_url('dm_runtime', app['DM_RUNTIME_DATABASE_PASSWORD'],
                                             os.environ['DB_HOST'], 'silicon_dm')
    runtime['DM_TEST_DATABASE_URL'] = database_url('dm_testing', app['DM_TEST_DATABASE_PASSWORD'],
                                                  os.environ['TEST_DB_HOST'], 'silicon_dm_test')
    # A normal migration rerun must not rotate role passwords or the testing
    # encryption key. Otherwise a later failure could invalidate live tasks
    # before replacement configuration is ready. Empty secrets are first use.
    metadata = secrets.describe_secret(SecretId=os.environ['RUNTIME_SECRET_ARN'])
    if metadata.get('DeletedDate'):
        raise RuntimeError('Runtime secret is scheduled for deletion')
    if metadata.get('VersionIdsToStages'):
        current = secret(os.environ['RUNTIME_SECRET_ARN'])
        for name in ['DM_DATABASE_URL', 'DM_TEST_DATABASE_URL', 'DM_TEST_KEY_ENCRYPTION_KEY']:
            if current.get(name) != runtime[name]:
                raise RuntimeError('Bootstrap cannot rotate ' + name + '; use a reviewed rotation procedure')
    print('Runtime credential continuity preflight complete', flush=True)

    configure_database(secret(os.environ['DB_SECRET_ARN']), os.environ['DB_HOST'], 'silicon_dm',
                       'dm_runtime', app['DM_RUNTIME_DATABASE_PASSWORD'])
    configure_database(secret(os.environ['TEST_DB_SECRET_ARN']), os.environ['TEST_DB_HOST'], 'silicon_dm_test',
                       'dm_testing', app['DM_TEST_DATABASE_PASSWORD'], testing=True)
    secrets.put_secret_value(SecretId=os.environ['RUNTIME_SECRET_ARN'], SecretString=json.dumps(runtime))
    print('Restricted runtime secret published; bootstrap completed', flush=True)


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        # Exception messages from AWS/subprocess libraries may contain secrets.
        detail = str(error) if type(error) is RuntimeError else type(error).__name__
        print('DM bootstrap failed: ' + detail, file=sys.stderr, flush=True)
        sys.exit(1)
