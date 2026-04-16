#!/usr/bin/env bash
set -euo pipefail

SCRIPT_NAME="$(basename "$0")"
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/deploy-common.sh"

usage() {
  cat <<'EOF'
Usage: scripts/deploy-infra.sh [--skip-healthcheck]

Runs `tofu apply` for the current stack, waits for ECS to stabilize, and
checks `app_url/healthz` unless skipped.
EOF
}

skip_healthcheck=0

while [[ $# -gt 0 ]]; do
  case "$1" in
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
require_command tofu
if [[ "$skip_healthcheck" -eq 0 ]]; then
  require_command curl
fi

init_deploy_context
init_aws_env

CLUSTER="$(tofu_output_raw ecs_cluster_name)"
SERVICE="$(tofu_output_raw ecs_service_name)"

apply_infra
wait_for_ecs_service "$CLUSTER" "$SERVICE"

APP_URL="$(tofu_output_raw app_url)"

if [[ "$skip_healthcheck" -eq 0 ]]; then
  healthcheck_app_url "$APP_URL"
fi

cat <<EOF

Infra apply complete.
App URL: $APP_URL

Try:
  curl "${APP_URL%/}/healthz"
EOF
