# ancilla

Personal LLM memory system.

## Runtime Split

Ancilla now ships as two separate programs:

- `ancilla-server`: the HTTP API plus the local admin CLI for inspecting and mutating memory state
- `ancilla-client`: a ratatui terminal UI that talks to a running server over HTTP

They use separate config files:

- server config: `~/.config/ancilla-server/config.toml`
- client config: `~/.config/ancilla-client/config.toml`

The old unified `~/.config/ancilla/ancilla.toml` is now legacy and is not used by the new binaries.

Reference docs:

- [`docs/runtime_programs_and_configs.md`](docs/runtime_programs_and_configs.md)
- [`docs/sql_schema_and_retrieval.md`](docs/sql_schema_and_retrieval.md)
- [`sql/v1_schema.sql`](sql/v1_schema.sql)
- [`sql/hybrid_memory_candidates.sql`](sql/hybrid_memory_candidates.sql)

Current embedding choice:

- `perplexity-ai/pplx-embed-v1-0.6b` for query and memory embeddings
- `perplexity-ai/pplx-embed-context-v1-0.6b` for artifact and chunk embeddings
- clients may precompute embeddings and send them to the server
- the local embedding helper defaults to `cuda -> mps -> cpu`

## Local AWS Setup

Project-local AWS config files are expected at:

- `.aws/config`
- `.aws/credentials`

They are gitignored by default. `.env.example` shows the recommended shell variables for deploy tooling and for the server process when it needs Bedrock.

Recommended shell setup:

```bash
export AWS_CONFIG_FILE="$PWD/.aws/config"
export AWS_SHARED_CREDENTIALS_FILE="$PWD/.aws/credentials"
export AWS_PROFILE=ancilla-dev
export AWS_REGION=us-west-2
export AWS_DEFAULT_REGION=us-west-2
```

Do not commit real AWS credentials into this repository.

## Server Config

`ancilla-server` loads runtime config in this order:

1. built-in defaults
2. `~/.config/ancilla-server/config.toml`
3. environment variables

Create the starter file:

```bash
cargo run --bin ancilla-server -- init-config
```

Overwrite the scaffold:

```bash
cargo run --bin ancilla-server -- init-config --force
```

Relevant server settings:

- `app_env`
- `data_file`
- `database_url`
- `aws_region`
- `aws_profile`
- `aws_config_file`
- `aws_shared_credentials_file`
- `bedrock_chat_model_id` as the default selected model
- `chat_models` as the curated server-side picker menu
- `bedrock_chat_max_tokens`
- `bedrock_chat_temperature`
- `accept_client_embeddings`
- `accept_client_transcripts`
- `local_embed_model`
- `local_context_embed_model`
- `local_embed_device`

Server-specific env vars use the `ANCILLA_SERVER_...` prefix. The server also accepts the standard deploy/runtime env vars already used elsewhere, including:

- `DATABASE_URL`
- `AWS_REGION`
- `AWS_PROFILE`
- `AWS_CONFIG_FILE`
- `AWS_SHARED_CREDENTIALS_FILE`
- `BEDROCK_CHAT_MODEL_ID`
- `BEDROCK_CHAT_MODELS_JSON`

Example `~/.config/ancilla-server/config.toml`:

```toml
app_env = "development"
data_file = ".ancilla/state.json"
# database_url = "postgres://user:password@host:5432/ancilla?sslmode=require"
aws_region = "us-west-2"
aws_profile = "ancilla-dev"
aws_config_file = "~/workspace/ancilla/.aws/config"
aws_shared_credentials_file = "~/workspace/ancilla/.aws/credentials"
bedrock_chat_model_id = "us.anthropic.claude-opus-4-6-v1"

[[chat_models]]
label = "Claude Opus 4.6"
model_id = "us.anthropic.claude-opus-4-6-v1"
description = "Deepest reasoning"
thinking_mode = "adaptive"

[[chat_models]]
label = "Claude Sonnet 4.6"
model_id = "us.anthropic.claude-sonnet-4-6"
description = "Balanced reasoning and speed"
thinking_mode = "adaptive"

[[chat_models]]
label = "Claude Haiku 4.5"
model_id = "us.anthropic.claude-haiku-4-5-20251001-v1:0"
description = "Fastest responses"

bedrock_chat_max_tokens = 800
bedrock_chat_temperature = 0.2
accept_client_embeddings = true
accept_client_transcripts = true
local_embed_model = "perplexity-ai/pplx-embed-v1-0.6b"
local_context_embed_model = "perplexity-ai/pplx-embed-context-v1-0.6b"
local_embed_device = "auto"
```

