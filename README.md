# ancilla

Personal LLM memory system.

## Runtime Split

Ancilla now ships as two separate programs:

- `ancilla-server`: the HTTP API plus the local admin CLI for inspecting and mutating memory state
- `ancilla-client`: a ratatui terminal UI that talks to a running server over HTTP

They use separate config files:

- server config: `~/.config/ancilla/server.toml`
- client config: `~/.config/ancilla/client.toml`

The old unified `~/.config/ancilla/ancilla.toml` is now legacy and is not used by the new binaries. The loaders still fall back to `~/.config/ancilla-server/config.toml` and `~/.config/ancilla-client/config.toml` if the new shared-dir files do not exist yet.

Reference docs:

- [`docs/runtime_programs_and_configs.md`](docs/runtime_programs_and_configs.md)
- [`docs/sql_schema_and_retrieval.md`](docs/sql_schema_and_retrieval.md)
- [`migrations/0001_init.sql`](migrations/0001_init.sql)
- [`migrations/0003_markdown_memories.sql`](migrations/0003_markdown_memories.sql)
- [`prompts/memory_creation.md`](prompts/memory_creation.md)

Current embedding choice:

- `perplexity-ai/pplx-embed-v1-0.6b` for query and memory embeddings
- clients may precompute embeddings and send them to the server
- the server can also call a separate `ancilla-embedder` service synchronously for both stored memories and live query embeddings
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
2. `~/.config/ancilla/server.toml`
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
- `embedder_base_url`
- `embedder_timeout_seconds`
- `aws_region`
- `aws_profile`
- `aws_config_file`
- `aws_shared_credentials_file`
- `aws_bearer_token_bedrock`
- `bedrock_chat_model_id` as the default selected model
- `bedrock_gate_model_id` as the default context-gate model
- `chat_models` as the curated server-side picker menu
- `bedrock_chat_max_tokens`
- `bedrock_chat_temperature`
- `accept_client_embeddings`
- `accept_client_transcripts`
- `local_embed_model`

Server-specific env vars use the `ANCILLA_SERVER_...` prefix. The server also accepts the standard deploy/runtime env vars already used elsewhere, including:

- `DATABASE_URL`
- `AWS_REGION`
- `AWS_PROFILE`
- `AWS_CONFIG_FILE`
- `AWS_SHARED_CREDENTIALS_FILE`
- `AWS_BEARER_TOKEN_BEDROCK`
- `BEDROCK_CHAT_MODEL_ID`
- `BEDROCK_GATE_MODEL_ID`
- `BEDROCK_CHAT_MODELS_JSON`

Example `~/.config/ancilla/server.toml`:

```toml
app_env = "development"
data_file = ".ancilla/state.json"
# database_url = "postgres://user:password@host:5432/ancilla?sslmode=require"
# embedder_base_url = "http://10.42.0.50:4000"
# embedder_timeout_seconds = 120
#
aws_region = "us-east-1"
aws_profile = "ancilla-dev"
aws_config_file = "~/workspace/ancilla/.aws/config"
aws_shared_credentials_file = "~/workspace/ancilla/.aws/credentials"
# aws_bearer_token_bedrock = "bedrock-api-key-..."
bedrock_chat_model_id = "moonshotai.kimi-k2.5"
bedrock_gate_model_id = "moonshotai.kimi-k2.5"

[[chat_models]]
label = "Kimi K2.5"
model_id = "moonshotai.kimi-k2.5"
description = "Moonshot general-purpose model"

bedrock_chat_max_tokens = 800
bedrock_chat_temperature = 0.2
accept_client_embeddings = true
accept_client_transcripts = true
local_embed_model = "perplexity-ai/pplx-embed-v1-0.6b"
```

Inspect the effective server config:

```bash
cargo run --bin ancilla-server -- show-config
cargo run --bin ancilla-server -- show-config --show-secrets
```

If `database_url` is set, the server uses Postgres. If it is unset, the server falls back to the local JSON state file.

If `bedrock_chat_model_id` is set, `POST /v1/chat/respond` uses Bedrock `Converse` and `POST /v1/chat/respond/stream` uses Bedrock `ConverseStream`. If it is unset, the server uses the deterministic synthetic backend for both routes.

