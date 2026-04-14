#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/redeploy.sh [--tag TAG] [--skip-healthcheck]

Builds a new linux/arm64 image, pushes it to ECR, updates
infra/tofu/terraform.tfvars, applies the stack, waits for ECS to stabilize,
prints the current task public IP, and optionally runs /healthz.

AWS_CONFIG_FILE / AWS_SHARED_CREDENTIALS_FILE override credential file paths.
AWS_PROFILE / AWS_REGION override deploy target selection.
EOF
}

log() {
  printf '[redeploy] %s\n' "$*"
}

fail() {
  printf '[redeploy] error: %s\n' "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

read_tfvar_string() {
  local key="$1"
  local fallback="$2"
  local value
  value="$(sed -nE "s/^[[:space:]]*${key}[[:space:]]*=[[:space:]]*\"([^\"]*)\"[[:space:]]*$/\\1/p" "$TFVARS" | head -n1)"
  if [[ -n "$value" ]]; then
    printf '%s\n' "$value"
  else
    printf '%s\n' "$fallback"
  fi
}

update_tfvar_tag() {
  local tag="$1"
  if grep -qE '^[[:space:]]*container_image_tag[[:space:]]*=' "$TFVARS"; then
    perl -0pi -e 's/^(\s*container_image_tag\s*=\s*)".*"$/\1"'"$tag"'"/m' "$TFVARS"
  else
    printf '\ncontainer_image_tag = "%s"\n' "$tag" >>"$TFVARS"
  fi
}

skip_healthcheck=0
tag=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag)
      [[ $# -ge 2 ]] || fail "--tag requires a value"
      tag="$2"
      shift 2
      ;;
    --skip-healthcheck)
      skip_healthcheck=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

require_command aws
require_command docker
require_command tofu
require_command perl
if [[ "$skip_healthcheck" -eq 0 ]]; then
  require_command curl
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
INFRA_DIR="$REPO_ROOT/infra/tofu"
TFVARS="$INFRA_DIR/terraform.tfvars"

[[ -f "$TFVARS" ]] || fail "missing $TFVARS"

AWS_CONFIG_PATH="${AWS_CONFIG_FILE:-$REPO_ROOT/.aws/config}"
AWS_CREDENTIALS_PATH="${AWS_SHARED_CREDENTIALS_FILE:-$REPO_ROOT/.aws/credentials}"
AWS_PROFILE_VALUE="${AWS_PROFILE:-$(read_tfvar_string aws_profile ancilla-dev)}"
AWS_REGION_VALUE="${AWS_REGION:-${AWS_DEFAULT_REGION:-$(read_tfvar_string aws_region us-west-2)}}"

[[ -f "$AWS_CONFIG_PATH" ]] || fail "missing AWS config file: $AWS_CONFIG_PATH"
[[ -f "$AWS_CREDENTIALS_PATH" ]] || fail "missing AWS shared credentials file: $AWS_CREDENTIALS_PATH"

export AWS_CONFIG_FILE="$AWS_CONFIG_PATH"
export AWS_SHARED_CREDENTIALS_FILE="$AWS_CREDENTIALS_PATH"
export AWS_PROFILE="$AWS_PROFILE_VALUE"
export AWS_REGION="$AWS_REGION_VALUE"
export AWS_DEFAULT_REGION="$AWS_REGION_VALUE"

tag="${tag:-deploy-$(date +%Y%m%d-%H%M)}"

log "using repo-local AWS config: $AWS_CONFIG_FILE"
log "using repo-local AWS credentials: $AWS_SHARED_CREDENTIALS_FILE"
log "using AWS profile: $AWS_PROFILE"
log "using AWS region: $AWS_REGION"
log "using image tag: $tag"

aws sts get-caller-identity >/dev/null

pushd "$INFRA_DIR" >/dev/null
ECR_URL="$(tofu output -raw ecr_repository_url)"
CLUSTER="$(tofu output -raw ecs_cluster_name)"
SERVICE="$(tofu output -raw ecs_service_name)"
popd >/dev/null

REGISTRY_HOST="${ECR_URL%/*}"

log "logging into ECR: $REGISTRY_HOST"
aws ecr get-login-password | docker login --username AWS --password-stdin "$REGISTRY_HOST" >/dev/null

log "building image: $ECR_URL:$tag"
pushd "$REPO_ROOT" >/dev/null
docker buildx build --platform linux/arm64 --load -t "$ECR_URL:$tag" .
docker push "$ECR_URL:$tag"
popd >/dev/null

log "updating terraform.tfvars container_image_tag"
update_tfvar_tag "$tag"

log "applying infrastructure"
pushd "$INFRA_DIR" >/dev/null
tofu apply -auto-approve

log "waiting for ECS service to stabilize"
aws ecs wait services-stable --cluster "$CLUSTER" --services "$SERVICE"

TASK_ARN="$(aws ecs list-tasks --cluster "$CLUSTER" --service-name "$SERVICE" --query 'taskArns[0]' --output text)"
ENI="$(aws ecs describe-tasks --cluster "$CLUSTER" --tasks "$TASK_ARN" --query "tasks[0].attachments[0].details[?name==\`networkInterfaceId\`].value | [0]" --output text)"
APP_IP="$(aws ec2 describe-network-interfaces --network-interface-ids "$ENI" --query 'NetworkInterfaces[0].Association.PublicIp' --output text)"
popd >/dev/null

log "task ARN: $TASK_ARN"
log "app IP: $APP_IP"

if [[ "$skip_healthcheck" -eq 0 ]]; then
  log "running healthcheck"
  curl -sS -o /tmp/ancilla-healthcheck.out -w '[redeploy] healthz status: %{http_code}\n' "http://$APP_IP:3000/healthz"
fi

cat <<EOF

Redeploy complete.
Image: $ECR_URL:$tag
Task:  $TASK_ARN
IP:    $APP_IP

Try:
  curl "http://$APP_IP:3000/healthz"
  curl "http://$APP_IP:3000/v1/timeline"

If you want the ratatui client to target this deploy, set:
  base_url = "http://$APP_IP:3000"
EOF
