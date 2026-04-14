# Ancilla MVP Infra

This directory contains Terraform/OpenTofu-compatible HCL for the first deployable MVP shape.

It creates:

- a dedicated VPC when `create_network = true`
- 2 public subnets, 2 private app subnets, and 2 private DB subnets
- an Internet Gateway, public route table, private route tables, and a single NAT Gateway
- an internet-facing ALB
- an ECS Fargate service for the Rust API
- an Aurora PostgreSQL cluster for runtime state and retrieval data
- an S3 bucket for artifacts/imports
- an ECR repository for the app image
- IAM roles for ECS execution, Bedrock invocation, and S3 access
- a Secrets Manager secret for `DATABASE_URL`

If you already have a network, set `create_network = false` and provide:

- an existing `vpc_id`
- public subnets for the ALB
- private app subnets for ECS
- private DB subnets for Aurora

## Preconditions

- Build and push the app image to the managed ECR repository, or set `container_image` directly.
- Use an Aurora PostgreSQL engine version that supports `pgvector`.
- If `create_network = true`, use at least two availability zones and matching subnet CIDR lists.
- After the database is live, connect once and run:

```sql
CREATE EXTENSION IF NOT EXISTS vector;
```

The app migrations will create the schema, but the extension must exist first.

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
