#!/usr/bin/env bash
set -euo pipefail

SCRIPT_NAME="${SCRIPT_NAME:-$(basename "$0")}"

log() {
  printf '[%s] %s\n' "$SCRIPT_NAME" "$*"
}

fail() {
  printf '[%s] error: %s\n' "$SCRIPT_NAME" "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

init_deploy_context() {
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
  INFRA_DIR="$REPO_ROOT/infra/tofu"
  TFVARS="$INFRA_DIR/terraform.tfvars"

  [[ -f "$TFVARS" ]] || fail "missing $TFVARS"
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

read_tfvar_bool() {
  local key="$1"
  local fallback="$2"
  local value
  value="$(sed -nE "s/^[[:space:]]*${key}[[:space:]]*=[[:space:]]*(true|false)[[:space:]]*$/\\1/ip" "$TFVARS" | head -n1 | tr '[:upper:]' '[:lower:]')"
  if [[ -n "$value" ]]; then
    printf '%s\n' "$value"
  else
    printf '%s\n' "$fallback"
  fi
}

update_tfvar_string() {
  local key="$1"
  local value="$2"
  if grep -qE "^[[:space:]]*${key}[[:space:]]*=" "$TFVARS"; then
    perl -0pi -e 's/^(\s*'"$key"'\s*=\s*)".*"$/\1"'"$value"'"/m' "$TFVARS"
  else
    printf '\n%s = "%s"\n' "$key" "$value" >>"$TFVARS"
  fi
}

init_aws_env() {
  AWS_CONFIG_PATH="${AWS_CONFIG_FILE:-$REPO_ROOT/.aws/config}"
  AWS_CREDENTIALS_PATH="${AWS_SHARED_CREDENTIALS_FILE:-$REPO_ROOT/.aws/credentials}"
  AWS_PROFILE_VALUE="${AWS_PROFILE:-$(read_tfvar_string aws_profile ancilla-dev)}"
  AWS_REGION_VALUE="${AWS_REGION:-${AWS_DEFAULT_REGION:-$(read_tfvar_string aws_region us-east-1)}}"
  EMBEDDER_ENABLED="$(read_tfvar_bool embedder_enabled true)"
  EMBEDDER_ACCELERATOR="$(read_tfvar_string embedder_accelerator gpu)"

  [[ -f "$AWS_CONFIG_PATH" ]] || fail "missing AWS config file: $AWS_CONFIG_PATH"
  [[ -f "$AWS_CREDENTIALS_PATH" ]] || fail "missing AWS shared credentials file: $AWS_CREDENTIALS_PATH"

  export AWS_CONFIG_FILE="$AWS_CONFIG_PATH"
  export AWS_SHARED_CREDENTIALS_FILE="$AWS_CREDENTIALS_PATH"
  export AWS_PROFILE="$AWS_PROFILE_VALUE"
  export AWS_REGION="$AWS_REGION_VALUE"
  export AWS_DEFAULT_REGION="$AWS_REGION_VALUE"

  log "using repo-local AWS config: $AWS_CONFIG_FILE"
  log "using repo-local AWS credentials: $AWS_SHARED_CREDENTIALS_FILE"
  log "using AWS profile: $AWS_PROFILE"
  log "using AWS region: $AWS_REGION"
  log "embedder enabled: $EMBEDDER_ENABLED"
  if [[ "$EMBEDDER_ENABLED" == "true" ]]; then
    log "embedder accelerator: $EMBEDDER_ACCELERATOR"
  fi

  aws sts get-caller-identity >/dev/null
}

bootstrap_ecr_repositories() {
  local mode="${1:-app}"
  pushd "$INFRA_DIR" >/dev/null
  case "$mode" in
    app)
      log "bootstrapping app ECR repository"
      tofu apply -auto-approve -target=aws_ecr_repository.app >/dev/null
      ;;
    embedder)
      log "bootstrapping embedder ECR repository"
      tofu apply -auto-approve -target=aws_ecr_repository.embedder >/dev/null
      ;;
    all)
      log "bootstrapping app and embedder ECR repositories"
      tofu apply -auto-approve \
        -target=aws_ecr_repository.app \
        -target=aws_ecr_repository.embedder >/dev/null
      ;;
    *)
      popd >/dev/null
      fail "unknown bootstrap mode: $mode"
      ;;
  esac
  popd >/dev/null
}

tofu_output_raw() {
  local key="$1"
  pushd "$INFRA_DIR" >/dev/null
  local value
  value="$(tofu output -raw "$key" 2>/dev/null || true)"
  popd >/dev/null
  printf '%s\n' "$value"
}

docker_login_ecr() {
  local image_url="$1"
  local registry_host="${image_url%/*}"
  log "logging into ECR: $registry_host"
  aws ecr get-login-password | docker login --username AWS --password-stdin "$registry_host" >/dev/null
}

build_and_push_app_image() {
  local image_url="$1"
  local tag="$2"
  log "building app image: $image_url:$tag"
  pushd "$REPO_ROOT" >/dev/null
  docker buildx build --platform linux/arm64 --load -t "$image_url:$tag" .
  docker push "$image_url:$tag"
  popd >/dev/null
}

build_and_push_embedder_image() {
  local image_url="$1"
  local tag="$2"
  local accelerator="$3"
  local torch_variant="cpu"
  if [[ "$accelerator" == "gpu" ]]; then
    torch_variant="cu124"
  fi

  log "building embedder image: $image_url:$tag ($accelerator)"
  pushd "$REPO_ROOT" >/dev/null
  docker buildx build \
    --platform linux/amd64 \
    --load \
    --build-arg TORCH_VARIANT="$torch_variant" \
    -f Dockerfile.embedder \
    -t "$image_url:$tag" .
  docker push "$image_url:$tag"
  popd >/dev/null
}

apply_infra() {
  log "applying infrastructure"
  pushd "$INFRA_DIR" >/dev/null
  tofu apply -auto-approve
  popd >/dev/null
}

wait_for_ecs_service() {
  local cluster="$1"
  local service="$2"
  [[ -n "$cluster" ]] || fail "missing ECS cluster name"
  [[ -n "$service" ]] || fail "missing ECS service name"
  log "waiting for ECS service to stabilize"
  aws ecs wait services-stable --cluster "$cluster" --services "$service"
}

current_task_arn() {
  local cluster="$1"
  local service="$2"
  aws ecs list-tasks \
    --cluster "$cluster" \
    --service-name "$service" \
    --query 'taskArns[0]' \
    --output text
}

healthcheck_app_url() {
  local app_url="$1"
  [[ -n "$app_url" ]] || fail "missing app URL"
  log "running healthcheck against ${app_url%/}/healthz"
  curl -sS -o /tmp/ancilla-healthcheck.out -w "[$SCRIPT_NAME] healthz status: %{http_code}\n" "${app_url%/}/healthz"
}
