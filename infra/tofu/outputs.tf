output "vpc_id" {
  description = "VPC ID used by the Ancilla MVP stack."
  value       = local.vpc_id
}

output "public_subnet_ids" {
  description = "Public subnet IDs used by the ECS service."
  value       = local.public_subnet_ids
}

output "private_app_subnet_ids" {
  description = "Private application subnet IDs reserved for future private service deployments."
  value       = local.private_app_subnet_ids
}

output "private_db_subnet_ids" {
  description = "Private database subnet IDs used by PostgreSQL."
  value       = local.private_db_subnet_ids
}

output "ecs_cluster_name" {
  description = "ECS cluster name for the Ancilla API."
  value       = aws_ecs_cluster.app.name
}

output "ecs_service_name" {
  description = "ECS service name for the Ancilla API."
  value       = aws_ecs_service.app.name
}

output "app_port" {
  description = "Public port exposed directly by the ECS task."
  value       = var.app_port
}

output "ecr_repository_url" {
  description = "Managed ECR repository URL for the Ancilla app image."
  value       = aws_ecr_repository.app.repository_url
}

output "assets_bucket_name" {
  description = "S3 bucket name for artifacts and imports."
  value       = aws_s3_bucket.assets.bucket
}

output "database_endpoint" {
  description = "RDS PostgreSQL endpoint."
  value       = aws_db_instance.postgres.address
}

output "database_url_secret_arn" {
  description = "Secrets Manager ARN that stores DATABASE_URL for ECS."
  value       = aws_secretsmanager_secret.database_url.arn
}
