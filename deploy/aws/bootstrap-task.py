#!/usr/bin/env python3
"""One-off ECS bootstrap: master credentials never enter runtime task definitions.

    bootstrap-task.py                 configure dm_runtime, migrate, grant, publish the runtime secret
    bootstrap-task.py status          print the applied schema migrations
    bootstrap-task.py migrate         apply migrations and runtime grants only
    bootstrap-task.py draft-identity-mapping
                                      draft the identity mapping from Silicon Accounts (prints it)
    bootstrap-task.py link-identities [--dry-run] [--force-relink]
                                      re-key IAM-era identities with the mapping in DM_IDENTITY_MAPPING

The image's entrypoint runs this script, so an ECS command override picks the command
(deploy/aws/run-bootstrap-task.py does that). The plain bootstrap publishes the runtime secret
the API and worker read: DM's Silicon Accounts app secret, webhook secret and data key, the
provider keys and the restricted database URL. Messages name fields and steps, never values.
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
DATABASE = 'silicon_dm'
RUNTIME_ROLE = 'dm_runtime'
MAPPING_HEADER = 'iam_principal_id,accounts_uuid'

# Fields of the operator-managed application secret (AppSecretArn).
REQUIRED = ['DM_APP_SECRET', 'DM_ACCOUNTS_WEBHOOK_SECRET', 'DM_DATA_KEY',
            'DM_GIPHY_API_KEY', 'DM_RUNTIME_DATABASE_PASSWORD']
OPTIONAL = ['DM_POSTMARK_SERVER_TOKEN']
# Runtime keys a bootstrap must never change once published: a new database URL
# would cut off running tasks, and a new data key resets every sync cursor and
# makes stored proofs unreadable. Rotations are separately reviewed procedures.
CONTINUOUS = ['DM_DATABASE_URL', 'DM_DATA_KEY']
# Runtime keys DM no longer reads: those from before Silicon Accounts, and the telemetry
# key (DM records no telemetry since 0.16). Earlier task definitions still name them,
# so they are carried over unchanged and a rollback to those task definitions can
# start. Remove them after the rollback window (docs/migration/cutover.md).
RETIRED = ['DM_IAM_APP_ID', 'DM_IAM_APP_SECRET', 'DM_IAM_WEBHOOK_SECRET', 'DM_IAM_WEBHOOK_KEY_VERSION',
           'DM_TEST_KEY_ENCRYPTION_KEY', 'DM_TEST_DATABASE_URL',
           'DM_HONEYCOMB_SERVICE_TOKEN', 'DM_HONEYCOMB_BASE_URL', 'DM_SPACE_STATION_TABLE_KEY']


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
    runtime['DM_DATABASE_URL'] = database_url(RUNTIME_ROLE, app['DM_RUNTIME_DATABASE_PASSWORD'],
                                             database_host, DATABASE)
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


def operator(arguments, environment, secrets, stdin=None):
    """Runs a dm-migrate operator command whose output is the point (reports, mappings).

    Returns stdout. On failure the error carries stderr with every known secret removed."""
    result = subprocess.run(arguments, input=stdin, text=True, env=environment,
                            capture_output=True, check=False)
    if result.returncode:
        detail = result.stderr.strip()[-4000:]
        for value in secrets:
            if value:
                detail = detail.replace(value, '[redacted]').replace(urllib.parse.quote(value, safe=''), '[redacted]')
        raise RuntimeError(' '.join(arguments[:2]) + ' failed with exit ' + str(result.returncode)
                           + (': ' + detail if detail else ''))
    return result.stdout


def master_environment(master, host, database, **extra):
    return dict(os.environ, PGHOST=host, PGPORT='5432', PGDATABASE=database,
                PGUSER=master['username'], PGPASSWORD=master['password'],
                PGSSLMODE='verify-full', PGSSLROOTCERT=CA, **extra)


def migration_environment(master, host, database):
    return dict(os.environ, DM_ENVIRONMENT='production',
                DM_DATABASE_URL=database_url(master['username'], master['password'], host, database),
                DM_DATABASE_MAX_CONNECTIONS='2', DM_DATABASE_ACQUIRE_TIMEOUT_SECONDS='15',
                DM_DATABASE_STATEMENT_TIMEOUT_SECONDS='300', DM_LOG_FILTER='silicon_dm=info')


def migrate_and_grant(master, host, database, role):
    print('Applying production migrations', flush=True)
    command(['dm-migrate'], migration_environment(master, host, database))
    print('Applying production runtime grants', flush=True)
    command(['psql', '-X', '--set', 'ON_ERROR_STOP=1', '--set', 'runtime_role=' + role,
             '--file', '/opt/silicon-dm/runtime-grants.sql'], master_environment(master, host, database))
    print('Production migrations and runtime grants complete', flush=True)


def configure_database(master, host, database, role, password):
    environment = master_environment(master, host, database, DM_ROLE_PASSWORD=password)
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
    migrate_and_grant(master, host, database, role)


def operator_environment(master, host, database, app):
    """dm-migrate's operator commands: the migrator database role plus DM's Silicon Accounts app."""
    environment = migration_environment(master, host, database)
    environment['DM_LOG_FILTER'] = 'error'  # dm-migrate logs to stdout; keep it to the report
    environment['DM_APP_SECRET'] = (app.get('DM_APP_SECRET') or '').strip()
    if not environment['DM_APP_SECRET'] or not os.environ.get('ACCOUNTS_URL', '').strip():
        raise RuntimeError('ACCOUNTS_URL (task environment) and DM_APP_SECRET (application secret) are '
                           'needed to check uuids with Silicon Accounts')
    return environment


def migration_status(master, host, database):
    """The applied schema migrations, oldest first, as `version|description|success` lines."""
    return operator(['psql', '-X', '-A', '-t', '-F', '|', '--command',
                     'SELECT version, description, success FROM public._sqlx_migrations ORDER BY version'],
                    master_environment(master, host, database), [master['password']])


def split_draft(text):
    """(CSV lines, report) from `dm-migrate draft-identity-mapping --out /dev/stdout`.

    The mapping is written first (header, then one `id,uuid` row per line), then the JSON report."""
    lines = text.splitlines()
    if MAPPING_HEADER not in lines:
        raise RuntimeError('draft-identity-mapping printed no mapping')
    start = end = lines.index(MAPPING_HEADER)
    end += 1
    while end < len(lines) and not lines[end].startswith('{') and lines[end].count(',') == 1:
        end += 1
    return lines[start:end], '\n'.join(lines[end:]).strip()


def checked_mapping(text):
    """DM_IDENTITY_MAPPING as dm-migrate reads it; refuses an empty or headerless one."""
    if not text or not text.strip():
        raise RuntimeError('DM_IDENTITY_MAPPING is empty: pass the reviewed mapping.csv in the task override')
    first = next((line.strip() for line in text.splitlines()
                  if line.strip() and not line.strip().startswith('#')), '')
    if first != MAPPING_HEADER:
        raise RuntimeError('DM_IDENTITY_MAPPING must start with the header ' + MAPPING_HEADER)
    return text if text.endswith('\n') else text + '\n'


def link_flags(arguments):
    allowed = {'--dry-run', '--force-relink'}
    unknown = [argument for argument in arguments if argument not in allowed]
    if unknown:
        raise RuntimeError('link-identities takes only --dry-run and --force-relink, not ' + ' '.join(unknown))
    return [argument for argument in ['--dry-run', '--force-relink'] if argument in arguments]


def main(argv):
    import boto3  # only the real task needs the AWS SDK; the unit tests do not

    secrets = boto3.client('secretsmanager')

    def secret(arn):
        return json.loads(secrets.get_secret_value(SecretId=arn)['SecretString'])

    name, arguments = (argv[0], argv[1:]) if argv else ('bootstrap', [])
    host = os.environ['DB_HOST']
    if name == 'bootstrap' and not arguments:
        app = secret(os.environ['APP_SECRET_ARN'])
        # A normal rerun must not rotate the role password or the data key. Otherwise
        # a later failure could invalidate live tasks before replacement
        # configuration is ready. An empty runtime secret is first use.
        metadata = secrets.describe_secret(SecretId=os.environ['RUNTIME_SECRET_ARN'])
        if metadata.get('DeletedDate'):
            raise RuntimeError('Runtime secret is scheduled for deletion')
        current = secret(os.environ['RUNTIME_SECRET_ARN']) if metadata.get('VersionIdsToStages') else None
        runtime = runtime_configuration(app, host, current)
        print('Runtime credential continuity preflight complete', flush=True)
        configure_database(secret(os.environ['DB_SECRET_ARN']), host, DATABASE, RUNTIME_ROLE,
                           app['DM_RUNTIME_DATABASE_PASSWORD'])
        secrets.put_secret_value(SecretId=os.environ['RUNTIME_SECRET_ARN'], SecretString=json.dumps(runtime))
        print('Restricted runtime secret published; bootstrap completed', flush=True)
    elif name == 'status' and not arguments:
        print(migration_status(secret(os.environ['DB_SECRET_ARN']), host, DATABASE).strip(), flush=True)
    elif name == 'migrate' and not arguments:
        migrate_and_grant(secret(os.environ['DB_SECRET_ARN']), host, DATABASE, RUNTIME_ROLE)
    elif name == 'draft-identity-mapping' and not arguments:
        master, app = secret(os.environ['DB_SECRET_ARN']), secret(os.environ['APP_SECRET_ARN'])
        environment = operator_environment(master, host, DATABASE, app)
        output = operator(['dm-migrate', 'draft-identity-mapping', '--out', '/dev/stdout'], environment,
                          [master['password'], environment['DM_APP_SECRET']])
        rows, report = split_draft(output)
        print('----- BEGIN mapping.csv -----\n' + '\n'.join(rows) + '\n----- END mapping.csv -----', flush=True)
        print(report, flush=True)
    elif name == 'link-identities':
        flags = link_flags(arguments)
        mapping = checked_mapping(os.environ.get('DM_IDENTITY_MAPPING', ''))
        master, app = secret(os.environ['DB_SECRET_ARN']), secret(os.environ['APP_SECRET_ARN'])
        environment = operator_environment(master, host, DATABASE, app)
        print(operator(['dm-migrate', 'link-identities', '--file', '/dev/stdin', *flags], environment,
                       [master['password'], environment['DM_APP_SECRET']], stdin=mapping).strip(), flush=True)
    else:
        raise RuntimeError('Unknown command: ' + ' '.join(argv) + ' (use no command, status, migrate, '
                           'draft-identity-mapping or link-identities [--dry-run] [--force-relink])')


if __name__ == '__main__':
    try:
        main(sys.argv[1:])
    except Exception as error:
        # Exception messages from AWS/subprocess libraries may contain secrets;
        # this script's own RuntimeErrors name fields and steps only.
        detail = str(error) if type(error) is RuntimeError else type(error).__name__
        print('DM bootstrap failed: ' + detail, file=sys.stderr, flush=True)
        sys.exit(1)
