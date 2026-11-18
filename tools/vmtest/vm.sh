#!/usr/bin/env bash
# Throwaway AWS VMs for testing only3d on real Windows and Linux, with and without a GPU.
#
# Every VM deletes itself. It powers off at its deadline, and powering off means "terminate"
# (the disk goes with it). An EventBridge schedule also terminates it 15 minutes later, in case
# the OS hangs. You reach its desktop through Amazon DCV over an SSH tunnel; DCV has no login of
# its own, so only someone holding the tunnel's SSH key gets in.
#
#   vm.sh setup                    one time: SSH key, security group, IAM roles
#   vm.sh up <os> <gpu> [opts]     os: windows | linux; gpu: cpu | nvidia | amd (amd: Windows only)
#        --minutes N               lifetime (default 60)
#        --on-demand               no spot (costs more, can't be reclaimed mid-test)
#   vm.sh list                     the test VMs and their deadlines
#   vm.sh wait <name>              until setup inside the VM has finished (10-20 minutes)
#   vm.sh ssh <name> [command]     a shell, or one command (PowerShell on Windows)
#   vm.sh push <name> <file>...    copy files to C:\vmtest (Windows) or ~/vmtest (Linux)
#   vm.sh pull <name> <remote> [local]
#   vm.sh tunnel <name>            DCV at https://localhost:<port>; runs until Ctrl-C
#   vm.sh extend <name> [minutes]  a new deadline, N minutes from now (default 30)
#   vm.sh down <name|all>          terminate now
#
# A VM's name is only3d-<os>-<gpu>, e.g. only3d-windows-nvidia. AWS_PROFILE and AWS_REGION pick
# the account and region. Keys and state live in target/vmtest (never committed).
set -euo pipefail
# bash reads a script as it runs it: the braces make it read all of this first, so editing the
# file can't derail a copy that is still running (a long `wait` or `tunnel`)
{

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
state="$root/target/vmtest"
key="$state/id_ed25519"
tag=only3d-vmtest
region=${AWS_REGION:-$(aws configure get region || true)}
[ -n "$region" ] || { echo "Set AWS_REGION or a region in your AWS profile." >&2; exit 1; }
export AWS_REGION=$region AWS_PAGER=""

ssh_opts=(-i "$key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=10 -o ServerAliveInterval=30)

die() { echo "vm.sh: $*" >&2; exit 1; }
account() { aws sts get-caller-identity --query Account --output text; }
my_ip() { curl -fsS https://checkip.amazonaws.com | tr -d '[:space:]'; }

user_of() { case "$1" in only3d-windows-*) echo Administrator ;; *) echo ubuntu ;; esac; }

port_of() {
  case "$1" in
    only3d-windows-cpu) echo 8441 ;; only3d-windows-nvidia) echo 8442 ;; only3d-windows-amd) echo 8443 ;;
    only3d-linux-cpu) echo 8444 ;; only3d-linux-nvidia) echo 8445 ;; *) echo 8449 ;;
  esac
}

# instance id and public IP of a running (or starting) test VM
lookup() {
  aws ec2 describe-instances \
    --filters "Name=tag:project,Values=$tag" "Name=tag:Name,Values=$1" "Name=instance-state-name,Values=pending,running" \
    --query 'Reservations[].Instances[0].[InstanceId,PublicIpAddress]' --output text | head -1
}

ip_of() {
  local r; r=$(lookup "$1"); [ -n "$r" ] || die "no running VM named $1 (see: vm.sh list)"
  echo "$r" | awk '{print $2}'
}

sg_id() {
  aws ec2 describe-security-groups --filters "Name=group-name,Values=$tag" --query 'SecurityGroups[0].GroupId' --output text
}

# SSH from this machine's current address only
allow_me() {
  local sg=$1 ip; ip=$(my_ip)
  aws ec2 authorize-security-group-ingress --group-id "$sg" \
    --ip-permissions "IpProtocol=tcp,FromPort=22,ToPort=22,IpRanges=[{CidrIp=$ip/32,Description=only3d-vmtest}]" >/dev/null 2>&1 || true
}

# one TLS certificate for every VM's DCV, so a browser only has to accept it once
ensure_cert() {
  [ -f "$state/dcv.pem" ] && return
  openssl req -x509 -newkey rsa:2048 -nodes -days 825 -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost" \
    -keyout "$state/dcv.key" -out "$state/dcv.pem" 2>/dev/null
}

