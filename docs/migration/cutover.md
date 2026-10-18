# DM cutover to Silicon Accounts and Silicon Apps (runbook)

DM 0.15.0 (HTTP contract 4) signs every account in with Silicon Accounts, has no organizations, and
ships the `dm` command through Silicon Apps. Moving production is one coordinated change: the
database migrations, the re-key of existing identities, the new service and CLI, the new website,
Silicon Interface's switch to proofs and the Silicon fleet's switch to the new sign-in all land in
the same window. Nothing here has been run against production.

**Conventions.** Every command that reads or changes production is marked **run at cutover** and
needs a Carbon's go-ahead. Work in a private directory (`umask 077`) on a machine with the AWS CLI,
`jq` and Python 3; secrets are read from files and never typed on a command line. Shared settings:

```sh
export AWS_PROFILE=silicon-production AWS_REGION=us-east-1
# DM's Fargate stack (the one whose ClusterName output is silicon-dm-production)
STACK=$(aws cloudformation describe-stacks --output text --query \
  "Stacks[?Outputs[?OutputKey=='ClusterName' && OutputValue=='silicon-dm-production']].StackName")
APP_SECRET_ARN=$(aws cloudformation describe-stacks --stack-name "$STACK" --output text \
  --query "Stacks[0].Parameters[?ParameterKey=='AppSecretArn'].ParameterValue")
```

## 0. The order at a glance

| # | When | Step | Effect on people |
| --- | --- | --- | --- |
| 1 | days before | Prerequisites outside DM (section 1) | none |
| 2 | days before | Release artifacts, images, secrets, sign-in setup, draft and review the identity mapping (section 2) | none |
| 3 | window | Stop 0.14 and register the 0.15 task definitions (stack update, desired count 0), then snapshot | DM unavailable |
| 4 | window | Bootstrap: migrations 0039–0040, grants, runtime secret | DM unavailable |
| 5 | window | `link-identities` dry run, then for real | DM unavailable |
| 6 | window | Start 0.15 (stack update, desired count 1); set the webhook URL | DM back, on Silicon Accounts |
| 7 | window | Promote `dm` 0.15.0 on Silicon Apps; switch Silicon Interface and the website; the fleet switches | old CLIs get `unsupported_contract` until updated |
| 8 | window | Verify (section 4) | |
| 9 | +7 days | Close the rollback window: remove the retired runtime keys and app-secret fields (section 5) | none |

The window is short if section 2 is done: steps 3–6 take about 20 minutes plus the length of the
re-key, which a dry run measures.

## 1. Prerequisites outside DM

### Silicon Accounts and Silicon Apps

- The app `dm` exists in production Silicon Accounts and Silicon Apps. Two-character ids cannot be
  created with `silicon-apps create`; the platform owners register `dm` (Silicon Accounts' internal
  app sync, then the Apps import). The Carbon who releases DM is an author of `dm` in Silicon Apps.
- Every Carbon and Silicon that uses DM has a Silicon Accounts account, and every Silicon its
  custodian. The identity mapping (2.5) can only point at accounts that exist.

### Silicon Interface

Interface reads and writes DM for Carbons and Silicons. It must switch in the same window to User
verification proofs (`Authorization: Proof sap_…`, receiving app `dm`) with the scopes it needs:
`dm.conversations.read`, `dm.messages.read`, `dm.messages.write`, `dm.receipts.write`,
`dm.drafts.write`. DM accepts them only from the apps in `DM_PROOF_ISSUERS`; the stack parameter
`ProofIssuers` names `interface` for all five, so if Interface's production app id differs, change
the parameter. Messages Interface sends this way carry `via_app` with its app id, which the other
participants see. Interface builds against `silicon-dm-client` 0.15 (`Client::with_proof`), published
before the window (2.1). Interface builds that send IAM tokens and `X-Org-ID` stop working at the
switch.

### The Silicon fleet

The Silicon runtime (stemcell `silicon connect`) is what runs `dm` for every Silicon. At the switch
it must:

1. Install `dm` through Silicon Apps on every host (`silicon-apps install dm`, Silicon Apps itself
   installed first), and stop resolving the older copies: the Honeycomb-installed `dm` and the
   stale `dm` in the Silicon CLI bundle's `~/.local/share/silicon/bin`. `command -v dm` must name
   the Silicon Apps copy, and `dm --version` must print `dm 0.15.0`.
