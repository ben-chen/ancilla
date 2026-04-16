#!/usr/bin/env bash
set -euo pipefail

SCRIPT_NAME="$(basename "$0")"
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/deploy-common.sh"

usage() {
  cat <<'EOF'
Usage: scripts/redeploy.sh [--tag TAG] [--skip-healthcheck]

Backward-compatible wrapper that deploys the embedder first when enabled, then
deploys the main app image. Prefer the more targeted scripts directly:

  scripts/deploy-app.sh
  scripts/deploy-embedder.sh
  scripts/deploy-infra.sh
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

require_command bash
init_deploy_context
init_aws_env

tag="${tag:-deploy-$(date +%Y%m%d-%H%M)}"
log "using image tag: $tag"

common_args=(--tag "$tag")
if [[ "$skip_healthcheck" -eq 1 ]]; then
  common_args+=(--skip-healthcheck)
fi

if [[ "$EMBEDDER_ENABLED" == "true" ]]; then
  log "deploying embedder"
  "$SCRIPT_DIR/deploy-embedder.sh" --tag "$tag"
fi

log "deploying app"
"$SCRIPT_DIR/deploy-app.sh" "${common_args[@]}"