cmd_setup() {
  mkdir -p "$state"
  [ -f "$key" ] || ssh-keygen -q -t ed25519 -N "" -C "$tag" -f "$key"
  ensure_cert
  aws ec2 delete-key-pair --key-name "$tag" >/dev/null 2>&1 || true
  aws ec2 import-key-pair --key-name "$tag" --public-key-material "fileb://$key.pub" \
    --tag-specifications "ResourceType=key-pair,Tags=[{Key=project,Value=$tag}]" >/dev/null
  echo "key pair $tag imported"

  local vpc sg
  vpc=$(aws ec2 describe-vpcs --filters Name=isDefault,Values=true --query 'Vpcs[0].VpcId' --output text)
  [ "$vpc" != None ] || die "this region has no default VPC"
  sg=$(sg_id)
  if [ "$sg" = None ]; then
    sg=$(aws ec2 create-security-group --group-name "$tag" --description "only3d test VMs: SSH from the tester only" --vpc-id "$vpc" \
      --tag-specifications "ResourceType=security-group,Tags=[{Key=project,Value=$tag}]" --query GroupId --output text)
  else
    # forget old addresses
    local old; old=$(aws ec2 describe-security-groups --group-ids "$sg" --query 'SecurityGroups[0].IpPermissions' --output json)
    [ "$old" = "[]" ] || aws ec2 revoke-security-group-ingress --group-id "$sg" --ip-permissions "$old" >/dev/null
  fi
  allow_me "$sg"
  echo "security group $sg allows SSH from this machine only"

  local acct; acct=$(account)
  if ! aws iam get-role --role-name "$tag-instance" >/dev/null 2>&1; then
    aws iam create-role --role-name "$tag-instance" --tags "Key=project,Value=$tag" \
      --assume-role-policy-document '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}' >/dev/null
    aws iam create-instance-profile --instance-profile-name "$tag-instance" >/dev/null
    aws iam add-role-to-instance-profile --instance-profile-name "$tag-instance" --role-name "$tag-instance"
    sleep 10 # IAM is eventually consistent
  fi
  # GPU drivers come from AWS's driver buckets; DCV checks its free EC2 licence in S3
  local buckets='"arn:aws:s3:::ec2-windows-nvidia-drivers","arn:aws:s3:::ec2-windows-nvidia-drivers/*",
    "arn:aws:s3:::ec2-linux-nvidia-drivers","arn:aws:s3:::ec2-linux-nvidia-drivers/*",
    "arn:aws:s3:::ec2-amd-windows-drivers","arn:aws:s3:::ec2-amd-windows-drivers/*",
    "arn:aws:s3:::dcv-license.'"$region"'/*"'
  aws iam put-role-policy --role-name "$tag-instance" --policy-name drivers-and-dcv \
    --policy-document '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:GetObject","s3:ListBucket"],"Resource":['"$buckets"']}]}'
  echo "instance role $tag-instance can read the GPU driver buckets"

  if ! aws iam get-role --role-name "$tag-scheduler" >/dev/null 2>&1; then
    aws iam create-role --role-name "$tag-scheduler" --tags "Key=project,Value=$tag" \
      --assume-role-policy-document '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"scheduler.amazonaws.com"},"Action":"sts:AssumeRole","Condition":{"StringEquals":{"aws:SourceAccount":"'"$acct"'"}}}]}' >/dev/null
  fi
  aws iam put-role-policy --role-name "$tag-scheduler" --policy-name terminate-test-vms \
    --policy-document '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"ec2:TerminateInstances","Resource":"arn:aws:ec2:*:'"$acct"':instance/*","Condition":{"StringEquals":{"aws:ResourceTag/project":"'"$tag"'"}}}]}'
  echo "scheduler role $tag-scheduler can terminate test VMs only"
}