2. Sign each Silicon in with `silicon-accounts login --app dm -q | dm login --slt-stdin` (the
   positional `dm login <slt>` the runtime uses today still works).
3. Read `dm accounts --json` for discovery; the hidden `dm iam --json` prints the same object for one
   release, so an unchanged runtime keeps working.
4. Receive by supervising `dm inbox --watch` (JSON lines, resumes from its saved cursor) instead of
   `dm delivery login` + `dm webhook … --all-apps`, which now answer `command_retired`.

### Ting

Ting 0.3 accepts Silicon Accounts proofs. DM 0.16.1 uses a User verification proof with scope
`subscriptions.register` for enrolment and an App verification proof with scope `tings.send`
for delivery. Both proofs are issued by `dm` for receiving app `ting`. HTTP app calls to Ting
use `Authorization: Bearer sap_…`; recipients are immutable UUID strings. Register the
`dm.sync.changed` notification type as a DM author, then set `TingUrl` (or `DM_TING_URL`)
to `https://backend.ting.teamofsilicons.com` on both API and worker.

With Ting unset, handoffs are retained with reason `ting_not_configured` and clients must
poll the sync feed. Enabling Ting does not replay those discarded records. Recover only
reviewed messages by resetting their unaccepted handoffs; preserve their IDs and original
bytes, exclude organization-era references and never bulk replay historical notifications.
DM names Ting's app id with `DM_TING_APP_ID` (default `ting`); the templates do not pass it, so only
if Ting is registered at Silicon Accounts under another id does the task definition need it.

### The website

The SolidJS website and its token gateway sign in with the earlier system and stop working at the
switch. The Next.js website that replaces them is built in the web stage of this migration and must
deploy in the same window:

- It is a standalone Next.js server (`NEXT_OUTPUT=standalone`) in a separate ECS service behind the DM
  ALB, at `https://dm.teamofsilicons.com`; it streams request and response bodies to
  `https://backend.dm.teamofsilicons.com` (messages reach 128 MiB, beyond what Vercel Functions
  accept).
- Its settings: `PUBLIC_URL=https://dm.teamofsilicons.com`, `ACCOUNTS_URL`, `APP_API_URL`, and two
  secrets kept in Secrets Manager: `APP_SECRET` (DM's app secret) and `SESSION_SECRET` (a new
  `openssl rand -base64 48`).
- DNS: `dm.teamofsilicons.com` moves from Vercel to the DM ALB, with a listener rule for the host
  and a certificate that covers it. Its sign-in redirect is
  `https://dm.teamofsilicons.com/auth/callback` (2.3).
- The deployment configuration is `deploy/aws/web.yaml` and `README.web.md`. The old
  `gateway.yaml` and bootstrap remain for rollback only; never use their stateful runtime
  configuration with the new standalone image. Start the new stack with zero tasks until cutover.

### Order relative to the other apps

DM calls no other app of this migration (attachments are links and voice transcripts come from
the clients), and none of them calls DM. DM's window therefore depends only on Silicon Accounts and
Silicon Apps being live in production, on Interface and the fleet switching with it, and on the
website. It can run before, after or together with the other apps' windows.

## 2. Before the window (no effect on people)

### 2.1 Release artifacts and crates

**Run at cutover** (days before). The release workflow publishes nothing; the development release
is installable only as `dm>dev`:

```sh
git tag v0.15.0 <reviewed commit> && git push origin v0.15.0      # starts release.yml
gh run download <run id> --name dm-silicon-apps-release --dir release
(cd release && sha256sum --check SHA256SUMS)
silicon-apps upload dm --target linux-x86_64 release/dm-0.15.0-linux-x86_64.tar.gz
silicon-apps upload dm --target linux-aarch64 release/dm-0.15.0-linux-aarch64.tar.gz
silicon-apps packages dm                                          # both accepted; note the ids
silicon-apps release dm --version 0.15.0 --package <linux-x86_64 id> --package <linux-aarch64 id> \
  --notes "Signs in with Silicon Accounts; installed and updated by Silicon Apps."
cargo publish -p silicon-dm-protocol      # 0.13.0
cargo publish -p silicon-dm-client        # 0.15.0, for Interface
```

