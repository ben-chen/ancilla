# Ancilla MVP Infra

This directory contains Terraform/OpenTofu-compatible HCL for a low-cost deployable MVP shape.

It creates infrastructure for the `ancilla-server` API binary:

- a dedicated VPC when `create_network = true`
- 2 public subnets, 2 reserved private app subnets, and 2 private DB subnets
- an Internet Gateway and route tables, with no NAT Gateway by default
- a public ECS Fargate service for the Rust API server
- an optional always-on EC2 instance for the separate `ancilla-embedder` service
- an optional public ALB plus Route 53 alias records for a stable hostname
- a single-instance RDS PostgreSQL database for runtime state and retrieval data
- an S3 bucket for artifacts/imports
- ECR repositories for the server image and the embedder image
- IAM roles for ECS execution, Bedrock invocation, and S3 access
- a Secrets Manager secret for `DATABASE_URL`
- environment for a small curated Bedrock model catalog exposed to the TUI

If you already have a network, set `create_network = false` and provide:

- an existing `vpc_id`
- public subnets for ECS
- private DB subnets for PostgreSQL

## Preconditions

- Build and push an ARM64 or multi-arch `ancilla-server` image to the managed ECR repository, or set `container_image` directly.
- Build and push an AMD64 `ancilla-embedder` image to the managed embedder ECR repository, or set `embedder_image` directly.
- Use an RDS PostgreSQL engine version that supports `pgvector`.
- Make sure the AWS account has non-zero EC2 GPU quota in the target region if you want `embedder_accelerator = "gpu"`. CPU mode does not need GPU quota.
- If `create_network = true`, use at least two availability zones and matching subnet CIDR lists.
- After the database is live, connect once and run:

```sql
CREATE EXTENSION IF NOT EXISTS vector;
```

The app migrations will create the schema, but the extension must exist first.

## Cost Shape

The defaults are tuned for a personal side project:

- ECS runs directly in public subnets with a public IP
- no NAT Gateway
- ALB/domain support is optional and only turns on when `domain_name` is set
- PostgreSQL defaults to `db.t4g.micro`
- Fargate defaults to ARM `256 CPU / 1024 MiB`
- the optional embedder defaults to a single `t3.large` CPU EC2 host

If you later want a GPU embedder, switch to `embedder_accelerator = "gpu"` and use a GPU-capable instance type like `g6f.large`.

That keeps the fixed AWS baseline much lower than the original NAT + Aurora setup. If you leave `domain_name = null`, the app endpoint is still tied to the current ECS task public IP. If you set `domain_name`, the stack creates an ALB and Route 53 alias records so you get a stable hostname, at the cost of the ALB hourly charge.

## Domain and HTTPS

Set these variables in `terraform.tfvars` when you want a custom hostname:

- `domain_name = "ancillabot.com"`
- `create_api_record = true` if you want `api.ancillabot.com` for the API
- `create_www_record = true` if you also want `www.ancillabot.com`
- `enable_https_listener = false` for the first apply
- `basic_auth_enabled = true` if you want simple auth on the public API
- `basic_auth_username = "ancilla"` or another username of your choice

The first apply creates:

- the ALB
- Route 53 alias records pointing the domain at the ALB
- an ACM certificate request
- ACM DNS validation CNAMEs inside the hosted zone

Once the ACM certificate status is `ISSUED`, set `enable_https_listener = true` and apply again. That second apply adds:

- an HTTPS listener on `443`
- an HTTP `80 -> 443` redirect

When `basic_auth_enabled = true`, ECS gets:

- `ANCILLA_SERVER_BASIC_AUTH_USERNAME` as plaintext env
- `ANCILLA_SERVER_BASIC_AUTH_PASSWORD` from Secrets Manager

The server leaves `/healthz` unauthenticated so the target group health check continues to work.

## Commands

OpenTofu:

```bash
cd infra/tofu
cp terraform.tfvars.example terraform.tfvars
tofu init
tofu plan
```

Terraform:

```bash
cd infra/tofu
cp terraform.tfvars.example terraform.tfvars
terraform init
terraform plan
```

Do not run `apply` until you are ready to create resources.
