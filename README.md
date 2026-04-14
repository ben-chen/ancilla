# ancilla

Personal LLM memory system.

Current spec artifacts:

- [`docs/sql_schema_and_retrieval.md`](/Users/benchen/workspace/ancilla/docs/sql_schema_and_retrieval.md)
- [`sql/v1_schema.sql`](/Users/benchen/workspace/ancilla/sql/v1_schema.sql)
- [`sql/hybrid_memory_candidates.sql`](/Users/benchen/workspace/ancilla/sql/hybrid_memory_candidates.sql)

Current embedding choice:

- `perplexity-ai/pplx-embed-v1-0.6b` for query and memory embeddings
- `perplexity-ai/pplx-embed-context-v1-0.6b` for artifact/chunk embeddings
- clients/frontends may precompute embeddings and send them to the backend
- local embedding helper defaults to `cuda -> mps -> cpu`

## Local AWS Setup

Project-local AWS config files are expected at:

- `.aws/config`
- `.aws/credentials`

They are gitignored by default. Placeholder values are already scaffolded in the repo-local files, and `.env.example` shows the environment variables to point AWS SDKs, the AWS CLI, and future Terraform/OpenTofu runs at this project-local profile.

Recommended shell setup:

```bash
export AWS_CONFIG_FILE="$PWD/.aws/config"
export AWS_SHARED_CREDENTIALS_FILE="$PWD/.aws/credentials"
export AWS_PROFILE=ancilla-dev
export AWS_REGION=us-west-2
export AWS_DEFAULT_REGION=us-west-2
```

Do not commit real AWS credentials into this repository.

## App Config

The binary now reads runtime config from env:

- `ANCILLA_DATA_FILE`
- `DATABASE_URL`
- `AWS_REGION`
- `AWS_PROFILE`
- `BEDROCK_CHAT_MODEL_ID`
- `BEDROCK_CHAT_MAX_TOKENS`
- `BEDROCK_CHAT_TEMPERATURE`
- `ANCILLA_ACCEPT_CLIENT_EMBEDDINGS`
- `ANCILLA_ACCEPT_CLIENT_TRANSCRIPTS`
- `ANCILLA_LOCAL_EMBED_MODEL`
- `ANCILLA_LOCAL_CONTEXT_EMBED_MODEL`
- `ANCILLA_LOCAL_EMBED_DEVICE`

See [`.env.example`](/Users/benchen/workspace/ancilla/.env.example).

If `DATABASE_URL` is set, the app uses Postgres for persistence.
If it is unset, the app falls back to the local JSON state file at `ANCILLA_DATA_FILE`.
If `BEDROCK_CHAT_MODEL_ID` is set, chat responses use Bedrock `Converse`.
If it is unset, the app uses the deterministic synthetic backend.

## Ingest Contract

The backend no longer assumes it owns transcription or embedding generation.
Clients can:

- send text entries directly
- send audio-origin entries with `transcript_text`
- send `prepared_artifacts`
- send `prepared_memories`
- send `query_embedding` on search / context assembly / chat requests

If a query embedding and stored memory embeddings are present, the semantic leg uses those vectors.
If not, the app falls back to the local placeholder semantic scorer used for current tests and offline development.

## Local Embed Helper

Use [scripts/pplx_embed.py](/Users/benchen/workspace/ancilla/scripts/pplx_embed.py) from a frontend or local workflow to generate embeddings with PyTorch:

```bash
python3 scripts/pplx_embed.py \
  --model-id perplexity-ai/pplx-embed-v1-0.6b \
  --device auto \
  --text "I prefer Rust for backend services."
```

`--device auto` resolves in this order:

1. `cuda`
2. `mps`
3. `cpu`

## Migrations

Postgres-ready schema scaffolding is in:

- [migrations/0001_init.sql](/Users/benchen/workspace/ancilla/migrations/0001_init.sql)

When `DATABASE_URL` is set, the app runs these migrations automatically at startup through `sqlx`.

Current runtime note:

- the Postgres runtime path now reads and writes the normalized tables directly
- the Postgres runtime path uses `sql/hybrid_memory_candidates.sql` for candidate retrieval, then runs gating and context assembly in Rust
- when `BEDROCK_CHAT_MODEL_ID` is configured, `POST /v1/chat/respond` calls Bedrock `Converse`
- the JSON file path remains available as a fallback when `DATABASE_URL` is unset
- the Postgres path expects `pgvector` to be available in the target database

## Infra

Terraform/OpenTofu scaffolding for the MVP is in [infra/tofu/README.md](/Users/benchen/workspace/ancilla/infra/tofu/README.md).
It now supports both modes:

- create a dedicated MVP VPC, subnets, and route tables
- or attach the app stack to an existing VPC/subnet layout

The default side-project shape uses a small public ECS Fargate service, single-instance RDS PostgreSQL, S3, ECR, IAM, and Secrets Manager.
Nothing in this repo applies infrastructure automatically.