Only the Linux validation workers are live, so the macOS and Windows archives stay with the
workflow run. `silicon-apps install 'dm>dev'` on a test machine tries the release against the
local stack or, after the window, production.

### 2.2 Backend and bootstrap images

**Run at cutover** (days before):

```sh
gh workflow run deployment-images.yml --ref v0.15.0
gh run download <run id> --name dm-deployment-backend --dir images/backend
gh run download <run id> --name dm-deployment-bootstrap --dir images/bootstrap
(cd images/backend && sha256sum --check SHA256SUMS) && (cd images/bootstrap && sha256sum --check SHA256SUMS)
jq -r .revision images/backend/backend.metadata.json               # the tagged commit
REGISTRY=234951665042.dkr.ecr.us-east-1.amazonaws.com; REPO=$REGISTRY/silicon-dm-production
aws ecr get-login-password | crane auth login --username AWS --password-stdin "$REGISTRY"
crane push images/backend/backend.tar "$REPO:0.15.0"
crane push images/bootstrap/bootstrap.tar "$REPO:bootstrap-0.15.0"
BACKEND_IMAGE="$REPO@$(crane digest "$REPO:0.15.0")"
BOOTSTRAP_IMAGE="$REPO@$(crane digest "$REPO:bootstrap-0.15.0")"
```

### 2.3 DM's sign-in setup and webhook secret at Silicon Accounts

Save DM's app secret (`sa_app_…`, from the platform owners) as `dm-app-secret.txt`. **Run at
cutover** (days before):

```sh
export OPS_HOME=$PWD/accounts-ops && mkdir -p "$OPS_HOME"    # keeps the app secret out of your own home
SILICON_HOME=$OPS_HOME silicon-accounts app use dm --secret-stdin < dm-app-secret.txt
SILICON_HOME=$OPS_HOME silicon-accounts app config get > signin-before.json   # note config_version
cat > signin.json <<'EOF'
{
  "redirect_uris": ["https://dm.teamofsilicons.com/auth/callback"],
  "device_flow": true,
  "public_client": true,
  "required_fields": [],
  "optional_fields": ["email", "timezone"]
}
EOF
SILICON_HOME=$OPS_HOME silicon-accounts app config set - --expected-version <config_version> < signin.json
silicon-apps webhook dm rotate        # prints the webhook secret once: save it as dm-webhook-secret.txt
```

Lists replace, so `signin.json` carries every redirect URI production needs. `device_flow` lets
Carbons run `dm login`; `public_client` lets the CLI exchange a Silicon's short-lived token with no
secret; DM requires no account details. The webhook URL is set only in the window (step 6): until
0.15 answers, deliveries would fail against 0.14.

### 2.4 The application secret

**Run at cutover** (days before). This adds the new fields to `AppSecretArn`; the running 0.14 tasks
read the runtime secret, which changes only in the window:

```sh
umask 077
openssl rand -base64 32 > dm-data-key.txt    # back it up with DM's other secrets: cursors and stored proofs need it
aws secretsmanager get-secret-value --secret-id "$APP_SECRET_ARN" --query SecretString --output text > app.json
jq --rawfile app dm-app-secret.txt --rawfile hook dm-webhook-secret.txt --rawfile key dm-data-key.txt \
  '. + {DM_APP_SECRET: ($app|rtrimstr("\n")), DM_ACCOUNTS_WEBHOOK_SECRET: ($hook|rtrimstr("\n")),
        DM_DATA_KEY: ($key|rtrimstr("\n"))}' app.json > app.next.json
aws secretsmanager put-secret-value --secret-id "$APP_SECRET_ARN" --secret-string file://app.next.json
rm -f app.json app.next.json
```

`DM_GIPHY_API_KEY`, `DM_RUNTIME_DATABASE_PASSWORD` and the optional Postmark key
are already there and keep their names. The 0.14 fields stay until section 5.

### 2.5 Draft and review the identity mapping

The draft only reads. It needs the 0.15 bootstrap image, so register a one-off task definition
from the current bootstrap one. **Run at cutover** (days before):

