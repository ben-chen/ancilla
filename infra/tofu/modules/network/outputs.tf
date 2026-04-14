output "vpc_id" {
  description = "Created VPC ID."
  value       = aws_vpc.this.id
}

output "public_subnet_ids" {
  description = "Created public subnet IDs."
  value       = aws_subnet.public[*].id
}

output "private_app_subnet_ids" {
  description = "Created private application subnet IDs reserved for future private service deployments."
  value       = aws_subnet.private_app[*].id
}

output "private_db_subnet_ids" {
  description = "Created private database subnet IDs."
  value       = aws_subnet.private_db[*].id
}