If a gate model is available, `POST /v1/context/assemble` also uses Bedrock to decide which candidate memories to inject. The server prefers `bedrock_gate_model_id` when set, otherwise it falls back to the latest configured Haiku model, then finally to `bedrock_chat_model_id`. If no Bedrock gate model is available or the Bedrock call fails, the server falls back to the deterministic gate.

If `embedder_base_url` is set, the server asks the embedder service for live query embeddings and for memory embeddings during explicit capture. If it is unset, retrieval falls back to the built-in lexical path and the placeholder semantic scorer used by local tests.

`bedrock_chat_model_id` is the default selection. `chat_models` is the short curated catalog returned by `GET /v1/chat/models` for the TUI model picker. Keep this list small.

While Anthropic approval is pending, the recommended temporary config is:

- Kimi K2.5

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
2. `~/.config/ancilla/client.toml`
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

Example `~/.config/ancilla/client.toml`:

```toml
base_url = "http://127.0.0.1:3000"
# basic_auth_username = "ancilla"
# basic_auth_password = "replace-me"
```

When the deployed server has HTTP Basic auth enabled, put the same username/password in `client.toml`. `ancilla-client` will send the `Authorization` header automatically.

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

- `capture`: store an explicit memory from text or audio, recording the original source modality in entry metadata
  - text capture now sends freeform context through the model-backed memory generator and may create zero memories if nothing is important enough to keep

  ```bash
  cargo run --bin ancilla-server -- capture --text "I prefer Rust for backend services." --timezone UTC
  ```

- `remember`: convenience command for storing one explicit memory directly
  - this wraps the given text into Ancilla's canonical markdown memory format
  - for full control over title, tags, and markdown body, use `POST /v1/memories`

  ```bash
  cargo run --bin ancilla-server -- remember "You prefer Rust for backend services." --kind semantic
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
  - this now replaces the stored markdown body directly

  ```bash
  cargo run --bin ancilla-server -- patch-memory 00000000-0000-0000-0000-000000000000 \
    --markdown "# Rust Preference\n\nTags: preference\n\nYou prefer Rust."
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

- memory browsing by default, with `Tab` to switch to the raw timeline
- memory inspection and entry inspection
- model selection from the server-advertised catalog with `m`
- retrieval/context preview with `s`, calling `POST /v1/context/assemble`; when the server has a gate model configured this still uses the Bedrock gate, but it does not generate a final chat answer
- sending chat questions to the live server with streamed answer rendering from `POST /v1/chat/respond/stream`
- capturing new memories on the live server

The client does not define its own model list. It fetches the catalog from `GET /v1/chat/models` and sends the selected `model_id` with each ask request.

## Local vs Deployed Runtime

The server and client interact like this:

- `cargo run --bin ancilla-server -- ask ...` runs directly against the configured store
- `cargo run --bin ancilla-server -- serve ...` exposes the HTTP API locally
- `cargo run --bin ancilla-client --` talks to whatever `base_url` points at
- `curl http://127.0.0.1:3000/...` talks to your local server process
- `curl http://ancillabot.com/...` talks to the deployed AWS service once the Route 53 domain and ACM validation have propagated
- `curl http://$ALB_DNS/...` talks to the deployed AWS service immediately after ALB creation, before the custom domain resolves everywhere

Local development example:

```bash
cargo run --bin ancilla-server -- serve --bind 127.0.0.1:3000
cargo run --bin ancilla-client --

curl http://127.0.0.1:3000/healthz
curl http://127.0.0.1:3000/v1/timeline
curl -X POST http://127.0.0.1:3000/v1/memories \
  -H 'content-type: application/json' \
  --data '{"content_markdown":"# Rust Preference\n\nTags: preference\n\nYou prefer Rust for backend services.","kind":"semantic","timezone":"UTC"}'
```

Deployed service example:

```bash
APP_URL=https://ancillabot.com

cargo run --bin ancilla-client -- --base-url "$APP_URL"
curl "$APP_URL/healthz"
curl -u ancilla:REPLACE_ME "$APP_URL/v1/timeline"
curl -u ancilla:REPLACE_ME -X POST "$APP_URL/v1/context/assemble" \
  -H 'content-type: application/json' \
  --data '{"query":"What am I building?"}'
```