Inspect the effective server config:

```bash
cargo run --bin ancilla-server -- show-config
cargo run --bin ancilla-server -- show-config --show-secrets
```

If `database_url` is set, the server uses Postgres. If it is unset, the server falls back to the local JSON state file.

If `bedrock_chat_model_id` is set, `POST /v1/chat/respond` uses Bedrock `Converse`. If it is unset, the server uses the deterministic synthetic backend.

`bedrock_chat_model_id` is the default selection. `chat_models` is the short curated catalog returned by `GET /v1/chat/models` for the TUI model picker. Keep this list small. The intended deploy shape is only the latest model in each line:

- Claude Opus 4.6
- Claude Sonnet 4.6
- Claude Haiku 4.5

For the deployed AWS service, the live `DATABASE_URL` is not stored in this repo. OpenTofu builds it in [`infra/tofu/main.tf`](infra/tofu/main.tf), stores it in AWS Secrets Manager, and ECS injects it into the container as `DATABASE_URL`.

Inspect the deployed secret value:

```bash
cd infra/tofu
DB_SECRET_ARN=$(tofu output -raw database_url_secret_arn)
aws secretsmanager get-secret-value \
  --secret-id "$DB_SECRET_ARN" \
  --query SecretString \
  --output text
```

## Client Config

`ancilla-client` loads runtime config in this order:

1. built-in defaults
2. `~/.config/ancilla-client/config.toml`
3. environment variables

Create the starter file:

```bash
cargo run --bin ancilla-client -- init-config
```

Inspect the effective client config:

```bash
cargo run --bin ancilla-client -- show-config
```

The client config is intentionally small. Its job is to know which server to call.

Example `~/.config/ancilla-client/config.toml`:

```toml
base_url = "http://127.0.0.1:3000"
```

Client env override:

- `ANCILLA_CLIENT_BASE_URL`

## Using `ancilla-server`

`ancilla-server` has two roles:

- `serve`: run the HTTP API
- admin CLI commands like `capture`, `ask`, `search`, `timeline`, and `review`

Start the local API:

```bash
cargo run --bin ancilla-server -- serve --bind 127.0.0.1:3000
```

Common admin commands:

- `capture`: ingest a new text or audio-origin entry and materialize its artifacts and memories

  ```bash
  cargo run --bin ancilla-server -- capture --text "I prefer Rust for backend services." --timezone UTC
  ```

- `ask`: run context assembly plus the configured chat backend for one question

  ```bash
  cargo run --bin ancilla-server -- ask "What do I prefer for backend services?"
  ```

- `search`: retrieve candidate memories without generating an answer

  ```bash
  cargo run --bin ancilla-server -- search "Rust"
  ```

- `timeline`: list entries in reverse chronological order

  ```bash
  cargo run --bin ancilla-server -- timeline
  ```

- `review`: list all memory records in reverse `updated_at` order

  ```bash
  cargo run --bin ancilla-server -- review
  ```

- `patch-memory`: update a memory record by ID

  ```bash
  cargo run --bin ancilla-server -- patch-memory 00000000-0000-0000-0000-000000000000 \
    --display-text "You prefer Rust."
  ```

- `forget`: delete a memory record by ID

  ```bash
  cargo run --bin ancilla-server -- forget 00000000-0000-0000-0000-000000000000
  ```

Use a specific JSON snapshot file:

```bash
cargo run --bin ancilla-server -- --data-file .ancilla/dev-state.json timeline
```

Build the binaries directly:

```bash
cargo build --release --bin ancilla-server --bin ancilla-client
./target/release/ancilla-server timeline
./target/release/ancilla-client
```

## Using `ancilla-client`

`ancilla-client` is a remote TUI. It does not read the database directly and it does not need AWS or Postgres settings.

Run it against the configured base URL:

```bash
cargo run --bin ancilla-client --
```

Override the target ad hoc:

```bash
cargo run --bin ancilla-client -- --base-url http://16.146.111.110:3000
```

The client UI supports:

- timeline browsing
- entry inspection
- model selection from the server-advertised catalog with `m`
- sending chat questions to the live server
- capturing new text entries on the live server

