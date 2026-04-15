# Ancilla MVP Infra

This directory contains Terraform/OpenTofu-compatible HCL for a low-cost deployable MVP shape.

It creates infrastructure for the `ancilla-server` API binary:

- a dedicated VPC when `create_network = true`
- 2 public subnets, 2 reserved private app subnets, and 2 private DB subnets
- an Internet Gateway and route tables, with no NAT Gateway by default
- a public ECS Fargate service for the Rust API server
- an optional always-on GPU EC2 instance for the separate `ancilla-embedder` service
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
- Make sure the AWS account has non-zero EC2 GPU quota in the target region if you want the dedicated embedder host enabled. In this account, both `Running On-Demand G and VT instances` and `All G and VT Spot Instance Requests` are currently `0`, so the checked-in `terraform.tfvars` leaves `embedder_enabled = false` until quota is raised.
- If `create_network = true`, use at least two availability zones and matching subnet CIDR lists.
- After the database is live, connect once and run:

```sql
CREATE EXTENSION IF NOT EXISTS vector;
```

The app migrations will create the schema, but the extension must exist first.

## Cost Shape

The defaults are tuned for a personal side project:

- ECS runs directly in public subnets with a public IP
- no ALB
- no NAT Gateway
- PostgreSQL defaults to `db.t4g.micro`
- Fargate defaults to ARM `256 CPU / 1024 MiB`
- the optional embedder defaults to a single `g6f.large` EC2 host

That keeps the fixed AWS baseline much lower than the original ALB + NAT + Aurora setup. The tradeoff is that the app endpoint is tied to the current ECS task public IP instead of a stable load balancer DNS name, and the optional GPU spend appears only if you enable the dedicated embedder host.

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