If the custom domain is still propagating, use the ALB DNS name from OpenTofu instead:

```bash
ALB_DNS=$(cd infra/tofu && tofu output -raw alb_dns_name)

cargo run --bin ancilla-client -- --base-url "http://$ALB_DNS"
curl "http://$ALB_DNS/healthz"
```

When Basic auth is enabled, `/healthz` stays public for the ALB health check, but API routes require credentials.

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

The backend does not assume it owns transcription.

`POST /v1/memories` is the explicit markdown memory store path. It accepts one canonical markdown memory document and stores it directly as a durable memory record.

`POST /v1/memories/generate` is the model-backed memory extraction path. It accepts freeform context text, runs the runtime prompt in [`prompts/memory_creation.md`](prompts/memory_creation.md), and may return zero or more memory documents if the model decides nothing is worth remembering.

Canonical stored memory format:

```md
# Building Ancilla

Tags: project, ancilla

I am building Ancilla, a personal memory system.
```

`POST /v1/entries/text` and `POST /v1/entries/audio` remain lower-level ingest endpoints for clients that want to manage artifacts or prepared memories explicitly.

`POST /v1/context/assemble` and `POST /v1/chat/respond` accept either a client-supplied `query_embedding` or recent conversation turns. When the client omits `query_embedding` and the server has `embedder_base_url` configured, the server asks the embedder for a query embedding directly.

If the client supplies embeddings:

- the server stores them
- retrieval uses them directly

If the client does not supply embeddings:

- the server can call the configured embedder service synchronously
- if no embedder is configured, retrieval falls back to the built-in lexical path and placeholder semantic scorer

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

## Embedder Service

The embedder path is now synchronous:

- `ancilla-server` stores explicit memories directly
- if `embedder_base_url` is configured, the server calls `ancilla-embedder` over HTTP to embed those memories immediately
- retrieval queries use the same embedder service for live query embeddings
- the embedder is intentionally separate from the API server so the heavy model runtime does not live inside the Fargate task
- the checked-in deploy uses a cheap CPU embedder host by default; switch to GPU only when you actually need it

## Runtime Notes

Postgres-ready schema scaffolding is in:

- [`migrations/0001_init.sql`](migrations/0001_init.sql)

When `DATABASE_URL` is set, the app runs these migrations automatically at startup through `sqlx`.

Current runtime notes:

- the Postgres path reads and writes the normalized tables directly
- `entries.kind` is normalized to `text`, `chat_turn`, or `import`; source modality like `text` vs `audio` lives in `entries.metadata.source_modality`
- memory content is stored as markdown, while `search_text` is derived plain text used for lexical search and embeddings
- candidate retrieval currently runs from the in-memory state view built from Postgres, using derived-text lexical search plus cosine similarity ranking
- when `BEDROCK_CHAT_MODEL_ID` is configured, `POST /v1/chat/respond` calls Bedrock `Converse`
- the JSON file path remains available as a fallback when `DATABASE_URL` is unset
- the Postgres path expects `pgvector` to be available in the target database

## Infra

Terraform/OpenTofu scaffolding for the deployed server is in [`infra/tofu/README.md`](infra/tofu/README.md).

The default side-project shape uses:

- a small public ECS Fargate service for `ancilla-server`
- an optional always-on `ancilla-embedder` EC2 host for query and memory embeddings
- a single-instance RDS PostgreSQL database
- S3 for assets
- ECR for images
- Secrets Manager for runtime secrets

## Deploying `ancilla-server`

Fast path:

```bash
scripts/redeploy.sh
```

That script builds a new immutable ARM64 server image and, when enabled, an AMD64 embedder image, pushes the needed images to ECR, updates `infra/tofu/terraform.tfvars`, runs `tofu apply`, waits for ECS to stabilize, prints the current task IP, and runs `/healthz`.
If `embedder_enabled = false`, the script skips the embedder image entirely. If the embedder is enabled, the script builds the embedder image for the selected accelerator mode.

The deploy script stays separate from app runtime config:

