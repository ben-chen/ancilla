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

output "alb_dns_name" {
  description = "Application Load Balancer DNS name when domain support is enabled."
  value       = local.domain_enabled ? aws_lb.app[0].dns_name : null
}

output "app_domain_name" {
  description = "Configured public domain name for Ancilla when enabled."
  value       = local.domain_name
}

output "app_hostnames" {
  description = "Public hostnames routed to the ALB."
  value       = local.app_hostnames
}

output "app_url" {
  description = "Primary public URL for Ancilla when a domain is configured."
  value       = local.domain_enabled ? format("%s://%s", var.enable_https_listener ? "https" : "http", local.domain_name) : null
}

output "basic_auth_enabled" {
  description = "Whether HTTP Basic auth is enabled for the public API."
  value       = var.basic_auth_enabled
}

output "basic_auth_username" {
  description = "HTTP Basic auth username when enabled."
  value       = var.basic_auth_enabled ? var.basic_auth_username : null
}

output "basic_auth_password_secret_arn" {
  description = "Secrets Manager ARN for the generated HTTP Basic auth password."
  value       = var.basic_auth_enabled ? aws_secretsmanager_secret.basic_auth_password[0].arn : null
}

output "acm_certificate_arn" {
  description = "ACM certificate ARN for the Ancilla public domain when enabled."
  value       = local.domain_enabled ? aws_acm_certificate.app[0].arn : null
}

output "acm_validation_records" {
  description = "DNS validation records created in Route 53 for the ACM certificate."
  value = local.domain_enabled ? {
    for name, record in aws_route53_record.acm_validation : name => {
      fqdn    = record.fqdn
      type    = record.type
      records = record.records
    }
  } : {}
}

output "ecr_repository_url" {
  description = "Managed ECR repository URL for the Ancilla app image."
  value       = aws_ecr_repository.app.repository_url
}

output "embedder_ecr_repository_url" {
  description = "Managed ECR repository URL for the embedder image."
  value       = aws_ecr_repository.embedder.repository_url
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

output "embedder_private_url" {
  description = "Private URL used by ancilla-server to reach the embedder service."
  value       = local.embedder_private_url
}

output "embedder_public_ip" {
  description = "Public IP of the embedder instance when enabled."
  value       = var.embedder_enabled ? aws_instance.embedder[0].public_ip : null
}

output "embedder_instance_id" {
  description = "EC2 instance ID for the embedder host when enabled."
  value       = var.embedder_enabled ? aws_instance.embedder[0].id : null
}