cmd_up() {
  local os=${1:-} gpu=${2:-} minutes=60 spot=1
  shift 2 || true
  while [ $# -gt 0 ]; do
    case "$1" in
      --minutes) minutes=$2; shift 2 ;;
      --on-demand) spot=0; shift ;;
      *) die "unknown option $1" ;;
    esac
  done
  local type ami_param disk
  case "$gpu" in
    cpu) type=t3.xlarge ;;
    nvidia) type=g4dn.xlarge ;;
    # g4ad (Radeon Pro V520) was the AMD option; AWS no longer offers it in any region checked
    # (2026-11-18). Azure's NVads V710 v5 is the remaining AMD GPU to rent.
    amd) type=g4ad.xlarge ;;
    *) die "gpu is cpu, nvidia or amd" ;;
  esac
  case "$os" in
    # g4ad (AMD) machines can't boot UEFI-only images such as Server 2025's: Server 2022 there
    windows) ami_param=/aws/service/ami-windows-latest/Windows_Server-2025-English-Full-Base; disk=60
      [ "$gpu" = amd ] && ami_param=/aws/service/ami-windows-latest/Windows_Server-2022-English-Full-Base ;;
    linux) ami_param=/aws/service/canonical/ubuntu/server/24.04/stable/current/amd64/hvm/ebs-gp3/ami-id; disk=40
      [ "$gpu" != amd ] || die "the AMD driver isn't set up for Linux here; use windows amd" ;;
    *) die "os is windows or linux" ;;
  esac
  [ -f "$key" ] || die "run vm.sh setup first"
  local name="only3d-$os-$gpu"
  [ -z "$(lookup "$name")" ] || die "$name is already running (vm.sh down $name first)"

  local ami root_dev sg deadline expires ud
  ami=$(aws ssm get-parameter --name "$ami_param" --query Parameter.Value --output text)
  root_dev=$(aws ec2 describe-images --image-ids "$ami" --query 'Images[0].RootDeviceName' --output text)
  sg=$(sg_id); [ "$sg" != None ] || die "run vm.sh setup first"
  allow_me "$sg"
  deadline=$(( $(date +%s) + minutes * 60 ))
  expires="$(utc "$deadline")Z"
  ud="$state/$name.userdata"
  ensure_cert
  local cert keyb64
  cert=$(base64 < "$state/dcv.pem" | tr -d '\n'); keyb64=$(base64 < "$state/dcv.key" | tr -d '\n')
  local src="$here/linux.sh"; [ "$os" = windows ] && src="$here/windows.ps1"
  sed -e "s|__DEADLINE__|$deadline|" -e "s|__GPU__|$gpu|" -e "s|__PUBKEY__|$(cat "$key.pub")|" \
      -e "s|__DCV_CERT__|$cert|" -e "s|__DCV_KEY__|$keyb64|" "$src" > "$ud"

  local market=() keypair=()
  [ "$spot" = 1 ] && market=(--instance-market-options 'MarketType=spot,SpotOptions={SpotInstanceType=one-time,InstanceInterruptionBehavior=terminate}')
  # Windows AMIs refuse ED25519 key pairs; windows.ps1 installs the SSH key itself
  [ "$os" = linux ] && keypair=(--key-name "$tag")
  # capacity runs out per availability zone, especially for GPUs on spot: try each zone
  local id="" subnet err
  for subnet in $(aws ec2 describe-subnets --filters Name=default-for-az,Values=true --query 'Subnets[].SubnetId' --output text); do
    if id=$(aws ec2 run-instances --image-id "$ami" --instance-type "$type" ${keypair[@]+"${keypair[@]}"} --subnet-id "$subnet" --security-group-ids "$sg" \
    --iam-instance-profile "Name=$tag-instance" --instance-initiated-shutdown-behavior terminate \
    --metadata-options HttpTokens=required \
    --block-device-mappings "DeviceName=$root_dev,Ebs={VolumeSize=$disk,VolumeType=gp3,DeleteOnTermination=true}" \
    --user-data "file://$ud" ${market[@]+"${market[@]}"} \
    --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=$name},{Key=project,Value=$tag},{Key=expires,Value=$expires}]" \
                         "ResourceType=volume,Tags=[{Key=project,Value=$tag}]" \
    --query 'Instances[0].InstanceId' --output text 2>"$state/run.err"); then
      break
    fi
    err=$(cat "$state/run.err"); id=""
    case "$err" in *InsufficientInstanceCapacity*|*capacity*) echo "no $type capacity in $subnet, trying the next zone" ;; *) die "$err" ;; esac
  done
  [ -n "$id" ] || die "no $type capacity in any zone of $region right now$([ "$spot" = 1 ] && echo '; try --on-demand')"
  echo "$name: $id ($type, $([ "$spot" = 1 ] && echo spot || echo on-demand)), deletes itself at $expires"

  backstop "$id" "$deadline"

  aws ec2 wait instance-running --instance-ids "$id"
  echo "running at $(ip_of "$name"); setup inside takes 10-20 minutes: vm.sh wait $name"
}

utc() { date -u -r "$1" +%Y-%m-%dT%H:%M:%S 2>/dev/null || date -u -d "@$1" +%Y-%m-%dT%H:%M:%S; }

# EventBridge terminates the VM 15 minutes after its deadline, even if the VM hangs
backstop() {
  local id=$1 deadline=$2 at acct; acct=$(account); at=$(utc $((deadline + 900)))
  aws scheduler delete-schedule --name "$tag-$id" >/dev/null 2>&1 || true
  aws scheduler create-schedule --name "$tag-$id" --schedule-expression "at($at)" --schedule-expression-timezone UTC \
    --flexible-time-window Mode=OFF --action-after-completion DELETE \
    --target '{"Arn":"arn:aws:scheduler:::aws-sdk:ec2:terminateInstances","RoleArn":"arn:aws:iam::'"$acct"':role/'"$tag"'-scheduler","Input":"{\"InstanceIds\":[\"'"$id"'\"]}"}' >/dev/null
  echo "backstop: EventBridge terminates it at ${at}Z"
}

