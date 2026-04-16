#!/usr/bin/env bash
set -euo pipefail

SCRIPT_NAME="$(basename "$0")"
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/deploy-common.sh"

usage() {
  cat <<'EOF'
Usage: scripts/deploy-app.sh [--tag TAG] [--skip-healthcheck]

Builds and pushes the ancilla-server image (including the embedded web UI),
updates `container_image_tag`, applies OpenTofu, waits for ECS to stabilize,
and checks `app_url/healthz` unless skipped.
EOF
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

init_deploy_context
init_aws_env

tag="${tag:-deploy-$(date +%Y%m%d-%H%M)}"
log "using image tag: $tag"

bootstrap_ecr_repositories app

ECR_URL="$(tofu_output_raw ecr_repository_url)"
CLUSTER="$(tofu_output_raw ecs_cluster_name)"
SERVICE="$(tofu_output_raw ecs_service_name)"

docker_login_ecr "$ECR_URL"
build_and_push_app_image "$ECR_URL" "$tag"

log "updating terraform.tfvars app image tag"
update_tfvar_string "container_image_tag" "$tag"

apply_infra
wait_for_ecs_service "$CLUSTER" "$SERVICE"

APP_URL="$(tofu_output_raw app_url)"
TASK_ARN="$(current_task_arn "$CLUSTER" "$SERVICE")"

if [[ "$skip_healthcheck" -eq 0 ]]; then
  healthcheck_app_url "$APP_URL"
fi

cat <<EOF

App deploy complete.
App image: $ECR_URL:$tag
App URL:   $APP_URL
Task:      $TASK_ARN

Try:
  curl "${APP_URL%/}/healthz"
EOF