```sh
aws ecs describe-task-definition --task-definition silicon-dm-bootstrap --query taskDefinition > bootstrap-current.json
jq --arg image "$BOOTSTRAP_IMAGE" '
  {family: "silicon-dm-cutover", taskRoleArn, executionRoleArn, networkMode, requiresCompatibilities, cpu, memory,
   runtimePlatform,
   containerDefinitions: [.containerDefinitions[0]
     | .image = $image
     | .environment = ([.environment[] | select(.name | startswith("TEST_DB_") | not)]
         + [{name: "ACCOUNTS_URL", value: "https://accounts.teamofsilicons.com"}, {name: "DM_APP_ID", value: "dm"}])]}' \
  bootstrap-current.json > cutover-task.json
aws ecs register-task-definition --cli-input-json file://cutover-task.json
python3 deploy/aws/run-bootstrap-task.py --stack "$STACK" --task-definition silicon-dm-cutover status
python3 deploy/aws/run-bootstrap-task.py --stack "$STACK" --task-definition silicon-dm-cutover \
  draft-identity-mapping > draft.txt
sed -n '/^----- BEGIN mapping.csv -----$/,/^----- END mapping.csv -----$/p' draft.txt | sed '1d;$d' > mapping.csv
```

`status` prints the applied migrations. The survey could not tell whether production is at 0037 or
0038: if 0038 is missing, the bootstrap applies it too, and 0038 deletes the stored Ting credentials
of the earlier sign-in (useless after the switch; the snapshot keeps them).

Review `mapping.csv` (`iam_principal_id,accounts_uuid`, one row per id DM stored). The draft
resolves each stored id by the account that holds that id now, so check every row: a handle that
now belongs to someone else would move history to the wrong account. The report after the mapping
in `draft.txt` lists the `unresolved` ids; add a row for each whose account exists under another id
(renamed, or a Silicon handle longer than 30 characters that had to change), with the uuid from
`silicon-accounts lookup <current id>`. Ids nobody maps stay readable as legacy identities, and a
later run can link them. One ECS override carries at most 8 KiB: split a larger mapping into files
and link them one run each (each run links its rows and keeps the earlier links).

### 2.6 Save the rollback point

**Run at cutover** (just before the window):

```sh
aws cloudformation get-template --stack-name "$STACK" --template-stage Original \
  --query TemplateBody --output text > template-0.14.yaml
aws cloudformation describe-stacks --stack-name "$STACK" --query 'Stacks[0].Parameters' > params-0.14.json
aws ecs describe-services --cluster silicon-dm-production --services silicon-dm-api silicon-dm-worker \
  --query 'services[].{service:serviceName,taskDefinition:taskDefinition,desired:desiredCount}' > services-0.14.json
aws rds describe-db-instances --db-instance-identifier silicon-dm-production --query 'DBInstances[0].{
  subnets: DBSubnetGroup.DBSubnetGroupName, groups: VpcSecurityGroups[].VpcSecurityGroupId,
  parameters: DBParameterGroups[0].DBParameterGroupName, class: DBInstanceClass}' > database-0.14.json
```

`services-0.14.json` names the exact task definitions running now (the image-swap recipe registers
them outside CloudFormation); `database-0.14.json` is what a restore needs.

### 2.7 Rehearse on a copy (recommended)

Restore the newest snapshot to a temporary instance in the stack's database subnet group and
security group (with `--manage-master-user-password`), let the bootstrap task role read its master
secret for the day, and register a variant of `silicon-dm-cutover` whose `DB_HOST` and
`DB_SECRET_ARN` name the copy. Run `migrate`, `link-identities --dry-run` and `link-identities`
against it, time them, sign in to a 0.15 service pointed at it if you can, then delete the copy
and the temporary permission. It proves the mapping on real data and sizes the window.

On the release commit, also run `scripts/e2e-accounts.sh` against a Silicon Accounts stack on a
workstation ([tests/e2e/README.md](../../tests/e2e/README.md)): its eight scenarios exercise the
release's sign-ins, webhook, proofs, Ting adapter, archive and restart behaviour with real tokens.
Stage 4 of the migration ran it: 151/151 checks (progress.md).

## 3. The window

### Step 3: stop 0.14, snapshot

One stack update stops 0.14 (desired count 0) and registers the 0.15 task definitions, which read
the new runtime keys. The snapshot comes after the stop, so it holds 0.14's final state. **Run at
cutover**:

