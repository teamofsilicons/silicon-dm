#!/bin/bash
# Appended after the CloudFormation-generated environment preamble. No secrets.
set -Eeuo pipefail
umask 077

signal_result() {
  local result=$?
  trap - EXIT
  local status=FAILURE
  if [ "$result" -eq 0 ]; then status=SUCCESS; fi
  aws cloudformation signal-resource --stack-name "$GATEWAY_STACK_ID" \
    --logical-resource-id GatewayReady --unique-id "$GATEWAY_STATE_VOLUME_ID" \
    --status "$status" || true
  exit "$result"
}
trap signal_result EXIT

dnf install -y docker e2fsprogs util-linux python3
metadata_token=$(curl --fail --silent --request PUT --header "X-aws-ec2-metadata-token-ttl-seconds: 60" http://169.254.169.254/latest/api/token)
export GATEWAY_INSTANCE_ID=$(curl --fail --silent --header "X-aws-ec2-metadata-token: $metadata_token" http://169.254.169.254/latest/meta-data/instance-id)
unset metadata_token
systemctl enable --now docker amazon-ssm-agent

# Nitro device order is not stable. Match the exact CloudFormation volume ID,
# never /dev/nvme1n1 by position and never the root device.
expected_serial=$(printf '%s' "$GATEWAY_STATE_VOLUME_ID" | tr -d '-')
state_device=''
for attempt in $(seq 1 120); do
  for serial_path in /sys/block/nvme*n1/device/serial; do
    [ -f "$serial_path" ] || continue
    serial=$(tr -d '[:space:]' < "$serial_path")
    if [ "$serial" = "$expected_serial" ]; then
      state_device=/dev/$(basename "$(dirname "$(dirname "$serial_path")")")
      break
    fi
  done
  [ -n "$state_device" ] && break
  sleep 5
done
[ -b "$state_device" ] || { echo 'Expected state volume did not attach.' >&2; exit 1; }
[ "$(lsblk -dn -o TYPE "$state_device")" = disk ] || exit 1
[ "$(lsblk -nr -o NAME "$state_device" | wc -l)" -eq 1 ] || {
  echo 'State device has unexpected partitions; refusing to modify it.' >&2; exit 1;
}
[ -z "$(lsblk -n -o MOUNTPOINTS "$state_device" | tr -d '[:space:]')" ] || {
  echo 'State device is already mounted; refusing bootstrap.' >&2; exit 1;
}

filesystem=$(blkid -p -s TYPE -o value "$state_device" || true)
new_filesystem=false
if [ -z "$filesystem" ]; then
  [ -z "$(wipefs --no-act --noheadings --output TYPE "$state_device")" ] || {
    echo 'Unexpected disk signature; refusing to format state.' >&2; exit 1;
  }
  # EBS encryption does not guarantee that uninitialized bytes read as zero.
  # Only initialize this stack-created, snapshot-free volume during its first
  # hour, attached to this exact instance. Older/damaged state fails closed.
  volume_metadata=$(aws ec2 describe-volumes --volume-ids "$GATEWAY_STATE_VOLUME_ID" --output json)
  export GATEWAY_VOLUME_METADATA="$volume_metadata"
  python3 - <<'CHECK_VOLUME'
import datetime, json, os
v = json.loads(os.environ["GATEWAY_VOLUME_METADATA"])["Volumes"][0]
tags = {t["Key"]: t["Value"] for t in v.get("Tags", [])}
created = datetime.datetime.fromisoformat(v["CreateTime"].replace("Z", "+00:00"))
age = (datetime.datetime.now(datetime.timezone.utc) - created).total_seconds()
assert v["VolumeId"] == os.environ["GATEWAY_STATE_VOLUME_ID"]
assert v["Encrypted"] and not v.get("SnapshotId") and 0 <= age <= 3600
assert tags.get("aws:cloudformation:stack-id") == os.environ["GATEWAY_STACK_ID"]
assert len(v["Attachments"]) == 1 and v["Attachments"][0]["State"] == "attached"
assert v["Attachments"][0]["InstanceId"] == os.environ["GATEWAY_INSTANCE_ID"]
CHECK_VOLUME
  unset GATEWAY_VOLUME_METADATA volume_metadata
  mkfs.ext4 -q -L dm-gateway-state "$state_device"
  new_filesystem=true
elif [ "$filesystem" != ext4 ] || \
  [ "$(blkid -p -s LABEL -o value "$state_device")" != dm-gateway-state ]; then
  echo 'Unexpected state filesystem or label; refusing bootstrap.' >&2
  exit 1
fi

state_uuid=$(blkid -p -s UUID -o value "$state_device")
[[ "$state_uuid" =~ ^[0-9a-f-]{36}$ ]] || exit 1
install -d -m 0700 /var/lib/silicon-dm-gateway
[ ! -L /var/lib/silicon-dm-gateway ] || exit 1
printf 'UUID=%s /var/lib/silicon-dm-gateway ext4 defaults,nosuid,nodev,noexec 0 2\n' \
  "$state_uuid" >> /etc/fstab
mount /var/lib/silicon-dm-gateway
[ "$(findmnt -n -o UUID --target /var/lib/silicon-dm-gateway)" = "$state_uuid" ] || exit 1
marker=/var/lib/silicon-dm-gateway/.volume-id
if [ "$new_filesystem" = true ]; then
  printf '%s\n' "$GATEWAY_STATE_VOLUME_ID" > "$marker"
else
  [ -f "$marker" ] && [ ! -L "$marker" ] && \
    [ "$(cat "$marker")" = "$GATEWAY_STATE_VOLUME_ID" ] || {
      echo 'Existing state lacks matching volume identity; manual recovery required.' >&2; exit 1;
    }
fi
chown 10001:10001 /var/lib/silicon-dm-gateway
chmod 0700 /var/lib/silicon-dm-gateway

install -d -m 0755 /etc/silicon-dm-gateway
cat > /etc/silicon-dm-gateway/runtime.env <<EOF
NODE_ENV=production
HOST=0.0.0.0
PORT=4315
DM_WEB_ORIGIN=$GATEWAY_ORIGIN
DM_FRONTEND_ORIGIN=$GATEWAY_FRONTEND_ORIGIN
DM_API_ORIGIN=https://backend.dm.teamofsilicons.com
IAM_LOGIN_ORIGIN=https://auth.iam.teamofsilicons.com
DM_WEB_APP_ID=tos>dm
DM_WEB_DEFAULT_ORG=tos
DM_WEB_STATE_DIR=/var/lib/silicon-dm-gateway
DM_WEB_MAX_BODY_BYTES=134217728
EOF
chmod 0600 /etc/silicon-dm-gateway/runtime.env

registry=$(printf '%s' "$GATEWAY_IMAGE" | cut -d/ -f1)
aws ecr get-login-password | docker login --username AWS --password-stdin "$registry"
docker pull "$GATEWAY_IMAGE"
docker logout "$registry"

# Hold a host-level lock for the container lifetime. A previous container's PID
# namespace is gone after removal, so its PID-only application lock is stale.
cat > /usr/local/bin/silicon-dm-gateway-prepare <<'PREPARE'
#!/bin/bash
set -euo pipefail
mountpoint -q /var/lib/silicon-dm-gateway
if docker inspect silicon-dm-gateway > /dev/null 2>&1; then
  [ "$(docker inspect --format '{{.State.Running}}' silicon-dm-gateway)" = false ] || exit 1
  docker rm silicon-dm-gateway
fi
rm -f /var/lib/silicon-dm-gateway/gateway.lock
PREPARE
chmod 0755 /usr/local/bin/silicon-dm-gateway-prepare

cat > /etc/systemd/system/silicon-dm-gateway.service <<EOF
[Unit]
Description=Silicon DM browser gateway
Requires=docker.service
After=docker.service network-online.target
Wants=network-online.target
RequiresMountsFor=/var/lib/silicon-dm-gateway
ConditionPathIsMountPoint=/var/lib/silicon-dm-gateway

[Service]
Type=simple
Restart=on-failure
RestartSec=5
TimeoutStartSec=120
TimeoutStopSec=25
ExecStartPre=/usr/bin/flock --nonblock /var/lib/silicon-dm-gateway/.host.lock /usr/local/bin/silicon-dm-gateway-prepare
ExecStart=/usr/bin/flock --nonblock /var/lib/silicon-dm-gateway/.host.lock /usr/bin/docker run --name silicon-dm-gateway --init --pull=never --user 10001:10001 --read-only --cap-drop=ALL --security-opt=no-new-privileges --pids-limit=128 --memory=3g --cpus=2 --stop-timeout=15 --tmpfs /tmp:rw,nosuid,nodev,noexec,size=16m --publish 4315:4315 --mount type=bind,src=/var/lib/silicon-dm-gateway,dst=/var/lib/silicon-dm-gateway --env-file /etc/silicon-dm-gateway/runtime.env --log-driver=awslogs --log-opt awslogs-region=$AWS_DEFAULT_REGION --log-opt awslogs-group=$GATEWAY_LOG_GROUP --log-opt awslogs-stream=gateway-$GATEWAY_STATE_VOLUME_ID --log-opt awslogs-create-group=false $GATEWAY_IMAGE
ExecStop=/usr/bin/docker stop --time 15 silicon-dm-gateway
ExecStopPost=-/usr/bin/docker rm silicon-dm-gateway

[Install]
WantedBy=multi-user.target
EOF
chmod 0644 /etc/systemd/system/silicon-dm-gateway.service
systemctl daemon-reload
systemctl enable --now silicon-dm-gateway
for attempt in $(seq 1 60); do
  if curl --fail --silent --max-time 3 http://127.0.0.1:4315/healthz > /dev/null; then
    echo 'Gateway bootstrap complete; persistent state mounted and readiness passed.'
    exit 0
  fi
  sleep 2
done
echo 'Gateway readiness failed.' >&2
exit 1
