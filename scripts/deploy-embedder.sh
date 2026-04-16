#!/usr/bin/env bash
set -euo pipefail

SCRIPT_NAME="$(basename "$0")"
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/deploy-common.sh"

usage() {
  cat <<'EOF'
Usage: scripts/deploy-embedder.sh [--tag TAG]

Builds and pushes the ancilla-embedder image, updates `embedder_image_tag`,
and applies OpenTofu. This is only valid when `embedder_enabled = true`.
EOF
}

tag=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag)
      [[ $# -ge 2 ]] || fail "--tag requires a value"
      tag="$2"
      shift 2
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

init_deploy_context
init_aws_env

[[ "$EMBEDDER_ENABLED" == "true" ]] || fail "embedder_enabled=false in $TFVARS"

tag="${tag:-deploy-$(date +%Y%m%d-%H%M)}"
log "using image tag: $tag"

bootstrap_ecr_repositories embedder

EMBEDDER_ECR_URL="$(tofu_output_raw embedder_ecr_repository_url)"
docker_login_ecr "$EMBEDDER_ECR_URL"
build_and_push_embedder_image "$EMBEDDER_ECR_URL" "$tag" "$EMBEDDER_ACCELERATOR"

log "updating terraform.tfvars embedder image tag"
update_tfvar_string "embedder_image_tag" "$tag"

apply_infra

EMBEDDER_URL="$(tofu_output_raw embedder_private_url)"
EMBEDDER_PUBLIC_IP="$(tofu_output_raw embedder_public_ip)"
EMBEDDER_INSTANCE_ID="$(tofu_output_raw embedder_instance_id)"

cat <<EOF

Embedder deploy complete.
Embedder image:      $EMBEDDER_ECR_URL:$tag
Embedder instance:   ${EMBEDDER_INSTANCE_ID:-unknown}
Embedder private URL:${EMBEDDER_URL:-disabled}
Embedder public IP:  ${EMBEDDER_PUBLIC_IP:-disabled}
EOF