```sh
jq --arg backend "$BACKEND_IMAGE" --arg bootstrap "$BOOTSTRAP_IMAGE" '
  [ .[] | select(.ParameterKey != "IamBaseUrl")
        | if .ParameterKey == "BackendImageUri" then {ParameterKey, ParameterValue: $backend}
          elif .ParameterKey == "BootstrapImageUri" then {ParameterKey, ParameterValue: $bootstrap}
          elif .ParameterKey == "RuntimeDesiredCount" then {ParameterKey, ParameterValue: "0"}
          else {ParameterKey, UsePreviousValue: true} end ]
  + [ {ParameterKey: "AccountsUrl", ParameterValue: "https://accounts.teamofsilicons.com"},
      {ParameterKey: "AccountsApiUrl", ParameterValue: ""},
      {ParameterKey: "AppId", ParameterValue: "dm"},
      {ParameterKey: "ProofIssuers", ParameterValue: "dm.conversations.read=interface,dm.messages.read=interface,dm.messages.write=interface,dm.receipts.write=interface,dm.drafts.write=interface"},
      {ParameterKey: "TingUrl", ParameterValue: ""} ]' params-0.14.json > params-stopped.json
aws cloudformation create-change-set --stack-name "$STACK" --change-set-name dm-015-stopped \
  --template-body file://deploy/aws/fargate.yaml --parameters file://params-stopped.json \
  --capabilities CAPABILITY_NAMED_IAM
aws cloudformation wait change-set-create-complete --stack-name "$STACK" --change-set-name dm-015-stopped
aws cloudformation describe-change-set --stack-name "$STACK" --change-set-name dm-015-stopped \
  --query 'Changes[].ResourceChange.[Action,LogicalResourceId,Replacement]' --output table
```

Expect exactly: the three task definitions replaced (new revisions), `ApiService` and
`WorkerService` modified, `BootstrapTaskRole` modified (it no longer reads the testing database's
secret). Stop if anything touches `Database`, `TestingDatabase`, the load balancer, its listeners,
the WAF, `RuntimeSecret` or a log group. Then:

```sh
aws cloudformation execute-change-set --stack-name "$STACK" --change-set-name dm-015-stopped
aws cloudformation wait stack-update-complete --stack-name "$STACK"
aws ecs wait services-stable --cluster silicon-dm-production --services silicon-dm-api silicon-dm-worker
SNAPSHOT=silicon-dm-before-accounts-$(date -u +%Y%m%d%H%M)
aws rds create-db-snapshot --db-instance-identifier silicon-dm-production --db-snapshot-identifier "$SNAPSHOT"
aws rds wait db-snapshot-available --db-snapshot-identifier "$SNAPSHOT"
```

### Step 4: bootstrap

Migrations 0039 and 0040 (and 0038 if `status` showed it missing), the runtime grants (which add
`dm.sync_group_conversation(uuid)` for the runtime role), and the runtime secret with the new keys.
The 0.14 keys are carried over unchanged. **Run at cutover**:

```sh
python3 deploy/aws/run-bootstrap-task.py --stack "$STACK"
python3 deploy/aws/run-bootstrap-task.py --stack "$STACK" status        # ends with 39 and 40
```

The log ends with `Restricted runtime secret published; bootstrap completed` and the command exits
0. A refusal names the field or step (`Missing or invalid secret field: DM_DATA_KEY`, `Bootstrap
cannot rotate DM_DATABASE_URL`) and changes nothing after it.

### Step 5: re-key identities

**Run at cutover**:

```sh
python3 deploy/aws/run-bootstrap-task.py --stack "$STACK" --mapping mapping.csv link-identities --dry-run
```

Read the JSON report: `linked` holds the rows you reviewed, `unused_mapping_rows` is empty (or you
know why), `unmatched` holds only ids you chose to leave as legacy identities, `superseded` counts
direct conversations that existed in two organizations (the less recently active one becomes
read-only and points at the other), and `warnings` says nothing new. The dry run rolls back. Then:

```sh
python3 deploy/aws/run-bootstrap-task.py --stack "$STACK" --mapping mapping.csv link-identities
```

It runs in one transaction and records every link in `dm.identity_links` and the run in
`dm.identity_link_runs`. Running it again changes nothing; a corrected mapping re-points links,
except for a uuid that has already signed in to DM (`--force-relink` overrides that).