# a new deadline, N minutes from now
cmd_extend() {
  local name=$1 minutes=${2:-30} id deadline
  id=$(lookup "$name" | awk '{print $1}'); [ -n "$id" ] || die "no running VM named $name"
  deadline=$(( $(date +%s) + minutes * 60 ))
  case "$name" in
    only3d-windows-*) remote "$name" "Set-Content C:\\vmtest\\deadline.txt $deadline" ;;
    *) remote "$name" "echo $deadline | sudo tee /var/lib/only3d-vmtest/deadline >/dev/null" ;;
  esac
  aws ec2 create-tags --resources "$id" --tags "Key=expires,Value=$(utc "$deadline")Z"
  echo "$name now deletes itself at $(utc "$deadline")Z"
  backstop "$id" "$deadline"
}

cmd_list() {
  aws ec2 describe-instances --filters "Name=tag:project,Values=$tag" "Name=instance-state-name,Values=pending,running,stopping,shutting-down" \
    --query 'Reservations[].Instances[].[Tags[?Key==`Name`]|[0].Value,InstanceId,InstanceType,State.Name,PublicIpAddress,Tags[?Key==`expires`]|[0].Value]' \
    --output table
}

remote() { # name, command
  local name=$1; shift
  ssh "${ssh_opts[@]}" "$(user_of "$name")@$(ip_of "$name")" "$@"
}

cmd_wait() {
  local name=$1 check started; started=$(date +%s)
  case "$name" in
    only3d-windows-*) check='if (Test-Path C:\vmtest\ready.txt) { "ready" }' ;;
    *) check='test -f /var/lib/only3d-vmtest/ready && echo ready' ;;
  esac
  until [ "$(remote "$name" "$check" 2>/dev/null | tr -d '[:space:]')" = ready ]; do
    [ $(( $(date +%s) - started )) -lt 2400 ] || die "$name isn't ready after 40 minutes; look at its setup log (vm.sh ssh $name)"
    sleep 20
  done
  echo "$name is ready ($(( ($(date +%s) - started) / 60 )) min). Desktop: vm.sh tunnel $name, then https://localhost:$(port_of "$name")"
}

cmd_push() {
  local name=$1; shift
  local dest; case "$name" in only3d-windows-*) dest="C:/vmtest/" ;; *) dest="vmtest/" ;; esac
  case "$name" in only3d-windows-*) ;; *) remote "$name" "mkdir -p vmtest" ;; esac
  scp "${ssh_opts[@]}" "$@" "$(user_of "$name")@$(ip_of "$name"):$dest"
}

cmd_pull() {
  local name=$1 src=$2 dst=${3:-.}
  scp "${ssh_opts[@]}" "$(user_of "$name")@$(ip_of "$name"):$src" "$dst"
}

cmd_tunnel() {
  local name=$1 port; port=$(port_of "$name")
  echo "DCV for $name at https://localhost:$port (Ctrl-C closes the tunnel)"
  ssh "${ssh_opts[@]}" -N -L "$port:localhost:8443" "$(user_of "$name")@$(ip_of "$name")"
}

cmd_down() {
  local ids
  if [ "${1:-}" = all ]; then
    ids=$(aws ec2 describe-instances --filters "Name=tag:project,Values=$tag" "Name=instance-state-name,Values=pending,running,stopping" --query 'Reservations[].Instances[].InstanceId' --output text)
  else
    ids=$(lookup "$1" | awk '{print $1}')
  fi
  [ -n "$ids" ] || { echo "nothing to terminate"; return; }
  aws ec2 terminate-instances --instance-ids $ids --query 'TerminatingInstances[].[InstanceId,CurrentState.Name]' --output text
  for id in $ids; do aws scheduler delete-schedule --name "$tag-$id" >/dev/null 2>&1 || true; done
}

case "${1:-}" in
  setup) shift; cmd_setup "$@" ;;
  up) shift; cmd_up "$@" ;;
  list) shift; cmd_list ;;
  wait) shift; cmd_wait "$@" ;;
  ssh) shift; remote "$@" ;;
  push) shift; cmd_push "$@" ;;
  pull) shift; cmd_pull "$@" ;;
  tunnel) shift; cmd_tunnel "$@" ;;
  down) shift; cmd_down "$@" ;;
  extend) shift; cmd_extend "$@" ;;
  *) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print } /^set /{ exit }' "$0"; exit 1 ;;
esac
exit
}