The client does not define its own model list. It fetches the catalog from `GET /v1/chat/models` and sends the selected `model_id` with each ask request.

## Local vs Deployed Runtime

The server and client interact like this:

- `cargo run --bin ancilla-server -- ask ...` runs directly against the configured store
- `cargo run --bin ancilla-server -- serve ...` exposes the HTTP API locally
- `cargo run --bin ancilla-client --` talks to whatever `base_url` points at
- `curl http://127.0.0.1:3000/...` talks to your local server process
- `curl http://$APP_IP:3000/...` talks to the deployed AWS service

Local development example:

```bash
cargo run --bin ancilla-server -- serve --bind 127.0.0.1:3000
cargo run --bin ancilla-client --

curl http://127.0.0.1:3000/healthz
curl http://127.0.0.1:3000/v1/timeline
curl -X POST http://127.0.0.1:3000/v1/entries/text \
  -H 'content-type: application/json' \
  --data '{"raw_text":"I prefer Rust.","timezone":"UTC"}'
```

Deployed service example:

```bash
APP_IP=16.146.111.110

cargo run --bin ancilla-client -- --base-url "http://$APP_IP:3000"
curl "http://$APP_IP:3000/healthz"
curl "http://$APP_IP:3000/v1/timeline"
curl -X POST "http://$APP_IP:3000/v1/context/assemble" \
  -H 'content-type: application/json' \
  --data '{"query":"What am I building?"}'
```

The default deploy places Postgres in private DB subnets, so your laptop cannot reach the database directly unless you add your own network path into the VPC.

If you need server admin commands against deployed data, run `ancilla-server` from a VPC-connected host with `DATABASE_URL` set, or exec into the ECS task and run the bundled binary there:

```bash
cd infra/tofu
CLUSTER=$(tofu output -raw ecs_cluster_name)
SERVICE=$(tofu output -raw ecs_service_name)
TASK_ARN=$(aws ecs list-tasks \
  --cluster "$CLUSTER" \
  --service-name "$SERVICE" \
  --query 'taskArns[0]' \
  --output text)

aws ecs execute-command \
  --cluster "$CLUSTER" \
  --task "$TASK_ARN" \
  --container ancilla \
  --interactive \
  --command "/bin/sh -lc '/usr/local/bin/ancilla-server timeline'"
```

## Ingest Contract

The backend does not assume it owns transcription or embedding generation.

`POST /v1/entries/text`, `POST /v1/entries/audio`, `POST /v1/context/assemble`, and `POST /v1/chat/respond` all accept client-prepared fields like transcripts, artifacts, memories, and query embeddings.

If the client supplies embeddings:

- the server stores them
- retrieval uses them directly
- the local placeholder scorer is bypassed for semantic ranking where possible

If the client does not supply embeddings:

- the server falls back to the local placeholder semantic scorer used for current tests

## Local Embed Helper

The helper scripts in [`scripts/`](scripts/) are managed with `uv`.

```bash
cd scripts
uv sync
uv run python pplx_embed.py \
  --model-id perplexity-ai/pplx-embed-v1-0.6b \
  --device auto \
  --text "I prefer Rust for backend services."
```

See [`scripts/README.md`](scripts/README.md) for the helper workflow and tests.

The first real model run may download model code and weights from Hugging Face, so expect the initial invocation to take longer.

## Runtime Notes

Postgres-ready schema scaffolding is in:

- [`migrations/0001_init.sql`](migrations/0001_init.sql)

When `DATABASE_URL` is set, the app runs these migrations automatically at startup through `sqlx`.

Current runtime notes:

- the Postgres path reads and writes the normalized tables directly
- candidate retrieval in Postgres uses [`sql/hybrid_memory_candidates.sql`](sql/hybrid_memory_candidates.sql)
- when `BEDROCK_CHAT_MODEL_ID` is configured, `POST /v1/chat/respond` calls Bedrock `Converse`
- the JSON file path remains available as a fallback when `DATABASE_URL` is unset
- the Postgres path expects `pgvector` to be available in the target database

## Infra

Terraform/OpenTofu scaffolding for the deployed server is in [`infra/tofu/README.md`](infra/tofu/README.md).

The default side-project shape uses:

- a small public ECS Fargate service for `ancilla-server`
- a single-instance RDS PostgreSQL database
- S3 for assets
- ECR for images
- Secrets Manager for runtime secrets