### Step 6: start 0.15, set the webhook URL

**Run at cutover**:

```sh
aws cloudformation describe-stacks --stack-name "$STACK" --query 'Stacks[0].Parameters' > params-now.json
jq '[ .[] | {ParameterKey} + (if .ParameterKey == "RuntimeDesiredCount" then {ParameterValue: "1"}
                             else {UsePreviousValue: true} end) ]' params-now.json > params-start.json
aws cloudformation create-change-set --stack-name "$STACK" --change-set-name dm-015-start \
  --use-previous-template --parameters file://params-start.json --capabilities CAPABILITY_NAMED_IAM
aws cloudformation wait change-set-create-complete --stack-name "$STACK" --change-set-name dm-015-start
aws cloudformation describe-change-set --stack-name "$STACK" --change-set-name dm-015-start \
  --query 'Changes[].ResourceChange.[Action,LogicalResourceId,Replacement]' --output table   # the two services
aws cloudformation execute-change-set --stack-name "$STACK" --change-set-name dm-015-start
aws cloudformation wait stack-update-complete --stack-name "$STACK"
aws ecs wait services-stable --cluster silicon-dm-production --services silicon-dm-api silicon-dm-worker
curl -s -o /dev/null -w '%{http_code}\n' https://backend.dm.teamofsilicons.com/ready      # 204
silicon-apps webhook dm set https://backend.dm.teamofsilicons.com/webhook/ \
  --event id_change --event display_name_change --event pfp_change --event custodian_change \
  --event access_removed --event account_deleted
SILICON_HOME=$OPS_HOME silicon-accounts app webhook test
SILICON_HOME=$OPS_HOME silicon-accounts app webhook deliveries --status delivered      # the ping, answered 2xx
```

`custodian_change` is not among Silicon Accounts' default picks and DM needs it; sign-out events
arrive without a pick. The save keeps the secret from 2.3. A change that happened before the URL
was set heals on its own: DM refreshes ids and custodians on every sign-in and lookup.

### Step 7: CLI, Interface, website, fleet

**Run at cutover**:

```sh
silicon-apps releases dm --channel development          # the 0.15.0 development release id
silicon-apps promote dm <development release id> --version 0.15.0
```

Every Silicon Apps updater moves installed copies to 0.15.0 within about a minute. Then Interface
deploys its proof-based build, the website deploys (section 1), and the fleet switches its runtime
(section 1). Silicons sign in again once each; Carbons run `dm login` once per machine.

## 4. Verify

**Run at cutover**:

```sh
curl -s https://backend.dm.teamofsilicons.com/api/v1/accounts \
  | jq '.data | {app_id, accounts_url, contract_version, transport: .delivery.transport}'
# {"app_id":"dm","accounts_url":"https://accounts.teamofsilicons.com","contract_version":4,"transport":"sync"}
dm --version && dm accounts --json | jq -c '{app_id, version}'      # dm 0.15.0
dm login                                          # a Carbon: approve the code it prints
dm login status --json                            # authenticated: true, kind carbon
dm conversations list                             # history from before the switch, right participants
dm gifs trending                                  # GIFs come back: the Giphy key works
```

On a fleet host, as a Silicon: `silicon-accounts login --app dm -q | dm login --slt-stdin`,
`dm conversations list` shows the conversation with its custodian, and
`dm messages send c:<custodian> --text 'cutover check' --wait` completes; the Carbon sees it with
`dm inbox --from beginning` (or `dm inbox --watch` running beforehand). On the website, sign in, open
a conversation and send. In Interface, open a DM conversation and reply. In CloudWatch
(`/silicon-dm/production/api`), no `these obsolete variables are set` warning appears. Record the
image digests, the migration level and what you saw (no secrets) in `docs/history/`.

## 5. After the window

- **Silicons still on the old CLI.** A 0.14 `dm` (installed by Honeycomb, or the copy in the Silicon
  CLI bundle) gets `406 unsupported_contract` from every 0.15 route, so that Silicon can neither
  send nor read until its host runs `dm` 0.15 from Silicon Apps. Nothing updates those copies any
  more: DM is no longer released through Honeycomb. Its queued sends stay on disk; 0.15 sends them
  when the same id signs in to that home again, and fails them with `account_changed` if another id
  does. Withdrawing DM from the Honeycomb catalog (outside DM) stops anyone installing the dead
  0.14 from there.