- `AWS_CONFIG_FILE` and `AWS_SHARED_CREDENTIALS_FILE` come from env if set, otherwise the repo-local `.aws/config` and `.aws/credentials`
- `AWS_PROFILE` and `AWS_REGION` come from env if set, otherwise `infra/tofu/terraform.tfvars`
- it does not read or modify either program’s config file

If you want the client to target the new deploy, copy the printed IP into `~/.config/ancilla/client.toml`:

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

Also verify the AWS account has non-zero EC2 GPU quota in `us-west-2` if you want to enable the dedicated embedder host:

- `Running On-Demand G and VT instances`
- `All G and VT Spot Instance Requests`

GPU quota is only required when `embedder_accelerator = "gpu"`. The checked-in `terraform.tfvars` now uses a CPU embedder host so online query embeddings work in this account without waiting for quota.

The checked-in low-cost default is:

- `embedder_enabled = true`
- `embedder_accelerator = "cpu"`
- `embedder_instance_type = "t3.large"`

When GPU quota is available, the intended low-cost GPU upgrade is `g6f.large` in `us-west-2`. That is currently the smallest Linux GPU instance in the region and is materially cheaper than `g4dn.xlarge`, so the embedder defaults also use a conservative `batch_size = 2` and `max_length = 8192`.

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
- `embedder_enabled`
- `embedder_accelerator`
- `embedder_image_tag` if the embedder is enabled
- `domain_name` if you want a stable Route 53 hostname
- `enable_https_listener = false` for the first custom-domain apply, then `true` after ACM shows `ISSUED`
- `basic_auth_enabled = true` if you want HTTP Basic auth on all API routes except `/healthz`

Use a new immutable `container_image_tag` on every deploy. Do not reuse `latest`.

4. Initialize OpenTofu and create ECR repositories if needed.

```bash
cd infra/tofu
tofu init
tofu apply \
  -target=aws_ecr_repository.app \
  -auto-approve
```

If the embedder is enabled, bootstrap its repository too:

```bash
cd infra/tofu
tofu apply \
  -target=aws_ecr_repository.embedder \
  -auto-approve
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

6. If the embedder is enabled, build and push the embedder image.

```bash
cd infra/tofu
EMBEDDER_ECR_URL=$(tofu output -raw embedder_ecr_repository_url)
TAG=deploy-$(date +%Y%m%d-%H%M)
cd ../..

docker buildx build \
  --platform linux/amd64 \
  --load \
  --build-arg TORCH_VARIANT=cpu \
  -f Dockerfile.embedder \
  -t "$EMBEDDER_ECR_URL:$TAG" .
docker push "$EMBEDDER_ECR_URL:$TAG"
```

Then set:

```toml
embedder_image_tag = "<the same TAG you just pushed>"
```

If you want a GPU embedder instead, change the build arg to `TORCH_VARIANT=cu124` and set `embedder_accelerator = "gpu"` plus a GPU-capable instance type.

7. Apply the full stack.

```bash
cd infra/tofu
tofu apply -auto-approve
```

8. Enable `pgvector` once from a VPC-connected shell.

```bash
cd infra/tofu
DB_SECRET_ARN=$(tofu output -raw database_url_secret_arn)
DATABASE_URL=$(aws secretsmanager get-secret-value \
  --secret-id "$DB_SECRET_ARN" \
  --query SecretString \
  --output text)

psql "$DATABASE_URL" -c 'CREATE EXTENSION IF NOT EXISTS vector;'
```

9. Find the live task public IP.

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

10. Smoke-test the deployed API.

```bash
curl "http://$APP_IP:3000/healthz"

curl -X POST "http://$APP_IP:3000/v1/memories" \
  -H 'content-type: application/json' \
  --data '{"content_markdown":"# Rust Preference\n\nTags: preference\n\nYou prefer Rust for backend services.","kind":"semantic","timezone":"UTC"}'

curl -X POST "http://$APP_IP:3000/v1/memories/generate" \
  -H 'content-type: application/json' \
  --data '{"context_text":"I am building Ancilla, a personal memory system.","kind":"semantic","timezone":"UTC"}'

curl "http://$APP_IP:3000/v1/timeline"
```