## Deploying `ancilla-server`

Fast path:

```bash
scripts/redeploy.sh
```

That script builds a new immutable ARM64 image, pushes it to ECR, updates `infra/tofu/terraform.tfvars`, runs `tofu apply`, waits for ECS to stabilize, prints the current task IP, and runs `/healthz`.

The deploy script stays separate from app runtime config:

- `AWS_CONFIG_FILE` and `AWS_SHARED_CREDENTIALS_FILE` come from env if set, otherwise the repo-local `.aws/config` and `.aws/credentials`
- `AWS_PROFILE` and `AWS_REGION` come from env if set, otherwise `infra/tofu/terraform.tfvars`
- it does not read or modify either program’s config file

If you want the client to target the new deploy, copy the printed IP into `~/.config/ancilla-client/config.toml`:

```toml
base_url = "http://<app-ip>:3000"
```

Useful options:

- `scripts/redeploy.sh --tag deploy-20260414-1530`
- `scripts/redeploy.sh --skip-healthcheck`

Manual flow:

1. Set AWS environment variables to the repo-local profile.

```bash
export AWS_CONFIG_FILE="$PWD/.aws/config"
export AWS_SHARED_CREDENTIALS_FILE="$PWD/.aws/credentials"
export AWS_PROFILE=ancilla-dev
export AWS_REGION=us-west-2
export AWS_DEFAULT_REGION=us-west-2
```

2. Install or verify required tools.

- `aws`
- `docker`
- `tofu`
- `psql`
- `session-manager-plugin` if you want `aws ecs execute-command`

3. Create deploy inputs.

```bash
cd infra/tofu
cp terraform.tfvars.example terraform.tfvars
```

Edit `infra/tofu/terraform.tfvars` and set at least:

- `aws_profile`
- `bedrock_chat_model_id`
- `db_instance_class`
- `container_image_tag`

Use a new immutable `container_image_tag` on every deploy. Do not reuse `latest`.

4. Initialize OpenTofu and create ECR if needed.

```bash
cd infra/tofu
tofu init
tofu apply -target=aws_ecr_repository.app -auto-approve
```

5. Build and push the server image.

```bash
cd infra/tofu
ECR_URL=$(tofu output -raw ecr_repository_url)
TAG=deploy-$(date +%Y%m%d-%H%M)
cd ../..

docker buildx build --platform linux/arm64 --load -t "$ECR_URL:$TAG" .
docker push "$ECR_URL:$TAG"
```

Then set:

```toml
container_image_tag = "<the same TAG you just pushed>"
```

6. Apply the full stack.

```bash
cd infra/tofu
tofu apply -auto-approve
```

7. Enable `pgvector` once from a VPC-connected shell.

```bash
cd infra/tofu
DB_SECRET_ARN=$(tofu output -raw database_url_secret_arn)
DATABASE_URL=$(aws secretsmanager get-secret-value \
  --secret-id "$DB_SECRET_ARN" \
  --query SecretString \
  --output text)

psql "$DATABASE_URL" -c 'CREATE EXTENSION IF NOT EXISTS vector;'
```

8. Find the live task public IP.

```bash
cd infra/tofu
CLUSTER=$(tofu output -raw ecs_cluster_name)
SERVICE=$(tofu output -raw ecs_service_name)
TASK_ARN=$(aws ecs list-tasks \
  --cluster "$CLUSTER" \
  --service-name "$SERVICE" \
  --query 'taskArns[0]' \
  --output text)
ENI=$(aws ecs describe-tasks \
  --cluster "$CLUSTER" \
  --tasks "$TASK_ARN" \
  --query "tasks[0].attachments[0].details[?name==\`networkInterfaceId\`].value | [0]" \
  --output text)
APP_IP=$(aws ec2 describe-network-interfaces \
  --network-interface-ids "$ENI" \
  --query 'NetworkInterfaces[0].Association.PublicIp' \
  --output text)
echo "$APP_IP"
```

9. Smoke-test the deployed API.

```bash
curl "http://$APP_IP:3000/healthz"

curl -X POST "http://$APP_IP:3000/v1/entries/text" \
  -H 'content-type: application/json' \
  --data '{"raw_text":"I prefer Rust for backend services.","timezone":"UTC"}'

curl "http://$APP_IP:3000/v1/timeline"
```