- **Sync cursors** from 0.14 fail once with `sync_reset_required` (the cursor key is now
  `DM_DATA_KEY`); clients take a reset cursor and continue.
- **Ting** stays off: every handoff is kept and marked discarded (`ting_not_configured`).
- **Request rate.** Without Ting every Silicon long-polls the sync feed (`dm inbox --watch` asks
  again at least every 30 seconds), and the Silicons of one fleet host share its address. The WAF
  allows about 2,000 requests per 5 minutes per address; 16 idle Silicons use about 160, and each
  message adds a few. Watch the WAF's blocked-request metric (`silicon-dm-production-ip-rate`)
  for the first days and raise the limit if a host comes close. The long-poll waits in memory, not
  on a database connection, so it does not change the database pool sizing.
- **Close the rollback window** after about seven days, once nobody wants 0.14 back. **Run at
  cutover**:

  ```sh
  RUNTIME_SECRET_ARN=$(aws cloudformation describe-stacks --stack-name "$STACK" --output text \
    --query "Stacks[0].Outputs[?OutputKey=='RuntimeSecretArn'].OutputValue")
  aws secretsmanager get-secret-value --secret-id "$RUNTIME_SECRET_ARN" --query SecretString --output text > runtime.json
  jq 'del(.DM_IAM_APP_ID, .DM_IAM_APP_SECRET, .DM_IAM_WEBHOOK_SECRET, .DM_IAM_WEBHOOK_KEY_VERSION,
          .DM_TEST_KEY_ENCRYPTION_KEY, .DM_TEST_DATABASE_URL, .DM_HONEYCOMB_SERVICE_TOKEN, .DM_HONEYCOMB_BASE_URL)' \
    runtime.json > runtime.next.json
  aws secretsmanager put-secret-value --secret-id "$RUNTIME_SECRET_ARN" --secret-string file://runtime.next.json
  aws secretsmanager get-secret-value --secret-id "$APP_SECRET_ARN" --query SecretString --output text > app.json
  jq 'del(.DM_IAM_APP_ID, .DM_IAM_APP_SECRET, .DM_IAM_WEBHOOK_SECRET, .DM_IAM_WEBHOOK_KEY_VERSION,
          .DM_TEST_KEY_ENCRYPTION_KEY, .DM_TEST_DATABASE_PASSWORD, .DM_HONEYCOMB_SERVICE_TOKEN, .DM_HONEYCOMB_BASE_URL)' \
    app.json > app.next.json
  aws secretsmanager put-secret-value --secret-id "$APP_SECRET_ARN" --secret-string file://app.next.json
  rm -f runtime.json runtime.next.json app.json app.next.json
  aws ecs deregister-task-definition --task-definition silicon-dm-cutover:1
  rm -rf "$OPS_HOME"
  ```

  The bootstrap carries the 0.14 keys over only while they exist, so this is final. Then retire
  DM's registration and webhook at the earlier identity service (outside DM). The retained testing
  RDS instance stays until a Carbon decides what happens to its data.

## CLI and Silicon runtimes (dm 0.15)

