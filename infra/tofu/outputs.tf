output "vpc_id" {
  description = "VPC ID used by the Ancilla MVP stack."
  value       = local.vpc_id
}

output "public_subnet_ids" {
  description = "Public subnet IDs used by the ALB."
  value       = local.public_subnet_ids
}

output "private_app_subnet_ids" {
  description = "Private application subnet IDs used by ECS."
  value       = local.private_app_subnet_ids
}

output "private_db_subnet_ids" {
  description = "Private database subnet IDs used by Aurora."
  value       = local.private_db_subnet_ids
}

output "alb_dns_name" {
  description = "Public DNS name for the Ancilla MVP ALB."
  value       = aws_lb.app.dns_name
}

output "api_base_url" {
  description = "HTTP base URL for the MVP API."
  value       = "http://${aws_lb.app.dns_name}"
}

output "ecr_repository_url" {
  description = "Managed ECR repository URL for the Ancilla app image."
  value       = aws_ecr_repository.app.repository_url
}

output "assets_bucket_name" {
  description = "S3 bucket name for artifacts and imports."
  value       = aws_s3_bucket.assets.bucket
}

output "aurora_cluster_endpoint" {
  description = "Aurora writer endpoint."
  value       = aws_rds_cluster.aurora.endpoint
}

output "database_url_secret_arn" {
  description = "Secrets Manager ARN that stores DATABASE_URL for ECS."
  value       = aws_secretsmanager_secret.database_url.arn
}