What changes on a machine that runs `dm` (the fleet's part is in section 1):

1. **Install** through Silicon Apps: `silicon-apps install dm`. A running 0.14 relay is replaced
   automatically by the first 0.15 command; queued requests survive.
2. **Sign in again**: Carbons run `dm login` and approve the code; Silicons run
   `silicon-accounts login --app dm -q | dm login --slt-stdin` (or `dm login <slt>`). Each Silicon
   needs its Silicon Accounts identity first.
3. **Discovery**: `dm accounts --json` replaces `dm iam --json`, which stays as a hidden alias printing
   the same object for one release. `dm login status --json` keeps its meaning.
4. **Receiving**: `dm delivery login`, `dm webhook … --all-apps` and `dm unhook` answer
   `command_retired`; supervise `dm inbox --watch` or poll `dm sync --wait 30`, and fetch messages as
   before (`dm messages show`).
5. **Local state**: 0.14 profiles load as signed out (their tokens are dropped); requests queued for
   test environments fail with `testing_environments_retired`; settings of retired features are
   dropped at the first write.
6. **Rust consumers** (Interface) move to `silicon-dm-client` 0.15 (`with_token` / `with_proof`; no
   organization header).

## Rollback

- **Step 3's change set fails**: CloudFormation rolls the stack back by itself and 0.14 runs again.
- **Anything fails from step 4 until 0.15 has served traffic**: nothing was written since the
  snapshot (the services were stopped first), so go back to it. The database keeps its identifier,
  so the runtime secret's database URL keeps working. **Run at cutover**, only to roll back:

  ```sh
  aws ecs update-service --cluster silicon-dm-production --service silicon-dm-api --desired-count 0
  aws ecs update-service --cluster silicon-dm-production --service silicon-dm-worker --desired-count 0
  aws ecs wait services-stable --cluster silicon-dm-production --services silicon-dm-api silicon-dm-worker
  aws rds modify-db-instance --db-instance-identifier silicon-dm-production \
    --new-db-instance-identifier silicon-dm-production-accounts-attempt --apply-immediately
  aws rds wait db-instance-available --db-instance-identifier silicon-dm-production-accounts-attempt
  aws rds restore-db-instance-from-db-snapshot --db-instance-identifier silicon-dm-production \
    --db-snapshot-identifier "$SNAPSHOT" --db-subnet-group-name "$(jq -r .subnets database-0.14.json)" \
    --vpc-security-group-ids $(jq -r '.groups[]' database-0.14.json) \
    --db-parameter-group-name "$(jq -r .parameters database-0.14.json)" \
    --db-instance-class "$(jq -r .class database-0.14.json)" --no-publicly-accessible --deletion-protection
  aws rds wait db-instance-available --db-instance-identifier silicon-dm-production
  jq '[.[] | {ParameterKey, ParameterValue}]' params-0.14.json > params-rollback.json
  aws cloudformation create-change-set --stack-name "$STACK" --change-set-name dm-014-rollback \
    --template-body file://template-0.14.yaml --parameters file://params-rollback.json \
    --capabilities CAPABILITY_NAMED_IAM
  aws cloudformation wait change-set-create-complete --stack-name "$STACK" --change-set-name dm-014-rollback
  aws cloudformation execute-change-set --stack-name "$STACK" --change-set-name dm-014-rollback
  aws cloudformation wait stack-update-complete --stack-name "$STACK"
  # Return each service to the task definition services-0.14.json names, if the stack's differs:
  aws ecs update-service --cluster silicon-dm-production --service silicon-dm-api --task-definition <api from services-0.14.json>
  aws ecs update-service --cluster silicon-dm-production --service silicon-dm-worker --task-definition <worker from services-0.14.json>
  aws ecs wait services-stable --cluster silicon-dm-production --services silicon-dm-api silicon-dm-worker
  SILICON_HOME=$OPS_HOME silicon-accounts app webhook remove
  silicon-apps withdraw dm <0.15.0 release id> --reason "DM is back on 0.14 while its move to Silicon Accounts is retried."
  ```

  The 0.14 tasks start because the runtime secret still holds the 0.14 keys. Interface and the fleet
  return to their previous builds; hosts whose Honeycomb `dm` was removed reinstall it. Keep the
  `-accounts-attempt` instance until the retry, then delete it (it has deletion protection on). The
  runtime role's password lives in the database, so the 0.14 tasks connect as before; master access
  and CloudFormation's view of `Database` need reconciling (drift) before the next bootstrap or
  stack update.
- **After 0.15 has served traffic**: restoring the snapshot loses everything written since the
  switch. Prefer fixing forward; roll back as above only for a failure that cannot wait, and accept
  that loss.

## Blocked on

- Production registration of the app `dm` in Silicon Accounts and Silicon Apps (two-character id;
  the platform owners).
- Silicon Interface sending User verification proofs instead of IAM tokens (same window).
- The Silicon runtime switching to Silicon Apps installs, the new sign-in and `dm inbox --watch`.
- Public-origin verification of the Next.js website and deployment in the coordinated window;
  local implementation, build and browser evidence are recorded in `web-verification.md`.
- Ting accepting Silicon Accounts proofs, only to turn Ting delivery back on (not for the cutover).
  Resolved on 17 October 2026: DM 0.16.1 uses Ting's current App/User proof contract,
  and production receive-and-reply delivery was verified. See
  [the activation receipt](ting-delivery-release-20261017.json).
